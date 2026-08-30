//! The application manifest, and why it is not simply `tauri_build::build()`.
//!
//! `tauri-build` embeds a Windows application manifest declaring a dependency on Common Controls
//! v6, and emits it as `cargo:rustc-link-arg-bins` — so it reaches the app binaries and nothing
//! else. That was sufficient until the QURATOR-161 command-guard tests took a dev-dependency on
//! `tauri`'s `test` feature: `tauri::test::mock_app` drags the Common Controls v6 entry points
//! (`TaskDialogIndirect`) into the `unittests src/lib.rs` harness, which then dies at load with
//! `STATUS_ENTRYPOINT_NOT_FOUND` (0xc0000139) before a single test runs. Those entry points only
//! resolve when the loading image carries a manifest naming the v6 side-by-side assembly, and a
//! test binary is not a bin. Upstream: tauri-apps/tauri#13419, open.
//!
//! So the manifest moves off `-bins` and onto plain `cargo:rustc-link-arg`, via embed-resource's
//! `compile_for_everything`, which reaches every artifact including that harness. Measured
//! 2026-08-30 on this crate, by emitting a deliberately invalid flag through each channel and
//! reading which link failed: `rustc-link-arg-tests` reaches `tests/*.rs` integration targets only
//! (0 hits on `--lib`, 2 on `--test rustls_provider_install`), while plain `rustc-link-arg` does
//! reach the lib unittest (2 hits). The `-tests` channel — the obvious candidate, and what
//! embed-resource's own docs hedge as "select types only" — cannot fix this.
//!
//! `new_without_app_manifest()` then takes the manifest away from tauri-build so exactly one
//! manifest resource exists: two would put two `RT_MANIFEST` ids into the same binary. Everything
//! else tauri-build embeds (icon, version info) is untouched and stays bins-scoped, and the
//! manifest the app binary ends up carrying is byte-identical to the one tauri-build supplied.

/// Verbatim `tauri-build`'s own `windows-app-manifest.xml` (2.6.3). Kept identical on purpose: this
/// replaces that file's contribution rather than changing it.
const WINDOWS_APP_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    // A build script runs on the host, so the target OS comes from cargo's env, not `cfg!`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        tauri_build::build();
        return;
    }

    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    if let Err(error) = tauri_build::try_build(attributes) {
        panic!("tauri-build failed: {error:#}");
    }

    let out_dir = std::path::PathBuf::from(
        std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"),
    );

    let manifest_path = out_dir.join("hb-app.manifest");
    std::fs::write(&manifest_path, WINDOWS_APP_MANIFEST).expect("write hb-app.manifest");

    // `1 24 "<path>"` is resource id 1, type RT_MANIFEST (24) — the id the loader looks for in an
    // executable. Backslashes are escaped because an .rc string is C-escaped.
    let rc_path = out_dir.join("hb-app-manifest.rc");
    let escaped = manifest_path.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc_path, format!("1 24 \"{escaped}\"\n")).expect("write hb-app-manifest.rc");

    embed_resource::compile_for_everything(&rc_path, embed_resource::NONE)
        .manifest_required()
        .expect("embed the Common Controls v6 application manifest");
}
