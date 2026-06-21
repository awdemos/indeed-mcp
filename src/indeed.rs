use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{info, warn};

const GRAPHQL_URL: &str = "https://apis.indeed.com/graphql";

pub struct IndeedClient {
    client: reqwest::Client,
}

impl IndeedClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("indeed-mcp/0.1.0")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    #[allow(dead_code)]
    pub fn new_with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    #[allow(dead_code)]
    /// Test if a token is usable by calling the userinfo endpoint.
    pub async fn test_token(&self, token: &str) -> Result<bool> {
        let resp = self
            .client
            .get("https://apis.indeed.com/sso/idp/userinfo")
            .header("Authorization", format!("Bearer {}", token))
            .header("User-Agent", "indeed-mcp/0.1.0")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;

        match resp {
            Ok(r) => Ok(r.status().is_success()),
            Err(e) => {
                warn!("Token test failed: {}", e);
                // Network error — assume token is still valid
                Ok(true)
            }
        }
    }

    /// Search for jobs using Indeed's GraphQL API.
    pub async fn search_jobs(
        &self,
        token: &str,
        keyword: &str,
        location: Option<&str>,
        radius: Option<u32>,
        limit: Option<u32>,
    ) -> Result<Value> {
        let limit = limit.unwrap_or(10).min(50);
        let radius = radius.unwrap_or(25);

        let location_input = match location {
            Some(loc) => format!(
                r#"{{ radius: {}, radiusUnit: MILES, where: "{}" }}"#,
                radius,
                loc.replace('"', r#"\""#)
            ),
            None => "null".to_string(),
        };

        let query = format!(
            r#"
            query SearchJobs {{
                jobSearch(
                    location: {},
                    what: "{}",
                    limit: {}
                ) {{
                    results {{
                        job {{
                            title
                            sourceEmployerName
                            locationName
                            salary {{
                                currency
                                period
                                min
                                max
                            }}
                            url
                            key
                            publishedDate
                        }}
                    }}
                    totalResults
                }}
            }}
            "#,
            location_input,
            keyword.replace('"', r#"\""#),
            limit,
        );

        let body = serde_json::json!({
            "query": query,
            "variables": {}
        });

        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .context("Failed to send GraphQL request")?;

        let status = resp.status();
        let response_body: Value = resp
            .json()
            .await
            .context("Failed to parse GraphQL response as JSON")?;

        if !status.is_success() {
            let msg = response_body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("GraphQL request failed");
            anyhow::bail!("HTTP {} from Indeed GraphQL: {}", status, msg);
        }

        if let Some(errors) = response_body.get("errors") {
            if errors.as_array().map_or(false, |a| !a.is_empty()) {
                let first = &errors[0];
                let msg = first
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown GraphQL error");
                info!("GraphQL returned errors: {}", msg);
                anyhow::bail!("Indeed API: {}", msg);
            }
        }

        Ok(response_body)
    }

    /// Get detailed information for a specific job.
    pub async fn get_job_detail(&self, token: &str, job_id: &str) -> Result<Value> {
        let query = format!(
            r#"
            query GetJobDetail {{
                job(id: "{}") {{
                    result {{
                        title
                        sourceEmployerName
                        locationName
                        salary {{
                            currency
                            period
                            min
                            max
                        }}
                        url
                        key
                        publishedDate
                        description {{ text }}
                        company {{
                            name
                            rating
                            reviewCount
                            locationName
                        }}
                        attributes {{
                            name
                            label
                        }}
                        jobTypes
                        remoteWorkType
                    }}
                }}
            }}
            "#,
            job_id,
        );

        let body = serde_json::json!({
            "query": query,
            "variables": {}
        });

        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .context("Failed to send GraphQL request")?;

        let status = resp.status();
        let response_body: Value = resp
            .json()
            .await
            .context("Failed to parse GraphQL response")?;

        if !status.is_success() {
            let msg = response_body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("GraphQL request failed");
            anyhow::bail!("HTTP {} from Indeed GraphQL: {}", status, msg);
        }

        if let Some(errors) = response_body.get("errors") {
            if errors.as_array().map_or(false, |a| !a.is_empty()) {
                let first = &errors[0];
                let msg = first
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown GraphQL error");
                info!("GraphQL errors: {}", msg);
                anyhow::bail!("Indeed API: {}", msg);
            }
        }

        Ok(response_body)
    }
}
