//! QURATOR-340 — the taskbar attention trigger (badge + flash) for a chat message that lands while
//! the window is not focused.
//!
//! **Owner ruling 2026-10-14 (verbatim, binding):**
//! > Fire ONLY while the window is not focused (unfocused, minimized, or hidden to tray). A message
//! > that lands while the window is focused does nothing at the OS level. Flash cadence is yours:
//! > recommended ONE `request_user_attention(Informational)` per new message while unfocused, never
//! > `Critical`. The persistent red-dot badge shows while unfocused/minimized/hidden and clears when
//! > the window gains focus. Windows-first: on other platforms the overlay is a no-op
//! > (request_user_attention is already cross-platform in Tauri).
//!
//! Split of duties: the UI fires ONE `set_unread_attention` per `navChatCount` change
//! (`+layout.svelte`); the FOCUS DECISION lives here, in Rust, so no JS window API and no new
//! capability permission is needed (the capabilities file is deliberately tight — QURATOR-352
//! removed two). The badge ships as RAW RGBA (`badge-unread.rgba`) embedded with `include_bytes!`
//! because in tauri 2.11.5 `Image::from_bytes`/`from_path` are
//! `#[cfg(any(feature = "image-ico", feature = "image-png"))]` and hb-app enables neither
//! (`tauri = { features = ["tray-icon"] }`), so a PNG load would fail to compile. The sibling
//! `badge-unread.png` in `crates/hb-app/icons/` is the same dot as a viewable PNG; regenerate the
//! `.rgba` from it with `convert badge-unread.png -depth 8 rgba:badge-unread.rgba`.
//!
//! Every window-API error is logged and swallowed — a stale window handle must never fail the UI
//! (same discipline as `reveal_main_window`).

use tauri::{AppHandle, Manager, UserAttentionType};
#[cfg(target_os = "windows")]
use tauri::image::Image;

use crate::error::CmdResult;

/// The 32x32 raw RGBA badge (4 bytes/pixel, row-major, top-to-bottom) — a crimson circle on
/// transparency. Generated from `badge-unread.png` (ImageMagick convert); see the module docs.
#[cfg(target_os = "windows")]
const BADGE_UNREAD_RGBA: &[u8] = include_bytes!("../../icons/badge-unread.rgba");

/// The decision the command executes, extracted pure so the focus gate is unit-pinnable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionPlan {
    /// Paint the persistent red-dot overlay icon.
    pub overlay: bool,
    /// Fire ONE `request_user_attention(Informational)` flash.
    pub flash: bool,
}

/// `attention_plan(focused, unread, new_arrival)`:
///
/// | focused | unread | new_arrival | overlay | flash |
/// |---------|--------|-------------|---------|-------|
/// | true    | any    | any         | no      | no    |
/// | false   | >0     | true        | yes     | yes   |
/// | false   | >0     | false       | yes     | no    |
/// | false   | 0      | any         | no      | no    |
fn attention_plan(focused: bool, unread: u32, new_arrival: bool) -> AttentionPlan {
    // The `!focused` term is the OWNER RULING's gate: a message that lands while the window is
    // focused does NOTHING at the OS level.
    let overlay = !focused && unread > 0;
    // ONE flash per NEW message only (cadence is ours, `Informational` never `Critical`).
    let flash = overlay && new_arrival;
    AttentionPlan { overlay, flash }
}

#[tauri::command]
pub fn set_unread_attention(app: AppHandle, unread: u32, new_arrival: bool) -> CmdResult<()> {
    let Some(window) = app.get_webview_window("main") else {
        tracing::debug!("set_unread_attention: no main window");
        return Ok(());
    };
    // On an `is_focused` error the window is treated as FOCUSED: no OS attention. Fail quiet,
    // fail dark.
    let focused = window.is_focused().unwrap_or(true);
    let plan = attention_plan(focused, unread, new_arrival);
    set_overlay(&window, plan.overlay);
    if plan.flash {
        if let Err(e) = window.request_user_attention(Some(UserAttentionType::Informational)) {
            tracing::debug!("set_unread_attention: request_user_attention: {e}");
        }
    }
    Ok(())
}

/// Paint or clear the taskbar overlay badge. `set_overlay_icon` is `#[cfg(target_os = "windows")]`
/// in tauri 2.11.5 — it does not EXIST on other targets (not a no-op: a compile error), so the
/// call is gated here and the badge bytes are only referenced on Windows.
#[cfg(target_os = "windows")]
pub(crate) fn set_overlay<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>, on: bool) {
    let icon = on.then(|| Image::new_owned(BADGE_UNREAD_RGBA.to_vec(), 32, 32));
    if let Err(e) = window.set_overlay_icon(icon) {
        tracing::debug!("set_unread_attention: overlay icon: {e}");
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_overlay<R: tauri::Runtime>(_window: &tauri::WebviewWindow<R>, _on: bool) {}

/// The focus-clears-the-badge arm (`WindowEvent::Focused(true)` in lib.rs) gets a plain `Window`,
/// not a `WebviewWindow`; same gate.
#[cfg(target_os = "windows")]
pub(crate) fn clear_overlay_on_window<R: tauri::Runtime>(window: &tauri::Window<R>) {
    if let Err(e) = window.set_overlay_icon(None) {
        tracing::debug!("focus: clear overlay icon: {e}");
    }
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn clear_overlay_on_window<R: tauri::Runtime>(_window: &tauri::Window<R>) {}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test names the ONE-LINE production edit that must red it (CLAUDE.md §9 / P-10 — a green
    // test proves nothing until seen red; the orchestrator applies the mutation, confirms the named
    // test reds, and reverts).

    /// MUTATION T1: in `attention_plan`, change `let overlay = !focused && unread > 0;` to
    /// `let overlay = unread > 0;` (drop `!focused`) — THIS test reds: focused + unread=5 must be
    /// {overlay: false, flash: false}; the mutant paints the badge on a focused window. T2-T4 stay
    /// green.
    #[test]
    fn focused_window_never_os_attentions() {
        let plan = attention_plan(true, 5, true);
        assert!(!(plan.overlay));
        assert!(!(plan.flash));
        assert!(!(attention_plan(true, 5, false).overlay));
        assert!(!(attention_plan(true, 0, true).overlay));
        assert!(!(attention_plan(true, 0, false).overlay));
    }

    /// MUTATION T2: in `attention_plan`, change `let overlay = !focused && unread > 0;` to
    /// `let overlay = !focused && unread > 1;` — THIS test reds: unfocused + unread=1 + new must be
    /// {overlay: true, flash: true}; the mutant leaves the very first message unbadged. T3 also
    /// reds (same mutation, same expectation shape).
    #[test]
    fn unfocused_new_unread_overlays_and_flashes() {
        let plan = attention_plan(false, 1, true);
        assert!(plan.overlay);
        assert!(plan.flash);
        assert!(attention_plan(false, 9, true).overlay);
    }

    /// MUTATION T3: in `attention_plan`, change `let flash = overlay && new_arrival;` to
    /// `let flash = overlay;` (drop `new_arrival`) — THIS test reds: unfocused + unread=1 + NOT-new
    /// must flash=false; the mutant re-flashes for a stale count. T2 also reds.
    #[test]
    fn unfocused_stale_unread_overlays_without_flash() {
        let plan = attention_plan(false, 1, false);
        assert!(plan.overlay);
        assert!(!(plan.flash));
        assert!(attention_plan(false, 9, false).overlay);
    }

    /// MUTATION T4: in `attention_plan`, change `let overlay = !focused && unread > 0;` to
    /// `let overlay = !focused;` (drop the unread term) — THIS test reds: unfocused + unread=0 must
    /// be neither; the mutant keeps the badge up with nothing left to read. T1-T3 keep their
    /// expectations, so only this test reds.
    #[test]
    fn unfocused_zero_unread_neither() {
        let plan = attention_plan(false, 0, true);
        assert!(!(plan.overlay));
        assert!(!(plan.flash));
        assert!(!(attention_plan(false, 0, false).flash));
    }
}
