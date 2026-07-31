use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::DoreanError;
use crate::permissions::PermissionMode;

/// Which provider layer to talk to. OpenRouter is the only provider today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    OpenRouter,
}

impl FromStr for Provider {
    type Err = DoreanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openrouter" => Ok(Provider::OpenRouter),
            other => Err(DoreanError::Config(format!(
                "unknown provider `{other}` (expected `openrouter`)"
            ))),
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("openrouter")
    }
}

/// Runtime configuration for dorean, loaded from `~/.dorean/config.json`
/// with `DOREAN_*` environment overrides layered on top.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: Provider,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub telemetry: bool,
    pub theme: Option<String>,
    pub max_tokens: Option<u32>,
    pub max_turns: u32,
    /// None means the caller picks the default (Ask in the TUI, Allow in
    /// non-interactive `-m` mode). See [`PermissionMode`].
    pub permission_mode: Option<PermissionMode>,
    /// Glob patterns of paths that may never be touched.
    pub permission_deny: Vec<String>,
    /// Roots the agent may write inside; defaults to the working directory.
    pub safe_dirs: Vec<PathBuf>,
    /// Max work rounds per sub-agent in an orchestrated run.
    pub sub_agent_rounds: usize,
    /// Per-sub-agent model overrides (agent name → model id). Applied when a
    /// sub-agent has no explicit model from the `/make` model picker.
    pub agent_models: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: Provider::default(),
            model: None,
            base_url: None,
            openrouter_api_key: None,
            telemetry: true,
            theme: None,
            max_tokens: None,
            max_turns: 20,
            permission_mode: None,
            permission_deny: Vec::new(),
            safe_dirs: Vec::new(),
            sub_agent_rounds: 10,
            agent_models: HashMap::new(),
        }
    }
}

impl Config {
    /// The config directory, `~/.dorean`.
    pub fn config_dir() -> Result<PathBuf, DoreanError> {
        let home = dirs::home_dir()
            .ok_or_else(|| DoreanError::Config("cannot determine home directory".to_string()))?;
        Ok(home.join(".dorean"))
    }

    /// Path to the config file, `~/.dorean/config.json`.
    pub fn config_path() -> Result<PathBuf, DoreanError> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    /// Load config from disk, returning defaults when no file exists yet.
    pub fn load() -> Result<Self, DoreanError> {
        Self::load_from(&Self::config_path()?)
    }

    /// Persist this config to `~/.dorean/config.json`, creating the directory
    /// if needed. Unknown fields in an existing file are lost, matching the
    /// struct's field set.
    pub fn save(&self) -> Result<(), DoreanError> {
        Self::save_to(self, &Self::config_path()?)
    }

    /// Write the config to a specific path (used by tests).
    pub fn save_to(&self, path: &Path) -> Result<(), DoreanError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(DoreanError::Io)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| DoreanError::Config(format!("failed to serialize config: {e}")))?;
        let mut text = json;
        text.push('\n');
        std::fs::write(path, text).map_err(DoreanError::Io)
    }

    /// Load config from a specific path (used by tests). A missing file
    /// yields the defaults; a malformed file is a hard error.
    pub fn load_from(path: &Path) -> Result<Self, DoreanError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                DoreanError::Config(format!("invalid config at {}: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(DoreanError::Io(e)),
        }
    }

    /// Overlay `DOREAN_*` environment variables on top of the loaded config.
    /// Values that fail to parse are ignored, matching common `VAR=bad` cases.
    pub fn apply_env(mut self) -> Self {
        if let Ok(value) = std::env::var("DOREAN_PROVIDER")
            && let Ok(provider) = Provider::from_str(&value)
        {
            self.provider = provider;
        }
        if let Ok(value) = std::env::var("DOREAN_MODEL") {
            self.model = Some(value);
        }
        if let Ok(value) = std::env::var("DOREAN_BASE_URL") {
            self.base_url = Some(value);
        }
        if let Ok(value) = std::env::var("DOREAN_OPENROUTER_API_KEY") {
            self.openrouter_api_key = Some(value);
        }
        if let Ok(value) = std::env::var("DOREAN_TELEMETRY") {
            self.telemetry = matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Ok(value) = std::env::var("DOREAN_THEME") {
            self.theme = Some(value);
        }
        if let Ok(value) = std::env::var("DOREAN_MAX_TOKENS") {
            self.max_tokens = value.parse().ok();
        }
        if let Ok(value) = std::env::var("DOREAN_MAX_TURNS") {
            self.max_turns = value.parse().unwrap_or(self.max_turns);
        }
        if let Ok(value) = std::env::var("DOREAN_PERMISSIONS")
            && let Ok(mode) = value.parse::<PermissionMode>()
        {
            self.permission_mode = Some(mode);
        }
        if let Ok(value) = std::env::var("DOREAN_PERMISSION_DENY") {
            self.permission_deny = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(value) = std::env::var("DOREAN_SUB_AGENT_ROUNDS") {
            self.sub_agent_rounds = value.parse().unwrap_or(self.sub_agent_rounds);
        }
        if let Ok(value) = std::env::var("DOREAN_AGENT_MODELS") {
            // Format: "backend=model-a,frontend-ui-ux=model-b"
            self.agent_models = value
                .split(',')
                .filter_map(|pair| {
                    let (name, model) = pair.split_once('=')?;
                    let name = name.trim().to_string();
                    let model = model.trim().to_string();
                    if name.is_empty() || model.is_empty() {
                        None
                    } else {
                        Some((name, model))
                    }
                })
                .collect();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_config(name: &str, contents: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("dorean-config-{}-{name}.json", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn defaults_when_no_file() {
        let cfg = Config::load_from(Path::new("/nonexistent/dorean/config.json")).unwrap();
        assert_eq!(cfg.provider, Provider::OpenRouter);
        assert!(cfg.telemetry);
        assert_eq!(cfg.model, None);
    }

    #[test]
    fn parses_valid_json() {
        let path = temp_config(
            "valid",
            r#"{"provider":"openrouter","model":"meta-llama/llama-3.3-70b-instruct:free","telemetry":false}"#,
        );
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.provider, Provider::OpenRouter);
        assert_eq!(
            cfg.model.as_deref(),
            Some("meta-llama/llama-3.3-70b-instruct:free")
        );
        assert!(!cfg.telemetry);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_malformed_json() {
        let path = temp_config("bad", "not json {");
        assert!(matches!(
            Config::load_from(&path),
            Err(DoreanError::Config(_))
        ));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let path = temp_config("unknown", r#"{"provider":"openrouter","future_flag":42}"#);
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.provider, Provider::OpenRouter);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path =
            std::env::temp_dir().join(format!("dorean-config-save-{}.json", std::process::id()));
        let cfg = Config {
            openrouter_api_key: Some("sk-test-123".to_string()),
            model: Some("meta-llama/llama-3.3-70b-instruct:free".to_string()),
            ..Config::default()
        };
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.openrouter_api_key.as_deref(), Some("sk-test-123"));
        assert_eq!(
            loaded.model.as_deref(),
            Some("meta-llama/llama-3.3-70b-instruct:free")
        );
        assert_eq!(loaded.max_turns, 20);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn env_overrides_config() {
        unsafe {
            std::env::set_var("DOREAN_MODEL", "env-model");
            std::env::set_var("DOREAN_TELEMETRY", "false");
            std::env::set_var("DOREAN_PROVIDER", "openrouter");
        }
        let cfg = Config::default().apply_env();
        assert_eq!(cfg.model.as_deref(), Some("env-model"));
        assert!(!cfg.telemetry);
        assert_eq!(cfg.provider, Provider::OpenRouter);
    }

    #[test]
    fn provider_parsing() {
        assert_eq!(
            "openrouter".parse::<Provider>().unwrap(),
            Provider::OpenRouter
        );
        assert_eq!(
            "OpenRouter".parse::<Provider>().unwrap(),
            Provider::OpenRouter
        );
        assert!("ollama".parse::<Provider>().is_err());
        assert!("bogus".parse::<Provider>().is_err());
    }

    #[test]
    fn parses_agent_models_env() {
        unsafe {
            std::env::set_var("DOREAN_AGENT_MODELS", "backend=m-a, db=m-b");
        }
        let cfg = Config::default().apply_env();
        assert_eq!(
            cfg.agent_models.get("backend").map(String::as_str),
            Some("m-a")
        );
        assert_eq!(cfg.agent_models.get("db").map(String::as_str), Some("m-b"));
        assert_eq!(cfg.agent_models.len(), 2);
    }
}
