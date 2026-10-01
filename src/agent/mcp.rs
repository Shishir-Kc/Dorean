//! MCP (Model Context Protocol) client stub: config + tool bridging.
//!
//! Full JSON-RPC stdio/SSE transport arrives incrementally; this module ships
//! the stable surface today: `.dorean/mcp.json` server registry, `mcp__`
//! tool-name mapping, and a graceful disabled-by-default path so unconfigured
//! runs never break. Servers marked `enabled` resolve to external commands;
//! actual spawn/transport is feature-gated behind `McpClient::is_configured`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One MCP server entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Registry: server name → entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: HashMap<String, McpServer>,
}

impl McpConfig {
    /// Load from `.dorean/mcp.json` or `~/.dorean/mcp.json`. Missing → empty.
    pub fn load(cwd: &Path) -> Self {
        for path in [
            cwd.join(".dorean").join("mcp.json"),
            dirs::home_dir()
                .map(|h| h.join(".dorean").join("mcp.json"))
                .unwrap_or_default(),
        ] {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(cfg) = serde_json::from_str::<McpConfig>(&text)
            {
                return cfg;
            }
        }
        McpConfig::default()
    }

    pub fn enabled_servers(&self) -> Vec<(&String, &McpServer)> {
        let mut servers: Vec<_> = self.servers.iter().filter(|(_, s)| s.enabled).collect();
        servers.sort_by(|a, b| a.0.cmp(b.0));
        servers
    }

    pub fn is_configured(&self) -> bool {
        !self.enabled_servers().is_empty()
    }
}

/// Map an MCP tool to a dorean tool name: `mcp__<server>__<tool>`.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Split an `mcp__` name back into (server, tool).
pub fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        None
    } else {
        Some((server, tool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_round_trips() {
        let name = mcp_tool_name("fs", "read");
        assert_eq!(parse_mcp_tool_name(&name), Some(("fs", "read")));
        assert!(parse_mcp_tool_name("read").is_none());
    }

    #[test]
    fn missing_config_is_empty_and_disabled() {
        let cfg = McpConfig::load(Path::new("/nonexistent-dorean-xyz"));
        assert!(!cfg.is_configured());
    }
}
