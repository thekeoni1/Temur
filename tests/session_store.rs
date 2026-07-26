//! T5 session-persistence tests. Fully offline: real files in a tempdir,
//! explicit paths everywhere (no process-env mutation, so these are safe to
//! run in parallel with everything else).

use temur::config;
use temur::provider::{ContentBlock, RequestMessage, Role, Usage};
use temur::session_store::{self as store, SessionFile, SessionFileRef, StoreError, FORMAT_VERSION};
use temur::tools::TodoItem;

fn user_text(t: &str) -> RequestMessage {
    RequestMessage {
        role: Role::User,
        content: vec![ContentBlock::Text { text: t.into() }],
    }
}

fn assistant(content: Vec<ContentBlock>) -> RequestMessage {
    RequestMessage {
        role: Role::Assistant,
        content,
    }
}

fn tool_result(id: &str, content: &str, is_error: bool) -> RequestMessage {
    RequestMessage {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: content.into(),
            is_error,
        }],
    }
}

fn file_with(history: Vec<RequestMessage>) -> SessionFile {
    SessionFile {
        version: FORMAT_VERSION,
        provider: "anthropic".into(),
        model: "claude-sonnet-5".into(),
        cwd: "/work".into(),
        history,
        session_usage: Usage {
            input_tokens: Some(120),
            output_tokens: Some(45),
            ..Default::default()
        },
        todos: vec![],
        last_context_used: Some(165),
        name: None,
    }
}

fn as_ref(f: &SessionFile) -> SessionFileRef<'_> {
    SessionFileRef {
        version: f.version,
        provider: &f.provider,
        model: &f.model,
        cwd: &f.cwd,
        history: &f.history,
        session_usage: f.session_usage,
        todos: &f.todos,
        last_context_used: f.last_context_used,
        name: f.name.as_deref(),
    }
}

/// Discard notices (most tests assert on the file, not the chatter).
fn quiet(_: String) {}

// ------------------------------------------------- (a) the ROADMAP acceptance

#[test]
fn every_block_kind_round_trips_including_thinking_signatures() {
    // One history containing every block the neutral vocabulary can hold.
    // This is the T5 acceptance criterion: persistence must not quietly lose
    // provider round-trip state.
    let history = vec![
        user_text("do the thing"),
        assistant(vec![
            ContentBlock::Thinking {
                thinking: "step one, then step two".into(),
                signature: Some("sig-abc123-OPAQUE".into()),
            },
            ContentBlock::RedactedThinking {
                data: "REDACTED-PAYLOAD".into(),
            },
            ContentBlock::Text {
                text: "I'll read the file.".into(),
            },
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "read".into(),
                input: serde_json::json!({"file_path": "/work/a.txt"}),
                input_raw: None,
            },
            ContentBlock::ToolUse {
                id: "toolu_2".into(),
                name: "write".into(),
                // The T4 case: arguments that failed to parse on the wire, so
                // input stayed {} and the raw string was preserved.
                input: serde_json::json!({}),
                input_raw: Some("{\"file_path\": \"/work/b.txt\", \"content\": \"trunc".into()),
            },
        ]),
        RequestMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: "file contents".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_2".into(),
                    content: "The tool call was NOT executed".into(),
                    is_error: true,
                },
            ],
        },
    ];
    let original = file_with(history);

    let json = serde_json::to_string(&as_ref(&original)).unwrap();
    // The opaque signature must survive VERBATIM — a provider that verifies
    // its own thinking blocks rejects the conversation otherwise.
    assert!(
        json.contains("sig-abc123-OPAQUE"),
        "thinking signature missing from the serialized session"
    );
    assert!(json.contains("REDACTED-PAYLOAD"));
    // input_raw needs no schema change: skip_serializing_if only elides None.
    assert!(json.contains("input_raw"));

    let back: SessionFile = serde_json::from_str(&json).unwrap();
    assert_eq!(back, original, "session did not round-trip unchanged");
}

// -------------------------------------------------------- (b) save/load on disk

#[test]
fn save_then_load_round_trips_and_leaves_no_tmp_litter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("s.json");
    let f = file_with(vec![user_text("hello"), assistant(vec![ContentBlock::Text {
        text: "hi".into(),
    }])]);

    store::save(&path, &as_ref(&f), 1_000_000, &mut quiet).unwrap();
    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded, f);

    // Exactly one file in the directory: the temp file was renamed, not left.
    let entries: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["s.json".to_string()], "tmp file left behind");
}

#[test]
fn save_overwrites_previous_file_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let first = file_with(vec![user_text("one")]);
    store::save(&path, &as_ref(&first), 1_000_000, &mut quiet).unwrap();
    let second = file_with(vec![user_text("one"), user_text("two")]);
    store::save(&path, &as_ref(&second), 1_000_000, &mut quiet).unwrap();
    assert_eq!(store::load(&path).unwrap().history.len(), 2);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn missing_file_is_a_distinct_error_naming_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.json");
    match store::load(&path) {
        Err(StoreError::Missing { .. }) => {}
        other => panic!("expected Missing, got {other:?}"),
    }
    let msg = store::load(&path).unwrap_err().to_string();
    assert!(msg.contains("absent.json"), "error names the path: {msg}");
}

#[test]
fn unknown_version_is_refused_and_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    std::fs::write(
        &path,
        r#"{"version":99,"provider":"anthropic","model":"m","cwd":"/w","history":[]}"#,
    )
    .unwrap();
    let err = match store::load(&path) {
        Err(e @ StoreError::Version { .. }) => e.to_string(),
        other => panic!("expected Version, got {other:?}"),
    };
    assert!(err.contains("s.json"), "error names the path: {err}");
    assert!(err.contains("99"), "error names the found version: {err}");
    assert!(err.contains("remove the file"), "error says what to do: {err}");
}

#[test]
fn corrupt_and_truncated_files_error_without_panicking() {
    let dir = tempfile::tempdir().unwrap();

    let truncated = dir.path().join("truncated.json");
    // Exactly the power-cut shape a NON-atomic writer would leave behind.
    std::fs::write(&truncated, r#"{"version":1,"provider":"anthropic","hist"#).unwrap();
    assert!(matches!(
        store::load(&truncated),
        Err(StoreError::Corrupt { .. })
    ));

    let garbage = dir.path().join("garbage.json");
    std::fs::write(&garbage, "not json at all").unwrap();
    assert!(matches!(
        store::load(&garbage),
        Err(StoreError::Corrupt { .. })
    ));

    // Valid JSON, right version, wrong shape.
    let wrong = dir.path().join("wrong.json");
    std::fs::write(&wrong, r#"{"version":1,"history":"not-an-array"}"#).unwrap();
    assert!(matches!(store::load(&wrong), Err(StoreError::Corrupt { .. })));

    // Valid JSON, no version field at all.
    let noversion = dir.path().join("noversion.json");
    std::fs::write(&noversion, r#"{"history":[]}"#).unwrap();
    assert!(matches!(
        store::load(&noversion),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn unknown_fields_are_tolerated_so_older_binaries_still_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    std::fs::write(
        &path,
        r#"{"version":1,"provider":"anthropic","model":"m","cwd":"/w",
            "history":[{"role":"user","content":[{"type":"text","text":"hi"}]}],
            "some_future_field":{"nested":true}}"#,
    )
    .unwrap();
    let f = store::load(&path).unwrap();
    assert_eq!(f.history.len(), 1);
    // Absent optional fields degrade to defaults, not errors.
    assert_eq!(f.session_usage, Usage::default());
    assert!(f.todos.is_empty());
    assert!(f.last_context_used.is_none());
}

#[test]
fn unknown_content_blocks_are_filtered_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    std::fs::write(
        &path,
        r#"{"version":1,"provider":"anthropic","model":"m","cwd":"/w","history":[
            {"role":"assistant","content":[
                {"type":"text","text":"kept"},
                {"type":"some_future_block","payload":1}
            ]}]}"#,
    )
    .unwrap();
    let f = store::load(&path).unwrap();
    // Same invariant the turn loop enforces: nothing unrecognized is ever
    // echoed back to a provider.
    assert_eq!(
        f.history[0].content,
        vec![ContentBlock::Text {
            text: "kept".into()
        }]
    );
}

// ------------------------------------------------------------------ size cap

/// Assert the saved history is replayable: it starts with a plain user
/// message, and every tool_result answers a tool_use that appears before it.
fn assert_replayable(history: &[RequestMessage]) {
    assert!(!history.is_empty());
    assert_eq!(history[0].role, Role::User);
    assert!(
        !history[0]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
        "saved history starts with tool results — the tool_use they answer was cut away"
    );
    let mut seen: Vec<String> = Vec::new();
    for m in history {
        for b in &m.content {
            match b {
                ContentBlock::ToolUse { id, .. } => seen.push(id.clone()),
                ContentBlock::ToolResult { tool_use_id, .. } => assert!(
                    seen.contains(tool_use_id),
                    "tool_result {tool_use_id} has no preceding tool_use"
                ),
                _ => {}
            }
        }
    }
}

/// Two complete exchanges, each with a large tool result.
fn two_big_exchanges(pad: usize) -> Vec<RequestMessage> {
    let big = "x".repeat(pad);
    vec![
        user_text("first task"),
        assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read".into(),
            input: serde_json::json!({"file_path": "/a"}),
            input_raw: None,
        }]),
        tool_result("t1", &big, false),
        assistant(vec![ContentBlock::Text {
            text: "done one".into(),
        }]),
        user_text("second task"),
        assistant(vec![ContentBlock::ToolUse {
            id: "t2".into(),
            name: "read".into(),
            input: serde_json::json!({"file_path": "/b"}),
            input_raw: None,
        }]),
        tool_result("t2", &big, false),
        assistant(vec![ContentBlock::Text {
            text: "done two".into(),
        }]),
    ]
}

#[test]
fn over_cap_trims_oldest_at_a_cut_point_and_notifies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let f = file_with(two_big_exchanges(4000));
    let mut notices: Vec<String> = Vec::new();

    // Fits one exchange (~4.1 KB) but not two.
    store::save(&path, &as_ref(&f), 6_000, &mut |n| notices.push(n)).unwrap();

    let loaded = store::load(&path).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() <= 6_000);
    // Trimmed to the second exchange: oldest dropped, cut at a plain user
    // message, tool_use/tool_result pairing intact.
    assert_eq!(loaded.history.len(), 4);
    assert_replayable(&loaded.history);
    assert_eq!(
        loaded.history[0].content,
        vec![ContentBlock::Text {
            text: "second task".into()
        }]
    );

    assert_eq!(notices.len(), 1, "exactly one trim notice: {notices:?}");
    let n = &notices[0];
    assert!(n.contains("6000"), "notice names the cap: {n}");
    assert!(n.contains("most recent 4 of 8 messages"), "notice: {n}");
    assert!(
        n.contains("in-memory history unchanged"),
        "notice states the in-memory invariant: {n}"
    );
}

#[test]
fn under_cap_never_trims_and_never_notifies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let f = file_with(two_big_exchanges(100));
    let mut notices: Vec<String> = Vec::new();
    store::save(&path, &as_ref(&f), 1_000_000, &mut |n| notices.push(n)).unwrap();
    assert_eq!(store::load(&path).unwrap().history.len(), 8);
    assert!(notices.is_empty(), "unexpected notices: {notices:?}");
}

#[test]
fn final_unit_over_cap_skips_the_write_and_leaves_the_previous_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");

    // A good save first — this is what must survive.
    let small = file_with(vec![user_text("keep me")]);
    store::save(&path, &as_ref(&small), 1_000_000, &mut quiet).unwrap();
    let before = std::fs::read(&path).unwrap();

    // Now a session whose most recent exchange alone blows the cap.
    let f = file_with(two_big_exchanges(4000));
    let mut notices: Vec<String> = Vec::new();
    let err = store::save(&path, &as_ref(&f), 1_000, &mut |n| notices.push(n)).unwrap_err();
    match &err {
        StoreError::UnitTooLarge { cap: 1_000 } => {}
        other => panic!("expected UnitTooLarge, got {other:?}"),
    }
    assert!(
        err.to_string().contains("previous session file is unchanged"),
        "error explains the outcome: {err}"
    );
    assert!(notices.is_empty(), "no trim notice when nothing was saved");

    // Byte-identical, and no litter.
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(store::load(&path).unwrap(), small);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

// ------------------------------------------------------------ paths + naming

#[test]
fn sessions_dir_precedence_is_state_not_config() {
    use std::path::{Path, PathBuf};
    assert_eq!(
        store::sessions_dir_from(Some("/custom/dir"), Some(Path::new("/st")), None),
        PathBuf::from("/custom/dir")
    );
    assert_eq!(
        store::sessions_dir_from(None, Some(Path::new("/st")), Some(Path::new("/h"))),
        PathBuf::from("/st/temur/sessions")
    );
    // The deliberate divergence from ROADMAP's "config dir" wording:
    // megabyte transcripts of tool output belong in state, not in a
    // dotfile-synced ~/.config.
    let fallback = store::sessions_dir_from(None, None, Some(Path::new("/h")));
    assert_eq!(fallback, PathBuf::from("/h/.local/state/temur/sessions"));
    assert!(!fallback.to_string_lossy().contains(".config"));
}

#[test]
fn filename_is_frozen_by_golden_hashes() {
    use std::path::Path;
    // FNV-1a/64 is hand-rolled precisely so these strings can never move: a
    // toolchain change that altered the hash would orphan every saved
    // session. If this test fails, the hash changed — do not "fix" the
    // expected values, fix the hash.
    //
    // A path that does not exist: canonicalize falls back to the path as
    // given, so the expected hash is FNV-1a of that exact string.
    assert_eq!(
        store::session_file_name(Path::new("/tmp/temur-golden-session-path")),
        "temur-golden-session-path-ff620d6a9bcc8310.json"
    );
    // Root: no basename at all -> "root", and "/" canonicalizes to itself.
    assert_eq!(
        store::session_file_name(Path::new("/")),
        "root-af63a24c860189fe.json"
    );
    // Different directories with the SAME basename must not collide.
    let a = store::session_file_name(Path::new("/tmp/nonexistent-a/project"));
    let b = store::session_file_name(Path::new("/tmp/nonexistent-b/project"));
    assert!(a.starts_with("project-") && b.starts_with("project-"));
    assert_ne!(a, b);
    // No timestamps anywhere — clock-less devices are in the niche, and the
    // name must therefore be STABLE for a given directory across runs.
    assert_eq!(a, store::session_file_name(Path::new("/tmp/nonexistent-a/project")));

    let dir = std::path::PathBuf::from("/state/sessions");
    assert_eq!(
        store::session_path(&dir, Path::new("/")),
        dir.join("root-af63a24c860189fe.json")
    );
}

#[test]
fn named_filenames_are_frozen_by_golden_hashes_too() {
    use std::path::Path;
    // T10: a named session is the default stem + "-{name}". Same frozen
    // FNV-1a digest — the default golden above pins the stem; this pins the
    // suffix rule. If either fails, fix the code, never the strings.
    assert_eq!(
        store::named_session_file_name(Path::new("/tmp/temur-golden-session-path"), "alpha"),
        "temur-golden-session-path-ff620d6a9bcc8310-alpha.json"
    );
    assert_eq!(
        store::named_session_file_name(Path::new("/"), "x2"),
        "root-af63a24c860189fe-x2.json"
    );
    // The default name is EXACTLY the pre-T10 name — no suffix, no change.
    assert_eq!(
        store::session_file_name(Path::new("/tmp/temur-golden-session-path")),
        "temur-golden-session-path-ff620d6a9bcc8310.json"
    );
}

#[test]
fn name_field_round_trips_and_default_files_keep_the_pre_t10_shape() {
    let dir = tempfile::tempdir().unwrap();

    // Default session (name: None): the serialized file must not mention
    // "name" at all — byte-compatible with what a pre-T10 binary writes.
    let f = file_with(vec![user_text("hello")]);
    let json = serde_json::to_string(&as_ref(&f)).unwrap();
    assert!(!json.contains("\"name\""), "default file grew a name field: {json}");

    // Named session: round-trips through save/load.
    let mut named = file_with(vec![user_text("hello")]);
    named.name = Some("alpha".into());
    let path = dir.path().join("s-alpha.json");
    store::save(&path, &as_ref(&named), 1_000_000, &mut quiet).unwrap();
    assert_eq!(store::load(&path).unwrap().name.as_deref(), Some("alpha"));
}

#[test]
fn pre_t10_files_load_as_the_default_session_and_tolerance_extends() {
    let dir = tempfile::tempdir().unwrap();
    // A file exactly as a pre-T10 binary wrote it: no name field.
    let path = dir.path().join("old.json");
    std::fs::write(
        &path,
        r#"{"version":1,"provider":"anthropic","model":"m","cwd":"/w",
            "history":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#,
    )
    .unwrap();
    assert_eq!(store::load(&path).unwrap().name, None);
    // And a FUTURE file carrying both a name and unknown fields still loads
    // (same tolerance rule the T5 test pins for the envelope).
    let path = dir.path().join("future.json");
    std::fs::write(
        &path,
        r#"{"version":1,"provider":"anthropic","model":"m","cwd":"/w","name":"beta",
            "history":[],"some_future_field":42}"#,
    )
    .unwrap();
    assert_eq!(store::load(&path).unwrap().name.as_deref(), Some("beta"));
}

#[test]
fn list_sessions_reads_facts_from_inside_files_and_orders_newest_first() {
    let dir = tempfile::tempdir().unwrap();

    let mut a = file_with(vec![user_text("first project task")]);
    a.cwd = "/proj/a".into();
    store::save(&dir.path().join("a-1111.json"), &as_ref(&a), 1_000_000, &mut quiet).unwrap();

    // Ensure a strictly later mtime for the second file (ns-resolution
    // filesystems make this cheap; the pure ordering rule is table-tested
    // in the module).
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut b = file_with(vec![user_text("second project task"), user_text("more")]);
    b.cwd = "/proj/b".into();
    b.name = Some("alpha".into());
    store::save(&dir.path().join("b-2222-alpha.json"), &as_ref(&b), 1_000_000, &mut quiet)
        .unwrap();

    let entries = store::list_sessions(dir.path());
    assert_eq!(entries.len(), 2);
    // Newest first.
    assert_eq!(entries[0].file_name, "b-2222-alpha.json");
    assert_eq!(entries[0].cwd, "/proj/b");
    assert_eq!(entries[0].name.as_deref(), Some("alpha"));
    assert_eq!(entries[0].title.as_deref(), Some("second project task"));
    assert_eq!(entries[0].messages, 2);
    assert!(entries[0].bytes > 0);
    assert!(entries[0].mtime.is_some());
    assert_eq!(entries[1].file_name, "a-1111.json");
    assert_eq!(entries[1].name, None, "pre-T10 shape lists as the default session");
    assert_eq!(entries[1].title.as_deref(), Some("first project task"));
}

#[test]
fn list_sessions_reports_unreadable_files_and_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let good = file_with(vec![user_text("fine")]);
    store::save(&dir.path().join("good-1111.json"), &as_ref(&good), 1_000_000, &mut quiet)
        .unwrap();
    std::fs::write(dir.path().join("corrupt-2222.json"), "{not json").unwrap();
    std::fs::write(
        dir.path().join("future-3333.json"),
        r#"{"version":99,"history":[]}"#,
    )
    .unwrap();
    // tmp litter (the atomic-writer suffix shape) is not a session.
    std::fs::write(dir.path().join("good-1111.json.tmp.999"), "x").unwrap();

    let entries = store::list_sessions(dir.path());
    assert_eq!(entries.len(), 3, "corrupt files are reported, tmp litter skipped");
    let unreadable: Vec<&str> = entries
        .iter()
        .filter(|e| e.cwd == "(unreadable)")
        .map(|e| e.file_name.as_str())
        .collect();
    assert_eq!(unreadable.len(), 2, "{entries:?}");
    assert!(unreadable.contains(&"corrupt-2222.json"));
    assert!(unreadable.contains(&"future-3333.json"));

    // A missing directory is an empty listing, not an error.
    assert!(store::list_sessions(&dir.path().join("nope")).is_empty());
}

#[test]
fn trim_and_cap_are_unaffected_by_the_name_field() {
    // The 4 MiB-cap trim path rebuilds SessionFileRef with a shorter history
    // slice; the name must ride along unchanged.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let mut f = file_with(two_big_exchanges(4000));
    f.name = Some("alpha".into());
    let mut notices: Vec<String> = Vec::new();
    store::save(&path, &as_ref(&f), 6_000, &mut |n| notices.push(n)).unwrap();
    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded.name.as_deref(), Some("alpha"), "name survives a trim");
    assert_eq!(loaded.history.len(), 4);
    assert_eq!(notices.len(), 1);
}

#[test]
fn awkward_basenames_are_sanitized() {
    use std::path::Path;
    let name = store::session_file_name(Path::new("/tmp/nonexistent/my project (v2)"));
    let stem = name.split('-').next().unwrap();
    assert_eq!(stem, "my");
    assert!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
        "unsanitized filename: {name}"
    );
    // Long basenames are capped (40 chars + '-' + 16 hex + ".json").
    let long = store::session_file_name(Path::new(&format!("/tmp/nonexistent/{}", "n".repeat(80))));
    assert_eq!(long.len(), 40 + 1 + 16 + 5);
}

// --------------------------------------------------------------- resume seam

#[test]
fn trailing_plain_user_message_is_dropped_but_tool_results_are_kept() {
    // The provider-error case: a prompt the model never answered. Replaying
    // it would make the model answer stale intent.
    let f = file_with(vec![
        user_text("first"),
        assistant(vec![ContentBlock::Text { text: "ok".into() }]),
        user_text("never answered"),
    ]);
    let (seed, notices) = store::prepare_seed(f);
    assert_eq!(seed.history.len(), 2);
    assert!(
        notices[0].contains("never answered"),
        "drop notice: {notices:?}"
    );
    // The summary counts what was SEEDED, not what was in the file.
    assert!(notices[1].contains("2 messages"), "summary: {notices:?}");

    // A trailing user message carrying tool results is factual and
    // wire-valid — a guard-stopped turn looks exactly like this.
    let f = file_with(vec![
        user_text("first"),
        assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read".into(),
            input: serde_json::json!({}),
            input_raw: None,
        }]),
        tool_result("t1", "contents", false),
    ]);
    let (seed, notices) = store::prepare_seed(f);
    assert_eq!(seed.history.len(), 3);
    assert_eq!(notices.len(), 1, "no drop notice expected: {notices:?}");
}

#[test]
fn resume_notice_renders_absent_usage_as_em_dash() {
    let mut f = file_with(vec![user_text("a"), user_text("b")]);
    let n = store::resume_notice(&f);
    assert_eq!(n, "resumed session: 2 messages, ~120 tokens in / 45 out");
    // A local server that reports no usage must not be shown a fake 0.
    f.session_usage = Usage::default();
    let n = store::resume_notice(&f);
    assert!(n.contains("~— tokens in / — out"), "{n}");
    assert!(!n.contains('0'), "absent usage must not render as zero: {n}");
    // No "turns" count: it would be an approximation presented as fact.
    assert!(!n.contains("turn"), "{n}");
}

#[test]
fn mismatch_notices_are_advisory_and_per_field() {
    let f = file_with(vec![user_text("a")]);
    assert!(store::mismatch_notices(&f, "anthropic", "claude-sonnet-5", "/work").is_empty());

    let n = store::mismatch_notices(&f, "openai-compat", "qwen3", "/elsewhere");
    assert_eq!(n.len(), 3, "one notice per differing field: {n:?}");
    assert!(n[0].contains("provider") && n[0].contains("openai-compat"));
    assert!(n[1].contains("model") && n[1].contains("qwen3"));
    assert!(n[2].contains("/elsewhere"));
    // Advisory: every one of them says the run continues.
    assert!(n.iter().all(|s| s.contains("continuing")), "{n:?}");
}

#[test]
fn seed_carries_todos_and_context_estimate() {
    let mut f = file_with(vec![user_text("a")]);
    f.todos = vec![TodoItem {
        id: Some("1".into()),
        content: "write the thing".into(),
        status: "in_progress".into(),
    }];
    let seed = store::seed(f);
    assert_eq!(seed.todos.len(), 1);
    assert_eq!(seed.last_context_used, Some(165));
    assert_eq!(seed.session_usage.input_tokens, Some(120));
}

// --------------------------------------------------------------- T10 replay

#[test]
fn replay_flattens_interleaved_history_and_is_documented_lossy() {
    use temur::session_store::ReplayItem;
    let history = vec![
        user_text("do the thing"),
        assistant(vec![
            // Thinking (signed or not) never replays — live rendering only
            // ever showed an indicator.
            ContentBlock::Thinking {
                thinking: "step one".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::Text {
                text: "I'll read the file.".into(),
            },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "read".into(),
                input: serde_json::json!({"file_path": "/a"}),
                input_raw: None,
            },
            ContentBlock::ToolUse {
                id: "t2".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
                input_raw: None,
            },
        ]),
        RequestMessage {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "big file contents".into(),
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "t2".into(),
                    content: "listing".into(),
                    is_error: false,
                },
            ],
        },
        assistant(vec![ContentBlock::Text { text: "done".into() }]),
        user_text("thanks"),
    ];
    let items = store::replay_items(&history);
    assert_eq!(
        items,
        vec![
            ReplayItem::User("do the thing".into()),
            ReplayItem::Assistant("I'll read the file.".into()),
            ReplayItem::Tool { name: "read".into() },
            ReplayItem::Tool { name: "bash".into() },
            ReplayItem::Assistant("done".into()),
            ReplayItem::User("thanks".into()),
        ]
    );
    // Lossy by design: tool outputs and arguments are nowhere in the replay.
    let flat = format!("{items:?}");
    assert!(!flat.contains("big file contents"));
    assert!(!flat.contains("/a") && !flat.contains("ls"));
}

#[test]
fn replay_handles_interrupt_markers_and_empty_history() {
    use temur::session_store::ReplayItem;
    // The T6 interrupted-turn shape: the tool_use replays as a Tool item;
    // its synthesized "[interrupted by user]" answer replays as nothing.
    let history = vec![
        user_text("go"),
        assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "sleep 60"}),
            input_raw: None,
        }]),
        tool_result("t1", temur::agent::INTERRUPT_MARKER, true),
    ];
    assert_eq!(
        store::replay_items(&history),
        vec![
            ReplayItem::User("go".into()),
            ReplayItem::Tool { name: "bash".into() },
        ]
    );
    // Empty history: no items, no panic — the caller renders notice-only.
    assert!(store::replay_items(&[]).is_empty());
    // A redacted-thinking-only assistant message produces nothing either.
    let odd = vec![assistant(vec![ContentBlock::RedactedThinking {
        data: "opaque".into(),
    }])];
    assert!(store::replay_items(&odd).is_empty());
}

// ------------------------------------------------------------------- config

#[test]
fn config_cap_default_and_floor() {
    let c = config::Config::default();
    assert_eq!(
        c.session_max_bytes().unwrap(),
        config::DEFAULT_SESSION_MAX_BYTES
    );
    assert_eq!(config::DEFAULT_SESSION_MAX_BYTES, 4 * 1024 * 1024);
    // All byte math is u64 — this is a 32-bit target.
    let _: u64 = config::DEFAULT_SESSION_MAX_BYTES;
    let _: u64 = config::MIN_SESSION_MAX_BYTES;
}

#[test]
fn interrupted_turn_shapes_survive_the_resume_seam() {
    // T6 landing shape A: interrupt left a synthesized "[interrupted by
    // user]" tool-result message at the tail. It is factual and wire-valid —
    // prepare_seed must keep it.
    let f = file_with(vec![
        user_text("go"),
        assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "sleep 60"}),
            input_raw: None,
        }]),
        tool_result("t1", temur::agent::INTERRUPT_MARKER, true),
    ]);
    let (seed, notices) = store::prepare_seed(f);
    assert_eq!(seed.history.len(), 3, "synthesized results are kept");
    assert_eq!(notices.len(), 1, "no drop notice expected: {notices:?}");
    assert!(matches!(
        &seed.history[2].content[0],
        ContentBlock::ToolResult { is_error: true, content, .. }
            if content == temur::agent::INTERRUPT_MARKER
    ));

    // T6 landing shape B: empty landing — the interrupt arrived before any
    // content, so history ends with the plain user prompt. The existing
    // dangling-prompt rule drops it with the existing notice.
    let f = file_with(vec![
        user_text("first"),
        assistant(vec![ContentBlock::Text { text: "ok".into() }]),
        user_text("interrupted before any reply"),
    ]);
    let (seed, notices) = store::prepare_seed(f);
    assert_eq!(seed.history.len(), 2, "trailing plain prompt dropped");
    assert!(notices[0].contains("never answered"), "{notices:?}");
}
