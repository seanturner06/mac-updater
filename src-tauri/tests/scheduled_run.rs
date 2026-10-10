// Runs the real `freshet --run-scheduled` binary the way launchd does — bare PATH, no terminal —
// against a stand-in `brew` and a temp HOME, and checks what it actually invoked.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

// Formula `node` has a skipped version, cask `chrome` is self-updating (only in the --greedy
// listing), and upgrading cask `zoom` fails the way sudo does with no terminal or askpass.
const FAKE_BREW: &str = r#"#!/bin/bash
echo "$*" >> "$HOME/brew-calls"
up="$HOME/upgraded"; touch "$up"
# One JSON item, unless it has already been upgraded in this run.
item() { grep -qx "$1" "$up" && return; printf '%s{"name":"%s","installed_versions":["%s"],"current_version":"%s"}' "$sep" "$1" "$2" "$3"; sep=,; }
formulae() { sep=; item git 2.49 2.50; item node 22 24; }
casks_normal() { sep=; item iterm2 3.5 3.6; item zoom 6.0 6.1; }
casks_greedy() { casks_normal; item chrome 139 140; }
case "$*" in
  "outdated --json=v2 --greedy") echo "{\"formulae\":[$(formulae)],\"casks\":[$(casks_greedy)]}" ;;
  "outdated --cask --json=v2") echo "{\"formulae\":[],\"casks\":[$(casks_normal)]}" ;;
  "--cache"|"--cache --cask "*) echo "$HOME/no-brew-cache" ;;
  "info --json=v2 --cask "*) echo '{"formulae":[],"casks":[]}' ;;
  "upgrade "*)
    ps -o pri= -p $$ | tr -d ' ' >> "$HOME/upgrade-priority"
    shift 2
    for n in "$@"; do
      echo "==> Upgrading $n"
      if [ "$n" = zoom ]; then
        echo "sudo: a terminal is required to read the password; either use the -S option to read from standard input or configure an askpass helper" >&2
        echo "sudo: a password is required" >&2
        failed=1
      else
        echo "$n" >> "$up"
      fi
    done
    [ -n "$failed" ] && { echo "Error: zoom: sudo failed" >&2; exit 1; }
    ;;
esac
exit 0
"#;

struct Home(PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup(name: &str, settings: &str) -> Home {
    let home = std::env::temp_dir().join(format!("freshet-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("bin")).unwrap();
    std::fs::create_dir_all(home.join(".freshet")).unwrap();
    let brew = home.join("bin/brew");
    std::fs::write(&brew, FAKE_BREW).unwrap();
    Command::new("/bin/chmod")
        .arg("+x")
        .arg(&brew)
        .status()
        .unwrap();
    std::fs::write(home.join(".freshet/settings.json"), settings).unwrap();
    // The weekly full signature scan would inspect the real /Applications (slow, and not what
    // these tests are about): mark it as just done.
    std::fs::write(
        home.join(".freshet/full-scan.stamp"),
        format!("{}", unix_now()),
    )
    .unwrap();
    Home(home)
}

// launchd's environment, plus the fake brew dir first on PATH. SHELL=/usr/bin/false makes the
// login-shell PATH lookup fail, exercising the fallback path.
fn run_scheduled(home: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_freshet"))
        .arg("--run-scheduled")
        .env_clear()
        .env("HOME", home)
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                home.join("bin").display()
            ),
        )
        .env("SHELL", "/usr/bin/false")
        .env("FRESHET_NO_NOTIFY", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn calls(home: &Path) -> Vec<String> {
    read(&home.join("brew-calls"))
        .lines()
        .map(String::from)
        .collect()
}

fn last_run(home: &Path) -> serde_json::Value {
    serde_json::from_str(&read(&home.join(".freshet/last-run.json"))).unwrap()
}

#[test]
fn installs_only_what_the_app_would_list_and_reports_admin_items() {
    let home = setup(
        "install",
        r#"{"greedy":false,"notify_only":false,"skipped":["brew::node::24"]}"#,
    );
    let out = run_scheduled(&home.0);

    let calls = calls(&home.0);
    let upgrades: Vec<&str> = calls
        .iter()
        .map(String::as_str)
        .filter(|c| c.starts_with("upgrade"))
        .collect();
    // node (skipped version) and chrome (self-updating, greedy off) are left alone, and each
    // kind is upgraded in one batched brew call.
    assert_eq!(
        upgrades,
        ["upgrade --formula git", "upgrade --cask iterm2 zoom"]
    );
    // The batch failed (zoom), so what's still outdated is re-checked once to get per-item truth.
    let rechecks = calls
        .iter()
        .filter(|c| *c == "outdated --json=v2 --greedy")
        .count();
    assert_eq!(
        rechecks, 2,
        "initial check + one post-batch re-check: {calls:?}"
    );
    assert_eq!(calls.first().map(String::as_str), Some("update"));
    assert_eq!(
        calls.iter().filter(|c| *c == "update").count(),
        1,
        "{calls:?}"
    );
    assert!(calls.contains(&"cleanup".to_string()), "{calls:?}");

    let summary = last_run(&home.0);
    assert_eq!(summary["available"], 3);
    assert_eq!(summary["updated"], serde_json::json!(["git", "iterm2"]));
    assert_eq!(summary["needs_admin"], serde_json::json!(["zoom"]));
    assert_eq!(summary["failed"], serde_json::json!([]));
    // Needs-admin is not success: non-zero exit so `launchctl print` shows it.
    assert_eq!(out.status.code(), Some(1));

    // Unattended runs use macOS background priority (taskpolicy -c background): brew runs at
    // scheduling priority 4 instead of the normal 31, so a big upgrade doesn't heat the Mac.
    let priorities = read(&home.0.join("upgrade-priority"));
    assert!(
        !priorities.is_empty() && priorities.lines().all(|p| p == "4"),
        "{priorities:?}"
    );
    // The run says when it starts, not just when it's done.
    let log_text = read(&home.0.join(".freshet/run.log"));
    assert!(
        log_text.contains("notify: Updating 3 Homebrew packages in the background."),
        "{log_text}"
    );
    // And it cleans up its "running" marker.
    assert!(!home.0.join(".freshet/scheduled-run.pid").exists());

    // Every attempted package lands in the update history, with its versions and outcome.
    let history: Vec<serde_json::Value> = read(&home.0.join(".freshet/history.jsonl"))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let outcomes: Vec<(String, String, String, String)> = history
        .iter()
        .map(|h| {
            let s = |k: &str| h[k].as_str().unwrap_or_default().to_string();
            (s("id"), s("outcome"), s("from"), s("source"))
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (
                "git".into(),
                "updated".into(),
                "2.49".into(),
                "scheduled".into()
            ),
            (
                "iterm2".into(),
                "updated".into(),
                "3.5".into(),
                "scheduled".into()
            ),
            (
                "zoom".into(),
                "needs_admin".into(),
                "6.0".into(),
                "scheduled".into()
            ),
        ]
    );

    let log = read(&home.0.join(".freshet/run.log"));
    assert!(log.contains("!!! zoom failed:"), "{log}");
    assert!(
        log.contains("notify: Updated 2 packages. 1 needs your password; open Freshet to finish."),
        "{log}"
    );
    assert!(log.ends_with("===== done =====\n"), "{log}");
}

#[test]
fn greedy_setting_includes_self_updating_casks() {
    let home = setup("greedy", r#"{"greedy":true}"#);
    run_scheduled(&home.0);
    assert!(calls(&home.0).contains(&"upgrade --cask iterm2 zoom chrome".to_string()));
}

#[test]
fn notify_only_installs_and_cleans_up_nothing() {
    let home = setup("notify", r#"{"notify_only":true}"#);
    let out = run_scheduled(&home.0);

    let calls = calls(&home.0);
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("upgrade") || c.starts_with("fetch") || c == "cleanup"),
        "{calls:?}"
    );
    assert!(out.status.success());
    let summary = last_run(&home.0);
    assert_eq!(summary["notify_only"], true);
    assert_eq!(summary["available"], 4);
    assert!(read(&home.0.join(".freshet/run.log"))
        .contains("notify: 4 updates available. Notify-only is on, so nothing was installed."));
}
