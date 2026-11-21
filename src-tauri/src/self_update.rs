// Freshet itself: its own updates, version, and launching at login.

use crate::process::{begin_operation, SCHEDULED_BUSY_MESSAGE};
use crate::scheduled::scheduled_run_active;
use serde::Serialize;

// --- Freshet updating itself (tauri-plugin-updater) ---
//
// Each release publishes latest.json beside the DMG; the update archive is signed with the key
// whose public half is in tauri.conf.json (plugins.updater.pubkey), and the plugin refuses
// anything that doesn't verify against it.

#[derive(Serialize)]
pub(crate) struct AppUpdate {
    pub(crate) version: String,
    pub(crate) notes: Option<String>,
}

#[tauri::command]
pub(crate) async fn check_app_update(app: tauri::AppHandle) -> Result<Option<AppUpdate>, String> {
    use tauri_plugin_updater::UpdaterExt as _;
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    Ok(update.map(|u| AppUpdate {
        version: u.version,
        notes: u.body,
    }))
}

#[tauri::command]
pub(crate) async fn install_app_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt as _;
    // Held until the restart: nothing may start (or be mid-flight — RUNNING_CHILD alone is zero
    // between brew invocations, during downloads, publisher checks and npm/App Store installs)
    // while the app replaces itself.
    let _busy = begin_operation().ok_or("Finish or stop the running update first.")?;
    if scheduled_run_active() {
        return Err(SCHEDULED_BUSY_MESSAGE.into());
    }
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Freshet is already up to date.")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart()
}

#[allow(clippy::needless_pass_by_value)] // AppHandle is a cheap clone; Tauri passes it by value.
#[tauri::command]
pub(crate) fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

// --- Launch at login (tauri-plugin-autostart, a LaunchAgent in ~/Library/LaunchAgents) ---

#[allow(clippy::needless_pass_by_value)] // AppHandle is a cheap clone; Tauri passes it by value.
#[tauri::command]
pub(crate) fn get_launch_at_login(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt as _;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[allow(clippy::needless_pass_by_value)] // AppHandle is a cheap clone; Tauri passes it by value.
#[tauri::command]
pub(crate) fn set_launch_at_login(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt as _;
    let launcher = app.autolaunch();
    if enabled {
        launcher.enable().map_err(|e| e.to_string())?;
    } else {
        launcher.disable().map_err(|e| e.to_string())?;
    }
    launcher.is_enabled().map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {

    use std::path::Path;

    // The self-update chain: a release build's Freshet.app.tar.gz.sig must verify against the
    // public key in tauri.conf.json — exactly what an installed Freshet checks before updating.
    // Needs a build with updater artifacts first (see docs/14-release-signing.md).
    //   cargo test -- --ignored updater_signature
    #[test]
    #[ignore = "needs a signed release build in target/release/bundle"]
    fn updater_signature_verifies_against_configured_key() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let conf: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string("tauri.conf.json").unwrap()).unwrap();
        let pubkey_text = String::from_utf8(
            b64.decode(conf["plugins"]["updater"]["pubkey"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        let bundle = Path::new("target/release/bundle/macos");
        let sig_text = String::from_utf8(
            b64.decode(
                std::fs::read_to_string(bundle.join("Freshet.app.tar.gz.sig"))
                    .unwrap()
                    .trim(),
            )
            .unwrap(),
        )
        .unwrap();
        let archive = std::fs::read(bundle.join("Freshet.app.tar.gz")).unwrap();

        let pk = minisign_verify::PublicKey::decode(&pubkey_text).unwrap();
        let sig = minisign_verify::Signature::decode(&sig_text).unwrap();
        pk.verify(&archive, &sig, true)
            .expect("signature must verify");

        // And a tampered archive must not.
        let mut tampered = archive;
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0xff;
        assert!(pk.verify(&tampered, &sig, true).is_err());
    }
}
