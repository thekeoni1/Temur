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
/// Session-file size cap (T5). 4 MiB bounds the load-time peak on a 32-bit
/// box — parsing JSON costs roughly 3–4x the file size in transient
/// allocations — and a bigger cap buys nothing: what makes a resumed session
/// useful is the recent history, not the whole of it.
pub const DEFAULT_SESSION_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Below this a cap cannot hold a realistic exchange (one tool result can be
/// tens of KiB), so every save would trim to nothing. A smaller value is a
/// configuration mistake, reported at startup rather than at save time.
pub const MIN_SESSION_MAX_BYTES: u64 = 64 * 1024;

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
    /// Sampling temperature, sent to whichever provider is selected.
    /// `None` = provider default (the field is absent from requests).
    pub temperature: Option<f32>,
    /// Nucleus sampling; same absent-when-`None` contract.
    pub top_p: Option<f32>,
    pub system_prompt: Option<String>,
    /// `:`-separated extra skill directories, searched before the always-included
    /// `.temur/skills` defaults. The `TEMUR_SKILLS_DIR` env var overrides this.
    pub skills_dir: Option<String>,
    /// Ceiling on provider round-trips within a single turn. Distinct from the
    /// doom-loop guard (identical-call detection), which stays hardcoded.
    pub max_turn_iterations: u32,
    /// Tool-prompt profile: `"full"` (= absent, the default) or `"compact"`
    /// (hand-trimmed descriptions + a shorter default system prompt for
    /// small-context local models). EXPLICIT-ONLY: never auto-selected from
    /// context_window or anything else; any other value is a startup
    /// config error.
    pub prompt_profile: Option<String>,
    /// Directory holding saved sessions (T5). `None` = the default state
    /// location, `$XDG_STATE_HOME/temur/sessions` falling back to
    /// `~/.local/state/temur/sessions`. A directory override and nothing
    /// more: filenames are derived from the working directory.
    pub sessions_dir: Option<String>,
    /// Size cap for a session FILE in bytes. `None` =
    /// [`DEFAULT_SESSION_MAX_BYTES`]. Over the cap the oldest exchanges are
    /// dropped from the file; the in-memory history is never touched.
    pub session_max_bytes: Option<u64>,
    /// Settings for `provider: "openai-compat"`; ignored otherwise.
    pub openai_compat: Option<OpenAiCompatConfig>,
    /// Named provider+model bundles selectable at runtime with `/model <name>`
    /// (T8). Absent = the feature is unused and everything behaves exactly as
    /// before profiles existed. Validated eagerly at startup — a typo in any
    /// profile is a startup error, never a surprise at switch time.
    pub profiles: Option<std::collections::BTreeMap<String, ProfileConfig>>,
    /// Startup profile name; must name an entry in `profiles`. Applied over
    /// the base provider/model fields at startup. Absent = the base fields
    /// select the provider, byte-identical to pre-T8 behavior.
    pub profile: Option<String>,
}

/// One named profile: a nickname bundling provider + model + endpoint +
/// credential path + limits, so switching between a local server and a hosted
/// model is one `/model` command instead of quit → edit JSON → `--continue`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProfileConfig {
    /// `"anthropic"` or `"openai-compat"` — anything else is a startup error.
    pub provider: String,
    /// Required and non-empty: there is no sensible cross-profile default.
    pub model: String,
    /// `None` = the provider's own default endpoint ([`DEFAULT_BASE_URL`] /
    /// [`DEFAULT_OPENAI_COMPAT_BASE_URL`]), NOT the top-level `base_url`
    /// field — a profile is self-contained.
    pub base_url: Option<String>,
    /// Path to a file holding the API key — by path only, the same isolation
    /// rule as `APP_SECRET_FILE`. `None` for openai-compat = keyless; `None`
    /// for anthropic = fall back to `APP_SECRET_FILE` at switch time (a
    /// deliberate, documented fallback within the by-path rule). The key is
    /// read when the profile is activated, never cached across switches.
    pub api_key_file: Option<String>,
    /// `None` = the global `max_tokens`.
    pub max_tokens: Option<u32>,
    /// Advisory context-window size of the served model; `None` = awareness
    /// off (same contract as `openai_compat.context_window`).
    pub context_window: Option<u64>,
    /// Tool-prompt profile for THIS profile: `"full"` or `"compact"` (T9).
    /// `None` = the global `prompt_profile` (which itself defaults to full).
    /// Same explicit-only contract as the global field — never inferred
    /// from context_window; any other value is a startup error.
    pub prompt_profile: Option<String>,
}

/// A fully resolved provider selection — every default already applied, so
/// provider construction and `/status` read plain values. Produced only by
/// the validated paths ([`Config::resolved_profiles`] /
/// [`Config::resolve_base`]); holding one is proof the selection was checked.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    /// `"anthropic"` or `"openai-compat"`, already validated.
    pub provider: String,
    pub model: String,
    pub base_url: String,
    /// See [`ProfileConfig::api_key_file`] for the `None` semantics per
    /// provider.
    pub api_key_file: Option<String>,
    pub max_tokens: u32,
    pub context_window: Option<u64>,
    /// Already resolved: this profile's own setting, else the global, else
    /// [`crate::tools::PromptProfile::Full`].
    pub prompt_profile: crate::tools::PromptProfile,
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
    /// Advisory context-window size (tokens) of the SERVED model — a
    /// property of the server (llama.cpp `-c`), which temur cannot query.
    /// `None` = awareness off. Powers warnings only: no compaction, no
    /// trimming, no request-side enforcement.
    pub context_window: Option<u64>,
}

impl Default for OpenAiCompatConfig {
    fn default() -> Self {
        OpenAiCompatConfig {
            base_url: DEFAULT_OPENAI_COMPAT_BASE_URL.to_string(),
            model: String::new(),
            api_key_file: None,
            context_window: None,
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
            temperature: None,
            top_p: None,
            system_prompt: None,
            skills_dir: None,
            max_turn_iterations: DEFAULT_MAX_TURN_ITERATIONS,
            prompt_profile: None,
            sessions_dir: None,
            session_max_bytes: None,
            openai_compat: None,
            profiles: None,
            profile: None,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self, crate::error::Error> {
        Ok(Self::load_reporting()?.0)
    }

    /// Like [`Config::load`], but also reports whether the config FILE was
    /// actually there (`false` = defaults from a missing file). The first-run
    /// quickstart (T14) keys off this: only a genuinely absent file may
    /// trigger it, so any existing config, valid or broken, behaves exactly
    /// as before.
    pub fn load_reporting() -> Result<(Self, bool), crate::error::Error> {
        Self::load_from_reporting(&config_path())
    }

    /// Resolve `prompt_profile` to the typed profile, rejecting anything
    /// but `"full"` / `"compact"` / absent at startup.
    pub fn prompt_profile(&self) -> Result<crate::tools::PromptProfile, crate::error::Error> {
        match self.prompt_profile.as_deref() {
            None | Some("full") => Ok(crate::tools::PromptProfile::Full),
            Some("compact") => Ok(crate::tools::PromptProfile::Compact),
            Some(other) => Err(crate::error::Error::Config(format!(
                "unknown prompt_profile {other:?} (expected \"full\" or \"compact\")"
            ))),
        }
    }

    /// Resolve the session file size cap, rejecting a uselessly small value at
    /// startup instead of trimming every save to nothing (same
    /// validated-accessor shape as [`Config::prompt_profile`]).
    pub fn session_max_bytes(&self) -> Result<u64, crate::error::Error> {
        match self.session_max_bytes {
            None => Ok(DEFAULT_SESSION_MAX_BYTES),
            Some(v) if v >= MIN_SESSION_MAX_BYTES => Ok(v),
            Some(v) => Err(crate::error::Error::Config(format!(
                "session_max_bytes {v} is below the {MIN_SESSION_MAX_BYTES}-byte minimum"
            ))),
        }
    }

    /// Resolve and validate EVERY named profile eagerly. Called at startup so
    /// a bad provider name or missing model is a config error before the
    /// first prompt — a later `/model` switch can then only fail on
    /// credential/IO problems, never on a typo discovered late.
    pub fn resolved_profiles(
        &self,
    ) -> Result<std::collections::BTreeMap<String, ResolvedProfile>, crate::error::Error> {
        let mut out = std::collections::BTreeMap::new();
        if let Some(profiles) = &self.profiles {
            for (name, p) in profiles {
                out.insert(name.clone(), self.resolve_profile_entry(name, p)?);
            }
        }
        Ok(out)
    }

    fn resolve_profile_entry(
        &self,
        name: &str,
        p: &ProfileConfig,
    ) -> Result<ResolvedProfile, crate::error::Error> {
        match p.provider.as_str() {
            "anthropic" | "openai-compat" => {}
            other => {
                return Err(crate::error::Error::Config(format!(
                    "profile {name:?}: unknown provider {other:?} (expected \"anthropic\" or \"openai-compat\")"
                )))
            }
        }
        if p.model.is_empty() {
            return Err(crate::error::Error::Config(format!(
                "profile {name:?}: model must be set and non-empty"
            )));
        }
        // Per-profile prompt profile (T9), validated as eagerly as the
        // provider name above: absent = the global setting.
        let prompt_profile = match p.prompt_profile.as_deref() {
            None => self.prompt_profile()?,
            Some("full") => crate::tools::PromptProfile::Full,
            Some("compact") => crate::tools::PromptProfile::Compact,
            Some(other) => {
                return Err(crate::error::Error::Config(format!(
                    "profile {name:?}: unknown prompt_profile {other:?} (expected \"full\" or \"compact\")"
                )))
            }
        };
        let base_url = p.base_url.clone().unwrap_or_else(|| {
            if p.provider == "openai-compat" {
                DEFAULT_OPENAI_COMPAT_BASE_URL.to_string()
            } else {
                DEFAULT_BASE_URL.to_string()
            }
        });
        Ok(ResolvedProfile {
            provider: p.provider.clone(),
            model: p.model.clone(),
            base_url,
            api_key_file: p.api_key_file.clone(),
            max_tokens: p.max_tokens.unwrap_or(self.max_tokens),
            context_window: p.context_window,
            prompt_profile,
        })
    }

    /// Resolve the BASE (non-profile) selection — the pre-T8 startup path,
    /// error messages included byte-for-byte. Used when no startup `profile`
    /// is set, so absent-profiles configs behave exactly as they always did.
    pub fn resolve_base(&self) -> Result<ResolvedProfile, crate::error::Error> {
        match self.provider.as_str() {
            "anthropic" => Ok(ResolvedProfile {
                provider: self.provider.clone(),
                model: self.model.clone(),
                base_url: self.base_url.clone(),
                api_key_file: None,
                max_tokens: self.max_tokens,
                context_window: None,
                prompt_profile: self.prompt_profile()?,
            }),
            "openai-compat" => {
                let oc = self.openai_compat.clone().unwrap_or_default();
                if oc.model.is_empty() {
                    return Err(crate::error::Error::Config(
                        "provider \"openai-compat\" requires openai_compat.model".into(),
                    ));
                }
                Ok(ResolvedProfile {
                    provider: self.provider.clone(),
                    model: oc.model,
                    base_url: oc.base_url,
                    api_key_file: oc.api_key_file,
                    max_tokens: self.max_tokens,
                    context_window: oc.context_window,
                    prompt_profile: self.prompt_profile()?,
                })
            }
            other => Err(crate::error::Error::Config(format!(
                "unknown provider {other:?} (expected \"anthropic\" or \"openai-compat\")"
            ))),
        }
    }

    /// The startup selection: `(active profile name, resolved selection)`.
    /// `profile` set → that named profile (unknown name = startup error);
    /// absent → the base fields via [`Config::resolve_base`].
    pub fn startup_selection(
        &self,
        profiles: &std::collections::BTreeMap<String, ResolvedProfile>,
    ) -> Result<(Option<String>, ResolvedProfile), crate::error::Error> {
        match self.profile.as_deref() {
            Some(name) => match profiles.get(name) {
                Some(r) => Ok((Some(name.to_string()), r.clone())),
                None => Err(crate::error::Error::Config(format!(
                    "startup profile {name:?} is not defined in \"profiles\""
                ))),
            },
            None => Ok((None, self.resolve_base()?)),
        }
    }

    fn load_from(path: &std::path::Path) -> Result<Self, crate::error::Error> {
        Ok(Self::load_from_reporting(path)?.0)
    }

    /// Load from an explicit path, reporting presence (see
    /// [`Config::load_reporting`]). Public so `doctor` reads through the
    /// exact same parse-or-default path as startup.
    pub fn load_from_reporting(
        path: &std::path::Path,
    ) -> Result<(Self, bool), crate::error::Error> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s)
                .map(|c| (c, true))
                .map_err(|e| crate::error::Error::Config(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Self::default(), false)),
            Err(e) => Err(e.into()),
        }
    }
}

/// `$XDG_CONFIG_HOME/temur/config.json`, falling back to
/// `~/.config/temur/config.json`. Public since T14: the quickstart, `init`,
/// and `doctor` all name this exact path to the user.
pub fn config_path() -> PathBuf {
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
    fn sampling_knobs_and_context_window_parse() {
        // 0.25 / 0.5 are exact in binary so the equality is airtight.
        let c: Config = serde_json::from_str(
            r#"{"temperature":0.25,"top_p":0.5,
                "openai_compat":{"model":"m","context_window":8192}}"#,
        )
        .unwrap();
        assert_eq!(c.temperature, Some(0.25));
        assert_eq!(c.top_p, Some(0.5));
        assert_eq!(c.openai_compat.unwrap().context_window, Some(8192));
        // Absent = provider defaults / awareness off.
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.temperature.is_none());
        assert!(c.top_p.is_none());
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
        assert!(c.sessions_dir.is_none());
        assert!(c.session_max_bytes.is_none());
        assert_eq!(c.session_max_bytes().unwrap(), DEFAULT_SESSION_MAX_BYTES);
    }

    #[test]
    fn session_settings_parse_and_validate() {
        let c: Config = serde_json::from_str(
            r#"{"sessions_dir":"/var/lib/temur","session_max_bytes":1048576}"#,
        )
        .unwrap();
        assert_eq!(c.sessions_dir.as_deref(), Some("/var/lib/temur"));
        assert_eq!(c.session_max_bytes().unwrap(), 1_048_576);
        // Absent = the default cap, not zero.
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.session_max_bytes().unwrap(), DEFAULT_SESSION_MAX_BYTES);
        // Below the floor is a startup error naming the floor, never a silent
        // clamp — a cap that trims every save to nothing is a mistake.
        let c: Config = serde_json::from_str(r#"{"session_max_bytes":1024}"#).unwrap();
        let err = c.session_max_bytes().unwrap_err().to_string();
        assert!(
            err.contains(&MIN_SESSION_MAX_BYTES.to_string()) && err.contains("1024"),
            "error names the value and the floor: {err}"
        );
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
    fn prompt_profile_explicit_only_and_invalid_is_startup_error() {
        // Absent = full, byte-for-byte the pre-T4 default path.
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.prompt_profile.is_none());
        assert_eq!(c.prompt_profile().unwrap(), crate::tools::PromptProfile::Full);
        // Explicit values.
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"full"}"#).unwrap();
        assert_eq!(c.prompt_profile().unwrap(), crate::tools::PromptProfile::Full);
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"compact"}"#).unwrap();
        assert_eq!(
            c.prompt_profile().unwrap(),
            crate::tools::PromptProfile::Compact
        );
        // Anything else is a config error, not a silent fallback. NOTE:
        // deliberately NO auto-selection — a small context_window must not
        // flip the profile.
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"tiny"}"#).unwrap();
        let err = c.prompt_profile().unwrap_err().to_string();
        assert!(err.contains("tiny"), "error names the bad value: {err}");
        let c: Config = serde_json::from_str(
            r#"{"openai_compat":{"model":"m","context_window":2048}}"#,
        )
        .unwrap();
        assert_eq!(c.prompt_profile().unwrap(), crate::tools::PromptProfile::Full);
    }

    #[test]
    fn parses_partial_json_and_tolerates_unknown_fields() {
        let c: Config =
            serde_json::from_str(r#"{"model":"claude-opus-4-8","future_field":123}"#).unwrap();
        assert_eq!(c.model, "claude-opus-4-8");
        assert_eq!(c.max_tokens, DEFAULT_MAX_TOKENS); // default preserved
    }

    // ------------------------------------------------------- T8: profiles

    const PROFILES_JSON: &str = r#"{
        "max_tokens": 2048,
        "profiles": {
            "local":  { "provider": "openai-compat", "model": "qwen3-1.7b",
                        "max_tokens": 1024, "context_window": 8192 },
            "sonnet": { "provider": "anthropic", "model": "claude-sonnet-5",
                        "max_tokens": 32000 }
        },
        "profile": "local"
    }"#;

    #[test]
    fn profiles_parse_resolve_and_apply_defaults() {
        let c: Config = serde_json::from_str(PROFILES_JSON).unwrap();
        let profiles = c.resolved_profiles().unwrap();
        assert_eq!(profiles.len(), 2);

        let local = &profiles["local"];
        assert_eq!(local.provider, "openai-compat");
        assert_eq!(local.model, "qwen3-1.7b");
        // Absent base_url = the PROVIDER's default endpoint, per kind.
        assert_eq!(local.base_url, DEFAULT_OPENAI_COMPAT_BASE_URL);
        assert_eq!(local.max_tokens, 1024);
        assert_eq!(local.context_window, Some(8192));
        assert!(local.api_key_file.is_none()); // keyless

        let sonnet = &profiles["sonnet"];
        assert_eq!(sonnet.provider, "anthropic");
        assert_eq!(sonnet.base_url, DEFAULT_BASE_URL);
        assert_eq!(sonnet.max_tokens, 32000);
        assert!(sonnet.context_window.is_none());
    }

    #[test]
    fn profile_max_tokens_falls_back_to_global() {
        let c: Config = serde_json::from_str(
            r#"{"max_tokens": 4096,
                "profiles": {"p": {"provider": "anthropic", "model": "m"}}}"#,
        )
        .unwrap();
        assert_eq!(c.resolved_profiles().unwrap()["p"].max_tokens, 4096);
    }

    #[test]
    fn profile_explicit_base_url_and_key_file_survive() {
        let c: Config = serde_json::from_str(
            r#"{"profiles": {"p": {"provider": "openai-compat", "model": "m",
                "base_url": "http://10.0.0.2:9999/v1",
                "api_key_file": "/etc/keys/p"}}}"#,
        )
        .unwrap();
        let p = &c.resolved_profiles().unwrap()["p"];
        assert_eq!(p.base_url, "http://10.0.0.2:9999/v1");
        assert_eq!(p.api_key_file.as_deref(), Some("/etc/keys/p"));
    }

    #[test]
    fn invalid_profiles_are_startup_errors_naming_the_profile() {
        // Unknown provider.
        let c: Config = serde_json::from_str(
            r#"{"profiles": {"bad": {"provider": "gemini", "model": "m"}}}"#,
        )
        .unwrap();
        let err = c.resolved_profiles().unwrap_err().to_string();
        assert!(err.contains("bad") && err.contains("gemini"), "{err}");

        // Missing/empty model.
        let c: Config = serde_json::from_str(
            r#"{"profiles": {"nomodel": {"provider": "anthropic"}}}"#,
        )
        .unwrap();
        let err = c.resolved_profiles().unwrap_err().to_string();
        assert!(err.contains("nomodel") && err.contains("model"), "{err}");

        // Missing provider (empty by default) is also invalid.
        let c: Config =
            serde_json::from_str(r#"{"profiles": {"nop": {"model": "m"}}}"#).unwrap();
        assert!(c.resolved_profiles().is_err());
    }

    // -------------------------------------------- T9: per-profile prompt_profile

    #[test]
    fn profile_prompt_profile_resolution_own_then_global_then_full() {
        use crate::tools::PromptProfile;
        // Own value wins over the global; absent falls back to the global;
        // both absent = Full.
        let c: Config = serde_json::from_str(
            r#"{"prompt_profile": "compact",
                "profiles": {
                    "own":    { "provider": "anthropic", "model": "m",
                                "prompt_profile": "full" },
                    "global": { "provider": "anthropic", "model": "m" }
                }}"#,
        )
        .unwrap();
        let profiles = c.resolved_profiles().unwrap();
        assert_eq!(profiles["own"].prompt_profile, PromptProfile::Full);
        assert_eq!(profiles["global"].prompt_profile, PromptProfile::Compact);

        let c: Config = serde_json::from_str(
            r#"{"profiles": {"p": {"provider": "anthropic", "model": "m"}}}"#,
        )
        .unwrap();
        assert_eq!(
            c.resolved_profiles().unwrap()["p"].prompt_profile,
            PromptProfile::Full
        );

        // resolve_base carries the GLOBAL setting (per-profile values only
        // exist on named profiles).
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"compact"}"#).unwrap();
        assert_eq!(c.resolve_base().unwrap().prompt_profile, PromptProfile::Compact);
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.resolve_base().unwrap().prompt_profile, PromptProfile::Full);
    }

    #[test]
    fn invalid_profile_prompt_profile_is_a_startup_error_naming_the_profile() {
        let c: Config = serde_json::from_str(
            r#"{"profiles": {"bad": {"provider": "anthropic", "model": "m",
                "prompt_profile": "tiny"}}}"#,
        )
        .unwrap();
        let err = c.resolved_profiles().unwrap_err().to_string();
        assert!(
            err.contains("\"bad\"") && err.contains("tiny") && err.contains("expected"),
            "error names profile and value: {err}"
        );
    }

    #[test]
    fn startup_selection_uses_named_profile_and_rejects_unknown() {
        let c: Config = serde_json::from_str(PROFILES_JSON).unwrap();
        let profiles = c.resolved_profiles().unwrap();
        let (name, r) = c.startup_selection(&profiles).unwrap();
        assert_eq!(name.as_deref(), Some("local"));
        assert_eq!(r.model, "qwen3-1.7b");
        assert_eq!(r.max_tokens, 1024);

        let mut c2 = c.clone();
        c2.profile = Some("nope".into());
        let err = c2.startup_selection(&profiles).unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn absent_profiles_resolve_base_is_byte_identical_to_pre_t8() {
        // Anthropic defaults: same fields main.rs used to read directly.
        let c: Config = serde_json::from_str(r#"{"model":"claude-sonnet-5"}"#).unwrap();
        let profiles = c.resolved_profiles().unwrap();
        assert!(profiles.is_empty());
        let (name, r) = c.startup_selection(&profiles).unwrap();
        assert!(name.is_none());
        assert_eq!(r.provider, DEFAULT_PROVIDER);
        assert_eq!(r.model, DEFAULT_MODEL);
        assert_eq!(r.base_url, DEFAULT_BASE_URL);
        assert!(r.api_key_file.is_none()); // = APP_SECRET_FILE fallback
        assert_eq!(r.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(r.context_window.is_none());

        // openai-compat pulls the section fields, same as before.
        let c: Config = serde_json::from_str(
            r#"{"provider":"openai-compat","max_tokens":1024,
                "openai_compat":{"model":"qwen3-1.7b","context_window":8192,
                                 "api_key_file":"/etc/k"}}"#,
        )
        .unwrap();
        let r = c.resolve_base().unwrap();
        assert_eq!(r.provider, "openai-compat");
        assert_eq!(r.model, "qwen3-1.7b");
        assert_eq!(r.base_url, DEFAULT_OPENAI_COMPAT_BASE_URL);
        assert_eq!(r.api_key_file.as_deref(), Some("/etc/k"));
        assert_eq!(r.max_tokens, 1024);
        assert_eq!(r.context_window, Some(8192));

        // Error strings unchanged from the pre-T8 startup path.
        let c: Config = serde_json::from_str(r#"{"provider":"openai-compat"}"#).unwrap();
        assert_eq!(
            c.resolve_base().unwrap_err().to_string(),
            "config: provider \"openai-compat\" requires openai_compat.model"
        );
        let c: Config = serde_json::from_str(r#"{"provider":"bedrock"}"#).unwrap();
        assert_eq!(
            c.resolve_base().unwrap_err().to_string(),
            "config: unknown provider \"bedrock\" (expected \"anthropic\" or \"openai-compat\")"
        );
    }

    #[test]
    fn missing_file_yields_defaults() {
        let c = Config::load_from(std::path::Path::new("/nonexistent/temur-test/config.json"))
            .unwrap();
        assert_eq!(c.model, DEFAULT_MODEL);
    }
}
