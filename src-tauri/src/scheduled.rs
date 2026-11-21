// The scheduled run: `freshet --run-scheduled`, invoked daily by launchd.

use crate::check::{brew_update_if_stale, check_homebrew, update_is_stale, versions_of};
use crate::history::{append_history, history_entries};
use crate::model::{CategoryStatus, Kind, ResultAction, UpdateResult, UpdateSelection};
use crate::process::{applescript_string, run_update_cmd, HEADLESS};
use crate::settings::{ignore_key, load_settings, Settings};
use crate::signing::{
    select_verify_results, verify_all_blocking, verify_updated_casks_blocking,
    FULL_SCAN_INTERVAL_SECS,
};
use crate::store::{freshet_dir, unix_now, write_private, write_private_atomic};
use crate::update::{install_selections, provenance_failed};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

// --- The scheduled run: `freshet --run-scheduled`, invoked daily by launchd ---
//
// Same code path as the app: the same checks, the same filters as the visible list (skipped
// versions, self-updating casks unless greedy), the same per-item install, the same signer-drift
// check. Scope is Homebrew (formulae + casks) plus `brew cleanup`.

// What the scheduled run would install: exactly what the app lists for brew + casks. Mirrors
// `allDisplayedSelections` in src/logic.ts. Pure → unit-tested.
fn plan_scheduled_run(categories: &[CategoryStatus], settings: &Settings) -> Vec<UpdateSelection> {
    let skipped: std::collections::HashSet<&str> =
        settings.skipped.iter().map(String::as_str).collect();
    categories
        .iter()
        .filter(|c| c.kind == Kind::Brew || c.kind == Kind::Cask)
        .flat_map(|c| {
            c.items
                .iter()
                .filter(|i| settings.greedy || !i.self_updating)
                .filter(|i| !skipped.contains(ignore_key(c.kind, &i.id, &i.latest).as_str()))
                .map(|i| UpdateSelection {
                    kind: c.kind,
                    id: i.id.clone(),
                    name: i.name.clone(),
                })
        })
        .collect()
}

// sudo's messages when it can't ask for a password (no terminal, no askpass) — what a cask that
// needs admin rights produces in the headless run. Those are "needs you", not "broken".
pub(crate) fn needs_admin(output: &str) -> bool {
    output.contains("a terminal is required to read the password")
        || output.contains("sudo: a password is required")
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct ScheduledRunSummary {
    // Unix seconds.
    pub(crate) finished_at: u64,
    pub(crate) notify_only: bool,
    // Updates found after filtering (what would be / was attempted).
    pub(crate) available: usize,
    pub(crate) updated: Vec<String>,
    // Need an admin password: left for the user to run from the app.
    pub(crate) needs_admin: Vec<String>,
    pub(crate) failed: Vec<String>,
    // Casks whose code-signing Team ID changed with this update.
    pub(crate) signer_changed: Vec<String>,
    // Refused before anything was installed, other than for a changed signer: a formula bottle
    // whose build provenance didn't verify, or a cask download that is unsigned, whose signature
    // doesn't verify, or that couldn't be checked against a pinned publisher.
    pub(crate) blocked: Vec<String>,
    // Left alone because the app was open; picked up by a later run. Not a failure.
    pub(crate) skipped_open: Vec<String>,
}

// Sort install results into the summary's buckets. Pure → unit-tested.
fn summarize_results(summary: &mut ScheduledRunSummary, results: &[UpdateResult]) {
    for r in results {
        if matches!(r.action, Some(ResultAction::TrustSigner { .. })) {
            summary.signer_changed.push(r.name.clone());
        } else if r.blocked || (!r.success && provenance_failed(&r.output)) {
            summary.blocked.push(r.name.clone());
        } else if r.success {
            summary.updated.push(r.name.clone());
        } else if matches!(r.action, Some(ResultAction::QuitAndUpdate { .. })) {
            summary.skipped_open.push(r.name.clone());
        } else if needs_admin(&r.output) {
            summary.needs_admin.push(r.name.clone());
        } else {
            summary.failed.push(r.name.clone());
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// The notification text. Pure → unit-tested.
fn summary_message(s: &ScheduledRunSummary) -> String {
    if s.notify_only {
        return if s.available == 0 {
            "Homebrew is up to date.".into()
        } else {
            format!(
                "{} available. Notify-only is on, so nothing was installed.",
                plural(s.available, "update", "updates")
            )
        };
    }
    let mut parts = Vec::new();
    if !s.signer_changed.is_empty() {
        parts.push(format!("Signer changed: {}.", s.signer_changed.join(", ")));
    }
    if !s.blocked.is_empty() {
        parts.push(format!(
            "Blocked before install (failed a safety check): {}.",
            s.blocked.join(", ")
        ));
    }
    if !s.updated.is_empty() {
        parts.push(format!(
            "Updated {}.",
            plural(s.updated.len(), "package", "packages")
        ));
    }
    if !s.needs_admin.is_empty() {
        parts.push(format!(
            "{} your password; open Freshet to finish.",
            plural(s.needs_admin.len(), "needs", "need")
        ));
    }
    if !s.failed.is_empty() {
        parts.push(format!("{} failed.", s.failed.len()));
    }
    if !s.skipped_open.is_empty() {
        parts.push(format!(
            "Skipped {} (open); {} next time.",
            s.skipped_open.join(", "),
            if s.skipped_open.len() == 1 {
                "it updates"
            } else {
                "they update"
            }
        ));
    }
    if parts.is_empty() {
        "Homebrew is up to date.".into()
    } else {
        parts.join(" ")
    }
}

// Freshet's bundle identifier (tauri.conf.json "identifier").
const BUNDLE_ID: &str = "com.freshet.app";

// A macOS notification shown as coming from Freshet — its name and icon — even from the headless
// scheduled run, where no Freshet window or menu-bar app is running. mac-notification-sys posts it
// on behalf of the installed bundle; if that fails (e.g. a dev build that was never installed),
// fall back to AppleScript, which macOS labels "Script Editor".
// FRESHET_NO_NOTIFY lets the integration tests run the real binary without popping banners.
fn notify(message: &str) {
    if std::env::var_os("FRESHET_NO_NOTIFY").is_some() {
        return;
    }
    if !notify_native(message) {
        let script = format!(
            "display notification {} with title \"Freshet\"",
            applescript_string(message)
        );
        let _ = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output();
    }
}

// True if delivered (or still being delivered after 5s: a synchronous send can block until macOS
// is done with it, and falling back then would show the message twice).
fn notify_native(message: &str) -> bool {
    let message = message.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Errors only if already set by an earlier notification in this process; harmless.
        let _ = mac_notification_sys::set_application(BUNDLE_ID);
        let sent = mac_notification_sys::send_notification("Freshet", None, &message, None);
        if let Err(e) = &sent {
            tracing::warn!(error = %e, "native notification failed; falling back to AppleScript");
        }
        let _ = tx.send(sent.is_ok());
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or(true)
}

// Append-only run log. A log write failing must never abort the run itself.
struct RunLog(Option<std::fs::File>);

impl RunLog {
    fn open(path: &Path) -> Self {
        Self(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .ok(),
        )
    }

    fn line(&mut self, text: &str) {
        if let Some(f) = &mut self.0 {
            let _ = writeln!(f, "{text}");
        }
    }

    // Successful steps log one line; failures include the tool's output so the log is
    // actionable without dumping 65 packages' worth of brew progress.
    fn result(&mut self, r: &UpdateResult) {
        if r.success {
            self.line(&format!(">>> {}: ok", r.name));
        } else {
            self.line(&format!("!!! {} failed:", r.name));
            for l in r.output.lines() {
                self.line(&format!("    {l}"));
            }
        }
    }
}

fn last_run_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("last-run.json"))
}

#[tauri::command]
pub(crate) fn get_last_scheduled_run() -> Option<ScheduledRunSummary> {
    let text = std::fs::read_to_string(last_run_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

// ~/.freshet/scheduled-run.pid exists (holding the run's pid) while a scheduled run is going, so
// the app can say so, hold off its own updates (two brews would fight over Homebrew's locks), and
// refresh itself when the run ends.
fn scheduled_pid_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("scheduled-run.pid"))
}

fn pid_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success())
}

// Running only if the marker names a live process (a crashed run leaves a stale marker).
pub(crate) fn scheduled_run_active() -> bool {
    scheduled_pid_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(pid_alive)
}

#[derive(Serialize)]
pub(crate) struct ScheduledState {
    pub(crate) running: bool,
    pub(crate) last: Option<ScheduledRunSummary>,
}

// Cheap enough to poll (a stat, a tiny file, one `kill -0`).
#[tauri::command(async)]
pub(crate) fn get_scheduled_state() -> ScheduledState {
    ScheduledState {
        running: scheduled_run_active(),
        last: get_last_scheduled_run(),
    }
}

// Returns the process exit code: 0 if everything that was attempted succeeded.
pub(crate) fn run_scheduled() -> i32 {
    HEADLESS.store(true, Ordering::Relaxed);
    let Some(dir) = freshet_dir() else {
        eprintln!("freshet: could not resolve ~/.freshet");
        return 1;
    };
    let marker = dir.join("scheduled-run.pid");
    let _ = write_private(&marker, &std::process::id().to_string(), false);
    let code = run_scheduled_inner(&dir);
    let _ = std::fs::remove_file(&marker);
    code
}

fn run_scheduled_inner(dir: &Path) -> i32 {
    let mut log = RunLog::open(&dir.join("run.log"));
    let started = Command::new("/bin/date")
        .output()
        .map_or_default(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    log.line(&format!("===== scheduled run {started} ====="));
    let settings = load_settings().unwrap_or_default();
    let mut summary = ScheduledRunSummary {
        notify_only: settings.notify_only,
        ..ScheduledRunSummary::default()
    };

    // Metadata only, installs nothing — so it runs in notify-only mode too; otherwise
    // "outdated" would be stale. Skipped if the app already did it within the hour.
    match brew_update_if_stale() {
        Some(update) => {
            log.result(&update);
            if !update.success {
                summary.failed.push(update.name);
            }
        }
        None => log.line(
            ">>> brew update: skipped (ran within the last hour, or another brew update was running)",
        ),
    }

    let categories: [CategoryStatus; 2] = check_homebrew().into();
    for c in &categories {
        if let Some(e) = &c.error {
            log.line(&format!("!!! checking {} failed: {e}", c.category));
            summary.failed.push(format!("{} check", c.category));
        }
    }
    let selections = plan_scheduled_run(&categories, &settings);
    summary.available = selections.len();
    log.line(&format!(
        "{} after skipped versions{}: {}",
        plural(selections.len(), "update", "updates"),
        if settings.greedy {
            ""
        } else {
            " and self-updating apps"
        },
        selections
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    ));

    if settings.notify_only {
        log.line("notify-only is on: nothing installed");
    } else {
        if !selections.is_empty() {
            let message = format!(
                "Updating {} in the background. You can keep working.",
                plural(selections.len(), "Homebrew package", "Homebrew packages")
            );
            log.line(&format!("notify: {message}"));
            notify(&message);
        }
        let results = install_selections(&selections, &|_| {});
        append_history(&history_entries(
            &selections,
            &results,
            &versions_of(&categories),
            "scheduled",
            unix_now(),
        ));
        for r in &results {
            log.result(r);
        }
        summarize_results(&mut summary, &results);

        let cleanup = run_update_cmd("brew", &["cleanup"]);
        let cleanup = UpdateResult {
            kind: Kind::Brew,
            name: "brew cleanup".into(),
            success: cleanup.0,
            output: cleanup.1,
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        };
        log.result(&cleanup);
        if !cleanup.success {
            summary.failed.push(cleanup.name);
        }

        let updated_casks: Vec<String> = selections
            .iter()
            .zip(&results)
            .filter(|(s, r)| s.kind == Kind::Cask && r.success)
            .map(|(s, _)| s.id.clone())
            .collect();
        for drift in verify_updated_casks_blocking(&updated_casks) {
            log.line(&format!(
                "!!! signer changed: {} ({})",
                drift.name, drift.output
            ));
            summary.signer_changed.push(drift.name);
        }
    }

    // Weekly, a full signature scan of /Applications: catches apps changed outside Homebrew
    // (direct downloads, self-updaters) that no update-time check sees.
    let scan_stamp = freshet_dir().map(|d| d.join("full-scan.stamp"));
    let last_scan = scan_stamp
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse().ok());
    if update_is_stale(last_scan, unix_now(), FULL_SCAN_INTERVAL_SECS) {
        let entries = verify_all_blocking(&|_, _| {});
        let drift: Vec<UpdateResult> = select_verify_results(entries, true);
        log.line(&format!(
            ">>> weekly signature scan: {}",
            if drift.is_empty() {
                "no publisher changes".to_string()
            } else {
                format!("{} changed", drift.len())
            }
        ));
        for d in drift {
            log.line(&format!(
                "!!! signer changed: {} ({})",
                d.name,
                d.output.lines().next().unwrap_or("")
            ));
            summary.signer_changed.push(d.name);
        }
        if let Some(p) = scan_stamp {
            let _ = write_private(&p, &unix_now().to_string(), false);
        }
    }

    summary.finished_at = unix_now();
    if let Some(path) = last_run_path() {
        if let Ok(json) = serde_json::to_string_pretty(&summary) {
            let _ = write_private_atomic(&path, &json);
        }
    }
    let message = summary_message(&summary);
    log.line(&format!("notify: {message}"));
    notify(&message);
    log.line("===== done =====");
    i32::from(
        !summary.failed.is_empty()
            || !summary.needs_admin.is_empty()
            || !summary.signer_changed.is_empty()
            || !summary.blocked.is_empty(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::history::outcome_of;
    use crate::model::{CategoryStatus, Kind, ResultAction, UpdateResult, UpdateSelection};

    use crate::settings::{ignore_key, Settings};
    use crate::test_support::{category, item};
    use crate::update::{failed_result, provenance_failed, skipped_open};
    use std::path::Path;

    fn planned_ids(categories: &[CategoryStatus], settings: &Settings) -> Vec<String> {
        plan_scheduled_run(categories, settings)
            .into_iter()
            .map(|s| format!("{}:{}", s.kind, s.id))
            .collect()
    }

    // The scheduled run must install exactly what the app would list: skipped versions and
    // (unless greedy) self-updating casks are left alone, and only brew/cask kinds are in scope.
    #[test]
    fn scheduled_plan_honours_skipped_and_greedy() {
        let categories = [
            category(
                Kind::Brew,
                vec![item("git", "2.50", false), item("node", "24", false)],
            ),
            category(
                Kind::Cask,
                vec![item("iterm2", "3.6", false), item("chrome", "140", true)],
            ),
            category(Kind::Npm, vec![item("typescript", "6", false)]),
        ];
        let mut settings = Settings {
            skipped: vec![ignore_key(Kind::Brew, "node", "24")],
            ..Settings::default()
        };
        assert_eq!(
            planned_ids(&categories, &settings),
            ["brew:git", "cask:iterm2"]
        );

        settings.greedy = true;
        assert_eq!(
            planned_ids(&categories, &settings),
            ["brew:git", "cask:iterm2", "cask:chrome"]
        );
    }

    // A skip is for one version: when a newer release ships, it's planned again.
    #[test]
    fn scheduled_plan_skip_is_per_version() {
        let categories = [category(Kind::Brew, vec![item("node", "25", false)])];
        let settings = Settings {
            skipped: vec![ignore_key(Kind::Brew, "node", "24")],
            ..Settings::default()
        };
        assert_eq!(planned_ids(&categories, &settings), ["brew:node"]);
    }

    // Real sudo output (macOS 26) when it can't prompt: no terminal, no askpass.
    #[test]
    fn needs_admin_matches_real_sudo_output() {
        assert!(needs_admin(
            "==> Upgrading zoom\nsudo: a terminal is required to read the password; either use \
             the -S option to read from standard input or configure an askpass helper\n\
             sudo: a password is required"
        ));
        assert!(needs_admin("sudo: a password is required"));
        assert!(!needs_admin("Error: Download failed: 404"));
    }

    fn result(name: &str, success: bool, output: &str) -> UpdateResult {
        UpdateResult {
            kind: Kind::Cask,
            name: name.into(),
            success,
            output: output.into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        }
    }

    #[test]
    fn results_are_bucketed_for_the_summary() {
        let mut summary = ScheduledRunSummary::default();
        summarize_results(
            &mut summary,
            &[
                result("git", true, ""),
                result("zoom", false, "sudo: a password is required"),
                result("broken", false, "Error: 404"),
            ],
        );
        assert_eq!(summary.updated, ["git"]);
        assert_eq!(summary.needs_admin, ["zoom"]);
        assert_eq!(summary.failed, ["broken"]);
    }

    #[test]
    fn summary_messages() {
        let base = ScheduledRunSummary::default();
        assert_eq!(summary_message(&base), "Homebrew is up to date.");

        let notify = ScheduledRunSummary {
            notify_only: true,
            available: 3,
            ..base.clone()
        };
        assert_eq!(
            summary_message(&notify),
            "3 updates available. Notify-only is on, so nothing was installed."
        );

        let mixed = ScheduledRunSummary {
            updated: vec!["a".into(), "b".into()],
            needs_admin: vec!["zoom".into()],
            failed: vec!["x".into()],
            signer_changed: vec!["Evil.app".into()],
            ..base
        };
        assert_eq!(
            summary_message(&mixed),
            "Signer changed: Evil.app. Updated 2 packages. 1 needs your password; open Freshet \
             to finish. 1 failed."
        );
    }

    // Real notification through Notification Center, as the scheduled run sends it.
    //   cargo test -- --ignored native_notification --nocapture
    #[test]
    #[ignore = "shows a real notification"]
    fn native_notification() {
        assert!(notify_native(
            "Test notification from Freshet's test suite."
        ));
    }

    // Homebrew's own wording (utils/attestation.rb) for a bottle that fails verification.
    #[test]
    fn provenance_failures_count_as_blocked() {
        let out = "Error: The bottle for xz has an invalid build provenance attestation.\n\nThis may indicate that the bottle was not produced by the expected\ntap, or was maliciously inserted into the expected tap's bottle\nstorage.";
        assert!(provenance_failed(out));
        assert!(!provenance_failed("Error: Download failed: 404"));
        let r = UpdateResult {
            kind: Kind::Brew,
            name: "xz".into(),
            success: false,
            output: out.into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        };
        assert_eq!(outcome_of(&r), "blocked");
        let mut summary = ScheduledRunSummary::default();
        summarize_results(&mut summary, &[r]);
        assert_eq!(summary.blocked, ["xz"]);
        assert_eq!(summary.failed, Vec::<String>::new());
        assert!(summary_message(&summary)
            .contains("Blocked before install (failed a safety check): xz"));
    }

    #[test]
    fn blocked_installs_count_as_signer_changes_in_the_summary() {
        let mut summary = ScheduledRunSummary::default();
        summarize_results(
            &mut summary,
            &[UpdateResult {
                kind: Kind::Cask,
                name: "discord".into(),
                success: false,
                output: "Blocked before install".into(),
                action: Some(ResultAction::TrustSigner {
                    bundle_id: "com.hnc.Discord".into(),
                    team: "EVILEVIL99".into(),
                }),
                elapsed_ms: None,
                skipped: false,
                steps: Vec::new(),
                blocked: false,
            }],
        );
        assert_eq!(summary.signer_changed, ["discord"]);
        assert_eq!(summary.failed, Vec::<String>::new());
    }

    // An app that was open is left for next time: reported, but not a failure.
    #[test]
    fn open_apps_are_skipped_not_failed_in_the_summary() {
        let sel = UpdateSelection {
            kind: Kind::Cask,
            id: "slack".into(),
            name: "slack".into(),
        };
        let mut summary = ScheduledRunSummary::default();
        summarize_results(
            &mut summary,
            &[skipped_open(&sel, Path::new("/Applications/Slack.app"))],
        );
        assert_eq!(summary.skipped_open, ["slack"]);
        assert_eq!(summary.failed, Vec::<String>::new());
        assert_eq!(
            summary_message(&summary),
            "Skipped slack (open); it updates next time."
        );
        // Summaries written before this field existed still load.
        let old: ScheduledRunSummary =
            serde_json::from_str(r#"{"finished_at":1,"failed":["x"]}"#).unwrap();
        assert_eq!(old.skipped_open, Vec::<String>::new());
    }

    // A block with no publisher to trust (unsigned, forged, or uncheckable for a pinned app) is
    // still a block everywhere it's reported, not an ordinary failure.
    #[test]
    fn publisher_check_blocks_are_reported_as_blocked_even_without_an_action() {
        let r = UpdateResult {
            blocked: true,
            ..failed_result(
                &UpdateSelection {
                    kind: Kind::Cask,
                    id: "zoom".into(),
                    name: "zoom".into(),
                },
                "Blocked before install: the new zoom download isn't code-signed".into(),
            )
        };
        assert_eq!(outcome_of(&r), "blocked");
        let mut summary = ScheduledRunSummary::default();
        summarize_results(&mut summary, std::slice::from_ref(&r));
        assert_eq!(summary.blocked, ["zoom"]);
        assert_eq!(summary.failed, Vec::<String>::new());
        assert_eq!(
            summary_message(&summary),
            "Blocked before install (failed a safety check): zoom."
        );
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["blocked"], true);
        // Only sent when set.
        let plain = failed_result(
            &UpdateSelection {
                kind: Kind::Brew,
                id: "x".into(),
                name: "x".into(),
            },
            "Error: 404".into(),
        );
        assert!(serde_json::to_value(&plain)
            .unwrap()
            .get("blocked")
            .is_none());
    }
}
