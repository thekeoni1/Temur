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
/// Default for [`Config::key_rotate_warn_days`] (T17): doctor WARNs about a
/// key file whose mtime is at least this many days old.
pub const DEFAULT_KEY_ROTATE_WARN_DAYS: u64 = 90;
/// Default for [`Config::cost_advisory_step_usd`] (T26): the mid-session cost
/// advisory fires every $5 of estimated spend. Operator decision 2026-08-11,
/// taken after a single agentic `-p` turn ran to roughly $26 unnoticed: on by
/// default wherever an estimate can be computed at all, because an opt-in
/// spend alarm is off exactly when it is needed. 0 in config disables it.
pub const DEFAULT_COST_ADVISORY_STEP_USD: f64 = 5.0;

/// The `"auto"` prompt-profile threshold (T41): a configured context
/// window STRICTLY below this many tokens selects
/// [`crate::tools::PromptProfile::Compact`].
///
/// Where the number comes from. T40 finding F6 measured temur's own prompt
/// floor against a live llama.cpp server on 2026-08-29: the full profile
/// costs 6,991 prompt tokens before the task starts, the compact profile
/// 2,763. At a 12,288-token window the full floor is 57% of everything the
/// model will ever see; at 16,384 it is 42%; at 20,480 it is 34%; at
/// 32,768 it is 21%. Desktop experiments 3 and 4 found context exhaustion
/// to be the dominant Terminal-Bench failure mode at those small windows.
///
/// The threshold is DERIVED from `doctor`'s own warning line, not picked
/// for roundness: it is the smallest power-of-2-ish window at which the
/// FULL floor sits under `PROMPT_FLOOR_WARN_PERCENT` (40%) with margin,
/// measured 34% and estimated 35% at 20480. v0.30.0 shipped 16384, where
/// the two disagreed by construction (16384 * 40% = 6,554, below the 6,991
/// the full profile actually costs): auto chose full and `doctor` then
/// WARNed that the same selection should be compact, on the very window
/// `temur init` writes from a 16k llama.cpp server. The tie is pinned by a
/// test in `doctor`, so changing either constant, or the prompts, fails
/// loudly instead of shipping the contradiction again. Compact is the
/// better choice at 16384 anyway: it leaves 13.6k tokens for the task
/// where full leaves 9.4k.
///
/// It stays a threshold rather than a ratio because a threshold is
/// something a user can read off their own config and predict.
pub const PROMPT_AUTO_COMPACT_BELOW: u64 = 20480;

/// The accepted `prompt_profile` spellings, quoted once so every error
/// message naming them cannot drift apart.
const PROMPT_PROFILE_EXPECTED: &str = "expected \"auto\", \"full\", or \"compact\"";

/// The pure `"auto"` rule (T41), the ONE place a window turns into a
/// profile. Compact strictly below [`PROMPT_AUTO_COMPACT_BELOW`]; full at
/// or above it; full when the window is unknown, because guessing smaller
/// would silently trim descriptions on a model that never needed it.
pub fn auto_prompt_profile(context_window: Option<u64>) -> crate::tools::PromptProfile {
    match context_window {
        Some(w) if w < PROMPT_AUTO_COMPACT_BELOW => crate::tools::PromptProfile::Compact,
        _ => crate::tools::PromptProfile::Full,
    }
}

/// How a resolved [`crate::tools::PromptProfile`] was chosen (T41). Only
/// reporting reads it: an `Auto` choice of compact is worth one startup
/// line, because a user who never wrote `"compact"` anywhere should not
/// have to wonder why the tool descriptions look short.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptProfileSource {
    /// Config named the profile outright (`"full"` or `"compact"`).
    #[default]
    Explicit,
    /// The `"auto"` spec (the default) ran [`auto_prompt_profile`].
    Auto,
}

/// A validated `prompt_profile` spelling, before any window is known
/// (T41). Splitting validation from resolution is what lets startup reject
/// a typo in the GLOBAL field even when every named profile overrides it,
/// while each selection still resolves `"auto"` against its OWN window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptProfileSpec {
    Auto,
    Explicit(crate::tools::PromptProfile),
}

impl PromptProfileSpec {
    /// Parse one spelling; `None` (the field absent) is [`Self::Auto`],
    /// the T41 default. `None` return = the value is not a spelling we
    /// accept, which every caller turns into a startup error naming it.
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s {
            None | Some("auto") => Some(PromptProfileSpec::Auto),
            Some("full") => Some(PromptProfileSpec::Explicit(crate::tools::PromptProfile::Full)),
            Some("compact") => {
                Some(PromptProfileSpec::Explicit(crate::tools::PromptProfile::Compact))
            }
            Some(_) => None,
        }
    }

    /// Resolve against a selection's own context window. Pure, total, and
    /// the only path from a spec to a profile.
    pub fn resolve(
        self,
        context_window: Option<u64>,
    ) -> (crate::tools::PromptProfile, PromptProfileSource) {
        match self {
            PromptProfileSpec::Explicit(p) => (p, PromptProfileSource::Explicit),
            PromptProfileSpec::Auto => {
                (auto_prompt_profile(context_window), PromptProfileSource::Auto)
            }
        }
    }
}

/// The one line printed when the auto rule picks compact (T41), shared by
/// startup and `/model` so the two can never word it differently. Only
/// ever called for an `Auto` selection that landed on compact, which by
/// construction has a window.
pub fn auto_compact_notice(context_window: u64) -> String {
    format!(
        "prompt profile: compact (context_window {context_window} is below \
         {PROMPT_AUTO_COMPACT_BELOW}; set prompt_profile to \"full\" to override)"
    )
}

/// T42 P4: does this selection want the startup `/props` probe?
///
/// Deliberately narrow, and every clause is load-bearing. `openai-compat`
/// with NO key file is the keyless local endpoint the probe was built for
/// in T22, and it is the only shape [`crate::provider::probe_props_context`]
/// can be pointed at: that function takes a base URL and nothing else, so
/// it cannot attach auth by construction. An UNSET `context_window` is the
/// only case worth asking about, because a configured one is authoritative
/// and is never probed over (doctor already warns when the two disagree,
/// which is the right place for that conversation). And `--mock` replays
/// fixtures with no server to ask.
///
/// Costs at most one 3-second GET, once, on a run that would otherwise
/// have had no window at all: no advisory, no auto-compaction, and the
/// unscaled tool-output ceiling.
pub fn wants_startup_context_probe(p: &ResolvedProfile, is_mock: bool) -> bool {
    !is_mock
        && p.provider == "openai-compat"
        && p.api_key_file.is_none()
        && p.context_window.is_none()
}

/// T42 P4: fold a probed window into the resolved selection, returning the
/// line that says so. In-memory only: nothing is written to disk, because
/// what the server allocates today is not a config decision, and doctor
/// still recommends the explicit `"context_window"` line for anyone who
/// wants one.
///
/// The prompt profile is recomputed ONLY for an `Auto` selection. An
/// explicit `"full"` or `"compact"` is a user's stated choice and a probe
/// result is not grounds to overrule it. When auto does flip to compact,
/// the existing T41 line ([`auto_compact_notice`]) says so right after
/// this one, in the same words startup and `/model` have always used.
pub fn apply_probed_context_window(p: &mut ResolvedProfile, n: u64) -> String {
    p.context_window = Some(n);
    if p.prompt_profile_source == PromptProfileSource::Auto {
        p.prompt_profile = auto_prompt_profile(Some(n));
    }
    probed_context_notice(n)
}

/// The T42 P4 startup line. Names the SOURCE, because a number nobody
/// configured appearing in `/status` is otherwise a mystery, and the
/// CONSEQUENCE, because the three things it switches on are exactly the
/// three a user would otherwise wonder about.
pub fn probed_context_notice(n: u64) -> String {
    format!(
        "context window {n} detected from the server (/props); the context advisory, \
         auto-compaction, and the tool-output cap now use it"
    )
}

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
    /// Tool-prompt profile: `"auto"` (= absent, the default since T41),
    /// `"full"`, or `"compact"` (hand-trimmed descriptions + a shorter
    /// default system prompt for small-context local models). `"auto"`
    /// picks compact below [`PROMPT_AUTO_COMPACT_BELOW`] and full at or
    /// above it, per [`auto_prompt_profile`]; an explicit `"full"` or
    /// `"compact"` is never second-guessed. Any other value is a startup
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
    /// Age in days after which `temur doctor` WARNs that a key file has not
    /// changed and suggests rotating the key (T17). Metadata only (mtime),
    /// advisory only. 0 disables the reminder; absent =
    /// [`DEFAULT_KEY_ROTATE_WARN_DAYS`].
    pub key_rotate_warn_days: u64,
    /// T18 escape hatch: when key files are configured but the kernel
    /// cannot provide the unprivileged-user-namespace bash sandbox, bash
    /// REFUSES by default. Setting this true accepts running bash without
    /// the sandbox on such hosts (the other tools stay guarded; a working
    /// sandbox is still used when available). Default false.
    pub allow_bash_without_key_sandbox: bool,
    /// T19 P3 (a recorded amendment to T4's "prose is never executed"
    /// policy): execute a tool call the model wrote as plain text when it
    /// is UNAMBIGUOUS: exactly one candidate in a known shape, inner JSON
    /// losslessly parsed, registered tool, object arguments. Default true;
    /// false restores the pre-T19 detect+nudge behavior exactly.
    pub prose_tool_calls: bool,
    /// Dollar step between mid-session cost advisories (T26). Absent =
    /// [`DEFAULT_COST_ADVISORY_STEP_USD`]; `0` disables the advisory
    /// entirely; any other positive finite value is the step. Negative or
    /// non-finite is a startup config error.
    ///
    /// Deliberately GLOBAL rather than per-profile: a price is a property of
    /// the provider, but a budget is a property of the person, and it should
    /// not reset because a `/model` switch landed on a profile that forgot to
    /// repeat it. It rides the same gate as the `/status` estimate, so an
    /// unpriced, keyless, or local selection never sees it whatever the value.
    pub cost_advisory_step_usd: Option<f64>,
    /// T40: when the context advisory would fire, compact the session
    /// automatically and continue the turn instead of only printing advice.
    /// `None` takes the per-mode default from
    /// [`Config::auto_compact_enabled`]; `Some(v)` is `v` in every mode.
    ///
    /// Base config only this cycle: there is deliberately no profile-level
    /// `auto_compact`. Whether an unattended run may spend a summary call to
    /// survive is a property of HOW temur was invoked, not of which model
    /// answered, so a `/model` switch must not change it.
    pub auto_compact: Option<bool>,
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
    /// Tool-prompt profile for THIS profile: `"auto"`, `"full"`, or
    /// `"compact"` (T9). `None` = the global `prompt_profile` (which itself
    /// defaults to `"auto"`). Same contract as the global field, resolved
    /// against THIS profile's own `context_window`; any other value is a
    /// startup error.
    pub prompt_profile: Option<String>,
    /// List price per MILLION input tokens, in the key's billing currency
    /// (USD for the values `init` bakes). Feeds nothing but the `/status`
    /// cost estimate (T24): an awareness figure computed locally from the
    /// session's own token counts, never a bill. `None` = the estimate is
    /// off for this profile, which is also what every unpriced profile
    /// does. Must be set together with [`ProfileConfig::price_output_per_mtok`].
    pub price_input_per_mtok: Option<f64>,
    /// List price per MILLION output tokens; see
    /// [`ProfileConfig::price_input_per_mtok`] for the contract. Must be
    /// set together with it.
    pub price_output_per_mtok: Option<f64>,
    /// Which wire key carries the token cap on the OpenAI-compatible wire
    /// (T25 F7): `"max_tokens"` or `"max_completion_tokens"`, anything else
    /// a startup error. `None` = `"max_tokens"`, so an absent field sends
    /// byte-identical requests to every config written before this existed.
    /// openai-compat only: setting it on an anthropic profile is a startup
    /// error rather than a silently ignored key.
    pub max_tokens_parameter: Option<String>,
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
    /// the `"auto"` default, run through [`auto_prompt_profile`] against
    /// [`ResolvedProfile::context_window`].
    pub prompt_profile: crate::tools::PromptProfile,
    /// How [`ResolvedProfile::prompt_profile`] was arrived at (T41): a
    /// config value that named it, or the auto rule. Only reporting reads
    /// this (the startup notice and the `/status` word); nothing about the
    /// prompt itself depends on it.
    pub prompt_profile_source: PromptProfileSource,
    /// Validated list prices per million tokens (T24), either both set or
    /// both absent (see [`ProfileConfig::price_input_per_mtok`]). Only the
    /// `/status` cost estimate reads them.
    pub price_input_per_mtok: Option<f64>,
    pub price_output_per_mtok: Option<f64>,
    /// Validated token-cap wire key (T25 F7), already defaulted. Only the
    /// OpenAI-compatible provider reads it; an anthropic selection carries
    /// the default and ignores it, because that wire uses `max_tokens`
    /// natively.
    pub max_tokens_parameter: crate::provider::MaxTokensParam,
}

impl ResolvedProfile {
    /// Whether this selection sends a credential. Anthropic always does
    /// (profile key file, else `APP_SECRET_FILE`); openai-compat does only
    /// when a key file is configured, since keyless local servers are the
    /// point of that provider. Mirrors the split in
    /// [`crate::provider::build_live_with_key`], and gates the `/status`
    /// cost estimate: an unkeyed endpoint is nobody's metered spend.
    pub fn is_keyed(&self) -> bool {
        self.provider == "anthropic" || self.api_key_file.is_some()
    }
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
    /// Which wire key carries the token cap (T25 F7). Same contract as
    /// [`ProfileConfig::max_tokens_parameter`]: `"max_tokens"` (the
    /// default when absent) or `"max_completion_tokens"`, anything else a
    /// startup error.
    pub max_tokens_parameter: Option<String>,
}

impl Default for OpenAiCompatConfig {
    fn default() -> Self {
        OpenAiCompatConfig {
            base_url: DEFAULT_OPENAI_COMPAT_BASE_URL.to_string(),
            model: String::new(),
            api_key_file: None,
            context_window: None,
            max_tokens_parameter: None,
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
            key_rotate_warn_days: DEFAULT_KEY_ROTATE_WARN_DAYS,
            allow_bash_without_key_sandbox: false,
            prose_tool_calls: true,
            cost_advisory_step_usd: None,
            auto_compact: None,
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

    /// Validate the GLOBAL `prompt_profile` spelling, rejecting anything
    /// but `"auto"` / `"full"` / `"compact"` / absent at startup. Returns
    /// the SPEC, not a profile: which profile an `"auto"` spec resolves to
    /// depends on a context window, which lives on the selection, not here.
    pub fn prompt_profile_spec(&self) -> Result<PromptProfileSpec, crate::error::Error> {
        PromptProfileSpec::parse(self.prompt_profile.as_deref()).ok_or_else(|| {
            let other = self.prompt_profile.as_deref().unwrap_or_default();
            crate::error::Error::Config(format!(
                "unknown prompt_profile {other:?} ({PROMPT_PROFILE_EXPECTED})"
            ))
        })
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

    /// Resolve the mid-session cost advisory step (T26), rejecting a value
    /// nobody could have meant at startup rather than silently never firing
    /// (same validated-accessor shape as [`Config::session_max_bytes`]).
    /// `0.0` comes back as `0.0` and means disabled: that is a real setting,
    /// not an error.
    pub fn cost_advisory_step_usd(&self) -> Result<f64, crate::error::Error> {
        match self.cost_advisory_step_usd {
            None => Ok(DEFAULT_COST_ADVISORY_STEP_USD),
            Some(v) if v.is_finite() && v >= 0.0 => Ok(v),
            Some(_) => Err(crate::error::Error::Config(
                "cost_advisory_step_usd must be a finite non-negative number (0 disables the advisory)".into(),
            )),
        }
    }

    /// Resolve the effective auto-compaction setting (T40). Infallible by
    /// type: a `bool` has nothing to validate, so unlike
    /// [`Config::cost_advisory_step_usd`] this is not a `Result`.
    ///
    /// An explicit value wins in every mode. Absent, the default is the
    /// answer to "is anyone here to act on the advisory?": one-shot `-p`
    /// has nobody, so it compacts itself; the plain REPL and the TUI have a
    /// user and keep the advisory plus `/compact`.
    pub fn auto_compact_enabled(&self, oneshot: bool) -> bool {
        self.auto_compact.unwrap_or(oneshot)
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
        // provider name above: absent = the global setting. T41: resolved
        // against THIS profile's window, so a small local profile and a
        // large hosted one in the same config each get the right answer.
        let spec = match p.prompt_profile.as_deref() {
            None => self.prompt_profile_spec()?,
            Some(other) => PromptProfileSpec::parse(Some(other)).ok_or_else(|| {
                crate::error::Error::Config(format!(
                    "profile {name:?}: unknown prompt_profile {other:?} ({PROMPT_PROFILE_EXPECTED})"
                ))
            })?,
        };
        let (prompt_profile, prompt_profile_source) = spec.resolve(p.context_window);
        // T24 prices, validated as eagerly as everything above. A negative
        // or non-finite rate is a typo, not a price; and exactly one of the
        // pair is the silent-disable case worth naming, since a profile
        // that looks priced but shows no estimate reads as a bug.
        for (field, value) in [
            ("price_input_per_mtok", p.price_input_per_mtok),
            ("price_output_per_mtok", p.price_output_per_mtok),
        ] {
            if let Some(v) = value {
                if !v.is_finite() || v < 0.0 {
                    return Err(crate::error::Error::Config(format!(
                        "profile {name:?}: {field} must be a finite non-negative number"
                    )));
                }
            }
        }
        if p.price_input_per_mtok.is_some() != p.price_output_per_mtok.is_some() {
            return Err(crate::error::Error::Config(format!(
                "profile {name:?}: price_input_per_mtok and price_output_per_mtok must be set together"
            )));
        }
        // T25 F7, validated as eagerly as everything above. The field names
        // an OpenAI-compatible wire key, so setting it on an anthropic
        // profile is a mistake worth naming rather than a silently ignored
        // key: that wire uses max_tokens natively and has no alternative.
        let max_tokens_parameter = match p.max_tokens_parameter.as_deref() {
            None => crate::provider::MaxTokensParam::default(),
            Some(_) if p.provider == "anthropic" => {
                return Err(crate::error::Error::Config(format!(
                    "profile {name:?}: max_tokens_parameter is openai-compat only (the anthropic wire uses \"max_tokens\" natively)"
                )))
            }
            Some(other) => crate::provider::MaxTokensParam::parse(other).ok_or_else(|| {
                crate::error::Error::Config(format!(
                    "profile {name:?}: unknown max_tokens_parameter {other:?} (expected \"max_tokens\" or \"max_completion_tokens\")"
                ))
            })?,
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
            prompt_profile_source,
            price_input_per_mtok: p.price_input_per_mtok,
            price_output_per_mtok: p.price_output_per_mtok,
            max_tokens_parameter,
        })
    }

    /// Resolve the BASE (non-profile) selection — the pre-T8 startup path,
    /// error messages included byte-for-byte. Used when no startup `profile`
    /// is set, so absent-profiles configs behave exactly as they always did.
    pub fn resolve_base(&self) -> Result<ResolvedProfile, crate::error::Error> {
        // T41: the base paths run the same rule as a named profile, each
        // against the window it actually has. The anthropic base carries
        // no window field at all, so auto lands on full there.
        let spec = self.prompt_profile_spec()?;
        let (base_prompt_profile, base_prompt_profile_source) = spec.resolve(None);
        match self.provider.as_str() {
            "anthropic" => Ok(ResolvedProfile {
                provider: self.provider.clone(),
                model: self.model.clone(),
                base_url: self.base_url.clone(),
                api_key_file: None,
                max_tokens: self.max_tokens,
                context_window: None,
                prompt_profile: base_prompt_profile,
                prompt_profile_source: base_prompt_profile_source,
                // T24: prices are a per-profile field, so the base
                // selection has nowhere to carry them and the estimate
                // stays off there.
                price_input_per_mtok: None,
                price_output_per_mtok: None,
                // T25 F7: the anthropic wire uses max_tokens natively and
                // the schema has no anthropic-side field to set, so the
                // default is the only reachable value here.
                max_tokens_parameter: crate::provider::MaxTokensParam::default(),
            }),
            "openai-compat" => {
                let oc = self.openai_compat.clone().unwrap_or_default();
                if oc.model.is_empty() {
                    return Err(crate::error::Error::Config(
                        "provider \"openai-compat\" requires openai_compat.model".into(),
                    ));
                }
                let max_tokens_parameter = match oc.max_tokens_parameter.as_deref() {
                    None => crate::provider::MaxTokensParam::default(),
                    Some(other) => crate::provider::MaxTokensParam::parse(other).ok_or_else(|| {
                        crate::error::Error::Config(format!(
                            "unknown openai_compat.max_tokens_parameter {other:?} (expected \"max_tokens\" or \"max_completion_tokens\")"
                        ))
                    })?,
                };
                let (oc_prompt_profile, oc_prompt_profile_source) =
                    spec.resolve(oc.context_window);
                Ok(ResolvedProfile {
                    provider: self.provider.clone(),
                    model: oc.model,
                    base_url: oc.base_url,
                    api_key_file: oc.api_key_file,
                    max_tokens: self.max_tokens,
                    prompt_profile: oc_prompt_profile,
                    prompt_profile_source: oc_prompt_profile_source,
                    context_window: oc.context_window,
                    price_input_per_mtok: None,
                    price_output_per_mtok: None,
                    max_tokens_parameter,
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

/// Persist a model id into the config FILE with a surgical
/// `serde_json::Value` edit (T15, `/model --save`). NEVER round-trips
/// through [`Config`]: that would silently drop unknown fields (the
/// schema tolerates them on purpose). Site: an active profile writes
/// `profiles.<name>.model`, fail-closed — the profile must still exist in
/// the FILE as an object, because inventing one here would write a config
/// that cannot start (profiles require a provider). Otherwise the base
/// selection's own model key: `openai_compat.model` for openai-compat
/// (the object is created if absent), the top-level `"model"` key for
/// anthropic — that IS the anthropic model key ([`Config::resolve_base`]
/// reads `self.model`; the schema has no nested anthropic object).
/// Atomic: temp file in the same directory, pretty 2-space with a
/// trailing newline, renamed over (serde_json's preserve_order keeps the
/// user's key order).
pub fn persist_model(
    cfg_path: &std::path::Path,
    active_profile: Option<&str>,
    provider: &str,
    model: &str,
) -> Result<(), crate::error::Error> {
    let raw = match std::fs::read_to_string(cfg_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(crate::error::Error::Config(
                "no config file; run temur init first".into(),
            ))
        }
        Err(e) => return Err(e.into()),
    };
    let mut v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| crate::error::Error::Config(format!("{}: {e}", cfg_path.display())))?;
    let Some(root) = v.as_object_mut() else {
        return Err(crate::error::Error::Config(format!(
            "{}: not a JSON object",
            cfg_path.display()
        )));
    };
    let model_value = serde_json::Value::String(model.to_string());
    match active_profile {
        Some(name) => {
            let Some(site) = root
                .get_mut("profiles")
                .and_then(|p| p.as_object_mut())
                .and_then(|p| p.get_mut(name))
                .and_then(|p| p.as_object_mut())
            else {
                return Err(crate::error::Error::Config(format!(
                    "profile {name:?} not found in {}",
                    cfg_path.display()
                )));
            };
            site.insert("model".to_string(), model_value);
        }
        None if provider == "openai-compat" => {
            let entry = root
                .entry("openai_compat".to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            let Some(site) = entry.as_object_mut() else {
                return Err(crate::error::Error::Config(format!(
                    "{}: \"openai_compat\" is not a JSON object",
                    cfg_path.display()
                )));
            };
            site.insert("model".to_string(), model_value);
        }
        None => {
            root.insert("model".to_string(), model_value);
        }
    }
    write_config_value(cfg_path, &v)
}

/// Serialize an edited config `Value` back to disk: pretty 2-space with a
/// trailing newline (serde_json's preserve_order keeps the user's key
/// order), temp-then-rename in the config's own directory so a crash
/// mid-write can never leave a truncated config behind. Shared by
/// [`persist_model`] and `init --add` (T17).
pub fn write_config_value(
    cfg_path: &std::path::Path,
    v: &serde_json::Value,
) -> Result<(), crate::error::Error> {
    let pretty = serde_json::to_string_pretty(v).expect("a parsed Value re-serializes");
    let dir = cfg_path.parent().filter(|d| !d.as_os_str().is_empty());
    let tmp = dir
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!(".temur-config.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{pretty}\n"))?;
    std::fs::rename(&tmp, cfg_path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
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

    /// A keyless local selection with no window and the auto profile: the
    /// exact shape T42 P4 probes for.
    fn keyless_local() -> ResolvedProfile {
        ResolvedProfile {
            provider: "openai-compat".into(),
            model: "local-gguf".into(),
            base_url: "http://127.0.0.1:8080/v1".into(),
            api_key_file: None,
            max_tokens: 3072,
            context_window: None,
            prompt_profile: crate::tools::PromptProfile::Full,
            prompt_profile_source: PromptProfileSource::Auto,
            price_input_per_mtok: None,
            price_output_per_mtok: None,
            max_tokens_parameter: crate::provider::MaxTokensParam::default(),
        }
    }

    #[test]
    fn the_startup_probe_gate_is_the_narrow_shape_it_claims_to_be() {
        assert!(wants_startup_context_probe(&keyless_local(), false));
        // --mock has no server to ask.
        assert!(!wants_startup_context_probe(&keyless_local(), true));
        // A configured window is authoritative and is never probed over.
        let mut p = keyless_local();
        p.context_window = Some(8192);
        assert!(!wants_startup_context_probe(&p, false));
        // A keyed endpoint: the T22 probe is keyless by construction and
        // is not pointed at anything that expects auth.
        let mut p = keyless_local();
        p.api_key_file = Some("/srv/secrets/key".into());
        assert!(!wants_startup_context_probe(&p, false));
        // Anthropic does not serve /props.
        let mut p = keyless_local();
        p.provider = "anthropic".into();
        assert!(!wants_startup_context_probe(&p, false));
    }

    #[test]
    fn a_probed_window_flows_into_the_selection_and_the_auto_profile() {
        // Below the auto threshold: the window lands and the profile flips.
        let mut p = keyless_local();
        let notice = apply_probed_context_window(&mut p, 12288);
        assert_eq!(p.context_window, Some(12288));
        assert_eq!(p.prompt_profile, crate::tools::PromptProfile::Compact);
        assert!(notice.contains("context window 12288 detected from the server (/props)"), "{notice}");
        assert!(!notice.contains('\n'), "one line: {notice}");
        assert!(notice.is_ascii(), "ASCII: {notice}");

        // At or above it: the window lands, the profile does not move.
        let mut p = keyless_local();
        apply_probed_context_window(&mut p, PROMPT_AUTO_COMPACT_BELOW);
        assert_eq!(p.context_window, Some(PROMPT_AUTO_COMPACT_BELOW));
        assert_eq!(p.prompt_profile, crate::tools::PromptProfile::Full);
    }

    #[test]
    fn an_explicit_prompt_profile_survives_a_probed_window() {
        // The window is a server fact; the profile is a user's decision.
        let mut p = keyless_local();
        p.prompt_profile = crate::tools::PromptProfile::Full;
        p.prompt_profile_source = PromptProfileSource::Explicit;
        apply_probed_context_window(&mut p, 4096);
        assert_eq!(p.context_window, Some(4096));
        assert_eq!(
            p.prompt_profile,
            crate::tools::PromptProfile::Full,
            "an explicit \"full\" is not overruled by a probe"
        );
    }

    #[test]
    fn the_startup_probe_never_asks_where_doctor_would_not() {
        // Parity pin. doctor's context_check probes when the selection is
        // openai-compat AND keyless (and the run allows network); startup
        // adds two further conditions. Startup must stay a strict SUBSET,
        // so no run can be probed at startup by a rule doctor does not
        // also consider safe.
        let doctor_would = |p: &ResolvedProfile| {
            p.provider == "openai-compat" && p.api_key_file.is_none()
        };
        let mut cases = vec![keyless_local()];
        for window in [None, Some(8192)] {
            for key in [None, Some("/k".to_string())] {
                for provider in ["openai-compat", "anthropic"] {
                    let mut p = keyless_local();
                    p.context_window = window;
                    p.api_key_file = key.clone();
                    p.provider = provider.into();
                    cases.push(p);
                }
            }
        }
        for p in &cases {
            for is_mock in [false, true] {
                if wants_startup_context_probe(p, is_mock) {
                    assert!(doctor_would(p), "startup probed where doctor would not: {p:?}");
                }
            }
        }
    }

    #[test]
    fn prose_tool_calls_defaults_true_and_false_parses() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.prose_tool_calls);
        let c: Config = serde_json::from_str(r#"{"prose_tool_calls":false}"#).unwrap();
        assert!(!c.prose_tool_calls);
    }

    #[test]
    fn key_rotate_warn_days_defaults_to_90_and_zero_parses() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c.key_rotate_warn_days, DEFAULT_KEY_ROTATE_WARN_DAYS);
        let c: Config = serde_json::from_str(r#"{"key_rotate_warn_days":0}"#).unwrap();
        assert_eq!(c.key_rotate_warn_days, 0);
    }

    #[test]
    fn allow_bash_without_key_sandbox_defaults_false_and_parses() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(!c.allow_bash_without_key_sandbox);
        let c: Config =
            serde_json::from_str(r#"{"allow_bash_without_key_sandbox":true}"#).unwrap();
        assert!(c.allow_bash_without_key_sandbox);
    }

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
        assert!(c.cost_advisory_step_usd.is_none());
        assert_eq!(
            c.cost_advisory_step_usd().unwrap(),
            DEFAULT_COST_ADVISORY_STEP_USD
        );
    }

    #[test]
    fn auto_compact_defaults_per_mode_and_an_explicit_value_wins() {
        // Absent: the default IS the invocation mode: one-shot has nobody
        // to act on an advisory, the REPL and TUI do.
        let c = Config::default();
        assert!(!c.auto_compact_enabled(false), "REPL/TUI default off");
        assert!(c.auto_compact_enabled(true), "one-shot -p default on");
        // Explicit true enables the same mechanism in an interactive session.
        let c: Config = serde_json::from_str(r#"{"auto_compact":true}"#).unwrap();
        assert!(c.auto_compact_enabled(false));
        assert!(c.auto_compact_enabled(true));
        // Explicit false restores advisory-only behaviour in one-shot.
        let c: Config = serde_json::from_str(r#"{"auto_compact":false}"#).unwrap();
        assert!(!c.auto_compact_enabled(false));
        assert!(!c.auto_compact_enabled(true));
        // Unknown-field tolerance is unchanged by the new key.
        let c: Config = serde_json::from_str(r#"{"auto_compact":true,"nope":1}"#).unwrap();
        assert_eq!(c.auto_compact, Some(true));
    }

    #[test]
    fn the_cost_advisory_step_parses_and_validates() {
        let c: Config = serde_json::from_str(r#"{"cost_advisory_step_usd":25}"#).unwrap();
        assert_eq!(c.cost_advisory_step_usd().unwrap(), 25.0);
        // 0 is the documented disable, not an error.
        let c: Config = serde_json::from_str(r#"{"cost_advisory_step_usd":0}"#).unwrap();
        assert_eq!(c.cost_advisory_step_usd().unwrap(), 0.0);
        // Absent = the default step.
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(
            c.cost_advisory_step_usd().unwrap(),
            DEFAULT_COST_ADVISORY_STEP_USD
        );
        // Negative and non-finite are typos, named by field at startup.
        let c: Config = serde_json::from_str(r#"{"cost_advisory_step_usd":-5}"#).unwrap();
        assert_eq!(
            c.cost_advisory_step_usd().unwrap_err().to_string(),
            "config: cost_advisory_step_usd must be a finite non-negative number (0 disables the advisory)"
        );
        let c = Config {
            cost_advisory_step_usd: Some(f64::NAN),
            ..Config::default()
        };
        assert!(c
            .cost_advisory_step_usd()
            .unwrap_err()
            .to_string()
            .contains("cost_advisory_step_usd"));
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

    // ------------------------------------------- T41: the "auto" rule

    /// The pure rule, at and around the threshold. An unknown window must
    /// stay Full: guessing smaller would trim descriptions on a model that
    /// never needed it.
    #[test]
    fn auto_prompt_profile_rule_table() {
        use crate::tools::PromptProfile;
        assert_eq!(auto_prompt_profile(None), PromptProfile::Full);
        assert_eq!(auto_prompt_profile(Some(0)), PromptProfile::Compact);
        assert_eq!(auto_prompt_profile(Some(16384)), PromptProfile::Compact);
        assert_eq!(auto_prompt_profile(Some(20479)), PromptProfile::Compact);
        assert_eq!(auto_prompt_profile(Some(20480)), PromptProfile::Full);
        assert_eq!(auto_prompt_profile(Some(20481)), PromptProfile::Full);
        assert_eq!(PROMPT_AUTO_COMPACT_BELOW, 20480);
    }

    /// An explicit spelling is never second-guessed, in either direction,
    /// at any window: that is the whole contract that survived T41.
    #[test]
    fn explicit_spellings_ignore_the_window_in_both_directions() {
        use crate::tools::PromptProfile;
        use PromptProfileSource::Explicit as E;
        let full = PromptProfileSpec::parse(Some("full")).unwrap();
        let compact = PromptProfileSpec::parse(Some("compact")).unwrap();
        for w in [None, Some(2048), Some(16384), Some(1_000_000)] {
            assert_eq!(full.resolve(w), (PromptProfile::Full, E), "window {w:?}");
            assert_eq!(compact.resolve(w), (PromptProfile::Compact, E), "window {w:?}");
        }
        // Absent and "auto" are the same spec, and it is the default.
        assert_eq!(PromptProfileSpec::parse(None), Some(PromptProfileSpec::Auto));
        assert_eq!(
            PromptProfileSpec::parse(Some("auto")),
            Some(PromptProfileSpec::Auto)
        );
        assert_eq!(
            PromptProfileSpec::Auto.resolve(Some(2048)),
            (PromptProfile::Compact, PromptProfileSource::Auto)
        );
        assert_eq!(
            PromptProfileSpec::Auto.resolve(Some(32768)),
            (PromptProfile::Full, PromptProfileSource::Auto)
        );
    }

    #[test]
    fn prompt_profile_spellings_and_invalid_is_startup_error() {
        // Absent = auto (T41 CHANGED this: it used to be full).
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.prompt_profile.is_none());
        assert_eq!(c.prompt_profile_spec().unwrap(), PromptProfileSpec::Auto);
        // Explicit values.
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"full"}"#).unwrap();
        assert_eq!(
            c.prompt_profile_spec().unwrap(),
            PromptProfileSpec::Explicit(crate::tools::PromptProfile::Full)
        );
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"compact"}"#).unwrap();
        assert_eq!(
            c.prompt_profile_spec().unwrap(),
            PromptProfileSpec::Explicit(crate::tools::PromptProfile::Compact)
        );
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"auto"}"#).unwrap();
        assert_eq!(c.prompt_profile_spec().unwrap(), PromptProfileSpec::Auto);
        // Anything else is a config error, not a silent fallback, and the
        // message names all three accepted spellings.
        let c: Config = serde_json::from_str(r#"{"prompt_profile":"tiny"}"#).unwrap();
        let err = c.prompt_profile_spec().unwrap_err().to_string();
        assert!(err.contains("tiny"), "error names the bad value: {err}");
        for word in ["auto", "full", "compact"] {
            assert!(err.contains(word), "error names {word:?}: {err}");
        }
    }

    /// The T41 default flip, pinned at the boundary. THROUGH v0.29.1 this
    /// case resolved Full ("never inferred from context_window"); a 2048
    /// window now resolves Compact through the auto rule, and an explicit
    /// "full" at the same window still resolves Full.
    #[test]
    fn a_small_window_now_auto_selects_compact_unless_config_says_otherwise() {
        use crate::tools::PromptProfile;
        let c: Config = serde_json::from_str(
            r#"{"provider":"openai-compat",
                "openai_compat":{"model":"m","context_window":2048}}"#,
        )
        .unwrap();
        let base = c.resolve_base().unwrap();
        assert_eq!(base.prompt_profile, PromptProfile::Compact);
        assert_eq!(base.prompt_profile_source, PromptProfileSource::Auto);

        let c: Config = serde_json::from_str(
            r#"{"provider":"openai-compat","prompt_profile":"full",
                "openai_compat":{"model":"m","context_window":2048}}"#,
        )
        .unwrap();
        let base = c.resolve_base().unwrap();
        assert_eq!(base.prompt_profile, PromptProfile::Full);
        assert_eq!(base.prompt_profile_source, PromptProfileSource::Explicit);

        // A large window under auto stays Full, and says it was auto.
        let c: Config = serde_json::from_str(
            r#"{"provider":"openai-compat",
                "openai_compat":{"model":"m","context_window":32768}}"#,
        )
        .unwrap();
        let base = c.resolve_base().unwrap();
        assert_eq!(base.prompt_profile, PromptProfile::Full);
        assert_eq!(base.prompt_profile_source, PromptProfileSource::Auto);
    }

    /// The anthropic base carries no window field, so auto can only land
    /// on Full there: no hosted config silently loses its descriptions.
    #[test]
    fn the_anthropic_base_has_no_window_so_auto_stays_full() {
        let c: Config = serde_json::from_str("{}").unwrap();
        let base = c.resolve_base().unwrap();
        assert_eq!(base.context_window, None);
        assert_eq!(base.prompt_profile, crate::tools::PromptProfile::Full);
        assert_eq!(base.prompt_profile_source, PromptProfileSource::Auto);
    }

    #[test]
    fn the_auto_compact_notice_names_the_window_the_threshold_and_the_override() {
        let line = auto_compact_notice(12288);
        assert_eq!(
            line,
            "prompt profile: compact (context_window 12288 is below 20480; \
             set prompt_profile to \"full\" to override)"
        );
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
    fn profile_prices_resolve_and_gate_keyedness() {
        let c: Config = serde_json::from_str(
            r#"{"profiles": {
                "priced": {"provider": "anthropic", "model": "m",
                           "price_input_per_mtok": 3.0, "price_output_per_mtok": 15.0},
                "plain":  {"provider": "openai-compat", "model": "m"}}}"#,
        )
        .unwrap();
        let profiles = c.resolved_profiles().unwrap();
        let priced = &profiles["priced"];
        assert_eq!(priced.price_input_per_mtok, Some(3.0));
        assert_eq!(priced.price_output_per_mtok, Some(15.0));
        assert!(priced.is_keyed(), "anthropic is keyed with no key file of its own");
        let plain = &profiles["plain"];
        assert_eq!(plain.price_input_per_mtok, None);
        assert!(!plain.is_keyed(), "keyless openai-compat");
        // The base selection has nowhere to carry prices (T24).
        assert_eq!(c.resolve_base().unwrap().price_input_per_mtok, None);
    }

    #[test]
    fn invalid_prices_are_startup_errors_naming_the_field() {
        let bad = |json: &str| -> String {
            let c: Config = serde_json::from_str(json).unwrap();
            c.resolved_profiles().unwrap_err().to_string()
        };
        assert_eq!(
            bad(r#"{"profiles": {"p": {"provider": "anthropic", "model": "m",
                    "price_input_per_mtok": -1.0, "price_output_per_mtok": 15.0}}}"#),
            "config: profile \"p\": price_input_per_mtok must be a finite non-negative number"
        );
        // Non-finite cannot arrive through JSON (serde_json rejects an
        // out-of-range literal at parse time), so drive the guard through
        // the struct the way any other caller of the type would.
        let c = Config {
            profiles: Some(std::collections::BTreeMap::from([(
                "p".to_string(),
                ProfileConfig {
                    provider: "anthropic".into(),
                    model: "m".into(),
                    price_input_per_mtok: Some(3.0),
                    price_output_per_mtok: Some(f64::INFINITY),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };
        assert_eq!(
            c.resolved_profiles().unwrap_err().to_string(),
            "config: profile \"p\": price_output_per_mtok must be a finite non-negative number"
        );
        // Half a pair silently disables the estimate; name both.
        assert_eq!(
            bad(r#"{"profiles": {"p": {"provider": "anthropic", "model": "m",
                    "price_input_per_mtok": 3.0}}}"#),
            "config: profile \"p\": price_input_per_mtok and price_output_per_mtok must be set together"
        );
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
    fn profile_prompt_profile_resolution_own_then_global_then_auto() {
        use crate::tools::PromptProfile;
        // Own value wins over the global; absent falls back to the global;
        // both absent = auto, which with no window on these profiles is
        // Full.
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

    /// T41: auto resolves per profile, against THAT profile's window. One
    /// config with a small local server and a large hosted model must get
    /// a different answer for each, from the same absent global field.
    #[test]
    fn auto_resolves_against_each_profiles_own_window() {
        use crate::tools::PromptProfile;
        let c: Config = serde_json::from_str(
            r#"{"profiles": {
                    "local":  { "provider": "openai-compat", "model": "m",
                                "context_window": 12288 },
                    "hosted": { "provider": "anthropic", "model": "claude-sonnet-5",
                                "context_window": 200000 },
                    "blind":  { "provider": "openai-compat", "model": "m" },
                    "pinned": { "provider": "openai-compat", "model": "m",
                                "context_window": 12288, "prompt_profile": "full" },
                    "asked":  { "provider": "openai-compat", "model": "m",
                                "context_window": 200000, "prompt_profile": "auto" }
                }}"#,
        )
        .unwrap();
        let p = c.resolved_profiles().unwrap();
        assert_eq!(p["local"].prompt_profile, PromptProfile::Compact);
        assert_eq!(p["local"].prompt_profile_source, PromptProfileSource::Auto);
        assert_eq!(p["hosted"].prompt_profile, PromptProfile::Full);
        assert_eq!(p["hosted"].prompt_profile_source, PromptProfileSource::Auto);
        // No window on the profile: nothing to infer from, so Full.
        assert_eq!(p["blind"].prompt_profile, PromptProfile::Full);
        // An explicit value beats the window, and says it was explicit.
        assert_eq!(p["pinned"].prompt_profile, PromptProfile::Full);
        assert_eq!(p["pinned"].prompt_profile_source, PromptProfileSource::Explicit);
        // A per-profile "auto" is a real spelling, not an error.
        assert_eq!(p["asked"].prompt_profile, PromptProfile::Full);
        assert_eq!(p["asked"].prompt_profile_source, PromptProfileSource::Auto);
    }

    /// The global spec still reaches a profile that names none: a global
    /// "full" survives a tiny per-profile window.
    #[test]
    fn a_global_explicit_value_still_covers_every_profile_that_names_none() {
        use crate::tools::PromptProfile;
        let c: Config = serde_json::from_str(
            r#"{"prompt_profile": "full",
                "profiles": {"tiny": {"provider": "openai-compat", "model": "m",
                                      "context_window": 2048}}}"#,
        )
        .unwrap();
        let p = c.resolved_profiles().unwrap();
        assert_eq!(p["tiny"].prompt_profile, PromptProfile::Full);
        assert_eq!(p["tiny"].prompt_profile_source, PromptProfileSource::Explicit);
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
            err.contains("\"bad\"")
                && err.contains("tiny")
                && err.contains("auto")
                && err.contains("full")
                && err.contains("compact"),
            "error names profile, value, and every accepted spelling: {err}"
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

    // ------------------------------------------- T15: /model --save edits

    fn persist_roundtrip(json: &str, profile: Option<&str>, provider: &str, model: &str) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, json).unwrap();
        persist_model(&path, profile, provider, model).unwrap();
        std::fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn persist_model_profile_site_edits_only_that_profile() {
        let saved = persist_roundtrip(PROFILES_JSON, Some("local"), "openai-compat", "new-id");
        let c: Config = serde_json::from_str(&saved).unwrap();
        let profiles = c.resolved_profiles().unwrap();
        assert_eq!(profiles["local"].model, "new-id");
        assert_eq!(profiles["sonnet"].model, "claude-sonnet-5", "other profile untouched");
        assert_eq!(c.max_tokens, 2048, "top-level fields untouched");
    }

    #[test]
    fn persist_model_base_openai_compat_site() {
        let saved = persist_roundtrip(
            r#"{"provider":"openai-compat","openai_compat":{"base_url":"http://h:1/v1","model":"old"}}"#,
            None,
            "openai-compat",
            "new-id",
        );
        let c: Config = serde_json::from_str(&saved).unwrap();
        let oc = c.openai_compat.unwrap();
        assert_eq!(oc.model, "new-id");
        assert_eq!(oc.base_url, "http://h:1/v1", "sibling keys survive");
    }

    #[test]
    fn persist_model_base_anthropic_site_is_the_top_level_model_key() {
        let saved = persist_roundtrip(r#"{"model":"old","max_tokens":64000}"#, None, "anthropic", "claude-opus-5");
        let c: Config = serde_json::from_str(&saved).unwrap();
        assert_eq!(c.model, "claude-opus-5");
        assert_eq!(c.max_tokens, 64_000);
    }

    #[test]
    fn persist_model_creates_an_absent_openai_compat_object() {
        let saved = persist_roundtrip(r#"{"provider":"openai-compat"}"#, None, "openai-compat", "m");
        let v: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(v["openai_compat"]["model"], "m");
    }

    #[test]
    fn persist_model_preserves_unknown_fields_and_key_order() {
        // Hand-ordered on purpose: model NOT first, unknown fields scattered.
        // preserve_order + the Value edit must keep every byte except the
        // model value (the fixture is already pretty 2-space).
        let fixture = "{\n  \"future_field\": 123,\n  \"provider\": \"openai-compat\",\n  \"openai_compat\": {\n    \"base_url\": \"http://h:9/v1\",\n    \"custom_note\": \"keep me\",\n    \"model\": \"old\",\n    \"context_window\": 8192\n  },\n  \"zeta\": true\n}\n";
        let saved = persist_roundtrip(fixture, None, "openai-compat", "new");
        assert_eq!(saved, fixture.replace("\"old\"", "\"new\""));
    }

    #[test]
    fn persist_model_missing_file_and_missing_profile_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        let err = persist_model(&path, None, "anthropic", "m").unwrap_err().to_string();
        assert!(err.contains("no config file") && err.contains("temur init"), "{err}");

        std::fs::write(&path, r#"{"profiles":{"real":{"provider":"anthropic","model":"m"}}}"#)
            .unwrap();
        let err = persist_model(&path, Some("ghost"), "anthropic", "m")
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"ghost\"") && err.contains("not found"), "{err}");
        // Fail-closed means the file is untouched.
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("\"real\"") && !after.contains("ghost"), "{after}");
    }

    #[test]
    fn missing_file_yields_defaults_and_reports_absence() {
        let (c, existed) = Config::load_from_reporting(std::path::Path::new(
            "/nonexistent/temur-test/config.json",
        ))
        .unwrap();
        assert_eq!(c.model, DEFAULT_MODEL);
        assert!(!existed, "a missing file must report existed=false");
    }
}
