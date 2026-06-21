//! Single-instance enforcement (M8, Track S). A second launch must focus the **existing** window
//! rather than spawn a duplicate — each duplicate would open its own relay connections and run its
//! own presence-publish loop, double-publishing presence under the same `npub`. Collapsing to one
//! process collapses that to one publisher.
//!
//! The OS-level second-launch is **not** unit-testable in CI (it needs a real windowed session), so
//! the *decision* is extracted here as a pure helper pinned with a fake window — including a
//! `set_focus → Err` arm (F10) proving the call site swallows a stale-handle error — and the
//! end-to-end behaviour is covered by the documented manual OS check in the M8 report.

/// What the single-instance callback decided to do with the second launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusAction {
    /// An existing window was found; best-effort unminimize → show → set_focus (errors absorbed).
    Focus,
    /// No existing window to focus. Logged by the caller, never a panic.
    NoWindow,
}

/// The subset of window operations the focus path needs, abstracted so the decision is testable with
/// a fake. The real implementation is for `tauri::WebviewWindow` (desktop only).
pub trait FocusableWindow {
    fn unminimize(&self) -> anyhow::Result<()>;
    fn show(&self) -> anyhow::Result<()>;
    fn set_focus(&self) -> anyhow::Result<()>;
}

/// Bring an existing window to the foreground for a second launch. This is the real panic-guard, not
/// just a decision: `Some(w)` → unminimize/show/set_focus with **every** error swallowed (a window
/// closed-then-queried can hand back a handle whose method calls error — that must never panic the
/// surviving instance) → `Focus`; `None` → `NoWindow`. No `?`, no `unwrap`, no `expect`.
pub fn focus_existing(window: Option<&dyn FocusableWindow>) -> FocusAction {
    match window {
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            FocusAction::Focus
        }
        None => FocusAction::NoWindow,
    }
}

// The real window the plugin callback hands us. Desktop-only: the single-instance plugin (and this
// whole concern) does not exist on mobile.
#[cfg(desktop)]
impl FocusableWindow for tauri::WebviewWindow {
    fn unminimize(&self) -> anyhow::Result<()> {
        Ok(tauri::WebviewWindow::unminimize(self)?)
    }
    fn show(&self) -> anyhow::Result<()> {
        Ok(tauri::WebviewWindow::show(self)?)
    }
    fn set_focus(&self) -> anyhow::Result<()> {
        Ok(tauri::WebviewWindow::set_focus(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeWindow {
        unminimized: Cell<bool>,
        shown: Cell<bool>,
        focused: Cell<bool>,
        set_focus_errors: bool,
    }
    impl FakeWindow {
        fn ok() -> Self {
            Self {
                unminimized: Cell::new(false),
                shown: Cell::new(false),
                focused: Cell::new(false),
                set_focus_errors: false,
            }
        }
        fn focus_fails() -> Self {
            Self { set_focus_errors: true, ..Self::ok() }
        }
    }
    impl FocusableWindow for FakeWindow {
        fn unminimize(&self) -> anyhow::Result<()> {
            self.unminimized.set(true);
            Ok(())
        }
        fn show(&self) -> anyhow::Result<()> {
            self.shown.set(true);
            Ok(())
        }
        fn set_focus(&self) -> anyhow::Result<()> {
            if self.set_focus_errors {
                anyhow::bail!("stale window handle");
            }
            self.focused.set(true);
            Ok(())
        }
    }

    #[test]
    fn some_window_is_unminimized_shown_and_focused() {
        let w = FakeWindow::ok();
        assert_eq!(focus_existing(Some(&w)), FocusAction::Focus);
        assert!(w.unminimized.get(), "a minimized window is restored");
        assert!(w.shown.get(), "a hidden (tray) window is shown");
        assert!(w.focused.get(), "the window is focused");
    }

    #[test]
    fn no_window_yields_nowindow_and_never_panics() {
        assert_eq!(focus_existing(None), FocusAction::NoWindow);
    }

    /// F10: the actual real-world risk is a stale handle whose `set_focus` ERRORS. The absorbing call
    /// must swallow it — the decision still returns `Focus`, and crucially there is no panic.
    #[test]
    fn set_focus_error_is_absorbed_no_panic() {
        let w = FakeWindow::focus_fails();
        assert_eq!(
            focus_existing(Some(&w)),
            FocusAction::Focus,
            "an erroring set_focus is swallowed; the focus decision stands"
        );
        assert!(w.unminimized.get());
        assert!(w.shown.get());
        assert!(!w.focused.get(), "set_focus errored (not recorded), but no panic propagated");
    }

    /// Registration guard: the single-instance plugin must be registered AND registered FIRST — the
    /// v2 docs require it so the second-instance argv is captured before other setup. Parse the
    /// embedded `lib.rs` setup order rather than spinning a real windowed Tauri app.
    #[test]
    fn single_instance_plugin_registered_first() {
        let src = include_str!("lib.rs");
        let si = src
            .find("tauri_plugin_single_instance::init")
            .expect("single-instance plugin must be registered in lib.rs");
        for marker in [
            "tauri_plugin_process::init",
            "tauri_plugin_dialog::init",
            "tauri_plugin_updater::Builder",
        ] {
            if let Some(other) = src.find(marker) {
                assert!(
                    si < other,
                    "single-instance plugin must be registered before {marker} (it must be first)"
                );
            }
        }
    }
}
