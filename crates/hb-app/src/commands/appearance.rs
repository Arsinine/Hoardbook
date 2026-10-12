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

/// The ONE rule for whether the see-through window is on: the user asked for it, glass is on,
/// and the theme is a glass theme (not "hoard", the base palette). Startup and the command both
/// go through it, so the window can never be see-through under hoard or with glass off.
pub(crate) fn see_through_effective(see_through: bool, glass: bool, theme: &str) -> bool {
    see_through && glass && theme != "hoard"
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
        // Mica FIRST, then transparency: if the background call fails, Mica sits behind an
        // opaque webview (harmless); the other order could leave a transparent window with no
        // material, showing the raw desktop unblurred.
        window
            .set_effects(EffectsBuilder::new().effect(Effect::Mica).build())
            .map_err(cmd_err)?;
        window
            .set_background_color(Some(Color(0, 0, 0, 0)))
            .map_err(cmd_err)?;
    } else {
        // Clear the material first, then restore the opaque background.
        window.set_effects(None).map_err(cmd_err)?;
        window
            .set_background_color(Some(OPAQUE_BG))
            .map_err(cmd_err)?;
    }
    Ok(())
}

/// Is the see-through window material (Mica) supported here? Windows 11+ only — `false` on
/// Windows 10, Linux and macOS, so the Settings toggle can be hidden or disabled off-Win11.
#[tauri::command]
pub fn window_material_supported() -> bool {
    material_supported()
}

/// Bring the see-through window (Mica, Windows 11 only) in line with the appearance the UI just
/// applied. The UI sends the three settings, not an on/off flag, so the decision is made here by
/// `see_through_effective` — the same rule startup uses. The UI calls this whenever the theme,
/// glass or see-through setting changes, so switching to hoard or turning glass off also turns
/// Mica off. A silent no-op on every OS without Mica.
#[tauri::command]
pub fn set_window_material(
    window: tauri::WebviewWindow,
    see_through: bool,
    glass: bool,
    theme: String,
) -> CmdResult<()> {
    apply_window_material(&window, see_through_effective(see_through, glass, &theme))
}

/// The startup half: re-apply the persisted see-through choice BEFORE the user sees the window.
/// Fires only for `see_through && glass && theme != "hoard"` — the see-through window is a
/// themed-glass extra, never part of the default hoard look (the hoard theme must stay
/// pixel-identical to today). Errors are swallowed: a failed Mica application must never block
/// startup, the user simply gets the opaque window.
pub(crate) fn startup_see_through(settings: &Settings, window: &tauri::WebviewWindow) {
    if see_through_effective(settings.see_through, settings.glass, &settings.theme) {
        let _ = apply_window_material(window, true);
    }
}

#[cfg(test)]
mod tests {
    use super::see_through_effective;

    #[test]
    fn see_through_is_on_only_for_a_glass_theme_with_glass_on() {
        // MUTATION (P-10): in `see_through_effective`, delete `&& theme != "hoard"` — the hoard
        // row reds. Delete `&& glass` — the glass-off row reds.
        assert!(see_through_effective(true, true, "vault"), "a glass theme with glass on");
        assert!(!see_through_effective(true, true, "hoard"), "never under the base palette");
        assert!(!see_through_effective(true, false, "vault"), "never with glass off");
        assert!(!see_through_effective(false, true, "vault"), "never unless asked for");
    }
}
