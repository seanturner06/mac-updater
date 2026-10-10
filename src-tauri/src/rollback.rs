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
pub(crate) fn restore_from(snapshot_dir: &Path, apps_dir: &Path) -> Result<Snapshot, String> {
    let meta = read_meta(snapshot_dir).ok_or("this snapshot is missing or damaged")?;
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
pub(crate) fn list_in(root: &Path, now: u64) -> Vec<Snapshot> {
    let mut snaps: Vec<Snapshot> = std::fs::read_dir(root)
        .map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|dir| {
            let meta = read_meta(&dir)?;
            if now.saturating_sub(meta.created_at) > KEEP_DAYS * 86_400 {
                let _ = std::fs::remove_dir_all(&dir);
                return None;
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
        version: installed_version(token),
        created_at: crate::unix_now(),
        apps: apps
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect(),
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
    root()
        .map(|r| list_in(&r, crate::unix_now()))
        .unwrap_or_default()
}

#[tauri::command]
pub(crate) async fn restore_snapshot(token: String) -> Result<String, String> {
    if !crate::is_valid_cask_token(&token) {
        return Err("invalid cask token".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let dir = root().ok_or("could not resolve ~/.freshet")?.join(&token);
        let meta = restore_from(&dir, Path::new("/Applications"))?;
        Ok(format!(
            "Restored {}{}. Homebrew still lists the newer version as installed; skip that version in \
             Freshet if you don't want it offered again.",
            meta.apps.join(", "),
            if meta.version.is_empty() { String::new() } else { format!(" {}", meta.version) },
        ))
    })
    .await
    .map_err(|e| e.to_string())?
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
        };
        assert!(snapshot_into(
            std::slice::from_ref(&app),
            &snaps.join("demo"),
            &meta
        ));

        std::fs::write(app.join("Contents/version"), "2.0").unwrap();
        let restored = restore_from(&snaps.join("demo"), &apps).unwrap();
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
        assert!(restore_from(&dir, &snaps).is_err());
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
            };
            std::fs::write(
                dir.join("snapshot.json"),
                serde_json::to_string(&meta).unwrap(),
            )
            .unwrap();
        }
        let now = 35 * 86_400;
        let tokens: Vec<String> = list_in(&root, now).into_iter().map(|s| s.token).collect();
        assert_eq!(tokens, ["b", "a"]);
        assert!(!root.join("old").exists(), "older than 30 days is pruned");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
