# Indeed MCP Server

An [MCP (Model Context Protocol)](https://modelcontextprotocol.io) server that brings Indeed job search to AI assistants. Search jobs by keyword and location, get detailed job postings — all through natural language.

Built with [rmcp](https://crates.io/crates/rmcp) and Indeed's GraphQL API.

## Tools

### `jobs_search`
Search for jobs on Indeed by keyword, location, radius, and result limit.

| Parameter   | Type     | Required | Description |
|------------|----------|----------|-------------|
| `keyword`  | `string` | ✅       | Job title, keywords, or skill |
| `location` | `string` | ❌       | City, state, or region |
| `radius`   | `uint32` | ❌       | Miles from location (default: 25) |
| `limit`    | `uint32` | ❌       | Max results (default: 10, max: 50) |

### `job_detail`
Get comprehensive information about a specific job posting.

| Parameter | Type     | Required | Description |
|-----------|----------|----------|-------------|
| `job_id`  | `string` | ✅       | Indeed job ID from search results |

## Setup

### Prerequisites
- [Rust](https://rustup.rs) 1.75+
- An Indeed account (free)

### Install

```bash
git clone https://github.com/awdemos/indeed-mcp.git
cd indeed-mcp
cargo build --release
cp target/release/indeed-mcp ~/.local/bin/
```

### MCP Client Configuration

<details>
<summary><b>OpenCode</b></summary>

Add to `opencode.json`:

```json
{
  "mcp": {
    "indeed": {
      "type": "local",
      "command": ["/home/youruser/.local/bin/indeed-mcp"],
      "enabled": true
    }
  }
}
```
</details>

<details>
<summary><b>Claude Code</b></summary>

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "indeed": {
      "type": "stdio",
      "command": "/home/youruser/.local/bin/indeed-mcp"
    }
  }
}
```
</details>

<details>
<summary><b>Codex CLI</b></summary>

Add to `.codex/settings.json` in your project root:

```json
{
  "mcpServers": {
    "indeed": {
      "type": "stdio",
      "command": "/home/youruser/.local/bin/indeed-mcp"
    }
  }
}
```
</details>

<details>
<summary><b>Claude Desktop</b></summary>

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "indeed": {
      "command": "/home/youruser/.local/bin/indeed-mcp"
    }
  }
}
```
</details>

<details>
<summary><b>Cursor</b></summary>

In Cursor settings → Features → MCP Servers, add:

```
Type:     stdio
Name:     indeed
Command:  /home/youruser/.local/bin/indeed-mcp
```
</details>

<details>
<summary><b>Continue (VS Code / JetBrains)</b></summary>

Add to `~/.continue/config.json`:

```json
{
  "experimental": {
    "mcpServers": {
      "indeed": {
        "type": "stdio",
        "command": "/home/youruser/.local/bin/indeed-mcp"
      }
    }
  }
}
```
</details>

<details>
<summary><b>Any MCP client (generic)</b></summary>

The server speaks standard MCP over stdio. Configure your client to spawn the binary as a subprocess with no arguments. JSON-RPC messages are exchanged over stdin/stdout.

```bash
# Test the server works with any client:
echo '{"jsonrpc":"2.0","id":"1","method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}' | /home/youruser/.local/bin/indeed-mcp
```
</details>

### Auth

On first use, the server opens a browser for Indeed OAuth authorization. Sign in with your Indeed account and approve the scopes. Tokens are cached locally.

## Why this exists

Indeed released an MCP server, but in their infinite wisdom, only made it compatible with Claude Desktop — which means it does not work with OpenCode, Codex, or most other MCP clients by default. I had to reverse-engineer the OAuth flow and wire it up properly with the rmcp Rust library so it speaks standard MCP over stdio. Figured I'd share it with the community so nobody else has to waste an afternoon on this.

## How it works

1. The MCP client sends a `jobs_search` or `job_detail` request
2. The server checks for a cached OAuth token (refreshing if expired)
3. Requests the Indeed GraphQL API with the token
4. Returns structured JSON results to the assistant

## License

MIT

---

*Built by engineers who vibe.*  
[Vibe Coding Agency](https://vibecodingagency.com) · [KV Cache Store](https://kvcachestore.com)
