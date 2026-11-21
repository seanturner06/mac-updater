// ~/.freshet: the private directory everything Freshet keeps lives in, and how files in it are
// written.

use crate::process::own_uid;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

// ~/.freshet, created 0700 (owner-only) so no other local user can read or tamper with the
// askpass helper, the scheduled run script, or the signing baseline that live inside it.
// Refuses (None) a ~/.freshet that is a symlink or belongs to someone else: everything Freshet
// trusts lives here, so it must be the user's own directory, not one planted for them.
pub(crate) fn freshet_dir() -> Option<PathBuf> {
    let mut p = dirs::home_dir()?;
    p.push(".freshet");
    std::fs::create_dir_all(&p).ok()?;
    let meta = std::fs::symlink_metadata(&p).ok()?;
    if !dir_is_safe(meta.file_type().is_symlink(), meta.uid(), own_uid()?) {
        tracing::error!(path = %p.display(), "~/.freshet is not this user's own directory; refusing to use it");
        return None;
    }
    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
    Some(p)
}

// Pure → unit-tested.
const fn dir_is_safe(is_symlink: bool, uid: u32, me: u32) -> bool {
    !is_symlink && uid == me
}

// Write a file owner-only (0700 for executables / 0600 for data is enough; the dir is already 0700).
pub(crate) fn write_private(path: &Path, contents: &str, executable: bool) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    let mode = if executable { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

// Write-then-rename, so the scheduled run never reads a half-written file. The temp name is
// unique per writer: the app and the scheduled run (or two threads) saving the same file must
// not share one temp file, or one renames the other's half-written contents into place.
pub(crate) fn write_private_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = write_private(&tmp, contents, false).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// Earlier versions ran ~/.freshet/run.sh; the job no longer uses it.
pub(crate) fn remove_legacy_run_script() {
    if let Some(dir) = freshet_dir() {
        let _ = std::fs::remove_file(dir.join("run.sh"));
    }
}

// Earlier versions kept a persistent ~/.freshet/askpass.sh; it's now created per use.
pub(crate) fn remove_legacy_askpass() {
    if let Some(dir) = freshet_dir() {
        let _ = std::fs::remove_file(dir.join("askpass.sh"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    #[test]
    fn freshet_dir_must_be_the_users_own_real_directory() {
        assert!(dir_is_safe(false, 501, 501));
        assert!(
            !dir_is_safe(true, 501, 501),
            "a symlink could point anywhere"
        );
        assert!(!dir_is_safe(false, 0, 501), "owned by someone else");
    }

    // Two writers of the same file never share a temp file, and nothing is left behind.
    #[test]
    fn atomic_writes_replace_the_file_and_leave_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("freshet-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("signing.json");
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = &path;
                scope.spawn(move || write_private_atomic(path, &format!("{{\"n\":{i}}}")).unwrap());
            }
        });
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&text).is_ok(),
            "one writer's complete contents: {text}"
        );
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["signing.json"]);
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            0o600,
            "still owner-only"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
