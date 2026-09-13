//! `temur init` (T14): a line-based wizard that writes a starter config.
//!
//! Line-based on purpose: answers can be piped in, so tests (and scripts)
//! drive it exactly like a human would. Key handling follows the by-path
//! rule: for keyed templates the wizard creates the key file EMPTY (mode
//! 600, parent dir 700 if it has to create it) and tells the user to paste
//! the key in with their editor. T17 amendment (operator-approved, narrow,
//! the same pattern as T15's keyless-GET amendment): the init wizard, and
//! no other surface, may additionally accept the key at a hidden prompt
//! right after creating (or finding) an EMPTY key file, writing it straight
//! to that file; see [`prompt_key_entry`] and the RUNBOOK amendment record.
//! It never echoes, logs, or stores key material anywhere else and never
//! takes it from argv or env. T51 amendment: a NON-empty key file can be
//! replaced, but only after an explicit `y` to a question that defaults to
//! No, and only in the interactive wizard. `--force` still governs the
//! config alone; that fence is deliberate (see [`setup_key_file`]).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// One selectable starter template. `key_slug` is `Some` for keyed
/// templates and names the provider piece of the default key file path
/// (`~/.secrets/temur-<slug>-key`).
struct Template {
    number: &'static str,
    name: &'static str,
    describe: &'static str,
    default_model: &'static str,
    key_slug: Option<&'static str>,
}

/// The hosted templates' default model ids carry live evidence from the
/// T13 acceptance run of 2026-08-05, where the shipped defaults were the
/// thing that failed:
///
/// - openai: `gpt-4o-mini` was absent from the operator's account listing
///   entirely (11 ids, all gpt-4o or gpt-5 family), so the template could
///   not make a call as shipped. `gpt-4o` is listed and answered a real
///   tool turn first try, with the completion limit below baked beside it.
///   The gpt-5 era ids reject `max_tokens` outright and want
///   `max_completion_tokens`; since T25 F7 that is one config field away
///   (`"max_tokens_parameter": "max_completion_tokens"` on the profile),
///   so those ids are reachable, just not from a template as shipped.
///   Nothing here bakes the field on purpose: `gpt-4o` is the default and
///   it wants the classic name, so a baked alternate would break the very
///   id this template was fixed to.
/// - gemini: `gemini-2.5-flash` is retired for new users, in Google's own
///   404 words. `gemini-3.6-flash` answered a real tool turn live.
///   Note that Gemini's listing prefixes every id with `models/`, so this
///   bare id is a wire id that works, not a listing entry.
/// - xai: no key was available, so `grok-4` is still spec-written, not
///   live-verified.
///
/// KNOWLEDGE-OF-THAT-DATE, like the anthropic table below: init never
/// makes an authenticated call, so nothing here is detected. Hosted model
/// ids retire, which is exactly how this list broke; `/models` in session
/// is where a stale default surfaces.
const TEMPLATES: [Template; 5] = [
    Template {
        number: "1",
        name: "local",
        describe: "a llama.cpp / Ollama / LM Studio server you run",
        default_model: "qwen3-4b",
        key_slug: None,
    },
    Template {
        number: "2",
        name: "anthropic",
        describe: "Anthropic API (your API key)",
        default_model: "claude-sonnet-5",
        key_slug: Some("anthropic"),
    },
    Template {
        number: "3",
        name: "openai",
        describe: "OpenAI API (your API key)",
        default_model: "gpt-4o",
        key_slug: Some("openai"),
    },
    Template {
        number: "4",
        name: "gemini",
        describe: "Gemini API (your API key)",
        default_model: "gemini-3.6-flash",
        key_slug: Some("gemini"),
    },
    // T17: flagship coverage. The default model id is free text like every
    // hosted template's, and T13 (parked until keys exist) live-verifies
    // the hosted providers, this one included.
    Template {
        number: "5",
        name: "xai",
        describe: "xAI Grok API (your API key)",
        default_model: "grok-4",
        key_slug: Some("xai"),
    },
];

/// The anthropic template's curated profile set (T16): one profile per
/// current Anthropic model tier, all sharing the ONE key file the wizard
/// asks for. NAME order on purpose: profiles is a BTreeMap downstream, so
/// this is also the order every listing shows.
///
/// The third field is the profile's baked `context_window` (T13 P2.5).
/// It is PER MODEL, not one shared constant: the operator's live
/// acceptance on 2026-08-04 read `max_input_tokens` off the authenticated
/// `/v1/models` wire and haiku reported a fifth of what the other three
/// do, so a single number would have been wrong for whichever tier it did
/// not match. Haiku's value was measured on `claude-haiku-4-5-20251001`,
/// because the bare `claude-haiku-4-5` alias this table bakes is not in
/// the listing at all (the T16 haiku-alias precedent, confirmed live);
/// treating the alias as that snapshot is an inference, not a reading.
///
/// KNOWLEDGE-OF-THAT-DATE, not auto-detected: init never makes an
/// authenticated call. The in-session `/models` command (T22 P3) and
/// `doctor` live-check these against the wire, so later drift surfaces
/// there rather than silently here.
///
/// The fourth and fifth fields are the profile's baked
/// `price_input_per_mtok` / `price_output_per_mtok` (T24), USD per
/// million tokens at Anthropic's STANDARD list rate,
/// knowledge-of-record 2026-08-19. They feed the `/status` cost estimate
/// and nothing else. Sonnet moved 3.0/15.0 -> 2.0/10.0 in T35: the
/// pricing page now records the $2/$10 launch rate as the standard
/// price and states that the increase to $3/$15 scheduled for
/// 2026-09-01 will not occur, so the older pair is no longer a rate
/// anyone gets charged. Same knowledge-of-that-date caveat as the
/// windows above: list prices change, and nothing here re-checks them.
const ANTHROPIC_PROFILES: [(&str, &str, u64, f64, f64); 4] = [
    ("fable", "claude-fable-5", 1_000_000, 10.0, 50.0),
    ("haiku", "claude-haiku-4-5", 200_000, 1.0, 5.0),
    ("opus", "claude-opus-5", 1_000_000, 5.0, 25.0),
    ("sonnet", "claude-sonnet-5", 1_000_000, 2.0, 10.0),
];

/// A baked price as JSON. serde_json is the authority for the number's
/// spelling (the `--add` path writes through it), so the wizard's
/// hand-rolled render routes through the same formatter and the two
/// paths stay byte-identical, the same invariant `context_window` has.
fn price_json(v: f64) -> String {
    serde_json::Number::from_f64(v)
        .expect("baked prices are finite")
        .to_string()
}

/// The startup profile the anthropic template defaults to. Keeps the
/// pre-T16 default model (claude-sonnet-5): no default flip.
const ANTHROPIC_DEFAULT_PROFILE: &str = "sonnet";

/// The local template's context_window when the /props probe could not
/// answer (server down, or not llama.cpp): the README recipe's baked
/// value, matching serve.sh's default CTX.
const LOCAL_BAKED_CONTEXT_WINDOW: u64 = 8192;

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
const XAI_BASE_URL: &str = "https://api.x.ai/v1";

/// The hosted openai-compat templates' fixed endpoints, one lookup shared
/// by the fresh render and `init --add` so a new template lands in both.
fn compat_base_url(template_name: &str) -> &'static str {
    match template_name {
        "openai" => OPENAI_BASE_URL,
        "gemini" => GEMINI_BASE_URL,
        "xai" => XAI_BASE_URL,
        other => unreachable!("template {other} has no fixed base URL"),
    }
}

/// A hosted template's baked `max_tokens`, for providers whose completion
/// cap is BELOW temur's global default of
/// [`crate::config::DEFAULT_MAX_TOKENS`]. Absent means "the global default
/// is fine", which is the answer for every provider but one.
///
/// gpt-4o caps completions at 16384 and rejects the request outright above
/// it, so a fresh openai profile inheriting the global 32000 failed every
/// call until the operator set this by hand (T13 acceptance, live
/// 2026-08-05). Gemini accepted 32000 on the same run, so it bakes
/// nothing. Same lookup shape as [`compat_base_url`], for the same reason:
/// the fresh render and `init --add` must agree.
fn compat_max_tokens(template_name: &str) -> Option<u32> {
    match template_name {
        "openai" => Some(16_384),
        _ => None,
    }
}

/// A hosted template's baked `context_window`, for providers whose served
/// window is a published property of the model id the template defaults to.
/// Absent means "leave it unset", which turns the context advisory,
/// auto-compaction and the context-scaled tool-output ceiling off for that
/// profile until the operator adds a line by hand.
///
/// Gemini is the one template that can bake this honestly: it serves a
/// single published window for the versioned id the template already
/// defaults to, `gemini-3.6-flash`, at 1,000,000 tokens as of this
/// commit's knowledge date, 2026-09-13. Same knowledge-of-date caveat the
/// anthropic profiles carry: a provider that changes the window under a
/// versioned id would make this stale, which is why the id is versioned
/// rather than a floating alias. No PRICES are baked for any hosted
/// non-anthropic provider (T51): those rot faster than windows do, and
/// docs/USAGE.md shows where to put them instead. openai and xai bake
/// nothing here, so their renders stay byte-identical.
///
/// Same lookup shape as [`compat_base_url`] and [`compat_max_tokens`], for
/// the same reason: the fresh render and `init --add` must agree.
fn compat_context_window(template_name: &str) -> Option<u64> {
    match template_name {
        "gemini" => Some(1_000_000),
        _ => None,
    }
}

/// How many listed model ids the picker prints before folding the rest
/// into an "... and N more" line (a number still selects any of them).
const MODEL_LIST_CAP: usize = 20;

/// Baked model shortlist (T15 P4), printed ONLY when the local template's
/// picker could not run (no server to ask). A hand-kept SUMMARY of
/// docs/OFFLINE.md, section "Recommended small models", which stays
/// canonical: update that table first and mirror it here.
/// When the picker works, the server's real listing wins and this never
/// prints.
///
/// The two entries are the table's primary row and its low-RAM floor, not
/// simply its top two rows (T30, the T29 finding 7 flip): a fallback
/// shortlist has to answer "what should I run" and "what if this machine
/// is small", and the second-highest scorer answers neither.
const MODEL_SHORTLIST: &[&str] = &[
    "Known-good small models:",
    "  Qwen3-4B-Instruct-2507 Q4_K_M (~3.4 GB RAM at 8k context)",
    "  Qwen3-1.7B Q4_K_M (~2.1 GB RAM; the low-RAM choice)",
    "Larger is better when RAM allows; 7B+ is qualitatively different.",
    "More in docs/OFFLINE.md, \"Recommended small models\".",
];

/// The one line under the hosted-compat Model question (T51). anthropic
/// has its profile table; openai/gemini/xai printed a bare
/// "Model [default]:" and an operator pressed Enter blind, not knowing
/// other ids existed. Deliberately NOT a list: baked hosted lists rot (the
/// TEMPLATES comment above is the record of that), so this names the live
/// source instead of becoming one more thing to keep current.
const HOSTED_MODEL_HINT: &str =
    "Any model id your account offers works; /models lists them once your key is in.";

/// Render the config JSON for a template. Built by hand (not serde) so the
/// field order matches the README recipes byte for byte; user-supplied
/// strings go through serde_json escaping. `base_url` is the local
/// template's answered base URL (T15); when it is the default the render
/// stays byte-identical to the pre-T15 recipe. `detected_window` is the
/// local template's /props answer (T22): written verbatim when present,
/// the baked [`LOCAL_BAKED_CONTEXT_WINDOW`] otherwise.
fn render_config(
    template: &Template,
    model: &str,
    key_file: Option<&str>,
    base_url: Option<&str>,
    detected_window: Option<u64>,
) -> String {
    let m = serde_json::to_string(model).expect("string serializes");
    match template.name {
        "local" => {
            let w = detected_window.unwrap_or(LOCAL_BAKED_CONTEXT_WINDOW);
            match base_url {
                Some(b) if b != crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL => {
                    let b = serde_json::to_string(b).expect("string serializes");
                    format!(
                        "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{ \"base_url\": {b},\n                     \"model\": {m}, \"context_window\": {w} }}\n}}\n"
                    )
                }
                _ => format!(
                    "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{ \"model\": {m}, \"context_window\": {w} }}\n}}\n"
                ),
            }
        }
        "anthropic" => {
            // T16: the curated profile set. `model` carries the answered
            // STARTUP PROFILE NAME (from pick_startup_profile), not a model
            // id; every profile reads the same key file.
            let k = serde_json::to_string(key_file.expect("anthropic is keyed"))
                .expect("string serializes");
            let mut s = String::from("{\n  \"profiles\": {\n");
            for (i, (name, model_id, window, pin, pout)) in ANTHROPIC_PROFILES.iter().enumerate() {
                let comma = if i + 1 == ANTHROPIC_PROFILES.len() { "" } else { "," };
                let label = format!("\"{name}\":");
                let (pin, pout) = (price_json(*pin), price_json(*pout));
                s.push_str(&format!(
                    "    {label:<9} {{ \"provider\": \"anthropic\", \"model\": \"{model_id}\",\n                \"api_key_file\": {k},\n                \"context_window\": {window},\n                \"price_input_per_mtok\": {pin}, \"price_output_per_mtok\": {pout} }}{comma}\n"
                ));
            }
            s.push_str(&format!("  }},\n  \"profile\": {m}\n}}\n"));
            s
        }
        "openai" | "gemini" | "xai" => {
            let base = compat_base_url(template.name);
            let k = serde_json::to_string(key_file.expect("keyed template"))
                .expect("string serializes");
            // Same top-level placement as the local template's limit, and
            // omitted entirely when the global default already fits, so the
            // templates that need nothing render byte-identically to before.
            let limit = match compat_max_tokens(template.name) {
                Some(n) => format!("  \"max_tokens\": {n},\n"),
                None => String::new(),
            };
            // Inside the openai_compat block, because that is where the field
            // lives for this provider shape. Omitted entirely when the
            // template bakes none, so openai and xai render byte-identically
            // to before.
            let window = match compat_context_window(template.name) {
                Some(n) => format!("                     \"context_window\": {n},\n"),
                None => String::new(),
            };
            format!(
                "{{\n  \"provider\": \"openai-compat\",\n{limit}  \"openai_compat\": {{ \"base_url\": \"{base}\",\n                     \"model\": {m},\n{window}                     \"api_key_file\": {k} }}\n}}\n"
            )
        }
        other => unreachable!("unknown template {other}"),
    }
}

/// Ask one question and read one line. Empty answer = `default`. EOF is an
/// error: with piped answers a short script is a bug, not a choice.
fn ask(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    prompt: &str,
    default: &str,
) -> Result<String, crate::error::Error> {
    write!(out, "{prompt} [{default}]: ")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(crate::error::Error::Config(
            "init: unexpected end of input (the wizard needs an answer per question)".into(),
        ));
    }
    let ans = line.trim();
    Ok(if ans.is_empty() {
        default.to_string()
    } else {
        ans.to_string()
    })
}

/// One yes/no question, default No (T51). Interactive wizard only, so
/// nothing here can consume a piped answer; EOF and an empty answer are
/// both No, which is always the do-nothing branch.
fn ask_yes_no(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    question: &str,
) -> Result<bool, crate::error::Error> {
    write!(out, "{question} [y/N]: ")?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(false);
    }
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Expand a leading `~/` against `home`. Anything else passes through.
fn expand_tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(path),
    }
}

/// Model picker over a live server listing (T15). Prints the ids numbered
/// (capped at [`MODEL_LIST_CAP`]; a number still selects any entry) and
/// asks one question whose answer is a NUMBER into the listing or a
/// free-text model id. Default: the template's default model when the
/// server lists it, else the first listed id.
fn pick_model(
    ids: &[String],
    template_default: &str,
    base_url: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<String, crate::error::Error> {
    writeln!(out, "Models on {base_url}:")?;
    for (i, id) in ids.iter().take(MODEL_LIST_CAP).enumerate() {
        writeln!(out, "  {}) {id}", i + 1)?;
    }
    if ids.len() > MODEL_LIST_CAP {
        writeln!(out, "  ... and {} more", ids.len() - MODEL_LIST_CAP)?;
    }
    let default = if ids.iter().any(|i| i == template_default) {
        template_default.to_string()
    } else {
        ids[0].clone()
    };
    let answer = ask(input, out, "Model (number or id)", &default)?;
    if answer.chars().all(|c| c.is_ascii_digit()) {
        match answer.parse::<usize>() {
            Ok(n) if (1..=ids.len()).contains(&n) => Ok(ids[n - 1].clone()),
            _ => Err(crate::error::Error::Config(format!(
                "init: model number {answer} is out of range (1-{})",
                ids.len()
            ))),
        }
    } else {
        Ok(answer)
    }
}

/// The local template's flow (T15): ask for the base URL, try the keyless
/// listing there and run the picker, falling back to the free-text model
/// question (after the baked shortlist) when the listing fails or is empty.
/// T22: then probe the same server's `/props` for its actual context
/// allocation: found, it is announced and returned for the render to
/// write verbatim; not found (server down, or not llama.cpp) is silent
/// and the baked value applies. Returns `(base_url, model, n_ctx,
/// server_up)`, where `server_up` records that the listing or /props
/// actually answered: T51 makes the closing lines depend on it, so the
/// wizard never tells you to start a server it just talked to.
/// Shared by the fresh wizard and `init --add`.
fn ask_local_base_and_model(
    template: &Template,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    list_models: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    probe_context: &dyn Fn(&str) -> Option<u64>,
) -> Result<(String, String, Option<u64>, bool), crate::error::Error> {
    let base = ask(
        input,
        out,
        "Base URL",
        crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL,
    )?;
    let mut server_up = false;
    let picked = match list_models(&base) {
        Ok(ids) if !ids.is_empty() => {
            server_up = true;
            pick_model(&ids, template.default_model, &base, input, out)?
        }
        outcome => {
            let why = match outcome {
                Ok(_) => "the server returned an empty listing".to_string(),
                Err(e) => e.to_string(),
            };
            writeln!(out, "could not list models from {base}: {why}")?;
            for line in MODEL_SHORTLIST {
                writeln!(out, "{line}")?;
            }
            ask(input, out, "Model id", template.default_model)?
        }
    };
    let detected = probe_context(&base);
    server_up |= detected.is_some();
    if let Some(n) = detected {
        writeln!(
            out,
            "Server reports {n} tokens of context (/props n_ctx); writing \"context_window\": {n}."
        )?;
    }
    Ok((base, picked, detected, server_up))
}

/// Does an answer to the key file PATH question look like pasted API key
/// material instead of a path (T21)? Heuristic as specified: no '/'
/// anywhere, at least 20 chars, and every char in [A-Za-z0-9_-]. A rare
/// slashless long filename trips it too; re-answering with a path (any
/// '/', e.g. ./name) gets through.
fn looks_like_key_material(answer: &str) -> bool {
    !answer.contains('/')
        && answer.chars().count() >= 20
        && answer
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The warning when a key-shaped value lands at the PATH question (T21).
/// It NEVER echoes the value; the value is dropped, never used or stored.
fn warn_key_shaped(out: &mut dyn Write) -> Result<(), crate::error::Error> {
    writeln!(out)?;
    writeln!(out, "WARNING: that looks like a key, not a path. It was not used or stored,")?;
    writeln!(out, "but it reached this terminal: if it was a real key, rotate it.")?;
    Ok(())
}

/// A path with no directory part at all (`cow`) has
/// `Path::parent() == Some("")`, which no directory API can create: T51/D18
/// turned that into "No such file or directory (os error 2)" naming nothing.
/// A directory-less path means the working directory, so say so.
fn normalize_key_path(path: PathBuf) -> PathBuf {
    match path.parent() {
        Some(dir) if dir.as_os_str().is_empty() => Path::new(".").join(path),
        _ => path,
    }
}

/// Everything about a key path that can be known before anything is
/// written. T51 makes init all-or-nothing, so a path that cannot work is
/// rejected while there is still no config to take back. Returns the plain
/// reason, not an `Error`: interactive callers print it and re-ask, and
/// only the piped path wraps it, so it is never printed twice.
fn check_key_path(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Err(format!(
            "init: {} is a directory; give the path of the key file itself",
            path.display()
        ));
    }
    match path.parent() {
        Some(dir) if dir.exists() && !dir.is_dir() => Err(format!(
            "init: {} is not a directory, so the key file {} cannot be created",
            dir.display(),
            path.display()
        )),
        _ => Ok(()),
    }
}

/// The key FILE PATH question for keyed templates. The key itself never
/// passes through temur, in any direction; a key-shaped answer is dropped
/// with a warning and the question re-asked (interactive) or the wizard
/// fails closed (piped, where re-asking would misalign the scripted
/// answers). Shared by the fresh wizard and `init --add`.
fn ask_key_file(
    slug: &str,
    home: Option<&Path>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    interactive: bool,
) -> Result<PathBuf, crate::error::Error> {
    let default = match home {
        Some(h) => h
            .join(".secrets")
            .join(format!("temur-{slug}-key"))
            .display()
            .to_string(),
        None => String::new(),
    };
    loop {
        let answer = ask(input, out, "Where to save your API key", &default)?;
        if answer.is_empty() {
            return Err(crate::error::Error::Config(
                "init: no HOME to derive a default key file path; enter one explicitly".into(),
            ));
        }
        if looks_like_key_material(&answer) {
            warn_key_shaped(out)?;
            if interactive {
                continue;
            }
            return Err(crate::error::Error::Config(
                "init: the answer to the key file path question was key-shaped; nothing was stored. Re-run init and answer with a file path".into(),
            ));
        }
        let path = normalize_key_path(expand_tilde(&answer, home));
        if let Err(why) = check_key_path(&path) {
            if interactive {
                writeln!(out, "{why}")?;
                continue;
            }
            return Err(crate::error::Error::Config(why));
        }
        return Ok(path);
    }
}

/// Terminal seam for the hidden key prompt (T17 P3). The real
/// implementation ([`StdinKeyTerminal`]) wraps stdin's termios; tests
/// inject a fake to assert the guard discipline without a pty.
pub trait KeyEntryTerminal {
    /// True when stdin is a real TTY, i.e. echo suppression is possible
    /// and needed.
    fn is_tty(&self) -> bool;
    /// Disable echo until [`KeyEntryTerminal::restore`]. Returns false when
    /// the terminal refused; the caller then reads the line plain.
    fn begin_hidden(&mut self) -> bool;
    /// Undo [`KeyEntryTerminal::begin_hidden`].
    fn restore(&mut self);
}

/// The real stdin terminal: termios ECHO off with the prior state saved,
/// and SIGINT ignored for the same span so a Ctrl+C cannot kill the
/// process while echo is off (init installs no signal handler, so the
/// default action would leave the operator's terminal not echoing).
/// Both are restored together by [`KeyEntryTerminal::restore`].
pub struct StdinKeyTerminal {
    saved: Option<(libc::termios, libc::sighandler_t)>,
}

impl StdinKeyTerminal {
    pub fn new() -> Self {
        StdinKeyTerminal { saved: None }
    }
}

impl Default for StdinKeyTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyEntryTerminal for StdinKeyTerminal {
    fn is_tty(&self) -> bool {
        (unsafe { libc::isatty(libc::STDIN_FILENO) }) == 1
    }

    fn begin_hidden(&mut self) -> bool {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return false;
            }
            let saved_termios = t;
            t.c_lflag &= !libc::ECHO;
            // TCSAFLUSH also drains type-ahead, the usual password-prompt
            // hygiene.
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &t) != 0 {
                return false;
            }
            let old_sigint = libc::signal(libc::SIGINT, libc::SIG_IGN);
            self.saved = Some((saved_termios, old_sigint));
            true
        }
    }

    fn restore(&mut self) {
        if let Some((t, old_sigint)) = self.saved.take() {
            unsafe {
                let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &t);
                libc::signal(libc::SIGINT, old_sigint);
            }
        }
    }
}

/// RAII over [`KeyEntryTerminal::restore`]: the terminal comes back on
/// EVERY exit from the hidden read, error paths included.
struct HiddenEntryGuard<'a> {
    term: &'a mut dyn KeyEntryTerminal,
    active: bool,
}

impl Drop for HiddenEntryGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.term.restore();
        }
    }
}

/// Best-effort zero of a secret-bearing buffer. Volatile so the zeroing is
/// not optimized away as a dead store. Best-effort ONLY: read_line and the
/// file-write path may hold copies safe Rust cannot reach (the RUNBOOK
/// amendment record says so out loud).
fn wipe(s: &mut String) {
    for b in unsafe { s.as_mut_str().as_bytes_mut() } {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    s.clear();
}

/// The hidden key prompt (T17 P3): the one place in the whole product that
/// accepts key material, a deliberate NARROW amendment of the T14 rule
/// "init never accepts key material" (contract in the RUNBOOK amendment
/// record). Called for a key file known to be empty, or for a non-empty one
/// the user has just consented to replace (T51); the write truncates either
/// way. Returns whether a key was saved. Empty answer or EOF = skip; the
/// answer is never echoed, logged, or included in any notice.
fn prompt_key_entry(
    key_path: &Path,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    term: &mut dyn KeyEntryTerminal,
) -> Result<bool, crate::error::Error> {
    write!(out, "Paste your API key (hidden; Enter to skip): ")?;
    out.flush()?;
    let mut line = String::new();
    let hidden = term.is_tty() && term.begin_hidden();
    let read = {
        let _guard = HiddenEntryGuard { term, active: hidden };
        input.read_line(&mut line)
        // _guard drops HERE: echo and SIGINT restored before anything
        // else happens, the error path included.
    };
    // Terminate the prompt line: on a TTY this is the newline the disabled
    // echo swallowed, and off one it is the newline no echo ever wrote.
    writeln!(out)?;
    let n = match read {
        Ok(n) => n,
        Err(e) => {
            wipe(&mut line);
            return Err(e.into());
        }
    };
    let key = line.trim();
    // EOF and an empty answer both mean skip: the prompt is optional by
    // contract, so piped wizards whose answers end here are fine.
    if n == 0 || key.is_empty() {
        wipe(&mut line);
        return Ok(false);
    }
    let written = (|| -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(key_path)?;
        f.write_all(key.as_bytes())?;
        // Trailing newline, exactly as an editor paste would leave it
        // (secret::load_api_key_from trims).
        f.write_all(b"\n")?;
        f.flush()?;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
    })();
    wipe(&mut line);
    written?;
    writeln!(out, "Saved to {}.", key_path.display())?;
    Ok(true)
}

/// Key file creation plus key entry, shared by the fresh wizard and
/// `init --add`: created EMPTY with tight modes. An empty key file, fresh
/// or found, gets the hidden prompt (T17); skipping it keeps the T14
/// editor instruction.
///
/// A NON-empty file may hold a real key, which temur must never read. T51
/// (D17: a wrong key was pasted and `init --force` answered "left
/// untouched", stranding the operator) adds the one way through: the
/// interactive wizard asks once, defaulting to No, and only an explicit
/// `y` reaches the hidden prompt, which truncates and rewrites at 0600.
/// Off a TTY there is no question, so the message carries the way out
/// instead. `--force` is NOT that way through and never becomes it: it
/// governs the config only (see [`run`]), so a config overwrite can never
/// silently take a key file with it.
fn setup_key_file(
    key_path: &Path,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    term: &mut dyn KeyEntryTerminal,
) -> Result<(), crate::error::Error> {
    let mut replacing = false;
    if key_path.exists() {
        if std::fs::metadata(key_path)?.len() != 0 {
            replacing = term.is_tty()
                && ask_yes_no(input, out, "Replace the key already in that file?")?;
            if !replacing {
                writeln!(out, "Left {} as it is.", key_path.display())?;
                if !term.is_tty() {
                    writeln!(out, "Edit it, or delete it and rerun init, to replace the key.")?;
                }
                return Ok(());
            }
        }
    } else {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if let Some(dir) = key_path.parent() {
            if !dir.exists() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
                // Modes pass through umask at creation; pin them exact.
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(key_path)?;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
        writeln!(out, "Created {} (mode 600).", key_path.display())?;
    }
    if prompt_key_entry(key_path, input, out, term)? {
        return Ok(());
    }
    // Skipped at the hidden prompt. Replacing: the old contents survive,
    // which is the truthful thing to say. Otherwise the T14 instruction.
    if replacing {
        writeln!(out, "Left {} as it is.", key_path.display())?;
    } else {
        writeln!(out, "Add it later: put your key in {}.", key_path.display())?;
    }
    Ok(())
}

/// Startup-profile question for the anthropic template (T16). Prints the
/// curated profiles numbered and asks which one the config should start
/// on; the answer is a NUMBER into the listing or a profile name. Default
/// sonnet; anything else re-asks (EOF stays an error via `ask`).
fn pick_startup_profile(
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<&'static str, crate::error::Error> {
    writeln!(out, "Profiles this template writes:")?;
    for (i, (name, model_id, ..)) in ANTHROPIC_PROFILES.iter().enumerate() {
        writeln!(out, "  {}) {name:<7} {model_id}", i + 1)?;
    }
    loop {
        let answer = ask(
            input,
            out,
            "Startup profile (number or name)",
            ANTHROPIC_DEFAULT_PROFILE,
        )?;
        if answer.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = answer.parse::<usize>() {
                if (1..=ANTHROPIC_PROFILES.len()).contains(&n) {
                    return Ok(ANTHROPIC_PROFILES[n - 1].0);
                }
            }
        } else if let Some((name, ..)) =
            ANTHROPIC_PROFILES.iter().find(|(name, ..)| *name == answer)
        {
            return Ok(name);
        }
        writeln!(
            out,
            "unknown profile {answer:?} (expected 1-{} or a profile name)",
            ANTHROPIC_PROFILES.len()
        )?;
    }
}

/// The wizard. Writes `cfg_path`; refuses to overwrite an existing config
/// unless `force`. Returns the lines it printed through `out`.
///
/// `list_models` and `probe_context` are the TWO network calls the wizard
/// may make (T15, extended by T22 under the same amendment): both
/// unauthenticated keyless GETs, injected from main (the real
/// [`crate::provider::list_models_keyless`] and
/// [`crate::provider::probe_props_context`]) so tests script them without
/// a network. They are only ever called for the keyless local template;
/// keyed templates stay free-text: their key files are created EMPTY
/// below, so no authenticated request is possible at init time even in
/// principle, and init never reads keys.
pub fn run(
    cfg_path: &Path,
    home: Option<&Path>,
    force: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    list_models: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    probe_context: &dyn Fn(&str) -> Option<u64>,
    term: &mut dyn KeyEntryTerminal,
) -> Result<(), crate::error::Error> {
    if cfg_path.exists() && !force {
        return Err(crate::error::Error::Config(format!(
            "config already exists at {}; rerun with --force to overwrite it",
            cfg_path.display()
        )));
    }

    writeln!(out, "temur init: config goes to {}", cfg_path.display())?;
    writeln!(out)?;
    writeln!(out, "Templates:")?;
    for t in &TEMPLATES {
        writeln!(out, "  {}) {:<10} {}", t.number, t.name, t.describe)?;
    }
    let choice = ask(input, out, "Template", "1")?;
    let template = TEMPLATES
        .iter()
        .find(|t| t.number == choice || t.name == choice)
        .ok_or_else(|| {
            crate::error::Error::Config(format!(
                "init: unknown template {choice:?} (expected 1-{} or a template name)",
                TEMPLATES.len()
            ))
        })?;

    // Local (keyless) template: ask where the server lives, then try its
    // listing so the model question offers real ids instead of a blind
    // free-text guess. Any listing problem falls back to exactly the old
    // free-text question after a one-line note — the wizard must complete
    // offline. Keyed templates: free text, unchanged (see `list_models`).
    let mut base_url: Option<String> = None;
    let mut detected_window: Option<u64> = None;
    let mut server_up = false;
    let model = if template.key_slug.is_none() {
        let (base, picked, detected, up) =
            ask_local_base_and_model(template, input, out, list_models, probe_context)?;
        base_url = Some(base);
        detected_window = detected;
        server_up = up;
        picked
    } else if template.name == "anthropic" {
        // T16: the anthropic template writes a fixed profile set, so the
        // model question becomes a startup-profile question. `model` holds
        // the chosen profile NAME from here on (see render_config).
        pick_startup_profile(input, out)?.to_string()
    } else {
        // T51: the hosted-compat templates used to print a bare
        // "Model [default]:" and an operator pressed Enter blind. No baked
        // list (the init.rs:33 comment says why those rot); just where the
        // real one lives.
        writeln!(out, "{HOSTED_MODEL_HINT}")?;
        ask(input, out, "Model id", template.default_model)?
    };

    // Keyed templates: ask for the key FILE PATH only. The key itself never
    // passes through temur, in any direction.
    let key_file: Option<PathBuf> = match template.key_slug {
        None => None,
        Some(slug) => Some(ask_key_file(slug, home, input, out, term.is_tty())?),
    };

    // Write the config (parent dir as needed; the config holds no secret,
    // so default directory modes are fine).
    if let Some(dir) = cfg_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let rendered = render_config(
        template,
        &model,
        key_file.as_ref().map(|p| p.display().to_string()).as_deref(),
        base_url.as_deref(),
        detected_window,
    );
    // T51/D18: init is all-or-nothing. The key path was validated before
    // this write, and anything the key step still fails on puts the file
    // back the way it was found - deleted when init created it, restored
    // byte for byte when --force overwrote one the user already had.
    let previous = if cfg_path.exists() {
        Some(std::fs::read(cfg_path)?)
    } else {
        None
    };
    std::fs::write(cfg_path, &rendered)?;
    writeln!(out)?;
    writeln!(out, "Wrote {}", cfg_path.display())?;

    if let Some(key_path) = &key_file {
        if let Err(e) = setup_key_file(key_path, input, out, term) {
            // The undo is best-effort and only ever CLAIMED when it
            // happened: an undo that itself failed must not mask the error
            // that caused it.
            let undone = match &previous {
                Some(bytes) => std::fs::write(cfg_path, bytes).is_ok(),
                None => std::fs::remove_file(cfg_path).is_ok(),
            };
            if undone {
                writeln!(
                    out,
                    "Rolled back {}: no config without a usable key file.",
                    cfg_path.display()
                )?;
            }
            return Err(e);
        }
    }

    writeln!(out)?;
    // Hosted, and local with a server that just answered, are the SAME
    // state: ready. Only a local server nobody could reach needs starting.
    if template.key_slug.is_some() || server_up {
        writeln!(out, "Next: temur to start (temur doctor checks the setup first).")?;
    } else {
        writeln!(out, "Next: start your server (docs/OFFLINE.md), then temur.")?;
    }
    // T16: sessions discoverability - autosave was routinely discovered by
    // accident, so the wizard says it once at the end.
    writeln!(out, "Sessions save per directory; temur --continue resumes the last.")?;
    Ok(())
}

/// `temur init --add <template>` (T17): merge one template into an EXISTING
/// config instead of writing a fresh one. Always merges AS PROFILES,
/// whatever shape the template's fresh render has: the base selection, the
/// startup `"profile"` key, and every other field in the file are left
/// alone (surgical `serde_json::Value` edit, same preserve_order +
/// temp-then-rename mechanics as [`crate::config::persist_model`]).
/// Fail-closed: if ANY profile name to be added already exists, the whole
/// merge aborts with the file untouched, never a silent overwrite of a
/// profile the user wrote by hand.
pub fn run_add(
    cfg_path: &Path,
    home: Option<&Path>,
    template_name: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    list_models: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    probe_context: &dyn Fn(&str) -> Option<u64>,
    term: &mut dyn KeyEntryTerminal,
) -> Result<(), crate::error::Error> {
    let template = TEMPLATES
        .iter()
        .find(|t| t.name == template_name)
        .ok_or_else(|| {
            let names: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
            crate::error::Error::Config(format!(
                "init --add: unknown template {template_name:?} (expected {})",
                names.join(", ")
            ))
        })?;
    let raw = match std::fs::read_to_string(cfg_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(crate::error::Error::Config(format!(
                "init --add: no config at {}; plain `temur init` creates one",
                cfg_path.display()
            )))
        }
        Err(e) => return Err(e.into()),
    };
    let mut v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| crate::error::Error::Config(format!("{}: {e}", cfg_path.display())))?;
    if !v.is_object() {
        return Err(crate::error::Error::Config(format!(
            "{}: not a JSON object",
            cfg_path.display()
        )));
    }
    if v.get("profiles").is_some_and(|p| !p.is_object()) {
        return Err(crate::error::Error::Config(format!(
            "{}: \"profiles\" is not a JSON object",
            cfg_path.display()
        )));
    }

    // Collisions are checked BEFORE any question runs, so a doomed merge
    // never wastes the user's answers.
    let adding: Vec<&'static str> = if template.name == "anthropic" {
        ANTHROPIC_PROFILES.iter().map(|(n, ..)| *n).collect()
    } else {
        vec![template.name]
    };
    let collisions: Vec<&str> = adding
        .iter()
        .copied()
        .filter(|n| {
            v.get("profiles")
                .and_then(|p| p.as_object())
                .is_some_and(|p| p.contains_key(*n))
        })
        .collect();
    if !collisions.is_empty() {
        let plural = if collisions.len() == 1 { "" } else { "s" };
        let quoted: Vec<String> = collisions.iter().map(|n| format!("{n:?}")).collect();
        return Err(crate::error::Error::Config(format!(
            "init --add {}: profile{plural} {} already in {}; nothing was changed \
             (rename or remove the existing profile{plural} first)",
            template.name,
            quoted.join(", "),
            cfg_path.display()
        )));
    }

    writeln!(
        out,
        "temur init --add {}: merging into {}",
        template.name,
        cfg_path.display()
    )?;
    writeln!(out)?;

    // The template's questions, mirroring the fresh wizard: base URL +
    // picker for local, free-text model for hosted compat templates. The
    // anthropic model ids are fixed (T16) and the startup "profile" key is
    // never written here, so only the key question runs for it.
    let mut new_profiles: Vec<(String, serde_json::Value)> = Vec::new();
    let mut key_file: Option<PathBuf> = None;
    match template.name {
        "local" => {
            let (base, model, detected, _up) =
                ask_local_base_and_model(template, input, out, list_models, probe_context)?;
            let mut p = serde_json::Map::new();
            p.insert("provider".to_string(), "openai-compat".into());
            p.insert("model".to_string(), model.into());
            if base != crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL {
                p.insert("base_url".to_string(), base.into());
            }
            // The fresh local template's small-model limits, carried into
            // the profile so switching to it behaves like a fresh local
            // config (a profile's absent max_tokens would inherit the
            // global value instead). context_window: the /props answer
            // when the probe got one (T22), the baked value otherwise.
            p.insert("max_tokens".to_string(), 4096.into());
            p.insert(
                "context_window".to_string(),
                detected.unwrap_or(LOCAL_BAKED_CONTEXT_WINDOW).into(),
            );
            new_profiles.push((template.name.to_string(), p.into()));
        }
        "anthropic" => {
            let key = ask_key_file("anthropic", home, input, out, term.is_tty())?;
            let k = key.display().to_string();
            for (name, model_id, window, pin, pout) in &ANTHROPIC_PROFILES {
                let mut p = serde_json::Map::new();
                p.insert("provider".to_string(), "anthropic".into());
                p.insert("model".to_string(), (*model_id).into());
                p.insert("api_key_file".to_string(), k.clone().into());
                p.insert("context_window".to_string(), (*window).into());
                // T24: same invariant as context_window: what --add
                // writes and what the fresh render writes must agree.
                p.insert("price_input_per_mtok".to_string(), (*pin).into());
                p.insert("price_output_per_mtok".to_string(), (*pout).into());
                new_profiles.push(((*name).to_string(), p.into()));
            }
            key_file = Some(key);
        }
        hosted => {
            writeln!(out, "{HOSTED_MODEL_HINT}")?;
            let model = ask(input, out, "Model id", template.default_model)?;
            let key = ask_key_file(
                template.key_slug.expect("hosted templates are keyed"),
                home,
                input,
                out,
                term.is_tty(),
            )?;
            let mut p = serde_json::Map::new();
            p.insert("provider".to_string(), "openai-compat".into());
            p.insert("base_url".to_string(), compat_base_url(hosted).into());
            p.insert("model".to_string(), model.into());
            // The provider's completion cap when it is below the global
            // default, exactly as the fresh render bakes it; a profile with
            // no max_tokens would inherit the global value instead.
            if let Some(n) = compat_max_tokens(hosted) {
                p.insert("max_tokens".to_string(), n.into());
            }
            // The served window where the template bakes one, exactly as the
            // fresh render does; a profile without it has the advisory,
            // auto-compaction and the scaled tool-output ceiling off.
            if let Some(n) = compat_context_window(hosted) {
                p.insert("context_window".to_string(), n.into());
            }
            p.insert("api_key_file".to_string(), key.display().to_string().into());
            new_profiles.push((hosted.to_string(), p.into()));
            key_file = Some(key);
        }
    }

    {
        let root = v.as_object_mut().expect("checked above");
        let entry = root
            .entry("profiles".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let profs = entry.as_object_mut().expect("checked above");
        for (name, p) in &new_profiles {
            profs.insert(name.clone(), p.clone());
        }
    }
    crate::config::write_config_value(cfg_path, &v)?;

    writeln!(out)?;
    let quoted: Vec<String> = new_profiles.iter().map(|(n, _)| format!("\"{n}\"")).collect();
    writeln!(
        out,
        "Added profile{} {} to {}.",
        if quoted.len() == 1 { "" } else { "s" },
        quoted.join(", "),
        cfg_path.display()
    )?;
    if let Some(key_path) = &key_file {
        setup_key_file(key_path, input, out, term)?;
    }
    writeln!(out)?;
    if new_profiles.len() == 1 {
        let name = &new_profiles[0].0;
        writeln!(
            out,
            "/model {name} switches to it; set \"profile\": \"{name}\" in config.json to\nmake it the startup default."
        )?;
    } else {
        writeln!(
            out,
            "/model <name> switches to one; set \"profile\": \"<name>\" in config.json to\nmake it the startup default."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The piped-stdin terminal every wizard test drives: no TTY, so the
    /// key prompt reads a plain line (or EOF = skip) exactly like a piped
    /// `temur init`.
    struct NoTty;

    impl KeyEntryTerminal for NoTty {
        fn is_tty(&self) -> bool {
            false
        }
        fn begin_hidden(&mut self) -> bool {
            panic!("begin_hidden must never be called off a TTY")
        }
        fn restore(&mut self) {}
    }

    #[test]
    fn tilde_expansion_only_rewrites_a_leading_tilde_slash() {
        let home = PathBuf::from("/home/u");
        assert_eq!(
            expand_tilde("~/.secrets/k", Some(&home)),
            PathBuf::from("/home/u/.secrets/k")
        );
        assert_eq!(expand_tilde("/abs/k", Some(&home)), PathBuf::from("/abs/k"));
        assert_eq!(expand_tilde("rel/k", Some(&home)), PathBuf::from("rel/k"));
        // No home: the literal survives (the caller rejects empty answers).
        assert_eq!(expand_tilde("~/.k", None), PathBuf::from("~/.k"));
    }

    #[test]
    fn every_template_renders_parseable_config_selecting_the_right_provider() {
        for t in &TEMPLATES {
            let key = t.key_slug.map(|_| "/tmp/k");
            // The anthropic template's "model" answer is a startup profile
            // name (T16); its default profile still resolves the template's
            // default model, which is what the assertion below checks.
            let model_arg = if t.name == "anthropic" {
                ANTHROPIC_DEFAULT_PROFILE
            } else {
                t.default_model
            };
            let rendered = render_config(t, model_arg, key, None, None);
            let cfg: crate::config::Config =
                serde_json::from_str(&rendered).unwrap_or_else(|e| {
                    panic!("template {} renders invalid config: {e}\n{rendered}", t.name)
                });
            let profiles = cfg.resolved_profiles().expect("profiles validate");
            let (_, resolved) = cfg.startup_selection(&profiles).expect("selection resolves");
            assert_eq!(resolved.model, t.default_model, "template {}", t.name);
            match t.name {
                "local" => {
                    assert_eq!(resolved.provider, "openai-compat");
                    assert!(resolved.api_key_file.is_none(), "local stays keyless");
                }
                "anthropic" => {
                    assert_eq!(resolved.provider, "anthropic");
                    assert_eq!(resolved.api_key_file.as_deref(), Some("/tmp/k"));
                }
                "openai" | "gemini" | "xai" => {
                    assert_eq!(resolved.provider, "openai-compat");
                    assert_eq!(resolved.api_key_file.as_deref(), Some("/tmp/k"));
                    assert_eq!(resolved.base_url, compat_base_url(t.name));
                    // T13: a fresh hosted config bakes the provider's
                    // completion cap only where it is below the global
                    // default; gpt-4o rejected 32000 live.
                    assert_eq!(
                        resolved.max_tokens,
                        compat_max_tokens(t.name)
                            .unwrap_or(crate::config::DEFAULT_MAX_TOKENS),
                        "template {}",
                        t.name
                    );
                }
                other => panic!("unknown template {other}"),
            }
        }
    }

    #[test]
    fn model_and_path_strings_are_json_escaped() {
        let t = &TEMPLATES[2]; // openai
        let rendered = render_config(t, "we\"ird", Some("/k\"ey"), None, None);
        let cfg: crate::config::Config = serde_json::from_str(&rendered).expect("escaped");
        let r = cfg.resolve_base().unwrap();
        assert_eq!(r.model, "we\"ird");
        assert_eq!(r.api_key_file.as_deref(), Some("/k\"ey"));
    }

    // ------------------------------------------- T15: base URL + model picker

    #[test]
    fn local_render_default_base_url_is_byte_identical_to_the_readme_recipe() {
        let t = &TEMPLATES[0]; // local
        let expect = "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": { \"model\": \"qwen3-4b\", \"context_window\": 8192 }\n}\n";
        // Both the no-answer path and an answered default render the recipe.
        assert_eq!(render_config(t, "qwen3-4b", None, None, None), expect);
        assert_eq!(
            render_config(
                t,
                "qwen3-4b",
                None,
                Some(crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL),
                None
            ),
            expect
        );
        // A detected n_ctx equal to the baked value renders the same bytes.
        assert_eq!(render_config(t, "qwen3-4b", None, None, Some(8192)), expect);
    }

    #[test]
    fn local_render_detected_window_replaces_the_baked_value() {
        let t = &TEMPLATES[0];
        let rendered = render_config(t, "m", None, None, Some(16384));
        assert!(rendered.contains("\"context_window\": 16384"), "{rendered}");
        assert!(!rendered.contains("8192"), "{rendered}");
        let cfg: crate::config::Config = serde_json::from_str(&rendered).unwrap();
        assert_eq!(cfg.resolve_base().unwrap().context_window, Some(16384));
    }

    #[test]
    fn local_render_custom_base_url_survives_and_parses() {
        let t = &TEMPLATES[0];
        let rendered = render_config(t, "m", None, Some("http://10.0.0.9:11434/v1"), None);
        let cfg: crate::config::Config = serde_json::from_str(&rendered).unwrap();
        let r = cfg.resolve_base().unwrap();
        assert_eq!(r.base_url, "http://10.0.0.9:11434/v1");
        assert_eq!(r.model, "m");
        assert!(r.api_key_file.is_none());
    }

    /// Drive the whole wizard with piped answers, a scripted listing, and
    /// a scripted /props probe (T22).
    fn run_wizard_probed(
        answers: &str,
        list: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
        probe: &dyn Fn(&str) -> Option<u64>,
    ) -> Result<(String, String), crate::error::Error> {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let mut input = std::io::Cursor::new(answers.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, list, probe, &mut NoTty)?;
        Ok((
            std::fs::read_to_string(&cfg_path).unwrap(),
            String::from_utf8(out).unwrap(),
        ))
    }

    /// [`run_wizard_probed`] with a probe that never answers, the pre-T22
    /// behavior every older test drives.
    fn run_wizard(
        answers: &str,
        list: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    ) -> Result<(String, String), crate::error::Error> {
        run_wizard_probed(answers, list, &|_| None)
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn picker_number_selects_from_the_listing() {
        let list = |_: &str| Ok(ids(&["alpha", "beta", "gamma"]));
        // Template default, base URL default, model by number.
        let (cfg, out) = run_wizard("\n\n2\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"beta\""), "{cfg}");
        assert!(out.contains("Models on "), "{out}");
        assert!(out.contains("  1) alpha") && out.contains("  3) gamma"), "{out}");
        // Default base URL answered: the recipe render, no base_url key.
        assert!(!cfg.contains("base_url"), "{cfg}");
    }

    #[test]
    fn picker_free_text_id_and_custom_base_url_survive() {
        let list = |base: &str| {
            assert_eq!(base, "http://10.0.0.9:11434/v1", "picker lists the ANSWERED base");
            Ok(ids(&["served-model"]))
        };
        let (cfg, _out) =
            run_wizard("\nhttp://10.0.0.9:11434/v1\nmy-custom\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"my-custom\""), "{cfg}");
        assert!(cfg.contains("\"base_url\": \"http://10.0.0.9:11434/v1\""), "{cfg}");
    }

    #[test]
    fn picker_default_is_template_default_when_listed_else_first() {
        // Template default present in the listing: empty answer picks it.
        let list = |_: &str| Ok(ids(&["other", "qwen3-4b"]));
        let (cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"qwen3-4b\""), "{cfg}");
        assert!(out.contains("[qwen3-4b]"), "default shown: {out}");
        // Absent: the first listed id becomes the default.
        let list = |_: &str| Ok(ids(&["first-served", "second"]));
        let (cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(cfg.contains("\"model\": \"first-served\""), "{cfg}");
        assert!(out.contains("[first-served]"), "{out}");
    }

    #[test]
    fn picker_caps_the_printed_listing_and_numbers_still_reach_the_tail() {
        let many: Vec<String> = (1..=25).map(|i| format!("m{i:02}")).collect();
        let list = move |_: &str| Ok(many.clone());
        let (cfg, out) = run_wizard("\n\n25\n", &list).unwrap();
        assert!(out.contains("  20) m20"), "{out}");
        assert!(!out.contains("m21"), "listing capped: {out}");
        assert!(out.contains("... and 5 more"), "{out}");
        assert!(cfg.contains("\"model\": \"m25\""), "a number past the cap selects: {cfg}");
    }

    #[test]
    fn picker_out_of_range_number_is_a_clean_error() {
        let list = |_: &str| Ok(ids(&["only"]));
        let err = run_wizard("\n\n7\n", &list).unwrap_err().to_string();
        assert!(err.contains("7") && err.contains("out of range (1-1)"), "{err}");
    }

    #[test]
    fn listing_failure_or_empty_falls_back_to_free_text_with_a_note() {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            Err(crate::error::Error::Models("connection refused".into()))
        };
        let (cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(out.contains("could not list models from"), "{out}");
        assert!(out.contains("connection refused"), "{out}");
        assert!(out.contains("Model id"), "free-text question asked: {out}");
        assert!(cfg.contains("\"model\": \"qwen3-4b\""), "{cfg}");

        let list = |_: &str| Ok(Vec::<String>::new());
        let (_cfg, out) = run_wizard("\n\ncustom\n", &list).unwrap();
        assert!(out.contains("empty listing"), "{out}");
    }

    #[test]
    fn shortlist_prints_only_when_the_picker_could_not_run() {
        // Fallback path: every baked line, including the canonical pointer.
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            Err(crate::error::Error::Models("connection refused".into()))
        };
        let (_cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        for line in MODEL_SHORTLIST {
            assert!(out.contains(line), "missing {line:?} in:\n{out}");
        }
        // Picker path: the server's listing wins, no shortlist.
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (_cfg, out) = run_wizard("\n\n\n", &list).unwrap();
        assert!(!out.contains("Known-good small models"), "{out}");
        assert!(
            !out.contains("Recommended small models"),
            "no shortlist pointer when the picker ran (the closing Next \
             line's OFFLINE.md mention is separate and fine): {out}"
        );
    }

    // ---------------------------------------- T22: context auto-fill (local)

    #[test]
    fn local_wizard_autofills_the_detected_context_window_with_a_notice() {
        let list = |_: &str| Ok(ids(&["served-model"]));
        let probe = |base: &str| {
            assert_eq!(
                base,
                crate::config::DEFAULT_OPENAI_COMPAT_BASE_URL,
                "probe asks the ANSWERED base"
            );
            Some(16384)
        };
        let (cfg, out) = run_wizard_probed("\n\n\n", &list, &probe).unwrap();
        assert!(cfg.contains("\"context_window\": 16384"), "{cfg}");
        assert!(!cfg.contains("8192"), "{cfg}");
        assert!(
            out.contains("Server reports 16384 tokens of context"),
            "{out}"
        );
        assert!(out.contains("/props"), "source named: {out}");
        assert!(out.contains("\"context_window\": 16384"), "{out}");
    }

    #[test]
    fn local_wizard_probe_none_keeps_the_baked_value_silently() {
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (cfg, out) = run_wizard_probed("\n\n\n", &list, &|_| None).unwrap();
        assert!(cfg.contains("\"context_window\": 8192"), "{cfg}");
        assert!(!out.contains("tokens of context"), "{out}");
        assert!(!out.contains("/props"), "{out}");
    }

    #[test]
    fn local_wizard_probe_runs_even_when_the_listing_fails() {
        // A llama.cpp server could in principle 500 the listing yet still
        // answer /props; more importantly the two calls are independent.
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            Err(crate::error::Error::Models("connection refused".into()))
        };
        let (cfg, out) = run_wizard_probed("\n\n\n", &list, &|_| Some(4096)).unwrap();
        assert!(cfg.contains("\"context_window\": 4096"), "{cfg}");
        assert!(out.contains("Server reports 4096 tokens of context"), "{out}");
    }

    #[test]
    fn keyed_templates_never_call_the_listing() {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            panic!("keyed templates must not attempt a listing")
        };
        let probe = |_: &str| -> Option<u64> {
            panic!("keyed templates must not attempt a /props probe")
        };
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key_path = tmp.path().join("some-key");
        let mut input =
            std::io::Cursor::new(format!("2\n\n{}\n", key_path.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        // home None + explicit key path; the wizard completes without ever
        // touching `list` or `probe`.
        run(&cfg_path, None, false, &mut input, &mut out, &list, &probe, &mut NoTty).unwrap();
        let cfg = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(cfg.contains("\"model\": \"claude-sonnet-5\""), "{cfg}");
        let printed = String::from_utf8(out).unwrap();
        assert!(!printed.contains("Base URL"), "keyed asks no base URL: {printed}");
    }

    // ------------------------------------- T16: anthropic profile set wizard

    /// Drive the wizard for the anthropic template with an explicit key
    /// path (home is None in tests, so there is no default key path).
    fn run_anthropic_wizard(profile_answers: &str) -> (String, String) {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            panic!("anthropic template must not attempt a listing")
        };
        let probe = |_: &str| -> Option<u64> {
            panic!("anthropic template must not attempt a /props probe")
        };
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key_path = tmp.path().join("throwaway-key");
        let mut input = std::io::Cursor::new(
            format!("2\n{profile_answers}{}\n", key_path.display()).into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, &list, &probe, &mut NoTty).unwrap();
        (
            std::fs::read_to_string(&cfg_path).unwrap(),
            String::from_utf8(out).unwrap(),
        )
    }

    #[test]
    fn the_gemini_render_bakes_its_context_window_and_resolves_it() {
        let t = &TEMPLATES[3];
        assert_eq!(t.name, "gemini");
        let rendered = render_config(t, t.default_model, Some("/tmp/k"), None, None);
        assert!(
            rendered.contains("\"context_window\": 1000000"),
            "{rendered}"
        );
        // Baked is not the same as reaching the profile: parse it and check
        // the resolved value, since an unset window turns the advisory,
        // auto-compaction and the scaled tool-output ceiling off.
        let cfg: crate::config::Config = serde_json::from_str(&rendered).expect("parses");
        let profiles = cfg.resolved_profiles().expect("profiles validate");
        let (_, resolved) = cfg.startup_selection(&profiles).expect("selection resolves");
        assert_eq!(resolved.context_window, Some(1_000_000));
    }

    #[test]
    fn the_context_window_lookup_bakes_only_for_gemini() {
        // The table itself. That the two WRITE paths agree is proven through
        // the real `init --add` in add_hosted_templates_add_one_profile_each,
        // which is why the lookup is a function and not a literal.
        assert_eq!(compat_context_window("gemini"), Some(1_000_000));
        for name in ["openai", "xai", "anthropic", "local"] {
            assert_eq!(compat_context_window(name), None, "{name}");
        }
    }

    #[test]
    fn the_openai_and_xai_renders_are_byte_exact_and_bake_no_window() {
        // Pinned byte-exactly because T62 item 6 added a field to this
        // render for gemini only: these two must not have moved. There was
        // no prior expected string for them, so these are the pins.
        let openai = render_config(&TEMPLATES[2], "gpt-4o", Some("/tmp/k"), None, None);
        assert_eq!(
            openai,
            "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 16384,\n  \"openai_compat\": { \"base_url\": \"https://api.openai.com/v1\",\n                     \"model\": \"gpt-4o\",\n                     \"api_key_file\": \"/tmp/k\" }\n}\n"
        );
        let xai = render_config(&TEMPLATES[4], "grok-4", Some("/tmp/k"), None, None);
        assert_eq!(
            xai,
            "{\n  \"provider\": \"openai-compat\",\n  \"openai_compat\": { \"base_url\": \"https://api.x.ai/v1\",\n                     \"model\": \"grok-4\",\n                     \"api_key_file\": \"/tmp/k\" }\n}\n"
        );
        assert!(!openai.contains("context_window"), "{openai}");
        assert!(!xai.contains("context_window"), "{xai}");
    }

    #[test]
    fn anthropic_render_is_byte_exact_for_the_default_profile() {
        let t = &TEMPLATES[1]; // anthropic
        let rendered =
            render_config(t, "sonnet", Some("/home/u/.secrets/temur-anthropic-key"), None, None);
        let expect = "{\n  \"profiles\": {\n    \"fable\":  { \"provider\": \"anthropic\", \"model\": \"claude-fable-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\",\n                \"context_window\": 1000000,\n                \"price_input_per_mtok\": 10.0, \"price_output_per_mtok\": 50.0 },\n    \"haiku\":  { \"provider\": \"anthropic\", \"model\": \"claude-haiku-4-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\",\n                \"context_window\": 200000,\n                \"price_input_per_mtok\": 1.0, \"price_output_per_mtok\": 5.0 },\n    \"opus\":   { \"provider\": \"anthropic\", \"model\": \"claude-opus-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\",\n                \"context_window\": 1000000,\n                \"price_input_per_mtok\": 5.0, \"price_output_per_mtok\": 25.0 },\n    \"sonnet\": { \"provider\": \"anthropic\", \"model\": \"claude-sonnet-5\",\n                \"api_key_file\": \"/home/u/.secrets/temur-anthropic-key\",\n                \"context_window\": 1000000,\n                \"price_input_per_mtok\": 2.0, \"price_output_per_mtok\": 10.0 }\n  },\n  \"profile\": \"sonnet\"\n}\n";
        assert_eq!(rendered, expect);
    }

    #[test]
    fn anthropic_render_parses_with_four_profiles_sharing_the_key() {
        let t = &TEMPLATES[1];
        let rendered = render_config(t, "opus", Some("/tmp/k"), None, None);
        let cfg: crate::config::Config = serde_json::from_str(&rendered).unwrap();
        let profiles = cfg.resolved_profiles().expect("profiles validate");
        assert_eq!(
            profiles.keys().cloned().collect::<Vec<_>>(),
            vec!["fable", "haiku", "opus", "sonnet"],
            "name order"
        );
        for ((name, model_id, window, pin, pout), (key, resolved)) in
            ANTHROPIC_PROFILES.iter().zip(&profiles)
        {
            assert_eq!(key.as_str(), *name);
            assert_eq!(resolved.model, *model_id);
            assert_eq!(resolved.provider, "anthropic");
            assert_eq!(resolved.api_key_file.as_deref(), Some("/tmp/k"), "shared key");
            assert_eq!(
                resolved.context_window,
                Some(*window),
                "T13 P2.5: the baked hosted window, per model"
            );
            assert_eq!(
                (resolved.price_input_per_mtok, resolved.price_output_per_mtok),
                (Some(*pin), Some(*pout)),
                "T24: the baked list prices, per model"
            );
        }
        // T13 P2.5: the per-model values are the point, so pin that they
        // are not all equal. A blanket constant would pass the loop above.
        assert_eq!(
            ANTHROPIC_PROFILES.map(|(_, _, w, ..)| w),
            [1_000_000, 200_000, 1_000_000, 1_000_000],
            "live 2026-08-04: haiku is the odd one out"
        );
        // T24, same reasoning: per-model USD-per-Mtok list rates,
        // knowledge-of-record 2026-08-19. Sonnet's 2.0/10.0 IS the
        // standard rate as of T35, not an introductory one.
        assert_eq!(
            ANTHROPIC_PROFILES.map(|(_, _, _, pin, pout)| (pin, pout)),
            [(10.0, 50.0), (1.0, 5.0), (5.0, 25.0), (2.0, 10.0)],
            "fable / haiku / opus / sonnet"
        );
        let (active, resolved) = cfg.startup_selection(&profiles).expect("selection resolves");
        assert_eq!(active.as_deref(), Some("opus"));
        assert_eq!(resolved.model, "claude-opus-5");
    }

    #[test]
    fn anthropic_startup_profile_accepts_name_number_and_default() {
        // Default (empty answer): sonnet, no default flip.
        let (cfg, out) = run_anthropic_wizard("\n");
        assert!(cfg.contains("\"profile\": \"sonnet\""), "{cfg}");
        assert!(out.contains("  1) fable   claude-fable-5"), "{out}");
        assert!(out.contains("  4) sonnet  claude-sonnet-5"), "{out}");
        assert!(out.contains("[sonnet]"), "default shown: {out}");
        // By name.
        let (cfg, _) = run_anthropic_wizard("opus\n");
        assert!(cfg.contains("\"profile\": \"opus\""), "{cfg}");
        // By number (2 = haiku in name order).
        let (cfg, _) = run_anthropic_wizard("2\n");
        assert!(cfg.contains("\"profile\": \"haiku\""), "{cfg}");
    }

    #[test]
    fn anthropic_startup_profile_reasks_on_anything_else() {
        for bad in ["bogus", "9", "claude-opus-5"] {
            let (cfg, out) = run_anthropic_wizard(&format!("{bad}\nfable\n"));
            assert!(
                out.contains(&format!("unknown profile \"{bad}\"")),
                "{bad}: {out}"
            );
            assert!(cfg.contains("\"profile\": \"fable\""), "{bad}: {cfg}");
        }
    }

    // -------------------------------------------------- T17: temur init --add

    /// A byte-fixed local config in exactly the pretty 2-space form
    /// `write_config_value` emits, so a merge's output is predictable to
    /// the byte.
    const LOCAL_FIXED: &str = "{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {\n    \"model\": \"qwen3-1.7b\",\n    \"context_window\": 8192\n  }\n}\n";

    fn no_listing(_: &str) -> Result<Vec<String>, crate::error::Error> {
        panic!("this template must not attempt a listing")
    }

    /// Write `cfg` into a tempdir and drive `run_add` with piped answers
    /// and a scripted /props probe (T22). Returns (result, config file
    /// content, printed output, tempdir).
    fn drive_add_probed(
        cfg: &str,
        template: &str,
        answers: &str,
        list: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
        probe: &dyn Fn(&str) -> Option<u64>,
    ) -> (
        Result<(), crate::error::Error>,
        String,
        String,
        tempfile::TempDir,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, cfg).unwrap();
        let mut input = std::io::Cursor::new(answers.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        let result =
            run_add(&cfg_path, None, template, &mut input, &mut out, list, probe, &mut NoTty);
        (
            result,
            std::fs::read_to_string(&cfg_path).unwrap(),
            String::from_utf8(out).unwrap(),
            tmp,
        )
    }

    /// [`drive_add_probed`] with a probe that never answers.
    fn drive_add(
        cfg: &str,
        template: &str,
        answers: &str,
        list: &dyn Fn(&str) -> Result<Vec<String>, crate::error::Error>,
    ) -> (
        Result<(), crate::error::Error>,
        String,
        String,
        tempfile::TempDir,
    ) {
        drive_add_probed(cfg, template, answers, list, &|_| None)
    }

    #[test]
    fn add_anthropic_golden_merge_touches_only_the_profiles_key() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        let (result, cfg, out, _tmp) = drive_add(
            LOCAL_FIXED,
            "anthropic",
            &format!("{}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        let k = key.display();
        let expect = format!(
            "{{\n  \"provider\": \"openai-compat\",\n  \"max_tokens\": 4096,\n  \"openai_compat\": {{\n    \"model\": \"qwen3-1.7b\",\n    \"context_window\": 8192\n  }},\n  \"profiles\": {{\n    \"fable\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-fable-5\",\n      \"api_key_file\": \"{k}\",\n      \"context_window\": 1000000,\n      \"price_input_per_mtok\": 10.0,\n      \"price_output_per_mtok\": 50.0\n    }},\n    \"haiku\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-haiku-4-5\",\n      \"api_key_file\": \"{k}\",\n      \"context_window\": 200000,\n      \"price_input_per_mtok\": 1.0,\n      \"price_output_per_mtok\": 5.0\n    }},\n    \"opus\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-opus-5\",\n      \"api_key_file\": \"{k}\",\n      \"context_window\": 1000000,\n      \"price_input_per_mtok\": 5.0,\n      \"price_output_per_mtok\": 25.0\n    }},\n    \"sonnet\": {{\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-sonnet-5\",\n      \"api_key_file\": \"{k}\",\n      \"context_window\": 1000000,\n      \"price_input_per_mtok\": 2.0,\n      \"price_output_per_mtok\": 10.0\n    }}\n  }}\n}}\n"
        );
        assert_eq!(cfg, expect, "golden merge: only a profiles key appended");
        // The startup "profile" key was NOT invented: the base selection
        // still runs the config.
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        assert!(parsed.profile.is_none(), "{cfg}");
        let profiles = parsed.resolved_profiles().unwrap();
        let (active, resolved) = parsed.startup_selection(&profiles).unwrap();
        assert!(active.is_none());
        assert_eq!(resolved.model, "qwen3-1.7b", "base selection untouched");
        // Key file created empty, mode 600; closing notice names /model.
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(
            out.contains("Added profiles \"fable\", \"haiku\", \"opus\", \"sonnet\""),
            "{out}"
        );
        assert!(out.contains("/model <name> switches to one"), "{out}");
        assert!(out.contains("Add it later: put your key in"), "{out}");
        // The path question carries what the T13 explainer used to say.
        assert!(out.contains("Where to save your API key ["), "{out}");
    }

    #[test]
    fn add_hosted_templates_add_one_profile_each() {
        for (template, base, default_model) in [
            ("openai", OPENAI_BASE_URL, "gpt-4o"),
            ("gemini", GEMINI_BASE_URL, "gemini-3.6-flash"),
            ("xai", XAI_BASE_URL, "grok-4"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let key = tmp.path().join("k");
            // Answers: model default (empty), explicit key path (home None).
            let (result, cfg, out, _tmp) = drive_add(
                LOCAL_FIXED,
                template,
                &format!("\n{}\n", key.display()),
                &no_listing,
            );
            result.unwrap();
            let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
            let profiles = parsed.resolved_profiles().unwrap();
            let p = &profiles[template];
            assert_eq!(p.provider, "openai-compat", "{template}");
            assert_eq!(p.base_url, base, "{template}");
            assert_eq!(p.model, default_model, "{template}");
            // T13: gpt-4o's completion cap is baked into the profile. The
            // others bake nothing and inherit the config's global, which in
            // this merge target is the local template's 4096.
            assert_eq!(p.max_tokens, compat_max_tokens(template).unwrap_or(4096), "{template}");
            // T62 item 6: gemini's served window is baked by `init --add`
            // exactly as the fresh render bakes it. Unlike max_tokens above, a
            // profile does NOT inherit the config's global context_window, so
            // the templates that bake none resolve to None and their advisory,
            // auto-compaction and scaled tool-output ceiling stay off. That
            // asymmetry is why baking it matters here.
            assert_eq!(p.context_window, compat_context_window(template), "{template}");
            assert_eq!(
                p.api_key_file.as_deref(),
                Some(key.display().to_string().as_str()),
                "{template}"
            );
            assert!(
                out.contains(&format!("Added profile \"{template}\"")),
                "{template}: {out}"
            );
            assert!(
                out.contains(&format!("/model {template} switches to it")),
                "{template}: {out}"
            );
            assert!(
                out.contains(&format!("\"profile\": \"{template}\"")),
                "{template}: {out}"
            );
        }
    }

    #[test]
    fn add_local_reuses_the_picker_and_stays_keyless() {
        let list = |base: &str| {
            assert_eq!(base, "http://10.0.0.9:11434/v1", "picker lists the ANSWERED base");
            Ok(ids(&["served-model"]))
        };
        let anthropic_base = "{\n  \"profiles\": {\n    \"sonnet\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-sonnet-5\",\n      \"api_key_file\": \"/tmp/k\"\n    }\n  },\n  \"profile\": \"sonnet\"\n}\n";
        // Custom base URL, model by number.
        let (result, cfg, out, _tmp) =
            drive_add(anthropic_base, "local", "http://10.0.0.9:11434/v1\n1\n", &list);
        result.unwrap();
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        let profiles = parsed.resolved_profiles().unwrap();
        let p = &profiles["local"];
        assert_eq!(p.provider, "openai-compat");
        assert_eq!(p.base_url, "http://10.0.0.9:11434/v1");
        assert_eq!(p.model, "served-model");
        assert!(p.api_key_file.is_none(), "local stays keyless");
        assert_eq!(p.max_tokens, 4096, "fresh local template's limit carried over");
        assert_eq!(p.context_window, Some(8192));
        // The startup profile key survives untouched.
        assert_eq!(parsed.profile.as_deref(), Some("sonnet"), "{cfg}");
        assert!(!out.contains("API key file"), "no key question: {out}");
        assert!(!out.contains("Paste your key"), "{out}");

        // Default base URL answered: the base_url key is omitted (profile
        // None = the openai-compat default, same meaning as the fresh
        // render's omission).
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (result, cfg, _out, _tmp) = drive_add(anthropic_base, "local", "\n\n", &list);
        result.unwrap();
        assert!(!cfg.contains("base_url"), "{cfg}");
    }

    #[test]
    fn add_local_autofills_the_detected_context_window() {
        let list = |_: &str| Ok(ids(&["served-model"]));
        let anthropic_base = "{\n  \"profiles\": {\n    \"sonnet\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-sonnet-5\",\n      \"api_key_file\": \"/tmp/k\"\n    }\n  },\n  \"profile\": \"sonnet\"\n}\n";
        let (result, cfg, out, _tmp) =
            drive_add_probed(anthropic_base, "local", "\n\n", &list, &|_| Some(32768));
        result.unwrap();
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        let profiles = parsed.resolved_profiles().unwrap();
        assert_eq!(profiles["local"].context_window, Some(32768));
        assert!(out.contains("Server reports 32768 tokens of context"), "{out}");
        // The existing profile's fields are untouched by the merge.
        assert!(profiles["sonnet"].context_window.is_none(), "{cfg}");
    }

    #[test]
    fn add_collision_fails_closed_naming_every_collision() {
        let cfg_before = "{\n  \"profiles\": {\n    \"opus\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"my-opus\"\n    },\n    \"sonnet\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"my-sonnet\"\n    }\n  }\n}\n";
        let (result, cfg_after, out, _tmp) =
            drive_add(cfg_before, "anthropic", "/never-asked\n", &no_listing);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("\"opus\", \"sonnet\""), "every collision named: {err}");
        assert!(err.contains("nothing was changed"), "{err}");
        assert_eq!(cfg_after, cfg_before, "file untouched on collision");
        assert!(out.is_empty(), "collision detected before any question: {out}");
    }

    #[test]
    fn add_requires_an_existing_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let mut input = std::io::Cursor::new(Vec::new());
        let mut out: Vec<u8> = Vec::new();
        let err = run_add(
            &cfg_path,
            None,
            "openai",
            &mut input,
            &mut out,
            &no_listing,
            &|_| None,
            &mut NoTty,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no config at"), "{err}");
        assert!(err.contains("temur init"), "points at the plain wizard: {err}");
        assert!(!cfg_path.exists(), "no config invented");
    }

    #[test]
    fn add_unknown_template_and_broken_config_are_clean_errors() {
        let (result, _cfg, _out, _tmp) =
            drive_add(LOCAL_FIXED, "bogus", "", &no_listing);
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown template \"bogus\"")
                && err.contains("local, anthropic, openai, gemini, xai"),
            "{err}"
        );
        // A "profiles" key that is not an object fails closed.
        let bad = "{\n  \"profiles\": 7\n}\n";
        let (result, cfg_after, _out, _tmp) = drive_add(bad, "openai", "\n/k\n", &no_listing);
        let err = result.unwrap_err().to_string();
        assert!(err.contains("\"profiles\" is not a JSON object"), "{err}");
        assert_eq!(cfg_after, bad);
    }

    #[test]
    fn add_preserves_unknown_fields_and_existing_profiles() {
        let cfg_before = "{\n  \"zeta_unknown\": true,\n  \"provider\": \"openai-compat\",\n  \"openai_compat\": {\n    \"model\": \"m\"\n  },\n  \"profiles\": {\n    \"mine\": {\n      \"provider\": \"anthropic\",\n      \"model\": \"claude-opus-5\"\n    }\n  },\n  \"profile\": \"mine\"\n}\n";
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        let (result, cfg, _out, _tmp) = drive_add(
            cfg_before,
            "openai",
            &format!("\n{}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        // Key order preserved: the unknown field stays FIRST, the existing
        // profile stays, the new one is appended inside "profiles".
        assert!(cfg.starts_with("{\n  \"zeta_unknown\": true,"), "{cfg}");
        let mine = cfg.find("\"mine\"").unwrap();
        let openai = cfg.find("\"openai\"").unwrap();
        assert!(mine < openai, "existing profile first: {cfg}");
        let parsed: crate::config::Config = serde_json::from_str(&cfg).unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("mine"), "startup key untouched");
        let profiles = parsed.resolved_profiles().unwrap();
        assert_eq!(profiles["mine"].model, "claude-opus-5");
        assert_eq!(profiles["openai"].model, "gpt-4o");
    }

    // ------------------------------------- T17 P3: hidden key entry (piped)

    const PLACEHOLDER: &str = "placeholder-not-a-real-key";

    #[test]
    fn piped_key_entry_writes_the_placeholder_and_never_echoes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        // Answers: model default, key path, then the key itself.
        let (result, _cfg, out, _tmp) = drive_add(
            LOCAL_FIXED,
            "openai",
            &format!("\n{}\n{PLACEHOLDER}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        assert_eq!(
            std::fs::read_to_string(&key).unwrap(),
            format!("{PLACEHOLDER}\n"),
            "trimmed key + trailing newline"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(out.contains("Paste your API key (hidden; Enter to skip)"), "{out}");
        assert!(
            out.contains(&format!("Saved to {}.", key.display())),
            "{out}"
        );
        assert!(!out.contains(PLACEHOLDER), "the key must never be echoed: {out}");
        assert!(
            !out.contains("Add it later"),
            "a saved key needs no editor instruction: {out}"
        );
    }

    #[test]
    fn piped_skip_and_eof_both_leave_the_key_file_empty() {
        // Explicit empty answer.
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        let (result, _cfg, out, _tmp2) = drive_add(
            LOCAL_FIXED,
            "openai",
            &format!("\n{}\n\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0, "skip leaves it empty");
        assert!(out.contains("Add it later: put your key in"), "editor instruction kept: {out}");

        // EOF right at the prompt (the pre-T17 answer scripts): same skip.
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        let (result, _cfg, out, _tmp2) = drive_add(
            LOCAL_FIXED,
            "openai",
            &format!("\n{}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0);
        assert!(out.contains("Add it later"), "{out}");
    }

    #[test]
    fn nonempty_existing_key_file_gets_no_prompt_and_stays_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        std::fs::write(&key, "EXISTING-MATERIAL\n").unwrap();
        // A key answer IS piped; it must never be consumed or written.
        let (result, _cfg, out, _tmp2) = drive_add(
            LOCAL_FIXED,
            "openai",
            &format!("\n{}\n{PLACEHOLDER}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "EXISTING-MATERIAL\n");
        assert!(!out.contains("Paste your API key"), "no prompt: {out}");
        assert!(out.contains("as it is"), "{out}");
        assert!(!out.contains("EXISTING-MATERIAL"), "{out}");
    }

    #[test]
    fn existing_empty_key_file_still_gets_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        std::fs::write(&key, "").unwrap();
        let (result, _cfg, out, _tmp2) = drive_add(
            LOCAL_FIXED,
            "openai",
            &format!("\n{}\n{PLACEHOLDER}\n", key.display()),
            &no_listing,
        );
        result.unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), format!("{PLACEHOLDER}\n"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o7777,
            0o600,
            "mode pinned even for a found file"
        );
        assert!(out.contains("Saved to"), "{out}");
    }

    #[test]
    fn fresh_wizard_offers_the_same_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key = tmp.path().join("k");
        let mut input = std::io::Cursor::new(
            format!("3\n\n{}\n{PLACEHOLDER}\n", key.display()).into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut NoTty)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), format!("{PLACEHOLDER}\n"));
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("Saved to"), "{printed}");
        assert!(!printed.contains(PLACEHOLDER), "{printed}");
    }

    // ------------------------------- T17 P3: the echo-guard seam (fake TTY)

    /// A fake TTY observing the guard discipline: counts begin/restore and
    /// remembers whether echo was ever left off.
    struct FakeTty {
        hidden: bool,
        begins: u32,
        restores: u32,
    }

    impl FakeTty {
        fn new() -> Self {
            FakeTty { hidden: false, begins: 0, restores: 0 }
        }
    }

    impl KeyEntryTerminal for FakeTty {
        fn is_tty(&self) -> bool {
            true
        }
        fn begin_hidden(&mut self) -> bool {
            self.begins += 1;
            self.hidden = true;
            true
        }
        fn restore(&mut self) {
            self.restores += 1;
            self.hidden = false;
        }
    }

    /// A reader whose first read fails, for the guard's error path.
    struct FailingReader;

    impl std::io::Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("tty read failed"))
        }
    }

    #[test]
    fn echo_guard_disables_and_restores_around_the_read() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        std::fs::write(&key, "").unwrap();
        let mut term = FakeTty::new();
        let mut input = std::io::Cursor::new(format!("{PLACEHOLDER}\n").into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let saved = prompt_key_entry(&key, &mut input, &mut out, &mut term).unwrap();
        assert!(saved);
        assert_eq!((term.begins, term.restores), (1, 1));
        assert!(!term.hidden, "echo restored");
        let printed = String::from_utf8(out).unwrap();
        // The prompt line ends with the hand-printed newline the disabled
        // echo swallowed, BEFORE the confirmation line.
        assert!(
            printed.contains("Enter to skip): \nSaved to"),
            "{printed}"
        );
        assert!(!printed.contains(PLACEHOLDER), "{printed}");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), format!("{PLACEHOLDER}\n"));
    }

    #[test]
    fn echo_guard_restores_on_a_read_error() {
        let tmp = tempfile::tempdir().unwrap();
        let key = tmp.path().join("k");
        std::fs::write(&key, "").unwrap();
        let mut term = FakeTty::new();
        let mut input = std::io::BufReader::new(FailingReader);
        let mut out: Vec<u8> = Vec::new();
        let err = prompt_key_entry(&key, &mut input, &mut out, &mut term).unwrap_err();
        assert!(err.to_string().contains("tty read failed"), "{err}");
        assert_eq!((term.begins, term.restores), (1, 1), "guard ran on the error path");
        assert!(!term.hidden, "echo restored despite the error");
        assert_eq!(std::fs::metadata(&key).unwrap().len(), 0, "nothing written");
    }

    // ------------------------------- T21 P2: key-shaped mis-paste catch

    /// A placeholder shaped like a pasted key (never real key material).
    const KEY_SHAPED: &str = "sk-placeholder-0123456789abcdef";

    #[test]
    fn key_shaped_heuristic_matches_keys_not_paths() {
        assert!(looks_like_key_material(KEY_SHAPED));
        assert!(looks_like_key_material("placeholder-not-a-real-key"));
        assert!(looks_like_key_material("AAAAAAAAAA_bbbbbbbbbb-1234"));
        // Any '/' is a path.
        assert!(!looks_like_key_material("~/.secrets/temur-openai-key"));
        assert!(!looks_like_key_material("/etc/keys/k"));
        assert!(!looks_like_key_material("./sk-placeholder-0123456789abcdef"));
        // Too short to be a key.
        assert!(!looks_like_key_material("shortname"));
        // Chars outside [A-Za-z0-9_-] (a dot, a space) read as a filename.
        assert!(!looks_like_key_material("my-very-long-keyfile.txt"));
        assert!(!looks_like_key_material("not a key just words here"));
    }

    #[test]
    fn piped_key_shaped_path_answer_fails_closed_and_stores_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // openai template, default model, then the mis-pasted "path".
        let mut input = std::io::Cursor::new(format!("3\n\n{KEY_SHAPED}\n").into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            unreachable!("keyed templates never list")
        };
        let err = run(
            &cfg_path,
            Some(&home),
            false,
            &mut input,
            &mut out,
            &list,
            &|_| None,
            &mut NoTty,
        )
        .unwrap_err();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("WARNING: that looks like a key, not a path"), "{printed}");
        assert!(printed.contains("rotate"), "{printed}");
        let msg = err.to_string();
        assert!(msg.contains("key-shaped"), "{msg}");
        assert!(msg.contains("nothing was stored"), "{msg}");
        // The pasted value appears in NO output and NO file; no config or
        // key file was created at all.
        assert!(!printed.contains(KEY_SHAPED), "{printed}");
        assert!(!msg.contains(KEY_SHAPED), "{msg}");
        assert!(!cfg_path.exists(), "fail-closed: no config written");
        let entries: Vec<_> = std::fs::read_dir(&home).unwrap().collect();
        assert!(entries.is_empty(), "fail-closed: nothing under HOME: {entries:?}");
    }

    #[test]
    fn interactive_key_shaped_answer_warns_and_reasks_storing_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // openai template, default model, mis-paste, then a real path; the
        // final empty line skips the hidden key prompt.
        let mut input = std::io::Cursor::new(
            format!("3\n\n{KEY_SHAPED}\n~/.secrets/temur-openai-key\n\n").into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            unreachable!("keyed templates never list")
        };
        let mut term = FakeTty::new();
        run(
            &cfg_path,
            Some(&home),
            false,
            &mut input,
            &mut out,
            &list,
            &|_| None,
            &mut term,
        )
        .unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("WARNING: that looks like a key, not a path"), "{printed}");
        assert!(printed.contains("if it was a real key, rotate it"), "{printed}");
        // Re-asked: the question printed twice, and the good answer won.
        assert_eq!(printed.matches("Where to save your API key [").count(), 2, "{printed}");
        // The T13 explainer precedes the question, once per call: the
        // re-ask after the mis-paste does not repeat it (the warning
        // already explains).
        assert_eq!(
            printed.matches("Where to save your API key [").count(),
            2,
            "{printed}"
        );
        let cfg = std::fs::read_to_string(&cfg_path).unwrap();
        let good = home.join(".secrets").join("temur-openai-key");
        assert!(cfg.contains(&good.display().to_string()), "{cfg}");
        // The pasted value appears in NO output and NO file.
        assert!(!printed.contains(KEY_SHAPED), "{printed}");
        assert!(!cfg.contains(KEY_SHAPED), "{cfg}");
        assert_eq!(
            std::fs::metadata(&good).unwrap().len(),
            0,
            "key file created empty; the mis-paste never landed anywhere"
        );
    }

    // ------------------------- T51 P1: repro-first (red on the parent)

    /// A replacement that is never real key material.
    const REPLACEMENT: &str = "placeholder-replacement-key";

    /// D17, live on the desktop dogfood: a wrong key had been pasted into
    /// the key file, `init --force` answered "left untouched" (--force
    /// governs only the config), and the wizard offered no way through.
    /// Consent, asked once, is that way through.
    #[test]
    fn interactive_replace_consent_rewrites_an_existing_key_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key = tmp.path().join("k");
        std::fs::write(&key, "WRONG-MATERIAL\n").unwrap();
        // openai, default model, the existing key path, yes, the new key.
        let mut input = std::io::Cursor::new(
            format!("3\n\n{}\ny\n{REPLACEMENT}\n", key.display()).into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        let mut term = FakeTty::new();
        run(&cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut term)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), format!("{REPLACEMENT}\n"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o7777,
            0o600,
            "the replacement is written under the same mode as a fresh key file"
        );
        // The hidden path was used, and neither the old nor the new
        // material reached the transcript.
        assert_eq!((term.begins, term.restores), (1, 1));
        let printed = String::from_utf8(out).unwrap();
        assert!(!printed.contains(REPLACEMENT), "{printed}");
        assert!(!printed.contains("WRONG-MATERIAL"), "{printed}");
    }

    /// The never-overwrite-without-consent invariant: Enter is No, and No
    /// writes nothing and never even suppresses echo.
    #[test]
    fn interactive_replace_defaults_to_no_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key = tmp.path().join("k");
        std::fs::write(&key, "EXISTING-MATERIAL\n").unwrap();
        let mut input =
            std::io::Cursor::new(format!("3\n\n{}\n\n", key.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let mut term = FakeTty::new();
        run(&cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut term)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "EXISTING-MATERIAL\n");
        assert_eq!(term.begins, 0, "no hidden read happens on the No path");
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("[y/N]"), "a No-defaulted question was asked: {printed}");
        assert!(!printed.contains("Paste your API key"), "{printed}");
        assert!(!printed.contains("EXISTING-MATERIAL"), "{printed}");
    }

    /// Off a TTY there is no question to ask, so the message itself has to
    /// carry the way out. This is the exact state D17 was stranded in.
    #[test]
    fn piped_existing_key_file_is_told_how_to_be_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key = tmp.path().join("k");
        std::fs::write(&key, "EXISTING-MATERIAL\n").unwrap();
        // A key answer IS piped; it must never be consumed or written.
        let mut input = std::io::Cursor::new(
            format!("3\n\n{}\n{REPLACEMENT}\n", key.display()).into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut NoTty)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "EXISTING-MATERIAL\n");
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("as it is"), "{printed}");
        assert!(printed.contains("rerun"), "the way out is named: {printed}");
        assert!(!printed.contains("[y/N]"), "no question off a TTY: {printed}");
        assert!(!printed.contains(REPLACEMENT), "{printed}");
    }

    /// D18 half one: a key path whose parent cannot hold a file is caught
    /// while there is still nothing to undo.
    #[test]
    fn a_key_path_under_a_regular_file_is_rejected_before_the_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "not a directory\n").unwrap();
        let key = blocker.join("k");
        let mut input =
            std::io::Cursor::new(format!("3\n\n{}\n", key.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let err = run(
            &cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut NoTty,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&key.display().to_string()), "names the path: {msg}");
        assert!(!cfg_path.exists(), "no config from a doomed run: {msg}");
    }

    /// A bad path is the wizard's to report, not to abort on, when someone
    /// is there to answer: the same shape as the key-shaped catch beside
    /// it, and the reason is printed exactly once.
    #[test]
    fn interactive_bad_key_path_is_reported_once_and_reasked() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "not a directory\n").unwrap();
        let good = tmp.path().join("k");
        // openai, default model, the doomed path, a good one, skip the key.
        let mut input = std::io::Cursor::new(
            format!("3\n\n{}/k\n{}\n\n", blocker.display(), good.display()).into_bytes(),
        );
        let mut out: Vec<u8> = Vec::new();
        let mut term = FakeTty::new();
        run(&cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut term)
            .unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert_eq!(
            printed.matches("is not a directory").count(),
            1,
            "reported once, not once per sink: {printed}"
        );
        assert_eq!(printed.matches("Where to save your API key [").count(), 2, "{printed}");
        assert!(good.is_file(), "{printed}");
        let cfg = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(cfg.contains(&good.display().to_string()), "{cfg}");
    }

    /// D18 half two: init writes the config BEFORE the key step, so a key
    /// step that fails anyway must take the config back with it.
    #[test]
    fn a_failed_key_step_leaves_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "not a directory\n").unwrap();
        // Parent "blocker/sub" does not exist and cannot be created.
        let key = blocker.join("sub").join("k");
        let mut input =
            std::io::Cursor::new(format!("3\n\n{}\n", key.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let err = run(
            &cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut NoTty,
        )
        .unwrap_err();
        assert!(!cfg_path.exists(), "init is all-or-nothing: {err}");
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("Rolled back"), "the undo is stated: {printed}");
    }

    /// --force overwrites a config; a key step that then fails must put the
    /// PREVIOUS config back, not delete the user's file.
    #[test]
    fn a_failed_key_step_restores_the_config_force_overwrote() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        std::fs::write(&cfg_path, "{ \"model\": \"previous\" }\n").unwrap();
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "not a directory\n").unwrap();
        let key = blocker.join("sub").join("k");
        let mut input =
            std::io::Cursor::new(format!("3\n\n{}\n", key.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, true, &mut input, &mut out, &no_listing, &|_| None, &mut NoTty)
            .unwrap_err();
        assert_eq!(
            std::fs::read_to_string(&cfg_path).unwrap(),
            "{ \"model\": \"previous\" }\n",
            "the config --force replaced is restored byte for byte"
        );
    }

    /// The closing lines describe the state the wizard just observed: a
    /// server that answered is not one you are told to go start.
    #[test]
    fn local_next_steps_drop_the_start_clause_when_the_server_answered() {
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (_cfg, out) = run_wizard_probed("\n\n\n", &list, &|_| Some(12288)).unwrap();
        assert!(!out.contains("start your server"), "{out}");
        assert!(out.contains("Next:"), "{out}");
        assert!(out.contains("doctor"), "doctor still named: {out}");
    }

    /// ...and a server that did not answer still gets the start clause.
    #[test]
    fn local_next_steps_keep_the_start_clause_when_the_probe_failed() {
        let list = |_: &str| -> Result<Vec<String>, crate::error::Error> {
            Err(crate::error::Error::Models("connection refused".into()))
        };
        let (_cfg, out) = run_wizard_probed("\n\n\n", &list, &|_| None).unwrap();
        assert!(out.contains("start your server"), "{out}");
    }

    /// A listing that answered is enough on its own: /props is silent on a
    /// server that is not llama.cpp, and that server is still up.
    #[test]
    fn a_listing_alone_counts_as_a_live_server() {
        let list = |_: &str| Ok(ids(&["served-model"]));
        let (_cfg, out) = run_wizard_probed("\n\n\n", &list, &|_| None).unwrap();
        assert!(!out.contains("start your server"), "{out}");
    }

    /// The hosted-compat templates print a bare "Model [default]:" today.
    /// One line has to say that Enter is fine, that other ids work, and
    /// where to see them; no baked list.
    #[test]
    fn hosted_compat_model_question_says_where_the_ids_are() {
        for answer in ["3", "4", "5"] {
            let tmp = tempfile::tempdir().unwrap();
            let cfg_path = tmp.path().join("config.json");
            let key = tmp.path().join("k");
            let mut input = std::io::Cursor::new(
                format!("{answer}\n\n{}\n", key.display()).into_bytes(),
            );
            let mut out: Vec<u8> = Vec::new();
            run(
                &cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None,
                &mut NoTty,
            )
            .unwrap();
            let printed = String::from_utf8(out).unwrap();
            let head = printed.split("Model id [").next().unwrap();
            assert!(head.contains("/models"), "guidance precedes the question: {printed}");
        }
    }

    /// The anthropic template asks for a startup profile, not a model id,
    /// and its own table is the guidance; the new line must not appear.
    #[test]
    fn the_anthropic_template_gets_no_hosted_compat_line() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_path = tmp.path().join("config.json");
        let key = tmp.path().join("k");
        let mut input =
            std::io::Cursor::new(format!("2\n\n{}\n", key.display()).into_bytes());
        let mut out: Vec<u8> = Vec::new();
        run(&cfg_path, None, false, &mut input, &mut out, &no_listing, &|_| None, &mut NoTty)
            .unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(!printed.contains("/models"), "{printed}");
    }
}
