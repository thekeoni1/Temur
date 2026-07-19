use serde::Deserialize;
use std::path::PathBuf;

pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_PROVIDER: &str = "anthropic";
/// llama.cpp's default listen address — the likeliest local endpoint.
pub const DEFAULT_OPENAI_COMPAT_BASE_URL: &str = "http://127.0.0.1:8080/v1";
pub const DEFAULT_MAX_TOKENS: u32 = 32_000;
/// Per-turn provider-round-trip ceiling. Raised to 400 because with the
/// moving cache breakpoint, marginal iterations are cheap, and real long
/// runs died at the old 50 cap with work unfinished. The doom-loop guard
/// (identical-call detection) is separate and unchanged.
pub const DEFAULT_MAX_TURN_ITERATIONS: u32 = 400;

/// Loaded from ~/.config/temur/config.json (or $XDG_CONFIG_HOME).
/// Unknown fields are tolerated so old binaries accept newer configs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `"anthropic"` (default) or `"openai-compat"`. Selecting the compat
    /// provider reads its settings from [`Config::openai_compat`]; the
    /// Anthropic fields below stay untouched, so switching back is a
    /// one-line change.
    pub provider: String,
    /// Anthropic model id. Sonnet-class by default: the agent loop is chatty
    /// and runs on a metered key; Opus is a config change, not the default.
    pub model: String,
    pub base_url: String,
    pub max_tokens: u32,
    /// Adaptive thinking. Deliberately OFF for v1 bring-up; flipping this on
    /// is a config change, not a refactor (wire types support it from M1).
    pub thinking: bool,
    pub system_prompt: Option<String>,
    /// `:`-separated extra skill directories, searched before the always-included
    /// `.temur/skills` defaults. The `TEMUR_SKILLS_DIR` env var overrides this.
    pub skills_dir: Option<String>,
    /// Ceiling on provider round-trips within a single turn. Distinct from the
    /// doom-loop guard (identical-call detection), which stays hardcoded.
    pub max_turn_iterations: u32,
    /// Settings for `provider: "openai-compat"`; ignored otherwise.
    pub openai_compat: Option<OpenAiCompatConfig>,
}

/// Per-provider settings for an OpenAI-compatible endpoint (llama.cpp,
/// Ollama, vLLM, LM Studio, or a hosted compat API).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OpenAiCompatConfig {
    /// Includes the version prefix, SDK-convention style
    /// (`http://127.0.0.1:8080/v1`, `https://api.openai.com/v1`, …).
    pub base_url: String,
    /// Required (validated at startup): there is no sensible cross-server
    /// default model id.
    pub model: String,
    /// Path to a file holding the API key — by path only, same isolation
    /// rule as `APP_SECRET_FILE`, never env or argv. `None` = keyless
    /// (local servers need no credential).
    pub api_key_file: Option<String>,
}

impl Default for OpenAiCompatConfig {
    fn default() -> Self {
        OpenAiCompatConfig {
            base_url: DEFAULT_OPENAI_COMPAT_BASE_URL.to_string(),
            model: String::new(),
            api_key_file: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: DEFAULT_PROVIDER.to_string(),
            model: DEFAULT_MODEL.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            thinking: false,
            system_prompt: None,
            skills_dir: None,
            max_turn_iterations: DEFAULT_MAX_TURN_ITERATIONS,
            openai_compat: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self, crate::error::Error> {
        Self::load_from(&config_path())
    }

    fn load_from(path: &std::path::Path) -> Result<Self, crate::error::Error> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| crate::error::Error::Config(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("temur").join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_defaults_to_anthropic_and_section_parses() {
        // No default flip: absent provider field = anthropic, exactly as
        // every pre-T2 config behaved.
        let c: Config = serde_json::from_str(r#"{"model":"claude-sonnet-5"}"#).unwrap();
        assert_eq!(c.provider, DEFAULT_PROVIDER);
        assert!(c.openai_compat.is_none());

        let c: Config = serde_json::from_str(
            r#"{"provider":"openai-compat","openai_compat":{"model":"qwen2.5-coder-7b"}}"#,
        )
        .unwrap();
        assert_eq!(c.provider, "openai-compat");
        let oc = c.openai_compat.unwrap();
        assert_eq!(oc.model, "qwen2.5-coder-7b");
        assert_eq!(oc.base_url, DEFAULT_OPENAI_COMPAT_BASE_URL);
        assert!(oc.api_key_file.is_none()); // keyless by default
        // Anthropic fields keep their defaults untouched.
        assert_eq!(c.model, DEFAULT_MODEL);
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn openai_compat_full_section_parses() {
        let c: Config = serde_json::from_str(
            r#"{"provider":"openai-compat","openai_compat":{
                "base_url":"http://192.168.1.10:11434/v1",
                "model":"llama3.2",
                "api_key_file":"/etc/keys/compat"
            }}"#,
        )
        .unwrap();
        let oc = c.openai_compat.unwrap();
        assert_eq!(oc.base_url, "http://192.168.1.10:11434/v1");
        assert_eq!(oc.model, "llama3.2");
        assert_eq!(oc.api_key_file.as_deref(), Some("/etc/keys/compat"));
    }

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.provider, DEFAULT_PROVIDER);
        assert!(c.openai_compat.is_none());
        assert_eq!(c.model, DEFAULT_MODEL);
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
        assert_eq!(c.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(!c.thinking);
        assert!(c.system_prompt.is_none());
        assert!(c.skills_dir.is_none());
        assert_eq!(c.max_turn_iterations, DEFAULT_MAX_TURN_ITERATIONS);
    }

    #[test]
    fn max_turn_iterations_from_json_and_default_when_absent() {
        // Custom value deliberately != DEFAULT_MAX_TURN_ITERATIONS so this
        // still fails if parsing silently falls back to the default.
        let c: Config = serde_json::from_str(r#"{"max_turn_iterations":7}"#).unwrap();
        assert_eq!(c.max_turn_iterations, 7);
        let c: Config = serde_json::from_str(r#"{"model":"claude-sonnet-5"}"#).unwrap();
        assert_eq!(c.max_turn_iterations, DEFAULT_MAX_TURN_ITERATIONS);
    }

    #[test]
    fn parses_partial_json_and_tolerates_unknown_fields() {
        let c: Config =
            serde_json::from_str(r#"{"model":"claude-opus-4-8","future_field":123}"#).unwrap();
        assert_eq!(c.model, "claude-opus-4-8");
        assert_eq!(c.max_tokens, DEFAULT_MAX_TOKENS); // default preserved
    }

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load_from(std::path::Path::new("/nonexistent/temur-test/config.json"))
            .unwrap();
        assert_eq!(c.model, DEFAULT_MODEL);
    }
}
