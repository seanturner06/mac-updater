// PATH for child processes.

use std::io::Read as _;
use std::process::Command;

// --- PATH for child processes ---
//
// An app opened from Finder, and a launchd job, start with PATH=/usr/bin:/bin:/usr/sbin:/sbin —
// no Homebrew, no nvm. Every `brew`/`mas`/`npm` we spawn would then fail with "not found". So at
// startup, if PATH has no Homebrew prefix on it, ask the user's login shell for its PATH (that's
// where nvm and brew shellenv put things) and merge it in, plus the known Homebrew prefixes as a
// fallback if the shell is slow or broken. Launched from a terminal (`pnpm tauri dev`) PATH
// already has brew on it and none of this runs.
const BREW_BIN_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"];

const PATH_MARKER: &str = "__FRESHET_PATH__";

fn path_has_brew_prefix(path: &str) -> bool {
    path.split(':')
        .any(|d| d == "/opt/homebrew/bin" || d == "/usr/local/bin")
}

// Interactive shells can print banners, motd, or prompts around our output; take only what's
// between the two markers.
fn extract_marked_path(output: &str) -> Option<&str> {
    let start = output.find(PATH_MARKER)? + PATH_MARKER.len();
    let len = output[start..].find(PATH_MARKER)?;
    Some(&output[start..start + len]).filter(|p| !p.is_empty())
}

// Order-preserving, de-duplicated union; existing entries keep priority over added ones.
fn merge_path<'a>(current: &'a str, extra: impl IntoIterator<Item = &'a str>) -> String {
    let mut seen = std::collections::HashSet::new();
    current
        .split(':')
        .chain(extra)
        .filter(|d| !d.is_empty() && seen.insert(*d))
        .collect::<Vec<_>>()
        .join(":")
}

fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/'))?;
    let script = format!("printf '%s%s%s' {PATH_MARKER} \"$PATH\" {PATH_MARKER}");
    let mut child = Command::new(shell)
        .args(["-ilc", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // A shell rc that hangs (a prompt, a slow plugin manager) must not hang the app.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    extract_marked_path(&out).map(String::from)
}

// Must run first thing in main, before any thread exists (set_var is not thread-safe).
pub(crate) fn augment_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    if path_has_brew_prefix(&current) {
        return;
    }
    let shell = login_shell_path().unwrap_or_default();
    let merged = merge_path(&current, shell.split(':').chain(BREW_BIN_DIRS));
    std::env::set_var("PATH", merged);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    #[test]
    fn brew_prefix_detection() {
        assert!(!path_has_brew_prefix("/usr/bin:/bin:/usr/sbin:/sbin"));
        assert!(path_has_brew_prefix("/usr/bin:/opt/homebrew/bin"));
        assert!(path_has_brew_prefix("/usr/local/bin:/usr/bin"));
        assert!(!path_has_brew_prefix("/opt/homebrew/binx"));
    }

    // Interactive shells may print around our output (motd, plugin notices); only the marked
    // span counts. Unmarked or empty output yields None so we fall back to the known prefixes.
    #[test]
    fn marked_path_extraction() {
        let out = format!("Welcome!\n{PATH_MARKER}/a:/b{PATH_MARKER}\nbye");
        assert_eq!(extract_marked_path(&out), Some("/a:/b"));
        assert_eq!(extract_marked_path("no markers"), None);
        assert_eq!(extract_marked_path(&format!("{PATH_MARKER}/a")), None);
        assert_eq!(
            extract_marked_path(&format!("{PATH_MARKER}{PATH_MARKER}")),
            None
        );
    }

    #[test]
    fn path_merge_keeps_order_and_dedupes() {
        let merged = merge_path(
            "/usr/bin:/bin",
            "/opt/homebrew/bin:/usr/bin:/nvm/bin"
                .split(':')
                .chain(BREW_BIN_DIRS),
        );
        assert_eq!(
            merged,
            "/usr/bin:/bin:/opt/homebrew/bin:/nvm/bin:/opt/homebrew/sbin:/usr/local/bin"
        );
        assert_eq!(merge_path("", ["/a", ""]), "/a");
    }
}
