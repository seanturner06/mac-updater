// One-click rollback for casks: before an upgrade, the installed .app is cloned into
// ~/.freshet/rollback/<token>/ with `cp -c` (APFS clonefile). A clone shares the original's
// blocks, so taking it is instant and costs no disk until the upgrade replaces the files — then
// the snapshot holds the old version's blocks, which is the point. One snapshot per app (the
// latest), pruned after 30 days.
//
// Restoring swaps the bundle back. It reverses the app, not data a new version already migrated,
// and Homebrew's own records still list the newer version (shown to the user with the result).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

const KEEP_DAYS: u64 = 30;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) token: String,
    // Installed version at snapshot time, from Homebrew's Caskroom (may be empty).
    pub(crate) version: String,
    // Unix seconds.
    pub(crate) created_at: u64,
    // Bundle names, e.g. ["Discord.app"].
    pub(crate) apps: Vec<String>,
    // Computed when listing, not stored: the installed app is this snapshot's version, i.e. it's
    // the one in use (typically because it was just restored).
    #[serde(default)]
    pub(crate) in_use: bool,
}

fn root() -> Option<PathBuf> {
    crate::freshet_dir().map(|d| d.join("rollback"))
}

// --- path-parameterized core (unit-tested against temp dirs) ---

// Clone `apps` into `dest` (replacing any previous snapshot there) and record metadata.
// Returns false — leaving nothing behind — if any clone fails (e.g. not APFS: we never fall back
// to a full copy, which could silently cost gigabytes).
pub(crate) fn snapshot_into(apps: &[PathBuf], dest: &Path, meta: &Snapshot) -> bool {
    let _ = std::fs::remove_dir_all(dest);
    if std::fs::create_dir_all(dest).is_err() {
        return false;
    }
    for app in apps {
        let ok = Command::new("/bin/cp")
            .args(["-c", "-R", "-p"])
            .arg(app)
            .arg(dest)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            let _ = std::fs::remove_dir_all(dest);
            return false;
        }
    }
    let Ok(json) = serde_json::to_string_pretty(meta) else {
        let _ = std::fs::remove_dir_all(dest);
        return false;
    };
    std::fs::write(dest.join("snapshot.json"), json).is_ok()
}

// Put each snapshotted bundle back into `apps_dir`. The current bundle is moved aside first and
// only deleted once the restored copy is in place, so a failed restore leaves the app as it was.
// `verify(snapshot_app, installed_app)` runs for every bundle before anything is touched; an Err
// aborts the restore with the app left as it is (see verify_snapshot).
pub(crate) fn restore_from(
    snapshot_dir: &Path,
    apps_dir: &Path,
    verify: &dyn Fn(&Path, &Path) -> Result<(), String>,
) -> Result<Snapshot, String> {
    let meta = read_meta(snapshot_dir).ok_or("this snapshot is missing or damaged")?;
    for app in &meta.apps {
        if !crate::is_valid_app_name(app) {
            return Err(format!(
                "refusing to restore unexpected bundle name {app:?}"
            ));
        }
        verify(&snapshot_dir.join(app), &apps_dir.join(app))?;
    }
    for app in &meta.apps {
        if !crate::is_valid_app_name(app) {
            return Err(format!(
                "refusing to restore unexpected bundle name {app:?}"
            ));
        }
        let source = snapshot_dir.join(app);
        let target = apps_dir.join(app);
        let aside = apps_dir.join(format!(".{app}.freshet-replaced"));
        let _ = std::fs::remove_dir_all(&aside);
        let had_current = target.exists();
        if had_current {
            std::fs::rename(&target, &aside)
                .map_err(|e| format!("couldn't move the current {app} aside: {e}"))?;
        }
        let ok = Command::new("/bin/cp")
            .args(["-c", "-R", "-p"])
            .arg(&source)
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            let _ = std::fs::remove_dir_all(&target);
            if had_current {
                let _ = std::fs::rename(&aside, &target);
            }
            return Err(format!(
                "couldn't restore {app}; the current version was left in place"
            ));
        }
        let _ = std::fs::remove_dir_all(&aside);
    }
    Ok(meta)
}

fn read_meta(dir: &Path) -> Option<Snapshot> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("snapshot.json")).ok()?).ok()
}

// Snapshots in `root`, newest first; ones older than KEEP_DAYS are deleted along the way.
pub(crate) fn list_in(root: &Path, apps_dir: &Path, now: u64) -> Vec<Snapshot> {
    let mut snaps: Vec<Snapshot> = std::fs::read_dir(root)
        .map_or_default(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .into_iter()
        .filter_map(|dir| {
            let meta = read_meta(&dir)?;
            if now.saturating_sub(meta.created_at) > KEEP_DAYS * 86_400 {
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            }
            let mut meta = meta;
            // The app inside the snapshot is the truth about its version (older snapshots were
            // labelled from Homebrew's record, which can disagree).
            if let Some(first) = meta.apps.first() {
                if let Some(v) = crate::bundle_version(&dir.join(first)) {
                    meta.version = v;
                }
                meta.in_use = !meta.version.is_empty()
                    && crate::bundle_version(&apps_dir.join(first)).as_deref()
                        == Some(&meta.version);
            }
            Some(meta)
        })
        .collect();
    snaps.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    snaps
}

// --- Homebrew-facing wrappers ---

// Installed version from the Caskroom (`<prefix>/Caskroom/<token>/<version>/`).
fn installed_version(token: &str) -> String {
    crate::find_brew()
        .and_then(|brew| {
            brew.parent()?
                .parent()
                .map(|p| p.join("Caskroom").join(token))
        })
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .and_then(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .max()
        })
        .unwrap_or_default()
}

// `apps`: this cask's bundles currently in /Applications (see installed_cask_apps).
pub(crate) fn snapshot_cask(token: &str, apps: &[PathBuf]) -> bool {
    if !crate::is_valid_cask_token(token) {
        return false;
    }
    let Some(root) = root() else { return false };
    if apps.is_empty() {
        return false;
    }
    let meta = Snapshot {
        token: token.to_string(),
        version: apps
            .first()
            .and_then(|a| crate::bundle_version(a))
            .unwrap_or_else(|| installed_version(token)),
        created_at: crate::unix_now(),
        apps: apps
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect(),
        in_use: false,
    };
    let started = std::time::Instant::now();
    let ok = snapshot_into(apps, &root.join(token), &meta);
    tracing::info!(
        cask = token,
        ok,
        elapsed_ms = crate::elapsed_ms(started),
        "rollback snapshot"
    );
    ok
}

#[tauri::command]
pub(crate) fn list_snapshots() -> Vec<Snapshot> {
    root().map_or_default(|r| list_in(&r, Path::new("/Applications"), crate::unix_now()))
}

#[tauri::command]
pub(crate) async fn restore_snapshot(token: String) -> Result<String, String> {
    if !crate::is_valid_cask_token(&token) {
        return Err("invalid cask token".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let dir = root().ok_or("could not resolve ~/.freshet")?.join(&token);
        let meta = restore_from(&dir, Path::new("/Applications"), &verify_snapshot)?;
        Ok(format!(
            "Restored {}{}. You won't be offered the version you rolled back from again, only \
             releases newer than it.",
            meta.apps.join(", ").replace(".app", ""),
            if meta.version.is_empty() {
                String::new()
            } else {
                format!(" {}", meta.version)
            },
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

// A restore is a plain file copy: Gatekeeper never looks at it (it only checks quarantined
// downloads). So before putting a snapshot back, check it ourselves with the two checks the rest
// of Freshet relies on: the code signature must be intact (nothing in the saved bundle changed
// since it was signed), and it must come from the same publisher as the app it replaces (or the
// trusted baseline). Otherwise a tampered snapshot could be "restored" silently.
fn verify_snapshot(snapshot_app: &Path, installed_app: &Path) -> Result<(), String> {
    let name = snapshot_app
        .file_stem()
        .map_or_default(|n| n.to_string_lossy().into_owned());
    if !signature_intact(snapshot_app) {
        return Err(format!(
            "Didn't restore {name}: the saved copy's code signature doesn't verify, so it may have \
             been modified. The current version was left in place."
        ));
    }
    let snapshot = crate::read_signer(snapshot_app);
    let expected = snapshot
        .bundle_id
        .as_deref()
        .and_then(crate::trusted_team)
        .or_else(|| crate::read_signer(installed_app).team);
    if snapshot_team_ok(snapshot.team.as_deref(), expected.as_deref()) {
        Ok(())
    } else {
        Err(format!(
            "Didn't restore {name}: the saved copy is signed by a different publisher than the \
             installed app. The current version was left in place."
        ))
    }
}

fn signature_intact(app: &Path) -> bool {
    Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .output()
        .is_ok_and(|o| o.status.success())
}

// With a known publisher, the snapshot must match it exactly (a missing Team ID doesn't). With
// none to compare against (an unsigned app), the intact-signature check stands alone.
// Pure → unit-tested.
fn snapshot_team_ok(snapshot: Option<&str>, expected: Option<&str>) -> bool {
    expected.is_none_or(|e| snapshot == Some(e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("freshet-rb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_app(dir: &Path, name: &str, version: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/version"), version).unwrap();
        app
    }

    // Real `cp -c` against the temp dir (APFS on any supported macOS): snapshot v1, "upgrade" to
    // v2, restore → v1 is back and nothing is left aside.
    #[test]
    fn snapshot_then_restore_round_trips() {
        let apps = temp("apps");
        let snaps = temp("snaps");
        let app = fake_app(&apps, "Demo.app", "1.0");
        let meta = Snapshot {
            token: "demo".into(),
            version: "1.0".into(),
            created_at: 100,
            apps: vec!["Demo.app".into()],
            in_use: false,
        };
        assert!(snapshot_into(
            std::slice::from_ref(&app),
            &snaps.join("demo"),
            &meta
        ));

        std::fs::write(app.join("Contents/version"), "2.0").unwrap();
        let restored = restore_from(&snaps.join("demo"), &apps, &|_, _| Ok(())).unwrap();
        assert_eq!(restored, meta);
        assert_eq!(
            std::fs::read_to_string(app.join("Contents/version")).unwrap(),
            "1.0"
        );
        assert!(!apps.join(".Demo.app.freshet-replaced").exists());
        // The snapshot survives a restore, so it can be used again.
        assert!(snaps.join("demo/Demo.app").exists());

        std::fs::remove_dir_all(&apps).unwrap();
        std::fs::remove_dir_all(&snaps).unwrap();
    }

    #[test]
    fn failed_snapshot_leaves_nothing() {
        let snaps = temp("fail");
        let meta = Snapshot {
            token: "x".into(),
            version: String::new(),
            created_at: 0,
            apps: vec!["Nope.app".into()],
            in_use: false,
        };
        assert!(!snapshot_into(
            &[snaps.join("Nope.app")],
            &snaps.join("x"),
            &meta
        ));
        assert!(!snaps.join("x").exists());
        std::fs::remove_dir_all(&snaps).unwrap();
    }

    #[test]
    fn restore_refuses_path_tricks_in_metadata() {
        let snaps = temp("evil");
        let dir = snaps.join("evil");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("snapshot.json"),
            r#"{"token":"evil","version":"","created_at":0,"apps":["../../etc.app"]}"#,
        )
        .unwrap();
        assert!(restore_from(&dir, &snaps, &|_, _| Ok(())).is_err());
        std::fs::remove_dir_all(&snaps).unwrap();
    }

    #[test]
    fn listing_is_newest_first_and_prunes_old() {
        let root = temp("list");
        for (token, at) in [("old", 0_u64), ("a", 10 * 86_400), ("b", 20 * 86_400)] {
            let dir = root.join(token);
            std::fs::create_dir_all(&dir).unwrap();
            let meta = Snapshot {
                token: token.into(),
                version: "1".into(),
                created_at: at,
                apps: vec![],
                in_use: false,
            };
            std::fs::write(
                dir.join("snapshot.json"),
                serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
        }
        let now = 35 * 86_400;
        let tokens: Vec<String> = list_in(&root, &root, now)
            .into_iter()
            .map(|s| s.token)
            .collect();
        assert_eq!(tokens, ["b", "a"]);
        assert!(!root.join("old").exists(), "older than 30 days is pruned");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn app_with_version(dir: &Path, name: &str, version: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"#),
        )
        .unwrap();
        app
    }

    // Listing reads the real version from the snapshot's own app (not the stored label) and
    // marks it in use when that's what's installed — so a restored snapshot reads "In use"
    // instead of offering Restore again as if nothing happened.
    #[test]
    fn listing_reports_real_version_and_in_use() {
        let root = temp("inuse");
        let apps = temp("inuse-apps");
        let snap = root.join("slack");
        app_with_version(&snap, "Slack.app", "4.51.185");
        let meta = Snapshot {
            token: "slack".into(),
            version: "4.52.171".into(), // the wrong label older snapshots got from Homebrew
            created_at: 100,
            apps: vec!["Slack.app".into()],
            in_use: false,
        };
        std::fs::write(
            snap.join("snapshot.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        app_with_version(&apps, "Slack.app", "4.52.178");
        let listed = &list_in(&root, &apps, 200)[0];
        assert_eq!(listed.version, "4.51.185");
        assert!(
            !listed.in_use,
            "newer version installed: restore is still available"
        );

        app_with_version(&apps, "Slack.app", "4.51.185");
        assert!(
            list_in(&root, &apps, 200)[0].in_use,
            "restored version is the one installed"
        );
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&apps).unwrap();
    }

    #[test]
    fn publisher_must_match_when_known() {
        assert!(snapshot_team_ok(Some("BQR82RBBHL"), Some("BQR82RBBHL")));
        assert!(!snapshot_team_ok(Some("EVILEVIL99"), Some("BQR82RBBHL")));
        assert!(
            !snapshot_team_ok(None, Some("BQR82RBBHL")),
            "unsigned can't replace signed"
        );
        assert!(
            snapshot_team_ok(None, None),
            "unsigned app: integrity check stands alone"
        );
    }

    // A failed verification leaves the installed app exactly as it was.
    #[test]
    fn rejected_snapshot_changes_nothing() {
        let apps = temp("reject-apps");
        let snaps = temp("reject-snaps");
        let app = fake_app(&apps, "Demo.app", "2.0");
        let meta = Snapshot {
            token: "demo".into(),
            version: "1.0".into(),
            created_at: 1,
            apps: vec!["Demo.app".into()],
            in_use: false,
        };
        assert!(snapshot_into(
            std::slice::from_ref(&app),
            &snaps.join("demo"),
            &meta
        ));
        let err =
            restore_from(&snaps.join("demo"), &apps, &|_, _| Err("tampered".into())).unwrap_err();
        assert_eq!(err, "tampered");
        assert_eq!(
            std::fs::read_to_string(app.join("Contents/version")).unwrap(),
            "2.0"
        );
        std::fs::remove_dir_all(&apps).unwrap();
        std::fs::remove_dir_all(&snaps).unwrap();
    }

    // Real codesign: an ad-hoc-signed bundle verifies; change one file inside it and it no
    // longer does — the case verify_snapshot exists to catch.
    #[test]
    fn tampered_bundle_fails_signature_check() {
        let dir = temp("sig");
        let app = app_with_version(&dir, "Signed.app", "1.0");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/Signed")).unwrap();
        std::fs::write(app.join("Contents/Resources/data.txt"), "original").unwrap();
        let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap().replace(
            "<dict>",
            "<dict><key>CFBundleExecutable</key><string>Signed</string><key>CFBundleIdentifier</key><string>com.freshet.test.signed</string>",
        );
        std::fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&app)
            .output()
            .unwrap();
        assert!(
            signed.status.success(),
            "{}",
            String::from_utf8_lossy(&signed.stderr)
        );

        assert!(signature_intact(&app));
        std::fs::write(app.join("Contents/Resources/data.txt"), "tampered").unwrap();
        assert!(!signature_intact(&app));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
