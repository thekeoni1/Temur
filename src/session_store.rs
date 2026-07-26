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
    /// T10 named sessions. `None` is the project's default session, and is
    /// deliberately NOT serialized: a default-session file stays
    /// byte-identical to the pre-T10 shape, so older binaries and the
    /// existing goldens are untouched. `#[serde(default)]` covers the other
    /// direction — a pre-T10 file loads as the default session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
    /// See [`SessionFile::name`]; `Option<&str>` keeps the struct `Copy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
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

/// The per-directory filename stem shared by the default session and every
/// named session of a project: readable basename + 16-hex FNV-1a digest of
/// the canonicalized path. The digest input and format are FROZEN (see
/// [`fnv1a64`]); T10 named sessions only append to this stem, they never
/// change it.
fn session_stem(cwd: &Path) -> String {
    let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let base = canonical
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let hash = fnv1a64(canonical.to_string_lossy().as_bytes());
    format!("{}-{:016x}", sanitize_basename(&base), hash)
}

/// The DEFAULT session filename for a working directory: a readable basename
/// (so an operator can tell files apart by eye) plus a 16-hex FNV-1a digest
/// of the canonicalized path (so two different directories sharing a
/// basename never collide). No timestamp — see the module docs. Unchanged by
/// T10: this is exactly the pre-T10 name, so existing sessions keep working.
pub fn session_file_name(cwd: &Path) -> String {
    format!("{}.json", session_stem(cwd))
}

/// A NAMED session's filename (T10): the default stem plus `-{name}`.
/// `name` must already be sanitized ([`sanitize_session_name`]) — the
/// commands layer owns rejecting bad names; this function just formats.
pub fn named_session_file_name(cwd: &Path, name: &str) -> String {
    format!("{}-{name}.json", session_stem(cwd))
}

/// Sanitize a user-supplied session name (T10): keep the same character set
/// as [`sanitize_basename`] (`[A-Za-z0-9._-]`), but DROP anything else
/// rather than replacing it — a name is an identifier the user will type
/// back at `/resume`, and `-`-padding junk would make `"///"` silently
/// become `"---"`. Cap at 32 chars; `None` when nothing survives.
pub fn sanitize_session_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-')
        .take(32)
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Full path of the session file for `cwd` inside `dir`.
pub fn session_path(dir: &Path, cwd: &Path) -> PathBuf {
    dir.join(session_file_name(cwd))
}

// ------------------------------------------------------------------- listing

/// One saved session as the listing sees it. Everything except `file_name`,
/// `bytes`, and `mtime` is read from INSIDE the file — hashed filenames are
/// uninformative on purpose, and `cwd` is stored precisely so listings can
/// say where a session came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    pub file_name: String,
    pub cwd: String,
    pub name: Option<String>,
    /// Derived at LIST TIME from the first user prompt in the history —
    /// display-only, never stored (no format change).
    pub title: Option<String>,
    pub messages: usize,
    pub bytes: u64,
    pub mtime: Option<std::time::SystemTime>,
}

/// Display title for a saved history: the first plain-text block of the
/// first `Role::User` message (tool-result messages carry no plain text and
/// fall through), first line only, truncated to ~60 columns. Display-only by
/// design — deriving beats storing, because a stored title could go stale
/// against the history it summarizes.
fn derived_title(history: &[RequestMessage]) -> Option<String> {
    const TITLE_COLS: usize = 60;
    for m in history {
        if m.role != Role::User {
            continue;
        }
        for b in &m.content {
            if let ContentBlock::Text { text } = b {
                let line = text.lines().next().unwrap_or("");
                if line.is_empty() {
                    continue;
                }
                let mut title: String = line.chars().take(TITLE_COLS).collect();
                if line.chars().count() > TITLE_COLS {
                    title.push('…');
                }
                return Some(title);
            }
        }
    }
    None
}

/// List every session file in `dir`, newest first.
///
/// Ordering is by filesystem mtime, descending, with `UNIX_EPOCH` as the
/// fallback and the file name as the tie-break. The FORMAT stays clock-less
/// (module docs): mtime is display-order metadata the filesystem already
/// keeps, read at list time and never written into a file — the same
/// precedent as `tools/glob.rs`. On a clock-less device every file sorts
/// equal and the lexicographic tie-break takes over; nothing breaks.
///
/// A file that cannot be read or parsed becomes an `"(unreadable)"` entry —
/// the listing REPORTS it rather than hiding it, and never panics or aborts
/// the rest of the listing. A missing/unreadable directory lists as empty.
pub fn list_sessions(dir: &Path) -> Vec<SessionEntry> {
    let mut out: Vec<SessionEntry> = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue; // skips .tmp.<pid> litter and anything foreign
        }
        let file_name = match path.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => continue,
        };
        let meta = entry.metadata().ok();
        let bytes: u64 = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta.and_then(|m| m.modified().ok());
        let e = match load(&path) {
            Ok(f) => SessionEntry {
                file_name,
                cwd: f.cwd.clone(),
                name: f.name.clone(),
                title: derived_title(&f.history),
                messages: f.history.len(),
                bytes,
                mtime,
            },
            Err(_) => SessionEntry {
                file_name,
                cwd: "(unreadable)".to_string(),
                name: None,
                title: None,
                messages: 0,
                bytes,
                mtime,
            },
        };
        out.push(e);
    }
    sort_entries(&mut out);
    out
}

/// mtime desc, `UNIX_EPOCH` fallback, file-name tie-break (see
/// [`list_sessions`]). Split out so the ordering rule is table-testable
/// without racing real filesystem timestamps.
fn sort_entries(entries: &mut [SessionEntry]) {
    entries.sort_by(|a, b| {
        let am = a.mtime.unwrap_or(std::time::UNIX_EPOCH);
        let bm = b.mtime.unwrap_or(std::time::UNIX_EPOCH);
        bm.cmp(&am).then_with(|| a.file_name.cmp(&b.file_name))
    });
}

/// Resolve a `/resume` key against a listing. Pure — no filesystem.
///
/// Precedence: (1) exact session name recorded in the CURRENT project
/// (`entry.cwd == cwd`), (2) an exact session name that is globally unique
/// across projects, (3) a unique file-name prefix (which is how the default
/// session, having no name, is addressed). Several matches at the deciding
/// tier is an error that lists the candidates WITH their cwds; no match at
/// any tier is an error too. Never guesses.
pub fn resolve_session_key<'a>(
    entries: &'a [SessionEntry],
    cwd: &str,
    key: &str,
) -> Result<&'a SessionEntry, String> {
    let describe = |c: &[&SessionEntry]| -> String {
        c.iter()
            .map(|e| format!("{} ({})", e.file_name, e.cwd))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // (1) exact name in the current project. Unique by construction: the
    // name is part of the filename, and one directory holds one file per
    // name.
    if let Some(e) = entries
        .iter()
        .find(|e| e.name.as_deref() == Some(key) && e.cwd == cwd)
    {
        return Ok(e);
    }
    // (2) exact name anywhere, if globally unique.
    let named: Vec<&SessionEntry> =
        entries.iter().filter(|e| e.name.as_deref() == Some(key)).collect();
    match named.len() {
        1 => return Ok(named[0]),
        0 => {}
        _ => {
            return Err(format!(
                "session name {key:?} exists in several projects: {} — resume by file-name prefix instead",
                describe(&named)
            ))
        }
    }
    // (3) file-name prefix.
    let prefixed: Vec<&SessionEntry> = entries
        .iter()
        .filter(|e| e.file_name.starts_with(key))
        .collect();
    match prefixed.len() {
        1 => Ok(prefixed[0]),
        0 => Err(format!(
            "no saved session matches {key:?} — /sessions lists what exists"
        )),
        _ => Err(format!(
            "session key {key:?} is ambiguous: {} — give more of the file name",
            describe(&prefixed)
        )),
    }
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
    fn session_name_sanitizing() {
        // Same character set as basenames, but disallowed chars are DROPPED.
        assert_eq!(sanitize_session_name("alpha"), Some("alpha".into()));
        assert_eq!(sanitize_session_name("Alpha_1.x-y"), Some("Alpha_1.x-y".into()));
        assert_eq!(sanitize_session_name("my project!"), Some("myproject".into()));
        // Nothing survives: an error, never a silent "---".
        assert_eq!(sanitize_session_name("///"), None);
        assert_eq!(sanitize_session_name(""), None);
        assert_eq!(sanitize_session_name("~~~"), None);
        // Cap at 32.
        assert_eq!(sanitize_session_name(&"x".repeat(80)).unwrap().len(), 32);
    }

    fn entry(file_name: &str, cwd: &str, name: Option<&str>) -> SessionEntry {
        SessionEntry {
            file_name: file_name.into(),
            cwd: cwd.into(),
            name: name.map(String::from),
            title: None,
            messages: 0,
            bytes: 0,
            mtime: None,
        }
    }

    #[test]
    fn ordering_is_mtime_desc_with_epoch_fallback_and_lexicographic_tie() {
        use std::time::{Duration, UNIX_EPOCH};
        let t = |secs: u64| Some(UNIX_EPOCH + Duration::from_secs(secs));
        let mut v = vec![
            entry("b.json", "/x", None),
            entry("a.json", "/x", None),
            entry("old.json", "/x", None),
            entry("new.json", "/x", None),
        ];
        v[0].mtime = None; // clock-less: sorts as UNIX_EPOCH
        v[1].mtime = None;
        v[2].mtime = t(100);
        v[3].mtime = t(200);
        sort_entries(&mut v);
        let names: Vec<&str> = v.iter().map(|e| e.file_name.as_str()).collect();
        // Newest first; the two epoch-fallback entries tie and break by name.
        assert_eq!(names, vec!["new.json", "old.json", "a.json", "b.json"]);
    }

    #[test]
    fn resolution_table() {
        let entries = vec![
            entry("proj-aaaa.json", "/cur", None),
            entry("proj-aaaa-alpha.json", "/cur", Some("alpha")),
            entry("other-bbbb-alpha.json", "/other", Some("alpha")),
            entry("other-bbbb-beta.json", "/other", Some("beta")),
            entry("third-cccc-beta.json", "/third", Some("beta")),
            entry("third-cccc-gamma.json", "/third", Some("gamma")),
            // A project whose DIRECTORY is called "alpha": its default
            // session's file name starts with the string "alpha".
            entry("alpha-dddd.json", "/alphaproj", None),
        ];
        let r = |cwd: &str, key: &str| resolve_session_key(&entries, cwd, key);

        // (1) exact name in the current project wins over the same name
        // elsewhere AND over any file-name prefix match.
        assert_eq!(r("/cur", "alpha").unwrap().file_name, "proj-aaaa-alpha.json");
        // (2) globally-unique name resolves from anywhere.
        assert_eq!(r("/cur", "gamma").unwrap().file_name, "third-cccc-gamma.json");
        // Duplicate name, neither in the current project: ambiguous, and the
        // error lists every candidate with its cwd.
        let err = r("/cur", "beta").unwrap_err();
        assert!(err.contains("several projects"), "{err}");
        assert!(err.contains("/other") && err.contains("/third"), "{err}");
        assert!(err.contains("other-bbbb-beta.json") && err.contains("third-cccc-beta.json"), "{err}");
        // But from a project that HAS a beta, that one wins.
        assert_eq!(r("/other", "beta").unwrap().file_name, "other-bbbb-beta.json");
        // (3) unique file-name prefix addresses the default session.
        assert_eq!(r("/cur", "proj-aaaa.").unwrap().file_name, "proj-aaaa.json");
        assert_eq!(r("/cur", "alpha-dddd").unwrap().file_name, "alpha-dddd.json");
        // Ambiguous prefix: error, candidates with cwds.
        let err = r("/cur", "proj-").unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("proj-aaaa.json") && err.contains("proj-aaaa-alpha.json"), "{err}");
        // No match at any tier.
        let err = r("/cur", "zzz").unwrap_err();
        assert!(err.contains("no saved session") && err.contains("zzz"), "{err}");
    }

    #[test]
    fn titles_derive_from_the_first_user_prompt() {
        // Plain case: first line of the first user message.
        assert_eq!(
            derived_title(&[user("fix the parser\nand more")]),
            Some("fix the parser".into())
        );
        // Tool-result user messages carry no plain text: fall through to the
        // next user message (the trimmed-history shape can't even start with
        // one, but the derivation must not depend on that).
        let results = RequestMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "ok".into(),
                is_error: false,
            }],
        };
        assert_eq!(
            derived_title(&[results, user("second prompt")]),
            Some("second prompt".into())
        );
        // Assistant text never titles a session.
        let assistant = RequestMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: "hello".into() }],
        };
        assert_eq!(derived_title(&[assistant]), None);
        assert_eq!(derived_title(&[]), None);
        // ~60-col truncation with an ellipsis.
        let long = "y".repeat(90);
        let t = derived_title(&[user(&long)]).unwrap();
        assert_eq!(t.chars().count(), 61);
        assert!(t.ends_with('…'));
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
