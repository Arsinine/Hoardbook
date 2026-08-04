//! M21 W1 regression test for the process-level rustls CryptoProvider install.
//!
//! See `planning/M21_PROMPT.md` W1 and `CLAUDE.md` §5 (network/crypto changes need regression
//! AND integration testing — this file is the regression half; the orchestrator runs the live
//! `hb-it` L2 integration suite separately, once W1+W2 are both in tree).
//!
//! ## What these tests DO and DO NOT prove
//!
//! - DO prove: after the startup idiom runs, `CryptoProvider::get_default()` is `Some` — i.e.
//!   a process-level default provider is installed. This is a necessary precondition for "no
//!   provider-less TLS panic".
//! - DO prove: a second `install_default()` returns `Err` and does NOT panic. This pins the
//!   exact contract that makes `let _ = ...install_default()` safe to call unconditionally at
//!   startup (the production app, the hb-wan-it harness, and any embedded user of this crate all
//!   rely on re-init being a harmless no-op).
//!
//! ## What these tests DO NOT prove (do not overclaim)
//!
//! - DO NOT prove: that the production app calls `install_default()` *before* the first
//!   TLS-handshake. That ordering lives in the Tauri `setup` closure in `src/lib.rs`, which
//!   cannot be exercised in-process (it pulls in the full Tauri runtime, the window manager, and
//!   an app-data dir). The launch-timing bug from the v0.12.11 log — provider-less TLS panics
//!   during the cold-launch relay flap — is therefore NOT caught by this file. It is caught by
//!   the live L2 integration suite and by code review of the placement in `lib.rs`.
//! - DO NOT prove: that `ring` (rather than `aws-lc-rs`) is the installed provider. rustls 0.23
//!   exposes no public name/identifier on `CryptoProvider` — both providers compile to the same
//!   shape (Vec of trait objects) and offer identical cipher-suite variant names. The choice of
//!   `ring` is a SOURCE-level property of the call site
//!   (`rustls::crypto::ring::default_provider()`) and is verified by reading `lib.rs`, not by
//!   interrogating the installed provider at runtime.
//! - WOULD NOT have caught the original bug in isolation: the launch storm was a *race* — the
//!   ambient process here is a single-threaded test binary; calling the idiom from one test
//!   cannot reproduce the concurrent first-connects that lost the race in the v0.12.11 log. The
//!   regression this file pins is the *contract* the fix relies on (install works, re-install is
//!   safe), not the race itself.

#![allow(clippy::unwrap_used)]

use rustls::crypto::CryptoProvider;

/// The startup idiom under test, kept in one place so the assertion and the production call
/// site in `src/lib.rs` cannot drift apart in wording.
fn install_ring_provider_like_startup() {
    // Identical to lib.rs and wan_it/mod.rs: `let _ =` because an already-installed provider
    // returns Err, which is not a failure for us.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Mirrors the precondition the lib.rs setup closure depends on: a default provider exists after
/// the idiom runs. If `get_default()` were `None` after this call, every provider-less TLS use
/// downstream would panic — exactly the v0.12.11 launch storm.
#[test]
fn default_provider_is_installed_after_startup_idiom() {
    install_ring_provider_like_startup();
    let provider = CryptoProvider::get_default();
    assert!(
        provider.is_some(),
        "CryptoProvider::get_default() must be Some after the startup install idiom ran"
    );
}

/// `install_default` is documented to succeed at most once per process and return `Err` on a
/// second call. The lib.rs call site discards the return value with `let _ =` precisely because
/// a re-init must be a harmless no-op — the test harness, the wan-it binary, and any embedded
/// user of this crate will hit this path when they run in the same process as the app. Pin that
/// contract: a second call returns Err and does not panic.
#[test]
fn second_install_default_is_err_and_does_not_panic() {
    install_ring_provider_like_startup(); // first call — Ok, or no-op if already installed
    let second = rustls::crypto::ring::default_provider().install_default();
    assert!(
        second.is_err(),
        "a second install_default() must return Err (provider already installed), not Ok"
    );
    // Reaching this line means it did not panic — that is the load-bearing part of the contract.
}
