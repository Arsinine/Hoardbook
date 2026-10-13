//! QURATOR-352 — appearance commands: the see-through window (Mica) switch and its support probe.
//!
//! Mica exists only on Windows 11 and up (build >= 22000). Every other OS — Windows 10, Linux,
//! macOS — stays opaque: the switch no-ops there and `window_material_supported` reports `false`,
//! so the UI can offer the toggle freely and the app fails closed. The window itself stays
//! NON-transparent in `tauri.conf.json`; transparency is switched on at runtime, only here, and
//! only after the build probe passes — a transparent window without Mica (Linux, Windows 10)
//! would show the raw desktop unblurred.

use tauri::window::{Color, Effect, EffectsBuilder};

use crate::{
    error::{cmd_err, CmdResult},
    store::Settings,
};

/// `--bg` (#0e0f11) as an opaque RGBA — the background restored when the see-through window is
/// switched OFF, so toggling it off lands on the plain opaque app background.
const OPAQUE_BG: Color = Color(0x0e, 0x0f, 0x11, 0xff);

/// True iff the see-through window material (Mica) is supported on this OS/build: Windows 11+
/// (build >= 22000), via `hb_dpapi::windows_build` (the one crate allowed to hold `unsafe`).
/// `false` on Windows 10, Linux, macOS — and on Windows itself if the build read ever fails.
fn material_supported() -> bool {
    // `hb-dpapi` is a Windows-only dependency of hb-app (Cargo.toml target table), so the probe
    // is cfg-gated here rather than relying on its non-Windows `None` stub.
    #[cfg(target_os = "windows")]
    {
        hb_dpapi::windows_build().is_some_and(|build| build >= 22000)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// The ONE rule for whether the see-through window is on: the user asked for it and glass is on.
/// Every theme is a glass theme (owner, 2026-10-13: hoard joined the shared glass design), so the
/// theme no longer matters. Startup and the command both go through it, so the window can never
/// be see-through with glass off.
pub(crate) fn see_through_effective(see_through: bool, glass: bool) -> bool {
    see_through && glass
}

/// The one body both the command and the startup hook call.
///
/// On any OS without Mica this is a silent no-op (`Ok(())`): there is nothing to apply and
/// nothing to undo. On Windows 11, `on` applies the window-level Mica material, then makes the
/// webview layer transparent (alpha 0 — wry passes an alpha of exactly 0 through on Windows 8+,
/// forcing ANY other alpha to 255, see `wry-0.55.1/src/webview2/mod.rs` `set_background_color`)
/// so Mica shows through; `!on` clears the effects and restores the opaque background.
fn apply_window_material(window: &tauri::WebviewWindow, on: bool) -> Result<(), String> {
    if !material_supported() {
        return Ok(());
    }
    if on {
        // Mica FIRST, then transparency: if a background call fails, Mica sits behind an opaque
        // layer (harmless); the other order could leave a see-through window with no material.
        window
            .set_effects(EffectsBuilder::new().effect(Effect::Mica).build())
            .map_err(cmd_err)?;
        // ⚠ The two layers are set SEPARATELY because no single value works for both (found
        // 2026-10-13, owner: "it does absolutely nothing"):
        //  - native window (tao): `None` = paint no brush, so the DWM backdrop shows. `Some(c)`
        //    paints a SOLID brush with the alpha IGNORED — `Color(0,0,0,0)` painted opaque black
        //    over Mica, which is why v0.23.1 showed nothing.
        //  - webview (wry): alpha 0 is passed through as transparent on Windows 8+; `None` is
        //    turned into opaque WHITE by tauri-runtime-wry.
        // The window must also have been CREATED transparent (tauri.windows.conf.json): tao only
        // enables DWM blur-behind at creation, and nothing re-runs it later.
        let webview: &tauri::Webview = window.as_ref();
        webview.window().set_background_color(None).map_err(cmd_err)?;
        webview.set_background_color(Some(Color(0, 0, 0, 0))).map_err(cmd_err)?;
    } else {
        // Clear the material first, then paint both layers opaque again.
        window.set_effects(None).map_err(cmd_err)?;
        paint_opaque(window)?;
    }
    Ok(())
}

/// Paint both the native window and the webview with the opaque app background. On Windows the
/// main window is CREATED transparent (tauri.windows.conf.json) so Mica can show through later;
/// until see-through is actually on, this keeps it looking exactly like an opaque window.
fn paint_opaque(window: &tauri::WebviewWindow) -> Result<(), String> {
    window.set_background_color(Some(OPAQUE_BG)).map_err(cmd_err)
}

/// Is the see-through window material (Mica) supported here? Windows 11+ only — `false` on
/// Windows 10, Linux and macOS, so the Settings toggle can be hidden or disabled off-Win11.
#[tauri::command]
pub fn window_material_supported() -> bool {
    material_supported()
}

/// Bring the see-through window (Mica, Windows 11 only) in line with the appearance the UI just
/// applied. The UI sends the settings, not an on/off flag, so the decision is made here by
/// `see_through_effective` — the same rule startup uses. The UI calls this whenever the glass or
/// see-through setting changes, so turning glass off also turns Mica off. A silent no-op on every
/// OS without Mica.
#[tauri::command]
pub fn set_window_material(window: tauri::WebviewWindow, see_through: bool, glass: bool) -> CmdResult<()> {
    apply_window_material(&window, see_through_effective(see_through, glass))
}

/// The startup half: re-apply the persisted see-through choice BEFORE the user sees the window.
/// Applies Mica only where it is supported and `see_through && glass` (see `see_through_effective`);
/// in every other case it paints the window opaque, because on Windows the window is created
/// transparent. Errors are swallowed: a failed Mica application must never block startup.
pub(crate) fn startup_see_through(settings: &Settings, window: &tauri::WebviewWindow) {
    if material_supported() && see_through_effective(settings.see_through, settings.glass) {
        let _ = apply_window_material(window, true);
    } else {
        // Everywhere else (Windows 10, see-through off, glass off) the transparent-created window
        // must not show the desktop: paint it opaque before the user sees it. On Linux and macOS
        // the window is created opaque, so this is a harmless repaint with the same colour.
        let _ = paint_opaque(window);
    }
}

#[cfg(test)]
mod tests {
    use super::see_through_effective;

    #[test]
    fn see_through_is_on_only_when_asked_for_with_glass_on() {
        // MUTATION (P-10): in `see_through_effective`, delete `&& glass` — the glass-off row reds.
        // Replace the body with `glass` — the not-asked-for row reds.
        assert!(see_through_effective(true, true), "asked for, glass on");
        assert!(!see_through_effective(true, false), "never with glass off");
        assert!(!see_through_effective(false, true), "never unless asked for");
    }

    /// The Windows overlay must be the base main-window entry plus `transparent: true` and
    /// NOTHING else. Tauri merges platform configs with JSON Merge Patch, which replaces arrays
    /// whole, so the overlay carries a full copy of the window entry; this pins the copy to the
    /// base so a change to one (size, title, drag-drop) cannot silently miss the other.
    /// MUTATION (P-10): change `"minWidth": 800` to 900 in tauri.windows.conf.json — this reds.
    /// Delete the `"transparent": true` line there — this reds too.
    #[test]
    fn windows_overlay_is_the_base_window_plus_transparent() {
        let base: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).expect("base config");
        let win: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.windows.conf.json")).expect("windows config");
        let mut expected = base["app"]["windows"][0].clone();
        expected["transparent"] = serde_json::Value::Bool(true);
        assert_eq!(
            win["app"]["windows"][0], expected,
            "tauri.windows.conf.json's window must equal tauri.conf.json's plus transparent: true"
        );
        assert_eq!(win["app"]["windows"].as_array().map(Vec::len), Some(1), "exactly one window");
        assert!(
            base["app"]["windows"][0].get("transparent").is_none(),
            "the BASE window must stay opaque: Linux and macOS must never be created transparent"
        );
    }
}
