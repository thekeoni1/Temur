//! Session persistence (T5): save and resume a conversation in the neutral
//! vocabulary, as plain JSON, with no new dependencies.
//!
//! Design constraints this module exists to satisfy:
//!
//! - **Neutral, not wire.** The saved history is `provider::types` — the same
//!   vocabulary every provider converts at its own boundary — so a session
//!   recorded against one provider resumes against another. Anthropic
//!   thinking signatures round-trip opaquely; the OpenAI-compat provider
//!   already drops thinking blocks at its own wire boundary, so cross-provider
//!   resume needs no code here.
//! - **Power-cut friendly.** Every save writes a sibling temp file, fsyncs it,
//!   and `rename(2)`s it over the target. A crash at any instant leaves either
//!   the previous complete file or the new complete file — never a partial one.
//! - **Clock-less.** There are no timestamps in the format or in filenames.
//!   Constrained devices boot with no RTC and no network time; a format that
//!   depended on a clock would be a format that lies on exactly the hardware
//!   this project targets.
//! - **32-bit discipline.** All byte math is `u64`.
//!
//! State, not config: sessions live under `$XDG_STATE_HOME/temur/sessions/`
//! (fallback `~/.local/state/temur/sessions/`). Transcripts carry tool output
//! and reach megabytes — they do not belong in a dotfile-synced `~/.config`.

use crate::provider::{ContentBlock, RequestMessage, Role, Usage};
use crate::tools::TodoItem;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// On-disk format version. A file carrying any other value is refused rather
/// than guessed at.
pub const FORMAT_VERSION: u32 = 1;

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("no saved session at {path}")]
    Missing { path: String },
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "{path}: session format version {found}, but this build understands version {expected} \
         — remove the file to start a new session"
    )]
    Version {
        path: String,
        found: String,
        expected: u32,
    },
    #[error("{path}: session file is unreadable ({detail}) — remove the file to start a new session")]
    Corrupt { path: String, detail: String },
    #[error(
        "the most recent exchange alone exceeds the {cap}-byte session size cap; nothing was \
         written and the previous session file is unchanged"
    )]
    UnitTooLarge { cap: u64 },
    #[error("could not serialize the session: {0}")]
    Serialize(String),
}

impl From<StoreError> for crate::error::Error {
    fn from(e: StoreError) -> Self {
        crate::error::Error::Session(e.to_string())
    }
}

fn io_err(path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Io {
        path: path.display().to_string(),
        source,
    }
}

// ------------------------------------------------------------------ envelope

/// The saved session, owned — what `load` produces.
///
/// Unknown fields are deliberately tolerated (no `deny_unknown_fields`): a
/// newer temur may add fields, and an older binary should still resume rather
/// than refuse. The `version` field is the compatibility gate; everything else
/// degrades.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    pub provider: String,
    pub model: String,
    pub cwd: String,
    pub history: Vec<RequestMessage>,
    #[serde(default)]
    pub session_usage: Usage,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub last_context_used: Option<u64>,
}

/// The saving half: borrowed fields so writing a multi-megabyte history never
/// clones it. Serialize-only by construction — `SessionFile` is the read side.
/// Every field is `Copy`, which is what lets the trim path rebuild it with a
/// shorter history slice and nothing else changed.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SessionFileRef<'a> {
    pub version: u32,
    pub provider: &'a str,
    pub model: &'a str,
    pub cwd: &'a str,
    pub history: &'a [RequestMessage],
    pub session_usage: Usage,
    pub todos: &'a [TodoItem],
    pub last_context_used: Option<u64>,
}

/// What a resumed `Session` is rebuilt from. Moved out of a `SessionFile`, so
/// the history is never copied.
pub struct SessionSeed {
    pub history: Vec<RequestMessage>,
    pub session_usage: Usage,
    pub todos: Vec<TodoItem>,
    pub last_context_used: Option<u64>,
}

// --------------------------------------------------------------------- paths

/// Resolve the sessions directory from already-read inputs.
///
/// Pure so it is unit-testable without mutating process env (same shape as
/// `skills::skill_dirs`). Precedence: explicit override, then
/// `$XDG_STATE_HOME/temur/sessions`, then `<home>/.local/state/temur/sessions`.
pub fn sessions_dir_from(
    override_dir: Option<&str>,
    xdg_state: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(d) = override_dir {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Some(x) = xdg_state {
        return x.join("temur").join("sessions");
    }
    home.map(Path::to_path_buf)
        .unwrap_or_default()
        .join(".local")
        .join("state")
        .join("temur")
        .join("sessions")
}

/// Env-reading wrapper over [`sessions_dir_from`].
pub fn sessions_dir(override_dir: Option<&str>) -> PathBuf {
    let xdg = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    sessions_dir_from(override_dir, xdg.as_deref(), home.as_deref())
}

/// FNV-1a, 64-bit, hand-rolled on purpose.
///
/// `DefaultHasher` would be one line, but its algorithm is explicitly
/// unspecified and may change between toolchains — which would silently
/// orphan every session file already on disk. This is a persistence key: it
/// must be frozen, so it is written out and locked by a golden test.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET_BASIS;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Sanitize a directory basename for use in a filename: keep `[A-Za-z0-9._-]`,
/// replace anything else with `-`, cap at 40 chars, and fall back to `root`
/// when nothing survives (e.g. cwd `/`).
fn sanitize_basename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    if cleaned.is_empty() {
        "root".to_string()
    } else {
        cleaned
    }
}

/// The session filename for a working directory: a readable basename (so an
/// operator can tell files apart by eye) plus a 16-hex FNV-1a digest of the
/// canonicalized path (so two different directories sharing a basename never
/// collide). No timestamp — see the module docs.
pub fn session_file_name(cwd: &Path) -> String {
    let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let base = canonical
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let hash = fnv1a64(canonical.to_string_lossy().as_bytes());
    format!("{}-{:016x}.json", sanitize_basename(&base), hash)
}

/// Full path of the session file for `cwd` inside `dir`.
pub fn session_path(dir: &Path, cwd: &Path) -> PathBuf {
    dir.join(session_file_name(cwd))
}

// ---------------------------------------------------------------------- load

/// Read a session file.
///
/// Refuses (never guesses) on an unknown format version, and reports corrupt
/// or truncated JSON as an error — never a panic. `ContentBlock::Unknown` is
/// filtered out of every message on the way in, the same invariant the turn
/// loop enforces before anything reaches a provider.
pub fn load(path: &Path) -> Result<SessionFile, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::Missing {
                path: path.display().to_string(),
            })
        }
        Err(e) => return Err(io_err(path, e)),
    };

    // Version first, off a generic parse: a file from a future format may not
    // fit `SessionFile` at all, and "unsupported version" is a far better
    // message than a field-level deserialization error.
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| StoreError::Corrupt {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(FORMAT_VERSION) => {}
        Some(v) => {
            return Err(StoreError::Version {
                path: path.display().to_string(),
                found: v.to_string(),
                expected: FORMAT_VERSION,
            })
        }
        None => {
            return Err(StoreError::Corrupt {
                path: path.display().to_string(),
                detail: "missing or non-numeric \"version\" field".into(),
            })
        }
    }

    let mut file: SessionFile =
        serde_json::from_value(value).map_err(|e| StoreError::Corrupt {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
    for m in &mut file.history {
        m.content.retain(|b| !matches!(b, ContentBlock::Unknown));
    }
    Ok(file)
}

// ---------------------------------------------------------------------- save

/// Is this message a legal place to start a saved history?
///
/// A plain user message (no `tool_result` blocks) is: replaying from it gives
/// the model a well-formed conversation. A user message carrying tool results
/// is not — it would be severed from the `tool_use` it answers, which every
/// provider rejects.
fn is_cut_point(m: &RequestMessage) -> bool {
    m.role == Role::User
        && !m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
}

/// Serialized length of `history[start..]` as compact JSON, computed from
/// per-message sizes instead of re-serializing: `[a,b,c]` is two brackets,
/// the elements, and one comma between each pair. Exact for serde_json's
/// compact writer, and keeps trimming O(n) rather than O(n²).
fn slice_len(sizes: &[u64], start: usize) -> u64 {
    let n = sizes.len().saturating_sub(start) as u64;
    if n == 0 {
        return 2;
    }
    2 + sizes[start..].iter().sum::<u64>() + (n - 1)
}

/// Write the session atomically, trimming the FILE (never memory) to fit.
///
/// The temp file is suffixed with the pid, and that is load-bearing: two temur
/// processes running in one directory would otherwise interleave writes into a
/// single fixed temp name and rename a corrupt mixture into place. With
/// per-pid temp files every rename publishes a file that was complete and
/// synced, so concurrent writers degrade to last-writer-wins of whole files.
///
/// `notify` receives a user-facing notice if (and only if) the file was
/// trimmed. Returns `UnitTooLarge` without writing anything when even the
/// final exchange cannot fit — the previous file is left intact rather than
/// replaced by something useless.
pub fn save(
    path: &Path,
    file: &SessionFileRef,
    max_bytes: u64,
    notify: &mut dyn FnMut(String),
) -> Result<(), StoreError> {
    let full = serde_json::to_string(file).map_err(|e| StoreError::Serialize(e.to_string()))?;

    let json = if full.len() as u64 <= max_bytes {
        full
    } else {
        // Per-message sizes, computed once.
        let mut sizes: Vec<u64> = Vec::with_capacity(file.history.len());
        for m in file.history {
            let s = serde_json::to_string(m).map_err(|e| StoreError::Serialize(e.to_string()))?;
            sizes.push(s.len() as u64);
        }
        // Everything in the envelope that is not the history array.
        let overhead = (full.len() as u64).saturating_sub(slice_len(&sizes, 0));

        // Trim OLDEST first: advance the start to the earliest cut point that
        // fits. Cut points guarantee the saved history begins with a plain
        // user message and never splits a tool_use from its tool_result.
        let start = (0..file.history.len())
            .find(|&i| is_cut_point(&file.history[i]) && overhead + slice_len(&sizes, i) <= max_bytes)
            .ok_or(StoreError::UnitTooLarge { cap: max_bytes })?;

        let trimmed = SessionFileRef {
            history: &file.history[start..],
            ..*file
        };
        let s =
            serde_json::to_string(&trimmed).map_err(|e| StoreError::Serialize(e.to_string()))?;
        // Belt and braces: the arithmetic above is exact, but the file on
        // disk is what matters, so the real length decides.
        if s.len() as u64 > max_bytes {
            return Err(StoreError::UnitTooLarge { cap: max_bytes });
        }
        notify(format!(
            "session file exceeded {max_bytes} bytes; saved the most recent {} of {} messages (in-memory history unchanged)",
            file.history.len() - start,
            file.history.len()
        ));
        s
    };

    write_atomic(path, json.as_bytes())
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.json".to_string());
    let tmp = path.with_file_name(format!("{name}.tmp.{}", std::process::id()));

    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        // Durability of the CONTENT before it becomes visible under the real
        // name; the rename below is what makes it visible.
        f.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp); // never leave litter behind
        return Err(io_err(&tmp, e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(path, e));
    }
    // Best-effort: fsync the directory so the rename itself survives a power
    // cut. Errors are ignored — only durability is at stake here, never
    // integrity, and Linux is the only ship target.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::File::open(parent).and_then(|d| d.sync_all());
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- resume

/// Move a loaded file into the seed a `Session` resumes from.
pub fn seed(file: SessionFile) -> SessionSeed {
    SessionSeed {
        history: file.history,
        session_usage: file.session_usage,
        todos: file.todos,
        last_context_used: file.last_context_used,
    }
}

/// Apply the dangling-user rule and produce the notices the UI should show.
///
/// A saved history can end with a user message the model never answered — the
/// provider-error case, where the turn is saved precisely because the failure
/// is real history. Replaying it would make the model answer a stale intent,
/// so a TRAILING PLAIN user message is dropped. A trailing user message
/// carrying tool RESULTS is kept: it is factual, wire-valid, and is what a
/// guard-stopped turn legitimately looks like.
///
/// Notices come back in display order (drop first, then the summary) and the
/// summary counts what was actually seeded, not what was in the file.
pub fn prepare_seed(mut file: SessionFile) -> (SessionSeed, Vec<String>) {
    let mut notices = Vec::new();
    let dangling = matches!(file.history.last(), Some(m) if is_cut_point(m));
    if dangling {
        file.history.pop();
        notices.push(
            "resumed session ended with a prompt the model never answered; it was dropped".into(),
        );
    }
    notices.push(resume_notice(&file));
    (seed(file), notices)
}

/// One notice per field that differs between the saved session and this run.
/// Pure, and advisory only — resume proceeds regardless, because the history
/// is provider-neutral and a moved or renamed workspace is still the same
/// conversation.
pub fn mismatch_notices(
    file: &SessionFile,
    provider: &str,
    model: &str,
    cwd: &str,
) -> Vec<String> {
    let mut v = Vec::new();
    if file.provider != provider {
        v.push(format!(
            "resumed session was recorded with provider {:?}; this run uses {:?} — continuing (history is provider-neutral)",
            file.provider, provider
        ));
    }
    if file.model != model {
        v.push(format!(
            "resumed session was recorded with model {:?}; this run uses {:?} — continuing",
            file.model, model
        ));
    }
    if file.cwd != cwd {
        v.push(format!(
            "resumed session was recorded in {:?}; this run is in {:?} — continuing",
            file.cwd, cwd
        ));
    }
    v
}

/// The one-line resume summary. Token counts go through `ui::fmt_tokens`, so a
/// session recorded against a server that reports no usage shows "—" rather
/// than a fabricated 0. Deliberately no "turns" count: the history holds
/// messages, and deriving turns from them would be an approximation presented
/// as a fact.
pub fn resume_notice(file: &SessionFile) -> String {
    format!(
        "resumed session: {} messages, ~{} tokens in / {} out",
        file.history.len(),
        crate::ui::fmt_tokens(file.session_usage.input_tokens),
        crate::ui::fmt_tokens(file.session_usage.output_tokens),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> RequestMessage {
        RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    #[test]
    fn sessions_dir_precedence() {
        // Override wins outright.
        assert_eq!(
            sessions_dir_from(Some("/custom"), Some(Path::new("/state")), Some(Path::new("/h"))),
            PathBuf::from("/custom")
        );
        // Then XDG_STATE_HOME.
        assert_eq!(
            sessions_dir_from(None, Some(Path::new("/state")), Some(Path::new("/h"))),
            PathBuf::from("/state/temur/sessions")
        );
        // Then the home fallback — state, deliberately NOT ~/.config.
        assert_eq!(
            sessions_dir_from(None, None, Some(Path::new("/h"))),
            PathBuf::from("/h/.local/state/temur/sessions")
        );
        // An empty override is not an override.
        assert_eq!(
            sessions_dir_from(Some(""), None, Some(Path::new("/h"))),
            PathBuf::from("/h/.local/state/temur/sessions")
        );
    }

    #[test]
    fn basename_sanitizing() {
        assert_eq!(sanitize_basename("temur"), "temur");
        assert_eq!(sanitize_basename("my project!"), "my-project-");
        assert_eq!(sanitize_basename(""), "root");
        assert_eq!(sanitize_basename("a/b"), "a-b");
        assert_eq!(sanitize_basename(&"x".repeat(80)).len(), 40);
    }

    #[test]
    fn cut_points() {
        assert!(is_cut_point(&user("hello")));
        let results = RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "ok".into(),
                is_error: false,
            }],
        };
        assert!(!is_cut_point(&results));
        let assistant = RequestMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: "hi".into() }],
        };
        assert!(!is_cut_point(&assistant));
    }

    #[test]
    fn slice_len_matches_real_serialization() {
        let history = vec![user("one"), user("two"), user("three")];
        let sizes: Vec<u64> = history
            .iter()
            .map(|m| serde_json::to_string(m).unwrap().len() as u64)
            .collect();
        for start in 0..=history.len() {
            let real = serde_json::to_string(&history[start..]).unwrap().len() as u64;
            assert_eq!(slice_len(&sizes, start), real, "start={start}");
        }
    }
}
