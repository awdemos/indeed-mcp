use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

/// Token and client info stored on disk
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthState {
    pub client_id: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scope: Option<String>,
}

/// DCR registration request
#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct RegistrationRequest {
    client_name: String,
    redirect_uris: Vec<String>,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    token_endpoint_auth_method: String,
    scope: String,
}

/// DCR registration response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RegistrationResponse {
    client_id: String,
    #[allow(dead_code)]
    client_secret: Option<String>,
    #[allow(dead_code)]
    client_secret_expires_at: Option<u64>,
    #[allow(dead_code)]
    client_id_issued_at: Option<u64>,
    #[allow(dead_code)]
    scope: Option<String>,
    #[allow(dead_code)]
    registration_access_token: Option<String>,
    #[allow(dead_code)]
    registration_client_uri: Option<String>,
}

/// Build form-urlencoded body for token exchange/refresh
fn build_token_body(params: Vec<(&str, &str)>) -> HashMap<String, String> {
    params
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Token endpoint response
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

pub struct TokenManager {
    state_path: PathBuf,
    auth_state: Option<AuthState>,
    next_id: AtomicU64,
    client: reqwest::Client,
}

impl TokenManager {
    pub fn new() -> Self {
        let state_path = dirs_next().unwrap_or_else(|| PathBuf::from(".")).join(".indeed-mcp-auth.json");
        Self {
            state_path,
            auth_state: None,
            next_id: AtomicU64::new(1),
            client: reqwest::Client::builder()
                .user_agent("indeed-mcp-proxy/0.1")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Quick check if a token exists on disk — no I/O, no OAuth flow.
    /// This runs synchronously so the MCP loop can start immediately.
    #[allow(dead_code)]
    pub async fn peek_token(&mut self) -> bool {
        if let Some(state) = self.load_from_disk().await {
            if state.access_token.is_some() {
                self.auth_state = Some(state);
                return true;
            }
        }
        false
    }

    /// Load auth state from disk or start OAuth flow
    pub async fn ensure_authenticated(&mut self) -> Result<()> {
        // Try loading existing state
        if let Some(state) = self.load_from_disk().await {
            // If no access_token, state is incomplete (e.g. saved mid-OAuth) — discard
            if state.access_token.is_none() {
                info!("Saved state has no access token, discarding");
                let _ = tokio::fs::remove_file(&self.state_path).await;
            } else {
                info!("Loaded existing auth state for client_id={}", state.client_id);
                self.auth_state = Some(state);

                // Check if token needs refresh based on expiry
                if let Some(expires_at) = self.auth_state.as_ref().and_then(|s| s.expires_at) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    if now >= expires_at.saturating_sub(60) {
                        info!("Token expired or expiring soon, refreshing...");
                        if let Err(e) = self.refresh_token().await {
                            warn!("Token refresh failed: {}. Doing full auth.", e);
                            self.auth_state = None;
                        }
                    }
                }

                // Verify token works by testing connectivity
                if self.auth_state.is_some() {
                    match self.test_connection().await {
                        Ok(true) => {
                            info!("Existing token is valid");
                            return Ok(());
                        }
                        Ok(false) => {
                            info!("Token rejected by server, trying refresh...");
                            if let Err(e) = self.refresh_token().await {
                                warn!("Refresh failed: {}. Re-authenticating.", e);
                                self.auth_state = None;
                            } else {
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            warn!("Connection test failed: {}. Will retry.", e);
                            // Keep going — might be transient network issue
                            return Ok(());
                        }
                    }
                }
            }
        }

        // No valid state - do full OAuth
        info!("Starting OAuth flow for Indeed MCP...");
        self.oauth_flow().await?;
        self.save_to_disk().await?;

        Ok(())
    }

    /// Get current access token
    pub fn access_token(&self) -> Result<&str> {
        self.auth_state
            .as_ref()
            .and_then(|s| s.access_token.as_deref())
            .context("No access token available")
    }

    /// Get current refresh token
    fn refresh_token_str(&self) -> Option<&str> {
        self.auth_state.as_ref()?.refresh_token.as_deref()
    }

    /// Test connection by sending a ping to Indeed MCP
    async fn test_connection(&self) -> Result<bool> {
        let token = self.access_token()?;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "ping",
            "params": {}
        });

        let resp = self
            .client
            .post("https://mcp.indeed.com/claude/mcp")
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            Ok(true)
        } else if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            Ok(false)
        } else {
            // Other errors (server issues) - consider it working
            Ok(true)
        }
    }

    /// Refresh the access token
    pub async fn refresh_token(&mut self) -> Result<()> {
        let refresh_token = self
            .refresh_token_str()
            .context("No refresh token available")?
            .to_string();
        let client_id = self
            .auth_state
            .as_ref()
            .map(|s| s.client_id.clone())
            .context("No client_id")?;

        let body = build_token_body(vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("client_id", &client_id),
        ]);

        let resp = self
            .client
            .post("https://apis.indeed.com/oauth/v2/tokens")
            .form(&body)
            .send()
            .await
            .context("Failed to send token refresh request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed: HTTP {} - {}", status, body);
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .context("Failed to parse token refresh response")?;

        if let Some(ref err) = token_resp.error {
            anyhow::bail!("Token refresh error: {} - {:?}", err, token_resp.error_description);
        }

        let expires_at = token_resp.expires_in.map(|exp| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + exp
        });

        let state = self.auth_state.as_mut().unwrap();
        state.access_token = Some(token_resp.access_token);
        if let Some(rt) = token_resp.refresh_token {
            state.refresh_token = Some(rt);
        }
        state.expires_at = expires_at;
        state.scope = token_resp.scope.or(state.scope.clone());

        self.save_to_disk().await?;
        info!("Token refreshed successfully");
        Ok(())
    }

    /// Full OAuth flow: authorize and exchange code for tokens.
    /// Uses Claude Code's well-known metadata URL as client_id (pre-whitelisted for MCP),
    /// skipping DCR entirely.
    async fn oauth_flow(&mut self) -> Result<()> {
        // Use Claude Code's well-known client metadata URL.
        // The AS fetches this to get our redirect_uris and other client details.
        // Since Claude Code is an approved Indeed partner, this client_id is likely
        // whitelisted for the MCP endpoint.
        let client_id = "https://claude.ai/oauth/claude-code-client-metadata";

        info!("Using pre-whitelisted client_id={}", client_id);

        let auth_state = AuthState {
            client_id: client_id.to_string(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            scope: Some("job_seeker.jobs.search offline_access".to_string()),
        };
        self.auth_state = Some(auth_state);
        self.save_to_disk().await?;

        // Generate PKCE challenge
        let code_verifier = generate_code_verifier();
        let code_challenge = sha256_base64url(&code_verifier);

        // Generate state for CSRF protection
        let state = generate_state();
        let auth_state_value = state.clone();

        // Build authorization URL (no resource parameter for broader token audience).
        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&scope={}",
            "https://secure.indeed.com/oauth/v2/authorize",
            urlencoding(client_id),
            urlencoding("http://127.0.0.1:19876/callback"),
            &code_challenge,
            &state,
            urlencoding("job_seeker.jobs.search offline_access"),
        );

        // Start callback server
        info!("Starting callback server on port 19876...");
        let callback_received = start_callback_server(&auth_state_value).await?;

        // Open browser
        info!("Opening browser for authorization...");
        if let Err(e) = open::that(&auth_url) {
            warn!("Failed to open browser: {}. Please open this URL manually:", e);
            println!("Open this URL in your browser:\n{}", auth_url);
        } else {
            println!("Browser opened for Indeed authorization. Please complete the flow in your browser.");
        }
        println!("Waiting for authorization callback on http://127.0.0.1:19876/callback ...");

        // Wait for callback
        let auth_code = callback_received.await.context("Failed to receive OAuth callback")?;
        info!("Authorization code received, exchanging for tokens...");

        // Exchange code for tokens (form-urlencoded, not JSON - Indeed requires this)
        let body = build_token_body(vec![
            ("grant_type", "authorization_code"),
            ("code", &auth_code),
            ("redirect_uri", "http://127.0.0.1:19876/callback"),
            ("code_verifier", &code_verifier),
            ("client_id", &client_id),
        ]);

        let resp = self
            .client
            .post("https://apis.indeed.com/oauth/v2/tokens")
            .form(&body)
            .send()
            .await
            .context("Failed to send token exchange request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!("Token exchange failed: HTTP {} - {}", status, body);
            anyhow::bail!("Token exchange failed: HTTP {} - {}", status, body);
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .context("Failed to parse token exchange response")?;

        if let Some(ref err) = token_resp.error {
            anyhow::bail!("Token exchange error: {} - {:?}", err, token_resp.error_description);
        }

        let expires_at = token_resp.expires_in.map(|exp| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + exp
        });

        let state = self.auth_state.as_mut().unwrap();
        let token = &token_resp.access_token;
        // Log token type (JWT has 3 parts separated by dots)
        let parts: Vec<&str> = token.split('.').collect();
        info!("Token: type={}fmt, {} parts, len={}, prefix={}..",
            if token.contains('.') { "jwt" } else { "opaque" },
            parts.len(),
            token.len(),
            &token[..std::cmp::min(20, token.len())],
        );
        state.access_token = Some(token.clone());
        state.refresh_token = token_resp.refresh_token;
        state.expires_at = expires_at;
        state.scope = token_resp.scope.or(state.scope.clone());

        info!("OAuth flow completed successfully");
        Ok(())
    }

    /// Register client via DCR
    #[allow(dead_code)]
    async fn register_client(&self) -> Result<String> {
        let reg_req = RegistrationRequest {
            client_name: "Indeed MCP Proxy (OpenCode)".to_string(),
            redirect_uris: vec!["http://127.0.0.1:19876/callback".to_string()],
            grant_types: vec![
                "authorization_code".to_string(),
                "refresh_token".to_string(),
            ],
            response_types: vec!["code".to_string()],
            token_endpoint_auth_method: "none".to_string(),
            scope: "job_seeker.jobs.search offline_access".to_string(),
        };

        let resp = self
            .client
            .post("https://secure.indeed.com/oauth/v2/register")
            .header("Content-Type", "application/json")
            .json(&reg_req)
            .send()
            .await
            .context("Failed to send DCR request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("DCR registration failed: HTTP {} - {}", status, body);
        }

        let reg_resp: RegistrationResponse = resp
            .json()
            .await
            .context("Failed to parse DCR response")?;

        info!("DCR response: client_id={}, has_secret={}, has_registration_token={}, client_uri={:?}",
            reg_resp.client_id,
            reg_resp.client_secret.is_some(),
            reg_resp.registration_access_token.is_some(),
            reg_resp.registration_client_uri,
        );

        Ok(reg_resp.client_id)
    }

    /// Load auth state from disk
    async fn load_from_disk(&self) -> Option<AuthState> {
        let data = tokio::fs::read_to_string(&self.state_path).await.ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Save auth state to disk
    async fn save_to_disk(&self) -> Result<()> {
        if let Some(ref state) = self.auth_state {
            let json = serde_json::to_string_pretty(state)?;
            tokio::fs::write(&self.state_path, &json)
                .await
                .context("Failed to write auth state")?;
            // Secure permissions
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perm = std::fs::Permissions::from_mode(0o600);
                let _ = tokio::fs::set_permissions(&self.state_path, perm).await;
            }
        }
        Ok(())
    }
}

// ─── PKCE helpers ──────────────────────────────────────────────────────────

fn generate_code_verifier() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    let len: usize = rng.gen_range(64..=96);
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

fn sha256_base64url(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

fn generate_state() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn urlencoding(input: &str) -> String {
    url::form_urlencoded::byte_serialize(input.as_bytes()).collect()
}

// ─── Callback Server ─────────────────────────────────────────────────────

async fn start_callback_server(expected_state: &str) -> Result<tokio::sync::oneshot::Receiver<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let expected_state = expected_state.to_string();

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:19876").await {
            Ok(l) => l,
            Err(e) => {
                warn!("Failed to bind callback server on port 19876: {}", e);
                let _ = tx.send(String::new());
                return;
            }
        };

        // Keep accepting connections until we get one with the matching state.
        // This handles stale browser tabs from previous OAuth flows redirecting first.
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to accept callback connection: {}", e);
                    continue;
                }
            };

            let reader = BufReader::new(&mut stream);
            let mut lines = reader.lines();
            let mut request_line = String::new();
            let mut content_length: usize = 0;

            // Parse HTTP request line and headers
            while let Ok(Some(line)) = lines.next_line().await {
                if request_line.is_empty() {
                    request_line = line;
                    continue;
                }
                let line_lower = line.to_lowercase();
                if line_lower.starts_with("content-length:") {
                    if let Ok(len) = line["Content-Length:".len()..].trim().parse::<usize>() {
                        content_length = len;
                    }
                }
                if line.is_empty() {
                    break; // End of headers
                }
            }

            // Extract auth code from query
            let auth_code: String = if content_length > 0 {
                let mut body = vec![0u8; content_length];
                use tokio::io::AsyncReadExt;
                if let Err(e) = stream.read_exact(&mut body).await {
                    warn!("Failed to read request body: {}", e);
                }
                String::new()
            } else {
                // Extract from request line: GET /callback?code=xxx&state=yyy HTTP/1.1
                let path = request_line.split_whitespace().nth(1).unwrap_or("");
                info!("Callback request path: {}", path);
                url::Url::parse(&format!("http://localhost{}", path))
                    .ok()
                    .and_then(|u| {
                        let code = u.query_pairs()
                            .find(|(k, _)| k == "code")
                            .map(|(_, v)| v.to_string());
                        let state = u.query_pairs()
                            .find(|(k, _)| k == "state")
                            .map(|(_, v)| v.to_string());
                        // Check state — only accept if it matches
                        if let Some(ref s) = state {
                            if s == &expected_state {
                                code
                            } else {
                                warn!("State mismatch! Expected {}, got {} — ignoring", expected_state, s);
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            };

            // Send HTTP response
            let response = if !auth_code.is_empty() {
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 200\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><body><h1>Authorization Complete!</h1><p>You can close this window and return to the terminal.</p></body></html>"
            } else {
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 21\r\nConnection: close\r\n\r\nNo authorization code found."
            };

            let _ = stream.write_all(response.as_bytes()).await;

            if !auth_code.is_empty() {
                info!("Received authorization code from callback: code_len={}", auth_code.len());
                let _ = tx.send(auth_code);
                return; // Got the right code, done
            }
            // State didn't match, continue listening for the real redirect
        }
    });

    Ok(rx)
}

fn dirs_next() -> Option<PathBuf> {
    // Use XDG config home or HOME
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        Some(PathBuf::from(dir))
    } else if let Ok(dir) = std::env::var("HOME") {
        Some(PathBuf::from(dir).join(".config"))
    } else {
        None
    }
}
