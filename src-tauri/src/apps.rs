// App bundles on disk: what is installed, what version it says it is, whether it is running,
// which bundles a cask owns, and its icon.

use crate::model::{ResultAction, UpdateResult};
use crate::process::run_cmd;
use crate::store::freshet_dir;
use crate::validate::{is_plain_file_name, is_valid_app_name, is_valid_cask_token};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

// The version an app reports about itself (CFBundleShortVersionString, else CFBundleVersion).
// For casks this is the truth about what's installed: Homebrew only records what *it* last
// installed, so after a rollback (or an app updating itself) the two disagree.
pub(crate) fn bundle_version(app: &Path) -> Option<String> {
    let info = app.join("Contents/Info.plist");
    let info = info.to_str()?;
    read_plist_string(info, "CFBundleShortVersionString")
        .or_else(|| read_plist_string(info, "CFBundleVersion"))
}

// Does Homebrew's recorded cask version describe the app on disk? Cask versions can carry a
// ",build" suffix, and apps sometimes report fewer or more components ("26" vs "26.0.1"), so
// one being a dotted prefix of the other counts as agreement. Pure → unit-tested.
pub(crate) fn versions_agree(cask_version: &str, bundle_version: &str) -> bool {
    let cask = cask_version.split(',').next().unwrap_or("").trim();
    let bundle = bundle_version.trim();
    cask == bundle
        || cask.starts_with(&format!("{bundle}."))
        || bundle.starts_with(&format!("{cask}."))
}

pub(crate) fn read_plist_string(info_path: &str, key: &str) -> Option<String> {
    let out = Command::new("/usr/bin/defaults")
        .args(["read", info_path, key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// --- App icons ---
//
// The installed app's own icon, as a PNG data URL (img-src allows data:). Converted once with
// `sips` and cached in ~/.freshet/icons, regenerated when the app's .icns is newer (an update can
// change the icon).

fn find_app_bundle(app: &str) -> Option<PathBuf> {
    let home_apps = dirs::home_dir().map(|h| h.join("Applications"));
    [Some(PathBuf::from("/Applications")), home_apps]
        .into_iter()
        .flatten()
        .map(|d| d.join(app))
        .find(|p| p.is_dir())
}

fn icns_path(bundle: &Path) -> Option<PathBuf> {
    let info = bundle.join("Contents/Info.plist");
    let name = read_plist_string(info.to_str()?, "CFBundleIconFile")?;
    // The name is whatever the app's own Info.plist says: keep it to a file inside Resources, so
    // a crafted one ("../../…", "/Users/…") can't point the icon reader anywhere else.
    if !is_plain_file_name(&name) {
        return None;
    }
    let file = if Path::new(&name).extension().is_some() {
        name
    } else {
        format!("{name}.icns")
    };
    let path = bundle.join("Contents/Resources").join(file);
    path.is_file().then_some(path)
}

pub(crate) fn modified(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn app_icon_png(app: &str) -> Option<Vec<u8>> {
    let icns = icns_path(&find_app_bundle(app)?)?;
    let dir = freshet_dir()?.join("icons");
    std::fs::create_dir_all(&dir).ok()?;
    let png = dir.join(format!("{}.png", app.trim_end_matches(".app")));
    let fresh = matches!((modified(&png), modified(&icns)), (Some(p), Some(i)) if p >= i);
    if !fresh {
        let ok = Command::new("/usr/bin/sips")
            .args(["-s", "format", "png", "-Z", "64"])
            .arg(&icns)
            .arg("--out")
            .arg(&png)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return None;
        }
    }
    std::fs::read(&png).ok()
}

#[tauri::command]
pub(crate) async fn app_icon(app: String) -> Option<String> {
    use base64::Engine as _;
    if !is_valid_app_name(&app) {
        return None;
    }
    tauri::async_runtime::spawn_blocking(move || app_icon_png(&app))
        .await
        .ok()
        .flatten()
        .map(|png| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
        })
}

// --- Is an app open? ---

// Full executable paths of every running process (`ps -axo comm=` prints them on macOS).
pub(crate) fn running_executables() -> Vec<String> {
    Command::new("/bin/ps")
        .args(["-axo", "comm="])
        .output()
        .map_or_default(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .collect()
        })
}

// Any process (the app or one of its helpers) running from inside `bundle`. Pure → tested.
pub(crate) fn bundle_is_running(bundle: &Path, executables: &[String]) -> bool {
    let prefix = format!("{}/Contents/", bundle.display());
    executables.iter().any(|e| e.starts_with(&prefix))
}

pub(crate) fn list_applications() -> Vec<PathBuf> {
    let mut apps = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/Applications") {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("app"))
            {
                apps.push(p);
            }
        }
    }
    apps
}

// --- Which .app bundles a cask owns ---
//
// From the public `brew info --json=v2 --cask` (one call for many tokens). Homebrew 7 removed
// the `api/cask.jws.json` cache file this used to read, which silently emptied the post-update
// check; its replacement is an internal, indexed format we don't depend on.

// token → its `app` artifact names. Pure → unit-tested against real output.
fn parse_cask_apps(json: &str) -> HashMap<String, Vec<String>> {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    parsed["casks"]
        .as_array()
        .map_or_default(Vec::as_slice)
        .iter()
        .filter_map(|c| {
            let token = c["token"].as_str()?.to_string();
            let apps = c["artifacts"]
                .as_array()
                .map_or_default(Vec::as_slice)
                .iter()
                .filter_map(|a| a["app"].as_array())
                .flatten()
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            Some((token, apps))
        })
        .collect()
}

pub(crate) fn cask_apps(tokens: &[String]) -> HashMap<String, Vec<String>> {
    let tokens: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|t| is_valid_cask_token(t))
        .collect();
    if tokens.is_empty() {
        return HashMap::new();
    }
    let info = |tokens: &[&str]| -> Option<HashMap<String, Vec<String>>> {
        let mut args = vec!["info", "--json=v2", "--cask"];
        args.extend(tokens);
        let parsed = parse_cask_apps(&run_cmd("brew", &args).ok()?);
        (!parsed.is_empty()).then_some(parsed)
    };
    // `brew info` is all-or-nothing: one cask it refuses to load (e.g. from an untrusted tap)
    // fails the whole call with no output. Then ask per cask, so one bad token only costs itself
    // — otherwise every cask in the batch would silently lose its pre-install check and snapshot.
    info(&tokens).unwrap_or_else(|| {
        tracing::warn!("bulk brew info failed; looking casks up one by one");
        let next = AtomicUsize::new(0);
        let found = std::sync::Mutex::new(HashMap::new());
        std::thread::scope(|scope| {
            for _ in 0..tokens.len().min(8) {
                scope.spawn(|| {
                    while let Some(token) = tokens.get(next.fetch_add(1, Ordering::SeqCst)) {
                        if let Some(one) = info(&[token]) {
                            found
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .extend(one);
                        }
                    }
                });
            }
        });
        found
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    })
}

// The named bundles that are actually present in /Applications (case-insensitive match).
pub(crate) fn installed_cask_apps(app_names: &[String]) -> Vec<PathBuf> {
    let wanted: std::collections::HashSet<String> =
        app_names.iter().map(|a| a.to_lowercase()).collect();
    list_applications()
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|f| wanted.contains(&f.to_string_lossy().to_lowercase()))
        })
        .collect()
}

pub(crate) fn info_plist_mtime(app: &Path) -> Option<u64> {
    modified(&app.join("Contents/Info.plist"))?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// App bundle names → their paths in /Applications, skipping anything that isn't a plain
// "Name.app" or isn't installed. Pure apart from the existence check → unit-tested.
pub(crate) fn app_store_bundles(apps_dir: &Path, names: &[String]) -> Vec<PathBuf> {
    names
        .iter()
        .filter(|n| is_valid_app_name(n))
        .map(|n| apps_dir.join(n))
        .filter(|p| p.is_dir())
        .collect()
}

// --- macOS App Management ---
//
// Since macOS 13, an app may not change another developer's app bundle unless the user allows it
// under Privacy & Security › App Management — and sudo doesn't get round it. Homebrew hits this
// when adopting (`chgrp -hR admin` over the bundle) or replacing an app, and every file in the
// bundle then fails with "Operation not permitted". Recognised here so the result says what to do
// instead of showing a hundred chgrp lines.

const APP_MANAGEMENT_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles";

// The bundle macOS refused to let us change ("Postman.app"), if that's why this output failed.
// Pure → unit-tested against real brew output.
pub(crate) fn app_management_blocked(output: &str) -> Option<String> {
    output
        .lines()
        .filter(|l| l.ends_with(": Operation not permitted"))
        .find_map(|l| {
            let path = &l[l.find("/Applications/")? + "/Applications/".len()..];
            let end = path.find(".app")? + ".app".len();
            Some(path[..end].to_string())
        })
}

// Swap the wall of refusals for what happened and what to do, keep one sample line and brew's
// own error, and offer to open the settings pane.
pub(crate) fn explain_app_management(r: &mut UpdateResult) {
    if r.success || r.action.is_some() {
        return;
    }
    let Some(app) = app_management_blocked(&r.output) else {
        return;
    };
    let mut kept: Vec<&str> = Vec::new();
    let mut refused = 0;
    for line in r.output.lines() {
        if line.ends_with(": Operation not permitted") {
            refused += 1;
            if refused > 1 {
                continue;
            }
        }
        kept.push(line);
    }
    let mut log = kept.join("\n");
    if refused > 1 {
        let _ = write!(log, "\n(…and {} more refused the same way)", refused - 1);
    }
    r.output = format!(
        "macOS didn't let Freshet change {app}: it's from another developer, and changing those \
         needs your permission (System Settings › Privacy & Security › App Management). Nothing \
         was changed. Allow Freshet there, then try again.\n\n{log}"
    );
    r.action = Some(ResultAction::AllowAppManagement { app });
}

#[tauri::command]
pub(crate) fn open_app_management_settings() -> Result<(), String> {
    Command::new("/usr/bin/open")
        .arg(APP_MANAGEMENT_PANE)
        .status()
        .map_err(|e| e.to_string())
        .and_then(|st| {
            st.success()
                .then_some(())
                .ok_or_else(|| "couldn't open System Settings".into())
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::check::check_homebrew;

    use crate::rollback;
    use std::path::Path;

    // Real I/O against this Mac: Safari ships in /Applications on every macOS install.
    //   cargo test -- --ignored app_icon_from_real_bundle
    #[test]
    #[ignore = "reads /Applications and writes ~/.freshet/icons"]
    fn app_icon_from_real_bundle() {
        let png = app_icon_png("Safari.app").expect("Safari icon");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(app_icon_png("No Such App.app").is_none());
    }

    // Trimmed from real `brew info --json=v2 --cask discord slack` output.
    #[test]
    fn cask_apps_from_brew_info() {
        let json = r#"{"formulae":[],"casks":[
          {"token":"discord","artifacts":[{"app":["Discord.app"]},{"zap":[{"trash":["~/Library/Application Support/discord"]}]}]},
          {"token":"slack","artifacts":[{"uninstall":[{"quit":"com.tinyspeck.slackmacgap"}]},{"app":["Slack.app"]}]},
          {"token":"some-pkg","artifacts":[{"pkg":["Installer.pkg"]}]}]}"#;
        let apps = parse_cask_apps(json);
        assert_eq!(apps["discord"], ["Discord.app"]);
        assert_eq!(apps["slack"], ["Slack.app"]);
        assert_eq!(apps["some-pkg"], Vec::<String>::new());
        assert!(parse_cask_apps("nope").is_empty());
    }

    // Homebrew's record vs. what the app says about itself.
    #[test]
    fn cask_and_bundle_versions_agree() {
        assert!(versions_agree("4.52.178", "4.52.178"));
        assert!(
            versions_agree("2025.2.1,252.25557.131", "2025.2.1"),
            "cask build suffix"
        );
        assert!(versions_agree("26.0", "26"), "app reports fewer components");
        assert!(
            versions_agree("1.2", "1.2.0.4"),
            "app reports more components"
        );
        // The case that misled the list: rolled back to 0.0.414, Homebrew still says 0.0.415.
        assert!(!versions_agree("0.0.415", "0.0.414"));
        assert!(!versions_agree("4.52.178", "4.51.185"));
        assert!(!versions_agree("1.2", "1.20"), "not a dotted prefix");
    }

    // Reads a real Info.plist the way macOS stores it.
    #[test]
    fn bundle_version_reads_info_plist() {
        let app = std::env::temp_dir().join(format!("freshet-bv-{}/Demo.app", std::process::id()));
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>0.0.414</string><key>CFBundleVersion</key><string>414</string></dict></plist>"#,
        )
        .unwrap();
        assert_eq!(bundle_version(&app).as_deref(), Some("0.0.414"));
        assert_eq!(bundle_version(Path::new("/nonexistent/X.app")), None);
        std::fs::remove_dir_all(app.parent().unwrap()).unwrap();
    }

    // Real /Applications and ~/.freshet: what the list and Settings now show for rolled-back apps.
    //   cargo test -- --ignored rollback_state_live --nocapture
    #[test]
    #[ignore = "reads the real /Applications, Homebrew and ~/.freshet/rollback"]
    fn rollback_state_live() {
        let recorded = check_homebrew().1;
        for item in &recorded.items {
            let apps = cask_apps(std::slice::from_ref(&item.id));
            let names = apps.get(&item.id).cloned().unwrap_or_default();
            let on_disk = installed_cask_apps(&names)
                .first()
                .and_then(|a| bundle_version(a));
            eprintln!(
                "{:10} Homebrew record {:10} on disk {:10} latest {:10} → list shows from {}",
                item.id,
                item.current,
                on_disk.clone().unwrap_or_default(),
                item.latest,
                on_disk
                    .filter(|v| !versions_agree(&item.current, v))
                    .unwrap_or_else(|| item.current.clone())
            );
        }
        for snap in rollback::list_snapshots() {
            eprintln!(
                "snapshot {:8} version {:10} in_use {}",
                snap.token, snap.version, snap.in_use
            );
        }
    }

    #[test]
    fn app_store_bundles_resolve_only_real_app_names() {
        let dir = std::env::temp_dir().join(format!("freshet-mas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Notability.app")).unwrap();
        let names = vec![
            "Notability.app".to_string(),
            "Missing.app".to_string(),
            "../etc.app".to_string(),
            "Notability".to_string(),
        ];
        assert_eq!(
            app_store_bundles(&dir, &names),
            [dir.join("Notability.app")]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // Real `ps -axo comm=` lines: helpers inside the bundle count; a same-prefix app doesn't.
    #[test]
    fn open_app_detection() {
        let ps = vec![
            "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/140/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper".to_string(),
            "/usr/libexec/trustd".to_string(),
        ];
        assert!(bundle_is_running(
            Path::new("/Applications/Google Chrome.app"),
            &ps
        ));
        assert!(!bundle_is_running(
            Path::new("/Applications/Google Chrome Canary.app"),
            &ps
        ));
        assert!(!bundle_is_running(
            Path::new("/Applications/Slack.app"),
            &ps
        ));
    }

    // Trimmed from a real `brew install --cask postman --adopt` on macOS 26.5.
    const POSTMAN_ADOPT: &str = "==> Fetching downloads for: postman
✔︎ Cask postman (12.37.0)
==> Installing Cask postman
==> Adopting existing App at '/Applications/Postman.app'
chgrp: /Applications/Postman.app/Contents/_CodeSignature/CodeResources: Operation not permitted
chgrp: /Applications/Postman.app/Contents/MacOS/Postman: Operation not permitted
chgrp: /Applications/Postman.app/Contents/Frameworks/Postman Helper (GPU).app/Contents: Operation not permitted
chgrp: /Applications/Postman.app: Operation not permitted
Error: postman: Failure while executing; `/usr/bin/sudo -A -E -- chgrp -hR admin /Applications/Postman.app` exited with 1. Here's the output:
chgrp: /Applications/Postman.app/Contents/_CodeSignature/CodeResources: Operation not permitted
chgrp: /Applications/Postman.app: Operation not permitted
==> Purging files for version 12.37.0 of Cask postman";

    fn failed(output: &str) -> UpdateResult {
        UpdateResult {
            kind: crate::model::Kind::Cask,
            name: "postman".into(),
            success: false,
            output: output.into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        }
    }

    #[test]
    fn app_management_refusal_is_recognised() {
        assert_eq!(
            app_management_blocked(POSTMAN_ADOPT).as_deref(),
            Some("Postman.app")
        );
        // An ordinary failure, or a refusal outside an app bundle, isn't this.
        assert_eq!(app_management_blocked("Error: Download failed: 404"), None);
        assert_eq!(
            app_management_blocked("rm: /usr/local/bin/x: Operation not permitted"),
            None
        );
    }

    #[test]
    fn app_management_refusal_is_explained_with_the_settings_action() {
        let mut r = failed(POSTMAN_ADOPT);
        explain_app_management(&mut r);
        assert!(r
            .output
            .starts_with("macOS didn't let Freshet change Postman.app"));
        assert!(r.output.contains("App Management"));
        assert_eq!(
            r.output.matches("Operation not permitted").count(),
            1,
            "one sample kept"
        );
        assert!(r.output.contains("(…and 5 more refused the same way)"));
        assert!(r.output.contains("Error: postman: Failure while executing"));
        assert_eq!(
            r.action,
            Some(ResultAction::AllowAppManagement {
                app: "Postman.app".into()
            })
        );
        // Successes and other failures are left alone.
        let mut other = failed("Error: Download failed: 404");
        explain_app_management(&mut other);
        assert_eq!(other.output, "Error: Download failed: 404");
        assert_eq!(other.action, None);
    }
}
