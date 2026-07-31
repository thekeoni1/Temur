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
use temur::tools::{Registry, ToolCtx, ToolError, APPROVAL_DENIED, SANDBOX_REFUSAL};

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
    ctx.bash_approver = Some(Box::new(move |cmd: &str| {
        record.borrow_mut().push(cmd.to_string());
        false
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
    ctx.bash_approver = Some(Box::new(|_| true));
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
    ctx.bash_approver = Some(Box::new(move |_| {
        n.set(n.get() + 1);
        true
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
    ctx.bash_approver = Some(Box::new(|_| {
        panic!("an interrupted turn must never open an approval prompt")
    }));
    ctx.cancel.set();
    let err = bash(&Registry::standard(), &mut ctx, "echo hi").unwrap_err();
    assert_eq!(err, APPROVAL_DENIED);
}

#[test]
fn keyless_ctx_never_asks_even_with_an_approver_installed() {
    force_probe_fail();
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ToolCtx::new(dir.path().to_path_buf());
    ctx.bash_approver = Some(Box::new(|_| {
        panic!("keyless configs must never consult the approver")
    }));
    let out = bash(&Registry::standard(), &mut ctx, "echo keyless-ran").unwrap();
    assert!(out.contains("keyless-ran"), "{out}");
}

#[test]
fn override_silences_the_ask_entirely() {
    force_probe_fail();
    let (_dir, _key, mut ctx) = guarded_ctx();
    ctx.allow_unsandboxed_bash = true;
    ctx.bash_approver = Some(Box::new(|_| {
        panic!("the override must silence the ask, not route through it")
    }));
    let out = bash(&Registry::standard(), &mut ctx, "echo override-ran").unwrap();
    assert!(out.contains("override-ran"), "{out}");
}
