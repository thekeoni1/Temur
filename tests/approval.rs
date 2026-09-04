//! T21 bash approval: execute-time tests of the Ask arm, with the probe
//! forced to FAIL via the one-way TEMUR_TEST_SANDBOX_UNAVAILABLE seam.
//!
//! Own suite file on purpose: `sandbox_available()` caches its answer per
//! process, so a binary that forces the probe cannot share a process with
//! tests asserting the environment's real answer (tests/tools.rs). Every
//! test here calls [`force_probe_fail`] first, so the cached answer in
//! THIS binary is always "unavailable".
//!
//! HARD RULE (as in tools.rs): every test key is a placeholder string
//! created by the test itself; no real key material is ever touched.

use serde_json::json;
use temur::tools::{
    ApprovalAnswer, ApprovalRequest, Registry, ToolCtx, ToolError, APPROVAL_DENIED,
    SANDBOX_REFUSAL,
};

/// Set the probe-fail seam before anything in this process can cache the
/// real probe. Called first by every test; the `Once` makes the write
/// race-free while the other tests are still blocked on it.
fn force_probe_fail() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| std::env::set_var("TEMUR_TEST_SANDBOX_UNAVAILABLE", "1"));
    assert!(
        !temur::tools::sandbox_available(),
        "the probe seam must force sandbox-unavailable in this suite"
    );
}

/// A tempdir with one placeholder key file and a ToolCtx guarding it
/// (the tools.rs `guarded_ctx` shape).
fn guarded_ctx() -> (tempfile::TempDir, std::path::PathBuf, ToolCtx) {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("api.key");
    std::fs::write(&key, "placeholder-not-a-real-key\n").unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    ctx.guard = temur::tools::KeyGuard::from_paths(vec![key.clone()]);
    (dir, key, ctx)
}

fn bash(reg: &Registry, ctx: &mut ToolCtx, command: &str) -> Result<String, String> {
    reg.execute("bash", json!({"command": command}), ctx)
        .map(|o| o.output)
        .map_err(|e| e.to_string())
}

#[test]
fn no_approver_refuses_with_the_amended_wording() {
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    let err = bash(&Registry::standard(), &mut ctx, "echo hi").unwrap_err();
    assert_eq!(err, SANDBOX_REFUSAL);
}

#[test]
fn approver_gets_the_exact_command_and_a_no_denies_with_the_constant() {
    force_probe_fail();
    let (dir, _key, mut ctx) = guarded_ctx();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let record = std::rc::Rc::clone(&seen);
    ctx.approver = Some(Box::new(move |req: &ApprovalRequest| {
        record.borrow_mut().push(req.summary.clone());
        // T46: a guarded command composes both questions into this one.
        assert!(req.no_key_sandbox, "guarded bash must compose the T21 question");
        ApprovalAnswer::Deny
    }));
    let err = bash(&Registry::standard(), &mut ctx, "echo denied > d.txt").unwrap_err();
    assert_eq!(err, APPROVAL_DENIED);
    assert_eq!(seen.borrow().as_slice(), ["echo denied > d.txt"]);
    assert!(
        !dir.path().join("d.txt").exists(),
        "a denied command must not have run"
    );
    // The denial is a normal Failed tool error (is_error tool_result on the
    // wire), so the turn continues; nothing panicked to get here.
    let err = Registry::standard()
        .execute("bash", json!({"command": "true"}), &mut ctx)
        .unwrap_err();
    assert!(matches!(err, ToolError::Failed(_)));
}

#[test]
fn approved_command_runs_plain_this_once() {
    force_probe_fail();
    let (dir, key, mut ctx) = guarded_ctx();
    ctx.approver = Some(Box::new(|_: &ApprovalRequest| ApprovalAnswer::AllowOnce));
    let out = bash(
        &Registry::standard(),
        &mut ctx,
        &format!("cat {} && echo ran > approved.txt && echo ran", key.display()),
    )
    .unwrap();
    // Plain spawn by definition: no sandbox masks the (placeholder) key.
    assert!(out.contains("placeholder-not-a-real-key"), "{out}");
    assert!(out.contains("ran"), "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("approved.txt")).unwrap(),
        "ran\n"
    );
}

#[test]
fn every_command_is_asked_separately_nothing_is_cached() {
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    let calls = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let n = std::rc::Rc::clone(&calls);
    ctx.approver = Some(Box::new(move |_: &ApprovalRequest| {
        n.set(n.get() + 1);
        ApprovalAnswer::AllowOnce
    }));
    let reg = Registry::standard();
    bash(&reg, &mut ctx, "true").unwrap();
    bash(&reg, &mut ctx, "true").unwrap();
    assert_eq!(calls.get(), 2, "an approval must never carry over");
}

#[test]
fn already_set_cancel_token_denies_without_prompting() {
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    ctx.approver = Some(Box::new(|_: &ApprovalRequest| {
        panic!("an interrupted turn must never open an approval prompt")
    }));
    ctx.cancel.set();
    let err = bash(&Registry::standard(), &mut ctx, "echo hi").unwrap_err();
    assert_eq!(err, APPROVAL_DENIED);
}

#[test]
fn keyless_ctx_never_asks_the_sandbox_question_but_t46_still_asks_its_own() {
    // T21's invariant, kept: a keyless config has no key-isolation question,
    // so `no_key_sandbox` is false and the T21 wording never appears.
    // T46's addition, pinned beside it: bash still mutates, so the mutation
    // question is asked, and it is the ONE prompt this command draws.
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    let asks = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let n = std::rc::Rc::clone(&asks);
    ctx.approver = Some(Box::new(move |req: &ApprovalRequest| {
        assert!(!req.no_key_sandbox, "keyless has no sandbox question to compose");
        assert_eq!(req.tool, "bash");
        n.set(n.get() + 1);
        ApprovalAnswer::AllowOnce
    }));
    let out = bash(&Registry::standard(), &mut ctx, "echo keyless-ran").unwrap();
    assert!(out.contains("keyless-ran"), "{out}");
    assert_eq!(asks.get(), 1, "exactly one prompt for one command");
}

#[test]
fn override_silences_the_sandbox_ask_but_not_the_mutation_ask() {
    // T21: allow_bash_without_key_sandbox takes the Plain arm, so the
    // key-isolation question is gone. T46: it was never an answer to the
    // mutation question, and does not silence that one.
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    ctx.allow_unsandboxed_bash = true;
    ctx.approver = Some(Box::new(|req: &ApprovalRequest| {
        assert!(
            !req.no_key_sandbox,
            "the override must silence the sandbox question, not route through it"
        );
        ApprovalAnswer::AllowOnce
    }));
    let out = bash(&Registry::standard(), &mut ctx, "echo override-ran").unwrap();
    assert!(out.contains("override-ran"), "{out}");
}

// ------------------------------------------------ T46: the mutation approver

#[test]
fn write_and_edit_ask_at_the_registry_and_a_deny_stops_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let record = std::rc::Rc::clone(&seen);
    ctx.approver = Some(Box::new(move |req: &ApprovalRequest| {
        record.borrow_mut().push(format!("{}|{}", req.tool, req.summary));
        assert!(!req.no_key_sandbox, "only bash composes the sandbox question");
        ApprovalAnswer::Deny
    }));
    let reg = Registry::standard();
    let target = dir.path().join("denied.txt");
    let err = reg
        .execute(
            "write",
            json!({"filePath": target.to_str().unwrap(), "content": "nope"}),
            &mut ctx,
        )
        .unwrap_err();
    let msg = match err {
        ToolError::Failed(m) | ToolError::InvalidInput(m) => m,
    };
    assert!(msg.contains("the user declined this write call"), "{msg}");
    assert!(msg.contains("do not retry it unchanged"), "{msg}");
    assert!(!target.exists(), "a denied write must not touch the disk");
    let calls = seen.borrow().clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].starts_with("write|write "), "{calls:?}");
    assert!(calls[0].contains("(4 bytes)"), "summary names the size: {calls:?}");
}

#[test]
fn read_only_tools_never_reach_the_approver() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    ctx.approver = Some(Box::new(|req: &ApprovalRequest| {
        panic!("read-only tool asked for approval: {}", req.tool)
    }));
    let reg = Registry::standard();
    reg.execute(
        "read",
        json!({"filePath": dir.path().join("f.txt").to_str().unwrap()}),
        &mut ctx,
    )
    .unwrap();
    reg.execute("glob", json!({"pattern": "*.txt", "path": dir.path().to_str().unwrap()}), &mut ctx)
        .unwrap();
    reg.execute("todoread", json!({}), &mut ctx).unwrap();
}

#[test]
fn a_session_allow_is_per_tool_and_does_not_leak() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let record = std::rc::Rc::clone(&asked);
    ctx.approver = Some(Box::new(move |req: &ApprovalRequest| {
        record.borrow_mut().push(req.tool.clone());
        ApprovalAnswer::AllowSession
    }));
    let reg = Registry::standard();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    reg.execute("write", json!({"filePath": a.to_str().unwrap(), "content": "1"}), &mut ctx)
        .unwrap();
    // Second write: the session allow answers it, so no second ask.
    reg.execute("write", json!({"filePath": b.to_str().unwrap(), "content": "2"}), &mut ctx)
        .unwrap();
    // bash is a DIFFERENT tool, so it asks on its own account.
    bash(&reg, &mut ctx, "echo session-scope").unwrap();
    assert_eq!(
        asked.borrow().as_slice(),
        ["write", "bash"],
        "one ask per tool, and allowing write never allowed bash"
    );
}

#[test]
fn a_bash_session_allow_does_not_answer_the_sandbox_question() {
    // The corollary of the composition rule: the two questions are
    // different questions, and only one of them has been answered.
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    ctx.session_allows.insert("bash".to_string());
    let asked = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let n = std::rc::Rc::clone(&asked);
    ctx.approver = Some(Box::new(move |req: &ApprovalRequest| {
        assert!(req.no_key_sandbox, "the sandbox question is still live");
        n.set(n.get() + 1);
        ApprovalAnswer::AllowOnce
    }));
    bash(&Registry::standard(), &mut ctx, "echo still-asks").unwrap();
    assert_eq!(asked.get(), 1, "a held bash session allow must not skip it");
}

#[test]
fn danger_classes_are_display_only_and_match_the_recorded_list() {
    use temur::tools::danger_class;
    assert_eq!(danger_class("rm -rf /tmp/x"), Some("recursive delete"));
    assert_eq!(danger_class("sudo rm -r ./build"), Some("recursive delete"));
    assert_eq!(danger_class("mkfs.ext4 /dev/sdb1"), Some("filesystem format"));
    assert_eq!(danger_class("dd if=/dev/zero of=/dev/sda"), Some("raw write to a device"));
    assert_eq!(danger_class("git reset --hard HEAD~1"), Some("discards uncommitted work"));
    assert_eq!(danger_class("git clean -fd"), Some("discards uncommitted work"));
    assert_eq!(danger_class("shred -u secret"), Some("shred"));
    // Misses are cosmetic BY DESIGN: the base rule already asked.
    assert_eq!(danger_class("rm one-file.txt"), None);
    assert_eq!(danger_class("echo hello"), None);
}

#[test]
fn the_permissive_default_is_untouched_and_ask_is_the_config_default() {
    // The design's load-bearing choice, pinned from both sides.
    //
    // ToolCtx built outside a session is approver-free and therefore
    // PERMISSIVE, byte-identical to pre-T46: this is what every
    // MockProvider loop test constructs, and moving the flip in here would
    // break them all for no safety gain.
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    assert!(ctx.approver.is_none(), "the default must stay permissive");
    let reg = Registry::standard();
    let f = dir.path().join("permissive.txt");
    reg.execute(
        "write",
        json!({"filePath": f.to_str().unwrap(), "content": "ok"}),
        &mut ctx,
    )
    .expect("an approver-free ctx must not ask and must not deny");
    assert!(f.exists());

    // And the CONFIG default is ask, so an interactive session that says
    // nothing about approvals gets the safe posture. The interactive
    // construction itself is pinned end-to-end by the pty tests in
    // tests/cli.rs, which drive the real binary with no config at all.
    let cfg = temur::config::Config::default();
    assert!(cfg.approve_mutations_ask(), "ask is the default");
    assert!(cfg.validate_approve_mutations().is_ok());

    let mut allow = temur::config::Config::default();
    allow.approve_mutations = Some("allow".to_string());
    assert!(!allow.approve_mutations_ask(), "allow restores pre-T46 behavior");
    assert!(allow.validate_approve_mutations().is_ok());

    let mut bad = temur::config::Config::default();
    bad.approve_mutations = Some("sometimes".to_string());
    let err = bad.validate_approve_mutations().unwrap_err();
    assert!(err.contains("sometimes"), "{err}");
    assert!(err.contains("\"ask\" or \"allow\""), "{err}");
    // Fails SAFE: an unvalidated bad value still reads as ask.
    assert!(bad.approve_mutations_ask());
}

// ---------------------------------------------- T46 P2: the -p refusal

/// The refusal a run with nobody to ask gives instead of asking. Reads the
/// wording from the crate so this file and the message cannot drift.
fn refusal(tool: &str) -> String {
    temur::tools::mutation_refusal_text(tool)
}

#[test]
fn refuse_mutations_fails_loud_and_names_the_way_out_for_every_mutating_tool() {
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nope.txt");
    let existing = dir.path().join("edit-me.txt");
    std::fs::write(&existing, "before\n").unwrap();
    let reg = Registry::standard();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    ctx.refuse_mutations = true;
    // Not a placeholder for an ask: NO approver is installed, which before
    // T46 meant permissive. The refusal is the whole point.
    assert!(ctx.approver.is_none());

    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("bash", json!({"command": "echo refused > r.txt"})),
        (
            "write",
            json!({"filePath": target.to_str().unwrap(), "content": "nope"}),
        ),
        (
            "edit",
            json!({
                "filePath": existing.to_str().unwrap(),
                "oldString": "before",
                "newString": "after"
            }),
        ),
    ];
    for (tool, input) in cases {
        let err = reg.execute(tool, input, &mut ctx).unwrap_err();
        let msg = match err {
            ToolError::Failed(m) | ToolError::InvalidInput(m) => m,
        };
        assert_eq!(msg, refusal(tool), "{tool} must give the one refusal");
        // The three facts the message exists to carry.
        assert!(msg.contains("--allow-mutations"), "{tool}: {msg}");
        assert!(msg.contains("\"approve_mutations\": \"allow\""), "{tool}: {msg}");
        assert!(msg.contains("cannot ask"), "{tool}: {msg}");
    }
    assert!(!target.exists(), "a refused write must not touch the disk");
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "before\n");
    assert!(!dir.path().join("r.txt").exists(), "a refused bash must not run");
}

#[test]
fn a_refusing_run_leaves_read_only_tools_untouched() {
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    ctx.refuse_mutations = true;
    let reg = Registry::standard();
    reg.execute(
        "read",
        json!({"filePath": dir.path().join("f.txt").to_str().unwrap()}),
        &mut ctx,
    )
    .unwrap();
    reg.execute(
        "glob",
        json!({"pattern": "*.txt", "path": dir.path().to_str().unwrap()}),
        &mut ctx,
    )
    .unwrap();
    reg.execute("todoread", json!({}), &mut ctx).unwrap();
}

#[test]
fn a_session_allow_cannot_survive_into_a_refusing_run() {
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    // Both of the things that mean "proceed" on the asking path, set at once:
    // an allow already held, and no approver to consult. Neither may soften
    // the refusal, which is why the check comes first.
    ctx.session_allows.insert("bash".to_string());
    ctx.session_allows.insert("write".to_string());
    ctx.refuse_mutations = true;
    let err = bash(&Registry::standard(), &mut ctx, "echo hi").unwrap_err();
    assert_eq!(err, refusal("bash"));
}

#[test]
fn t21_keeps_precedence_over_the_t46_refusal_where_both_could_speak() {
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    ctx.refuse_mutations = true;
    // Keys guarded, no sandbox, nobody to ask: T21 owns this and says so.
    // The T46 refusal is reached only where bash would otherwise have RUN,
    // and --allow-mutations is not a way around the key sandbox.
    let err = bash(&Registry::standard(), &mut ctx, "echo hi").unwrap_err();
    assert_eq!(err, SANDBOX_REFUSAL);
}

#[test]
fn the_refusal_is_a_normal_tool_error_so_the_turn_continues() {
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    ctx.refuse_mutations = true;
    let err = Registry::standard()
        .execute("bash", json!({"command": "true"}), &mut ctx)
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Failed(_)),
        "loud, but an is_error tool_result and not a crash: {err:?}"
    );
}
