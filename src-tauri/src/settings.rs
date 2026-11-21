// Settings shared by the app and the scheduled run (~/.freshet/settings.json).

use crate::model::Kind;
use crate::npm;
use crate::store::{freshet_dir, write_private_atomic};
use crate::validate::is_valid_npm_package;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// --- Settings shared by the app and the scheduled run ---
//
// Skipped versions, notify-only and greedy used to live in the webview's localStorage, which the
// launchd job can't read — so the scheduled run ignored all three. They now live in
// ~/.freshet/settings.json, written by the app (save_settings) and read by `--run-scheduled`.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) greedy: bool,
    pub(crate) notify_only: bool,
    // Opt-in: look up known vulnerabilities for npm updates on osv.dev (see vulns.rs).
    pub(crate) check_vulnerabilities: bool,
    // ignore_key()s of skipped versions.
    pub(crate) skipped: Vec<String>,
    // "name@version" npm packages the user allowed to run install scripts (see npm.rs). Managed
    // by approve_npm_scripts only; save_settings carries it over unchanged.
    pub(crate) npm_scripts_approved: Vec<String>,
}

// Must match `ignoreKey` in src/logic.ts. Keyed by version, so a newer release is not skipped.
pub(crate) fn ignore_key(kind: Kind, id: &str, latest: &str) -> String {
    format!("{kind}::{id}::{latest}")
}

fn settings_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("settings.json"))
}

// None when no settings file exists yet (the frontend then migrates its localStorage values).
pub(crate) fn load_settings() -> Option<Settings> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

// settings.json is read, changed and written back by two commands (save_settings and
// approve_npm_scripts); this keeps one's write from landing in the middle of the other's.
static SETTINGS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// The user allows `name@version` to run its install scripts (from the results panel).
#[allow(clippy::needless_pass_by_value)] // Tauri hands command arguments over by value.
#[tauri::command]
pub(crate) fn approve_npm_scripts(name: String, version: String) -> Result<(), String> {
    if !is_valid_npm_package(&name) || !npm::is_valid_version(&version) {
        return Err("invalid package or version".into());
    }
    let spec = format!("{name}@{version}");
    let _guard = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut settings = load_settings().unwrap_or_default();
    if !settings.npm_scripts_approved.contains(&spec) {
        settings.npm_scripts_approved.push(spec);
    }
    let path = settings_path().ok_or_else(|| "could not resolve ~/.freshet".to_string())?;
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    write_private_atomic(&path, &json).map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn get_settings() -> Option<Settings> {
    load_settings()
}

// Tauri deserializes command arguments into owned values; a reference isn't an option.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub(crate) fn save_settings(settings: Settings) -> Result<(), String> {
    let path = settings_path().ok_or_else(|| "could not resolve ~/.freshet".to_string())?;
    let _guard = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The frontend doesn't manage npm approvals; never let its save erase them.
    let settings = Settings {
        npm_scripts_approved: load_settings().map_or_default(|s| s.npm_scripts_approved),
        ..settings
    };
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    write_private_atomic(&path, &json).map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    // The frontend writes this shape; older/partial files must still load (serde(default)).
    #[test]
    fn settings_json_round_trips_and_tolerates_missing_fields() {
        let s: Settings = serde_json::from_str(
            r#"{"greedy":true,"notify_only":true,"skipped":["brew::git::2.50"]}"#,
        )
        .unwrap();
        assert!(s.greedy && s.notify_only);
        assert_eq!(s.skipped, ["brew::git::2.50"]);
        let partial: Settings = serde_json::from_str(r#"{"notify_only":true}"#).unwrap();
        assert_eq!(
            partial,
            Settings {
                notify_only: true,
                ..Settings::default()
            }
        );
    }
}
