//! F4 (v0.1.1): the SIGINT → CancelToken bridge, in-process. This file
//! deliberately holds ONE test: the flag is process-global, and a second
//! SIGINT while it is set calls `_exit(130)` — so exactly one `raise` may
//! ever happen in this binary. The second-press exit path and the
//! full-binary orphan check live in scripts/sigint_test.sh (black box,
//! own process per case).

use temur::provider::CancelToken;

#[test]
fn sigint_bridges_into_every_cancel_token_and_clear_resets_both() {
    temur::signal::install_plain_repl_handler().expect("sigaction");
    let token = CancelToken::new();
    let clone = token.clone();
    assert!(!token.is_set());
    assert!(!temur::signal::interrupted());

    // POSIX raise() returns only after the handler ran on this thread.
    unsafe { libc::raise(libc::SIGINT) };

    assert!(temur::signal::interrupted(), "handler sets the global flag");
    assert!(token.is_set(), "every token ORs the global flag in");
    assert!(clone.is_set(), "clones too");

    // clear() resets BOTH the token and the SIGINT flag (the plain REPL
    // calls it at submission — the F7 invariant).
    token.clear();
    assert!(!temur::signal::interrupted());
    assert!(!token.is_set());
    assert!(!clone.is_set());
}
