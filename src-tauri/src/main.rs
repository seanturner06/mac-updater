// src-tauri/src/main.rs
// Backend commands for Freshet
// - check_outdated: queries each package manager for what's outdated
// - run_updates: invokes the upgrade commands
// - install_schedule / uninstall_schedule / get_schedule: manages the daily launchd job
// - get_settings / save_settings: ~/.freshet/settings.json, shared with the scheduled run
// - `freshet --run-scheduled`: the headless run that launchd invokes (see run_scheduled)

use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use tauri::Emitter;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tauri_plugin_positioner::{Position, WindowExt as _};

mod preverify;
mod rollback;
mod vulns;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct OutdatedItem {
    // Identifier used to run the per-item update: formula/cask token, mas app id, or npm package name.
    id: String,
    name: String,
    current: String,
    latest: String,
    // True for casks that update themselves (only surfaced via `brew outdated --cask --greedy`).
    // The frontend hides these unless the user opts in. Always false for non-cask items.
    self_updating: bool,
    // Release-notes URL when the source provides one (Sparkle appcasts). Empty otherwise.
    #[serde(default)]
    notes_url: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct CategoryStatus {
    category: String,
    // Stable update kind the frontend echoes back in selections: "brew" | "cask" | "mas" | "npm".
    kind: String,
    items: Vec<OutdatedItem>,
    error: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
struct UpdateSelection {
    kind: String,
    id: String,
    name: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
struct UpdateResult {
    kind: String,
    name: String,
    success: bool,
    output: String,
    // A follow-up the user can take from the results panel (trust a new signer, roll back).
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<ResultAction>,
    // Wall time for this item; batched items share their batch's time.
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_ms: Option<u64>,
    // Deliberately not attempted (the app was open, or the user stopped the run): neither a
    // success nor a failure.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    skipped: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResultAction {
    // The publisher (Team ID) changed; trusting records `team` as the new baseline for `bundle_id`.
    TrustSigner {
        bundle_id: String,
        team: String,
    },
    // A pre-update snapshot of this cask's app exists and can be restored.
    Rollback {
        token: String,
    },
    // Skipped because the app was open: quit it, then update just this item.
    QuitAndUpdate {
        kind: String,
        id: String,
        name: String,
        app: String,
    },
}

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
fn augment_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    if path_has_brew_prefix(&current) {
        return;
    }
    let shell = login_shell_path().unwrap_or_default();
    let merged = merge_path(&current, shell.split(':').chain(BREW_BIN_DIRS));
    std::env::set_var("PATH", merged);
}

// --- Stopping a running update safely ---
//
// The batch is one `brew` process. Killing it outright (what quitting the app used to do) skips
// Homebrew's cleanup. SIGINT — Ctrl-C — doesn't: brew raises Interrupt, removes a half-poured
// new version, and its `ensure` relinks the old one (Homebrew install.rb `install_formula`,
// formula_installer.rb `pour`). So "Stop" waits for the next package boundary and sends SIGINT
// there; everything already upgraded stays upgraded, the package that was about to start
// doesn't, and nothing is left half-installed.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
// pid of the update process currently streaming output (0 = none).
static RUNNING_CHILD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

// SIGINT first. A process can start with SIGINT *ignored* (inherited from whatever launched us,
// e.g. any background job) and then can't un-ignore it; if it hasn't exited 3s later, send one
// SIGTERM, which Ruby turns into a SignalException that unwinds through the same cleanup and
// relink code. One signal only: Homebrew guards its cleanup against a second interrupt.
fn interrupt_running_child() {
    let pid = RUNNING_CHILD.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    // The child leads its own process group (see run_update_cmd_streaming), so `-pid` signals it
    // and everything it started (curl downloads, installers) — what Ctrl-C does in a terminal —
    // and nothing is left holding the output pipe open.
    let signal = move |sig: &str| {
        tracing::info!(pid, sig, "signalling update process group");
        let _ = Command::new("/bin/kill")
            .args([sig, "--", &format!("-{pid}")])
            .output();
    };
    signal("-INT");
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if RUNNING_CHILD.load(Ordering::SeqCst) == pid {
            signal("-TERM");
        }
    });
}

// "Stop after this package": takes effect at the next package boundary.
#[tauri::command]
fn stop_updates() {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

// A line where brew moves on to the next package — the safe place to interrupt. Pure → tested.
fn is_package_boundary(line: &str) -> bool {
    (line.starts_with("==> Upgrading ") && !line.contains("outdated package"))
        || line.starts_with("==> Installing ")
}

// Set by the headless scheduled run. Nobody is at the keyboard, so update commands must never
// put up the askpass password dialog; anything needing admin fails fast and is reported instead.
static HEADLESS: AtomicBool = AtomicBool::new(false);

// --- Helper: run a shell command and capture stdout ---
// A Command for `cmd`. In the headless scheduled run it's wrapped in `taskpolicy -c background`:
// macOS then keeps it (and everything it starts — brew's curl, codesign, spctl) on the efficiency
// cores at low priority. Slower, but nobody is waiting on an unattended run, and it's what keeps
// a 40-package upgrade from heating the Mac up while you work.
fn command(cmd: &str) -> Command {
    if HEADLESS.load(Ordering::Relaxed) {
        let mut c = Command::new("/usr/sbin/taskpolicy");
        c.args(["-c", "background", cmd]);
        c
    } else {
        Command::new(cmd)
    }
}

fn run_cmd(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = command(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {cmd}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // Some tools (npm outdated) exit non-zero when items are outdated; treat stdout as truth
        if output.stdout.is_empty() {
            return Err(stderr);
        }
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ~/.freshet, created 0700 (owner-only) so no other local user can read or tamper with the
// askpass helper, the scheduled run script, or the signing baseline that live inside it.
fn freshet_dir() -> Option<PathBuf> {
    let mut p = dirs::home_dir()?;
    p.push(".freshet");
    std::fs::create_dir_all(&p).ok()?;
    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
    Some(p)
}

// Write a file owner-only (0700 for executables / 0600 for data is enough; the dir is already 0700).
fn write_private(path: &Path, contents: &str, executable: bool) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    let mode = if executable { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

// Lazily write a GUI askpass helper and return its path. When SUDO_ASKPASS points at this and
// no terminal is available (the packaged app), sudo shows a native macOS password dialog instead
// of prompting on a hidden terminal. Written once per process.
fn askpass_program() -> Option<&'static str> {
    static ASKPASS: OnceLock<Option<String>> = OnceLock::new();
    ASKPASS
        .get_or_init(|| {
            let mut path = freshet_dir()?;
            path.push("askpass.sh");
            let script = r#"#!/bin/bash
osascript -e 'display dialog "Freshet needs your administrator password to update one or more apps." with title "Freshet" default answer "" with hidden answer with icon caution' -e 'text returned of result' 2>/dev/null
"#;
            write_private(&path, script, true).ok()?;
            Some(path.to_str()?.to_string())
        })
        .as_deref()
}

// mas can't update App Store apps acquired under a different Apple ID (or installed outside the
// App Store). Translate its cryptic "No downloads initiated" error into something actionable.
fn maybe_append_mas_hint(mut output: String) -> String {
    if output.contains("No downloads initiated for ADAM ID") {
        output.push_str(
            "\n\nThis App Store app was purchased under a different Apple ID, or installed \
             outside the App Store, so it can't be updated here. Update it via the app's own \
             updater (e.g. Microsoft AutoUpdate: Help → Check for Updates), or delete it and \
             reinstall from the App Store while signed in with your Apple ID.",
        );
    }
    output
}

// Merge a process's stdout and stderr into one log blob.
fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = String::from_utf8_lossy(stdout).to_string();
    let err = String::from_utf8_lossy(stderr);
    if !err.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
    combined
}

// --- Helper: run an upgrade command, returning whether it succeeded plus combined output ---
fn update_command(cmd: &str, args: &[&str]) -> Command {
    let mut command = command(cmd);
    command.args(args);
    if HEADLESS.load(Ordering::Relaxed) {
        command.env_remove("SUDO_ASKPASS");
    } else if let Some(askpass) = askpass_program() {
        command.env("SUDO_ASKPASS", askpass);
    }
    command
}

// Like run_update_cmd, but stdout and stderr share one pipe (as in a terminal), so a batch's
// error lines stay next to the package they belong to; and each line is handed to `on_line` as
// it arrives, so progress can be reported while a long batch runs.
fn run_update_cmd_streaming(
    cmd: &str,
    args: &[&str],
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    use std::io::BufRead as _;
    let mut run = || -> std::io::Result<(bool, String)> {
        let (reader, writer) = std::io::pipe()?;
        let mut command = update_command(cmd, args);
        command
            .stdin(std::process::Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            // Its own process group, so Stop can signal brew and all its children together.
            .process_group(0);
        let mut child = command.spawn()?;
        RUNNING_CHILD.store(child.id(), Ordering::SeqCst);
        // The Command still holds write ends of the pipe; drop it so EOF arrives at child exit.
        drop(command);
        let mut reader = std::io::BufReader::new(reader);
        let mut all = String::new();
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf)? > 0 {
            let line = String::from_utf8_lossy(&buf);
            on_line(line.trim_end());
            all.push_str(&line);
            buf.clear();
        }
        let status = child.wait();
        RUNNING_CHILD.store(0, Ordering::SeqCst);
        Ok((status?.success(), all.trim().to_string()))
    };
    run().unwrap_or_else(|e| (false, format!("Failed to run {cmd}: {e}")))
}

// The requested package a `==> Upgrading <name>` line starts on, if any. Pure → unit-tested.
fn upgrading_name<'a>(line: &str, wanted: &std::collections::HashSet<&'a str>) -> Option<&'a str> {
    let name = line
        .strip_prefix("==> Upgrading ")?
        .split_whitespace()
        .next()?;
    wanted.get(name).copied()
}

fn run_update_cmd(cmd: &str, args: &[&str]) -> (bool, String) {
    let mut command = update_command(cmd, args);
    match command.output() {
        Ok(out) => {
            let combined = maybe_append_mas_hint(combine_output(&out.stdout, &out.stderr));
            (out.status.success(), combined.trim().to_string())
        }
        Err(e) => (false, format!("Failed to run {cmd}: {e}")),
    }
}

// --- Pure parsers (no I/O) so they can be unit-tested against real CLI output ---

fn parse_brew_outdated(stdout: &str) -> Vec<OutdatedItem> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|_| serde_json::json!({}));
    parsed["formulae"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let name = f["name"].as_str().unwrap_or("").to_string();
            OutdatedItem {
                id: name.clone(),
                name,
                current: f["installed_versions"][0]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                latest: f["current_version"].as_str().unwrap_or("").to_string(),
                self_updating: false,
                notes_url: String::new(),
            }
        })
        .collect()
}

fn parse_cask_names(stdout: &str) -> std::collections::HashSet<String> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|_| serde_json::json!({}));
    parsed["casks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c["name"].as_str().map(String::from))
        .collect()
}

// Parse the greedy cask list, tagging self_updating = present in greedy but not in the normal set.
fn parse_casks(stdout: &str, normal: &std::collections::HashSet<String>) -> Vec<OutdatedItem> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|_| serde_json::json!({}));
    parsed["casks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let name = c["name"].as_str().unwrap_or("").to_string();
            let self_updating = !normal.contains(&name);
            OutdatedItem {
                id: name.clone(),
                name,
                current: c["installed_versions"][0]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                latest: c["current_version"].as_str().unwrap_or("").to_string(),
                self_updating,
                notes_url: String::new(),
            }
        })
        .collect()
}

// mas outdated lines look like: "462058435  Microsoft Excel       (16.71 -> 16.109.2)".
// Note the id is followed by multiple spaces and the name itself contains spaces, so naive
// space-splitting yields an empty name — parse the id token and the trailing "(old -> new)".
fn parse_mas_outdated(stdout: &str) -> Vec<OutdatedItem> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let line = line.trim();
            let ws = line.find(char::is_whitespace)?;
            let id = line[..ws].to_string();
            let rest = line[ws..].trim();
            let (name, current, latest) = rest.rfind('(').map_or_else(
                || (rest.to_string(), String::new(), String::new()),
                |idx| {
                    let name = rest[..idx].trim().to_string();
                    let inner = rest[idx..].trim_matches(['(', ')']);
                    let mut vers = inner.split("->").map(str::trim);
                    let current = vers.next().unwrap_or("").to_string();
                    let latest = vers.next().unwrap_or("").to_string();
                    (name, current, latest)
                },
            );
            Some(OutdatedItem {
                id,
                name,
                current,
                latest,
                self_updating: false,
                notes_url: String::new(),
            })
        })
        .collect()
}

fn parse_npm_outdated(stdout: &str) -> Vec<OutdatedItem> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).unwrap_or_else(|_| serde_json::json!({}));
    let obj = parsed.as_object().cloned().unwrap_or_default();
    obj.iter()
        .filter_map(|(name, info)| {
            let current = info["current"].as_str().unwrap_or("").to_string();
            let latest = info["latest"].as_str().unwrap_or("").to_string();
            // npm flags a package as outdated whenever current != latest, including when the
            // installed version is *newer* than the registry's latest (e.g. corepack, pnpm).
            // Updating those would be a downgrade and they'd never clear, so skip them.
            if let (Ok(c), Ok(l)) = (Version::parse(&current), Version::parse(&latest)) {
                if c >= l {
                    return None;
                }
            }
            Some(OutdatedItem {
                id: name.clone(),
                name: name.clone(),
                current,
                latest,
                self_updating: false,
                notes_url: String::new(),
            })
        })
        .collect()
}

// Lenient dotted-numeric version compare — Sparkle shortVersionStrings (e.g. "2.9", "32.1.2")
// aren't always valid 3-part semver, so we compare numeric components directly.
fn version_is_older(installed: &str, latest: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        s.split(['.', '-', '+'])
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect()
    }
    let a = parts(installed);
    let b = parts(latest);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x < y;
        }
    }
    false
}

// Find the newest version advertised in a Sparkle appcast. Versions appear either as
// <sparkle:shortVersionString>X</...> elements or sparkle:shortVersionString="X" attributes;
// items aren't guaranteed ordered, so take the max.
fn parse_appcast_latest(xml: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    for (open, close) in [
        ("<sparkle:shortVersionString>", "<"),
        ("sparkle:shortVersionString=\"", "\""),
    ] {
        let mut rest = xml;
        while let Some(i) = rest.find(open) {
            let after = &rest[i + open.len()..];
            match after.find(close) {
                Some(end) => {
                    let v = after[..end].trim();
                    if !v.is_empty() {
                        candidates.push(v.to_string());
                    }
                    rest = &after[end..];
                }
                None => break,
            }
        }
    }
    let mut best: Option<String> = None;
    for c in candidates {
        best = match best {
            Some(b) if version_is_older(&b, &c) => Some(c),
            Some(b) => Some(b),
            None => Some(c),
        };
    }
    best
}

// The release-notes URL from the first item's <sparkle:releaseNotesLink>, if present.
fn parse_appcast_notes(xml: &str) -> Option<String> {
    let open = "<sparkle:releaseNotesLink>";
    let i = xml.find(open)?;
    let after = &xml[i + open.len()..];
    let end = after.find('<')?;
    let url = after[..end].trim();
    if is_safe_url(url) {
        Some(url.to_string())
    } else {
        None
    }
}

// Parse cask tokens from plain-text `brew list --cask` / `brew search --cask` output: one token
// per line. Skips "==> Casks" headers and any prose lines (real tokens contain no whitespace).
fn parse_cask_tokens(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("==>") && !l.contains(char::is_whitespace))
        .map(String::from)
        .collect()
}

// --- Input validation (defense-in-depth) ---
// Commands receive these strings from the webview; validate at the boundary so a crafted value
// (e.g. a leading "-") can't be smuggled in as a flag to brew/open, even though we never use a shell.

fn is_valid_cask_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 128
        && token
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'@' | b'.' | b'_' | b'+'))
}

fn is_safe_search_query(query: &str) -> bool {
    let q = query.trim();
    !q.is_empty() && q.len() <= 64 && !q.starts_with('-') && q.chars().all(|c| !c.is_control())
}

fn is_safe_url(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://"))
        && url.len() <= 2048
        && url.chars().all(|c| !c.is_control() && c != ' ')
}

// Pull the host out of an http(s) URL (no scheme, no port, no path, no userinfo). Returns
// None for anything that doesn't look like an http(s) URL with a non-empty host. Hand-rolled
// because std doesn't ship a URL parser and a dep just for this is overkill.
fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // Strip any "user:pass@" prefix.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literals look like "[::1]" or "[fe80::1]:443".
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        let close = stripped.find(']')?;
        &stripped[..close]
    } else {
        // IPv4 / DNS — strip the optional ":port".
        host_port.split(':').next().unwrap_or(host_port)
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

// Hostnames and IP ranges we refuse to talk to when fetching app-supplied URLs (Sparkle feeds).
// Mitigates SSRF from a hostile local app that points its SUFeedURL at internal infrastructure
// (e.g. 169.254.169.254 cloud-metadata, 192.168/16 routers, localhost services). DNS rebinding
// can bypass an IP-literal check; we accept that as a known limitation.
// `.local` / `.internal` are mDNS / private hostname suffixes (RFC 6762), not file extensions —
// clippy's case_sensitive_file_extension_comparisons false-positives here. Callers pass a
// lowercased host from url_host(), so a plain ends_with is correct.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_local_or_private_host(host: &str) -> bool {
    if host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".localhost")
        || host.ends_with(".internal")
    {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return ip.is_private()
            || ip.is_loopback()
            || ip.is_link_local()
            || ip.is_unspecified()
            || ip.is_broadcast()
            // Carrier-grade NAT (RFC 6598) — not in std's helpers.
            || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64);
    }
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        let first = ip.segments()[0];
        return ip.is_loopback()
            || ip.is_unspecified()
            // Unique-local fc00::/7
            || (first & 0xfe00) == 0xfc00
            // Link-local fe80::/10
            || (first & 0xffc0) == 0xfe80;
    }
    false
}

// Stricter than is_safe_url: used for app-supplied Sparkle feed URLs that we (Freshet, not the
// user's browser) will fetch over the network. https only and no internal/private hosts.
fn is_safe_feed_url(url: &str) -> bool {
    if !url.starts_with("https://") || url.len() > 2048 {
        return false;
    }
    if url.chars().any(|c| c.is_control() || c == ' ') {
        return false;
    }
    url_host(url).is_some_and(|host| !is_local_or_private_host(&host))
}

// Mac App Store ADAM IDs are numeric.
fn is_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
}

// Validate an npm package name so a selection can't smuggle a flag or an arbitrary install spec
// (e.g. a git/tarball URL) into `npm install -g`.
fn is_valid_npm_package(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    if name.starts_with('-') || name.starts_with('.') || name.starts_with('_') {
        return false;
    }
    if name.contains("://") {
        return false;
    }
    // Scoped packages are "@scope/name"; allow that single leading '@'.
    let body = name.strip_prefix('@').unwrap_or(name);
    body.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.' | b'/')
    })
}

// --- Check functions (thin I/O wrappers around the pure parsers above) ---

// Formulae and casks from two `brew outdated` processes (each pays ~0.5s of Ruby start-up), run
// in parallel. The --greedy call lists formulae plus every outdated cask, self-updating ones
// included; the plain cask call tells us which of those are *not* self-updating. Casks are tagged
// self_updating so the frontend can hide them without a re-fetch.
#[tracing::instrument(skip_all)]
fn check_homebrew() -> (CategoryStatus, CategoryStatus) {
    let (greedy, normal) = std::thread::scope(|scope| {
        let normal = scope.spawn(|| run_cmd("brew", &["outdated", "--cask", "--json=v2"]));
        let greedy = run_cmd("brew", &["outdated", "--json=v2", "--greedy"]);
        (
            greedy,
            normal
                .join()
                .unwrap_or_else(|_| Err("brew check panicked".into())),
        )
    });
    match greedy {
        Ok(stdout) => {
            let normal = normal.map(|n| parse_cask_names(&n)).unwrap_or_default();
            (
                CategoryStatus {
                    category: "Homebrew".into(),
                    kind: "brew".into(),
                    items: parse_brew_outdated(&stdout),
                    error: None,
                },
                CategoryStatus {
                    category: "Applications".into(),
                    kind: "cask".into(),
                    items: parse_casks(&stdout, &normal),
                    error: None,
                },
            )
        }
        Err(e) => (
            failed_category_err("Homebrew", "brew", e.clone()),
            failed_category_err("Applications", "cask", e),
        ),
    }
}

fn check_mas() -> CategoryStatus {
    match run_cmd("mas", &["outdated"]) {
        Ok(stdout) => CategoryStatus {
            category: "App Store".into(),
            kind: "mas".into(),
            items: parse_mas_outdated(&stdout),
            error: None,
        },
        Err(e) => failed_category_err("App Store", "mas", e),
    }
}

fn check_npm() -> CategoryStatus {
    // npm outdated -g returns JSON of outdated globals; exits 1 when there are any
    match run_cmd("npm", &["outdated", "-g", "--json"]) {
        Ok(stdout) => CategoryStatus {
            category: "npm globals".into(),
            kind: "npm".into(),
            items: parse_npm_outdated(&stdout),
            error: None,
        },
        Err(e) => failed_category_err("npm globals", "npm", e),
    }
}

fn failed_category_err(category: &str, kind: &str, error: String) -> CategoryStatus {
    CategoryStatus {
        category: category.into(),
        kind: kind.into(),
        items: vec![],
        error: Some(error),
    }
}

fn read_plist_string(info_path: &str, key: &str) -> Option<String> {
    let out = Command::new("defaults")
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

fn fetch_url(url: &str) -> Option<String> {
    // https-only and no private/loopback host, enforced again at the fetch boundary (not just at
    // the caller): a plaintext feed can be MITM'd, and a redirect to http would silently downgrade.
    if !is_safe_feed_url(url) {
        return None;
    }
    let out = Command::new("curl")
        // https only, on the initial request AND across every redirect hop — no plaintext fallback,
        // no escape to file:// or other schemes. TLS cert validation stays on (we never pass -k).
        .args([
            "-sL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "8",
            // Cap the body a hostile feed can make us buffer. Best-effort: curl can only
            // enforce this when the server declares a Content-Length, but appcasts are tiny
            // and a well-behaved CDN always sends one.
            "--max-filesize",
            "5242880",
            url,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

// --- Detect updates for apps that ship a Sparkle appcast (the "Latest" approach) ---
// These apps update themselves, so this is detection-only; the frontend shows them without a
// checkbox. Feeds are fetched in parallel since each is a network round-trip.
// The handles `.collect::<Vec<_>>()` below is intentional: without it, spawn+join interleaves
// and the network fetches run serially instead of in parallel.
#[allow(clippy::needless_collect)]
fn check_sparkle() -> CategoryStatus {
    let feeds: Vec<(String, String, String)> = list_applications()
        .iter()
        .filter_map(|path| {
            let info = format!("{}/Contents/Info", path.to_str()?);
            let feed = read_plist_string(&info, "SUFeedURL")?;
            // Don't follow a hostile app's appcast into private/loopback hosts.
            if !is_safe_feed_url(&feed) {
                return None;
            }
            let current =
                read_plist_string(&info, "CFBundleShortVersionString").unwrap_or_default();
            let name = path.file_stem()?.to_str()?.to_string();
            Some((name, feed, current))
        })
        .collect();

    let items: Vec<OutdatedItem> = std::thread::scope(|s| {
        let handles: Vec<_> = feeds
            .iter()
            .map(|(name, feed, current)| {
                s.spawn(move || {
                    let xml = fetch_url(feed)?;
                    let latest = parse_appcast_latest(&xml)?;
                    if !current.is_empty() && version_is_older(current, &latest) {
                        Some(OutdatedItem {
                            id: name.clone(),
                            name: name.clone(),
                            current: current.clone(),
                            latest,
                            self_updating: false,
                            notes_url: parse_appcast_notes(&xml).unwrap_or_default(),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });

    CategoryStatus {
        category: "Other apps".into(),
        kind: "sparkle".into(),
        items,
        error: None,
    }
}

fn failed_category(category: &str, kind: &str) -> CategoryStatus {
    CategoryStatus {
        category: category.into(),
        kind: kind.into(),
        items: vec![],
        error: Some("check failed".into()),
    }
}

// --- The Tauri command the frontend calls ---
// Runs off the main thread (so the UI stays responsive) and the four checks run in parallel.
// Casks are always fetched with --greedy and tagged self_updating; the frontend filters those
// client-side so the "include self-updating apps" toggle is instant (no re-fetch).
// The fast, local checks. Sparkle (network) is intentionally NOT here — it's fetched separately
// by check_sparkle_apps so a slow/unreachable appcast can't stall the whole refresh.
#[tauri::command]
async fn check_outdated() -> Vec<CategoryStatus> {
    tauri::async_runtime::spawn_blocking(check_outdated_blocking)
        .await
        .unwrap_or_default()
}

#[tracing::instrument]
fn check_outdated_blocking() -> Vec<CategoryStatus> {
    // Without a recent `brew update`, `brew outdated` compares against stale metadata and misses
    // new releases. Throttled to once an hour, so most checks pay nothing for it.
    if let Some(r) = brew_update_if_stale() {
        if !r.success {
            tracing::warn!(output = %r.output, "brew update failed; checking against cached metadata");
        }
    }
    let ((brew, casks), mas, npm) = std::thread::scope(|scope| {
        let mas = scope.spawn(check_mas);
        let npm = scope.spawn(check_npm);
        let homebrew = check_homebrew();
        (
            homebrew,
            mas.join()
                .unwrap_or_else(|_| failed_category("App Store", "mas")),
            npm.join()
                .unwrap_or_else(|_| failed_category("npm globals", "npm")),
        )
    });
    let categories = vec![brew, casks, mas, npm];
    save_cached_check(&categories);
    categories
}

// --- Last check, cached so the window can show it instantly while a fresh check runs ---

#[derive(Serialize, Deserialize)]
struct CachedCheck {
    // Unix seconds.
    checked_at: u64,
    categories: Vec<CategoryStatus>,
}

fn cached_check_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("last-check.json"))
}

fn save_cached_check(categories: &[CategoryStatus]) {
    let cached = CachedCheck {
        checked_at: unix_now(),
        categories: categories.to_vec(),
    };
    if let (Some(path), Ok(json)) = (cached_check_path(), serde_json::to_string(&cached)) {
        let _ = write_private_atomic(&path, &json);
    }
}

// The saved list is only worth showing if nothing has changed since: a scheduled run that finished
// after it was saved has installed some of what it lists. Pure → unit-tested.
fn cache_is_current(checked_at: u64, last_scheduled_run: Option<u64>) -> bool {
    last_scheduled_run.is_none_or(|finished| finished < checked_at)
}

#[tauri::command]
fn get_cached_check() -> Option<CachedCheck> {
    let text = std::fs::read_to_string(cached_check_path()?).ok()?;
    let cached: CachedCheck = serde_json::from_str(&text).ok()?;
    let last_run = get_last_scheduled_run().map(|r| r.finished_at);
    cache_is_current(cached.checked_at, last_run).then_some(cached)
}

// --- `brew update`, at most once an hour ---

const BREW_UPDATE_MAX_AGE_SECS: u64 = 3600;
const FULL_SCAN_INTERVAL_SECS: u64 = 7 * 86_400;

fn brew_update_stamp_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("brew-update.stamp"))
}

// Stale if never run, older than max_age, or stamped in the future (clock change). Pure → tested.
fn update_is_stale(last: Option<u64>, now: u64, max_age: u64) -> bool {
    last.is_none_or(|l| l > now || now - l >= max_age)
}

// Runs `brew update` if the last successful one is over an hour old; None if skipped.
fn brew_update_if_stale() -> Option<UpdateResult> {
    let stamp = brew_update_stamp_path();
    let last = stamp
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse().ok());
    if !update_is_stale(last, unix_now(), BREW_UPDATE_MAX_AGE_SECS) {
        tracing::info!("brew update skipped: ran within the last hour");
        return None;
    }
    let started = std::time::Instant::now();
    let (success, output) = run_update_cmd("brew", &["update"]);
    let elapsed_ms = elapsed_ms(started);
    tracing::info!(elapsed_ms, success, "brew update");
    if success {
        if let Some(p) = stamp {
            let _ = write_private(&p, &unix_now().to_string(), false);
        }
    }
    Some(UpdateResult {
        kind: "brew".into(),
        name: "brew update".into(),
        success,
        output,
        action: None,
        elapsed_ms: Some(elapsed_ms),
        skipped: false,
    })
}

fn elapsed_ms(since: std::time::Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

// Network-bound Sparkle check, loaded separately/in the background by the frontend.
#[tauri::command]
async fn check_sparkle_apps() -> CategoryStatus {
    tauri::async_runtime::spawn_blocking(check_sparkle)
        .await
        .unwrap_or_else(|_| failed_category("Other apps", "sparkle"))
}

// --- Homebrew adoption: bring a manually-installed app under Homebrew management ---

#[derive(Serialize)]
struct CaskSuggestion {
    token: String,
    installed: bool,
}

// Search Homebrew casks by name; flag ones already installed via brew so the UI can mark them.
#[tauri::command]
async fn search_casks(query: String) -> Vec<CaskSuggestion> {
    tauri::async_runtime::spawn_blocking(move || {
        if !is_safe_search_query(&query) {
            return vec![];
        }
        let installed: std::collections::HashSet<String> = run_cmd("brew", &["list", "--cask"])
            .map(|s| parse_cask_tokens(&s).into_iter().collect())
            .unwrap_or_default();
        run_cmd("brew", &["search", "--cask", query.trim()]).map_or_else(
            |_| vec![],
            |out| {
                parse_cask_tokens(&out)
                    .into_iter()
                    .map(|token| CaskSuggestion {
                        installed: installed.contains(&token),
                        token,
                    })
                    .collect()
            },
        )
    })
    .await
    .unwrap_or_default()
}

#[derive(Serialize)]
struct AdoptableApp {
    name: String,
    token: String,
}

#[derive(Serialize)]
struct CaskInfo {
    token: String,
    name: String,
    desc: String,
    homepage: String,
    version: String,
    auto_updates: bool,
}

// Pull the user-facing fields out of `brew info --cask --json=v2 <token>` (note `name` is an array).
fn parse_cask_info(json: &str) -> Option<CaskInfo> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let c = v["casks"].as_array()?.first()?;
    Some(CaskInfo {
        token: c["token"].as_str().unwrap_or("").to_string(),
        name: c["name"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        desc: c["desc"].as_str().unwrap_or("").to_string(),
        homepage: c["homepage"].as_str().unwrap_or("").to_string(),
        version: c["version"].as_str().unwrap_or("").to_string(),
        auto_updates: c["auto_updates"].as_bool().unwrap_or(false),
    })
}

#[tauri::command]
async fn cask_info(token: String) -> Option<CaskInfo> {
    if !is_valid_cask_token(&token) {
        return None;
    }
    tauri::async_runtime::spawn_blocking(move || {
        run_cmd("brew", &["info", "--cask", "--json=v2", token.as_str()])
            .ok()
            .and_then(|s| parse_cask_info(&s))
    })
    .await
    .ok()
    .flatten()
}

// --- Package details for the update list: what is this thing, and where are its release notes ---

#[derive(Serialize, Clone, Debug, PartialEq, Eq, Default)]
struct PackageInfo {
    desc: String,
    homepage: String,
    license: String,
    // Formula installed only to satisfy another formula — not something the user asked for.
    dependency: bool,
    // Cask that updates itself (Homebrew's `auto_updates`).
    auto_updates: bool,
    // GitHub releases page when the project lives on GitHub; empty otherwise.
    release_notes: String,
    // The cask's .app bundle name, used to look up its icon.
    app: String,
}

// "https://github.com/owner/repo(.git)(/anything)" → "https://github.com/owner/repo/releases".
// First GitHub URL wins. Pure → unit-tested.
fn github_releases_url<'a>(urls: impl IntoIterator<Item = &'a str>) -> Option<String> {
    urls.into_iter().find_map(|u| {
        let rest = u
            .strip_prefix("https://github.com/")
            .or_else(|| u.strip_prefix("http://github.com/"))?;
        let mut parts = rest.split('/');
        let owner = parts.next().filter(|s| !s.is_empty())?;
        let repo = parts.next()?.trim_end_matches(".git");
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        (valid(owner) && valid(repo)).then(|| format!("https://github.com/{owner}/{repo}/releases"))
    })
}

fn json_str(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

// `brew info --json=v2 --formula …` → name → info. Pure → unit-tested against real output.
fn parse_formula_infos(json: &str) -> HashMap<String, PackageInfo> {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    parsed["formulae"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let homepage = json_str(&f["homepage"]);
            let release_notes = github_releases_url([
                homepage.as_str(),
                f["urls"]["head"]["url"].as_str().unwrap_or(""),
                f["urls"]["stable"]["url"].as_str().unwrap_or(""),
            ])
            .unwrap_or_default();
            let info = PackageInfo {
                desc: json_str(&f["desc"]),
                license: json_str(&f["license"]),
                dependency: f["installed"][0]["installed_on_request"] == serde_json::json!(false),
                release_notes,
                homepage,
                ..PackageInfo::default()
            };
            (json_str(&f["name"]), info)
        })
        .collect()
}

// `brew info --json=v2 --cask …` → token → info. Pure → unit-tested against real output.
fn parse_cask_infos(json: &str) -> HashMap<String, PackageInfo> {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    parsed["casks"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let homepage = json_str(&c["homepage"]);
            let app = c["artifacts"]
                .as_array()
                .and_then(|arts| arts.iter().find_map(|a| a["app"][0].as_str()))
                .unwrap_or("")
                .to_string();
            let info = PackageInfo {
                desc: json_str(&c["desc"]),
                auto_updates: c["auto_updates"].as_bool().unwrap_or(false),
                release_notes: github_releases_url([
                    homepage.as_str(),
                    c["url"].as_str().unwrap_or(""),
                ])
                .unwrap_or_default(),
                homepage,
                app,
                ..PackageInfo::default()
            };
            (json_str(&c["token"]), info)
        })
        .collect()
}

// One `brew info` per kind for the whole list (~0.4s each, run in parallel), keyed
// "brew::<name>" / "cask::<token>" to match the frontend's selKey.
#[tauri::command]
async fn package_details(
    formulae: Vec<String>,
    casks: Vec<String>,
) -> HashMap<String, PackageInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let info = |flag: &'static str, ids: &[String]| -> Option<String> {
            let ids: Vec<&str> = ids
                .iter()
                .map(String::as_str)
                .filter(|id| is_valid_cask_token(id))
                .collect();
            if ids.is_empty() {
                return None;
            }
            let mut args = vec!["info", "--json=v2", flag];
            args.extend(ids);
            run_cmd("brew", &args).ok()
        };
        let (formula_json, cask_json) = std::thread::scope(|scope| {
            let f = scope.spawn(|| info("--formula", &formulae));
            let c = info("--cask", &casks);
            (f.join().ok().flatten(), c)
        });
        let formula = formula_json
            .map(|j| parse_formula_infos(&j))
            .unwrap_or_default();
        let cask = cask_json.map(|j| parse_cask_infos(&j)).unwrap_or_default();
        formula
            .into_iter()
            .map(|(k, v)| (format!("brew::{k}"), v))
            .chain(cask.into_iter().map(|(k, v)| (format!("cask::{k}"), v)))
            .collect()
    })
    .await
    .unwrap_or_default()
}

// --- App icons ---
//
// The installed app's own icon, as a PNG data URL (img-src allows data:). Converted once with
// `sips` and cached in ~/.freshet/icons, regenerated when the app's .icns is newer (an update can
// change the icon).

// A bare "Something.app" bundle name — no path separators, so it can only name a bundle directly
// inside an Applications folder. Pure → unit-tested.
fn is_valid_app_name(app: &str) -> bool {
    app.len() <= 255
        && Path::new(app)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        && !app.contains('/')
        && !app.starts_with('.')
        && !app.chars().any(char::is_control)
}

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
    let file = if Path::new(&name).extension().is_some() {
        name
    } else {
        format!("{name}.icns")
    };
    let path = bundle.join("Contents/Resources").join(file);
    path.is_file().then_some(path)
}

fn modified(p: &Path) -> Option<std::time::SystemTime> {
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
async fn app_icon(app: String) -> Option<String> {
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

// Open an http(s) URL in the default browser (custom command, so no extra capability needed).
#[tauri::command]
fn open_url(url: String) {
    if is_safe_url(&url) {
        let _ = Command::new("open").arg(url).status();
    }
}

// Text shown next to the menu-bar icon: the count, or nothing when there are no updates.
fn badge_title(count: u32) -> Option<String> {
    if count == 0 {
        None
    } else {
        Some(count.to_string())
    }
}

// AppHandle is taken by value to match the Tauri command convention; it's a cheap Arc clone.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn set_badge(app: tauri::AppHandle, count: u32) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_title(badge_title(count));
    }
}

// Scan /Applications and suggest apps that aren't brew-managed but have a matching cask, so the
// user can adopt them in one click.
//
// Homebrew 7 removed the cask metadata file this used to read (and disabled `--eval-all`), so it
// now uses only public commands: guess each unmanaged app's likely cask tokens from its name, keep
// the guesses that are real casks (`brew casks`, all ~7,800 names in 0.1s), then confirm with one
// `brew info --cask` that each candidate really installs that exact .app. Apps whose cask is
// named nothing like the app (rare) aren't found.

// Likely cask tokens for an app name, following Homebrew's token rules (lowercase, spaces →
// hyphens, "+" → "-plus", "@" → "-at", other punctuation dropped), plus looser variants that
// the brew-info confirmation step filters. Pure → unit-tested.
fn cask_token_candidates(app_stem: &str) -> Vec<String> {
    let lower = app_stem.to_lowercase();
    let mut slug = String::new();
    for c in lower.chars() {
        match c {
            'a'..='z' | '0'..='9' | '.' => slug.push(c),
            '+' => slug.push_str("-plus"),
            '@' => slug.push_str("-at"),
            _ => slug.push('-'),
        }
    }
    let tidy = |s: &str| {
        s.split('-')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    };
    let base = tidy(&slug);
    let mut out = vec![
        base.clone(),
        tidy(&base.replace('.', "-")),
        tidy(&base.replace('.', "")),
        // "zoom.us" → "zoom", "IntelliJ IDEA CE" → "intellij"
        base.split(['-', '.']).next().unwrap_or("").to_string(),
    ];
    out.retain(|t| !t.is_empty());
    out.dedup();
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

// Pair unmanaged app bundles with candidate casks whose `app` artifact matches the bundle name
// exactly (case-insensitive). `apps`: (bundle filename, display name). Pure → unit-tested.
fn match_adoptions(
    apps: &[(String, String)],
    candidates: &HashMap<String, Vec<String>>,
    cask_apps: &HashMap<String, Vec<String>>,
) -> Vec<AdoptableApp> {
    let mut out: Vec<AdoptableApp> = apps
        .iter()
        .filter_map(|(file, name)| {
            let token = candidates.get(file)?.iter().find(|t| {
                cask_apps
                    .get(*t)
                    .is_some_and(|apps| apps.iter().any(|a| a.eq_ignore_ascii_case(file)))
            })?;
            Some(AdoptableApp {
                name: name.clone(),
                token: token.clone(),
            })
        })
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.token == b.token);
    out
}

#[tauri::command]
async fn suggest_adoptions() -> Vec<AdoptableApp> {
    tauri::async_runtime::spawn_blocking(suggest_adoptions_blocking)
        .await
        .unwrap_or_default()
}

fn suggest_adoptions_blocking() -> Vec<AdoptableApp> {
    {
        let all_casks: std::collections::HashSet<String> = run_cmd("brew", &["casks"])
            .map(|s| s.lines().map(|l| l.trim().to_string()).collect())
            .unwrap_or_default();
        if all_casks.is_empty() {
            return vec![];
        }
        // Apps Homebrew already manages: whatever bundles the installed casks own.
        let managed_tokens: Vec<String> = run_cmd("brew", &["list", "--cask"])
            .map(|s| parse_cask_tokens(&s))
            .unwrap_or_default();
        let managed_apps: std::collections::HashSet<String> = cask_apps(&managed_tokens)
            .into_values()
            .flatten()
            .map(|a| a.to_lowercase())
            .collect();

        let mut apps = Vec::new();
        let mut candidates: HashMap<String, Vec<String>> = HashMap::new();
        for path in list_applications() {
            let (Some(file), Some(stem)) = (
                path.file_name().and_then(|s| s.to_str()),
                path.file_stem().and_then(|s| s.to_str()),
            ) else {
                continue;
            };
            if managed_apps.contains(&file.to_lowercase()) {
                continue;
            }
            let tokens: Vec<String> = cask_token_candidates(stem)
                .into_iter()
                .filter(|t| all_casks.contains(t))
                .collect();
            if !tokens.is_empty() {
                candidates.insert(file.to_string(), tokens);
                apps.push((file.to_string(), stem.to_string()));
            }
        }
        let wanted: Vec<String> = candidates.values().flatten().cloned().collect();
        match_adoptions(&apps, &candidates, &cask_apps(&wanted))
    }
}

// The brew args that adopt an existing install. Pure so the --adopt flag can't silently regress.
fn adopt_args(token: &str) -> Vec<String> {
    vec![
        "install".into(),
        "--cask".into(),
        token.into(),
        "--adopt".into(),
    ]
}

fn adopt_one(token: &str) -> UpdateResult {
    if !is_valid_cask_token(token) {
        return UpdateResult {
            kind: "cask".into(),
            name: token.to_string(),
            success: false,
            output: "invalid cask token".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
        };
    }
    let args = adopt_args(token);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (success, output) = run_update_cmd("brew", &arg_refs);
    UpdateResult {
        kind: "cask".into(),
        name: token.to_string(),
        success,
        output,
        action: None,
        elapsed_ms: None,
        skipped: false,
    }
}

// Adopt an existing (manually-installed) app into Homebrew without redownloading it.
#[tauri::command]
async fn adopt_cask(token: String) -> UpdateResult {
    tauri::async_runtime::spawn_blocking(move || adopt_one(&token))
        .await
        .unwrap_or_else(|_| UpdateResult {
            kind: "cask".into(),
            name: "adopt".into(),
            success: false,
            output: "adoption task failed".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
        })
}

// Adopt several apps in one batch, returning a result per token.
#[tauri::command]
async fn adopt_casks(tokens: Vec<String>) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || tokens.iter().map(|t| adopt_one(t)).collect())
        .await
        .unwrap_or_default()
}

// mas 7 needs root to install App Store updates and gets it by running `sudo` itself — without
// `-A`, so it ignores SUDO_ASKPASS and, with no terminal, fails instantly ("a terminal is required
// to read the password"). Its README's supported route is to start it as root: so Freshet runs
// `sudo -A <mas> …` itself, and `-A` makes sudo ask through Freshet's native password dialog. The
// absolute path is needed because sudo replaces PATH with its secure_path (no /opt/homebrew/bin).
// sudo remembers the password per parent process for a few minutes, so several App Store updates
// in one run ask once. The headless scheduled run never updates App Store apps, so it never gets
// here; elsewhere commands pass through unchanged.
fn with_root_if_needed(cmd: &str, args: Vec<String>) -> (String, Vec<String>) {
    if cmd != "mas" || HEADLESS.load(Ordering::Relaxed) {
        return (cmd.to_string(), args);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    match find_in_path("mas", &path) {
        Some(mas) => sudo_askpass_command(&mas, args),
        None => (cmd.to_string(), args),
    }
}

// `/usr/bin/sudo -A <program> <args…>`. Pure → unit-tested.
fn sudo_askpass_command(program: &Path, args: Vec<String>) -> (String, Vec<String>) {
    let mut full = vec!["-A".to_string(), program.to_string_lossy().into_owned()];
    full.extend(args);
    ("/usr/bin/sudo".to_string(), full)
}

// First executable named `name` on a PATH string. Pure apart from the file checks → tested.
fn find_in_path(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

// Map a selected item (kind + id) to the command that updates just it. The id is validated per
// kind so a crafted selection can't inject a flag or an arbitrary install spec. Pure → unit-tested.
fn build_update_command(kind: &str, id: &str) -> Option<(&'static str, Vec<String>)> {
    match kind {
        "brew" if is_valid_cask_token(id) => Some(("brew", vec!["upgrade".into(), id.into()])),
        "cask" if is_valid_cask_token(id) => {
            Some(("brew", vec!["upgrade".into(), "--cask".into(), id.into()]))
        }
        "mas" if is_numeric_id(id) => Some(("mas", vec!["upgrade".into(), id.into()])),
        "npm" if is_valid_npm_package(id) => Some((
            "npm",
            vec!["install".into(), "-g".into(), format!("{}@latest", id)],
        )),
        _ => None,
    }
}

// Partition selected updates into the brew formula tokens and cask tokens worth pre-fetching,
// validating each id the same way the install path does so a crafted token can't reach `brew
// fetch` either. mas/npm have no fetch-without-install step, so they're skipped (they download
// inline at install). Pure → unit-tested.
fn fetch_batches(selections: &[UpdateSelection]) -> (Vec<String>, Vec<String>) {
    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    for s in selections {
        match s.kind.as_str() {
            "brew" if is_valid_cask_token(&s.id) => formulae.push(s.id.clone()),
            "cask" if is_valid_cask_token(&s.id) => casks.push(s.id.clone()),
            _ => {}
        }
    }
    (formulae, casks)
}

// Pre-download the selected casks with one `brew fetch --cask` so the pre-install signer check
// (preverify) has the new version on disk to inspect before anything is installed. Formulae are
// not prefetched: `brew upgrade` already downloads its whole batch in parallel through its own
// download queue (HOMEBREW_DOWNLOAD_CONCURRENCY, 2× cores), so a separate fetch was a redundant
// brew start-up plus a silent phase. Best-effort: a miss just means preverify can't inspect.
fn prefetch_casks(selections: &[UpdateSelection]) {
    let (_, casks) = fetch_batches(selections);
    if !casks.is_empty() {
        let mut args: Vec<&str> = vec!["fetch", "--cask"];
        args.extend(casks.iter().map(String::as_str));
        let _ = run_cmd("brew", &args);
    }
}

// Live progress for an update run, emitted to the frontend as "update-progress".
#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
struct UpdateProgress {
    // What's happening: "Refreshing Homebrew", "Downloading apps", "Checking publisher",
    // "Upgrading", "Installing".
    phase: String,
    // The package being worked on, if any.
    current: Option<String>,
    // Packages finished so far, out of `total` selected.
    done: usize,
    total: usize,
}

type ProgressFn<'a> = &'a (dyn Fn(UpdateProgress) + Sync);

fn progress_event(phase: &str, current: Option<&str>, done: usize, total: usize) -> UpdateProgress {
    UpdateProgress {
        phase: phase.into(),
        current: current.map(String::from),
        done,
        total,
    }
}

// --- Upgrade only the items the user selected, returning a result per item ---
#[tauri::command]
async fn run_selected_updates(
    app: tauri::AppHandle,
    selections: Vec<UpdateSelection>,
) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let results = run_selected_updates_blocking(&selections, &|p| {
            let _ = app.emit("update-progress", p);
        });
        record_manual_history(&selections, &results);
        results
    })
    .await
    .unwrap_or_default()
}

fn run_selected_updates_blocking(
    selections: &[UpdateSelection],
    progress: ProgressFn<'_>,
) -> Vec<UpdateResult> {
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    let mut results = Vec::new();
    // Refresh Homebrew's metadata first if any brew/cask item is selected (skipped if fresh).
    if selections
        .iter()
        .any(|s| s.kind == "brew" || s.kind == "cask")
    {
        progress(progress_event(
            "Refreshing Homebrew",
            None,
            0,
            selections.len(),
        ));
        results.extend(brew_update_if_stale());
    }
    results.extend(install_selections(selections, progress));
    results
}

// Install each selection, returning exactly one result per selection, in order. No `brew update`
// here — callers decide when metadata is refreshed (the scheduled run does it before checking).
//
// Homebrew items are upgraded in one `brew upgrade` per kind rather than one process per package:
// each brew start-up costs ~0.7s and re-resolves dependencies, so 10 formulae went from 7.1s of
// overhead to 0.8s (measured). Per-item results are recovered afterwards: what's still outdated
// failed, and each item's log is its own `==> Upgrading <name>` section of brew's output.
// Casks are checked for a publisher change *before* installing (see preverify) and snapshotted
// for rollback. mas and npm have no batch mode and run one at a time.
fn install_selections(
    selections: &[UpdateSelection],
    progress: ProgressFn<'_>,
) -> Vec<UpdateResult> {
    let total = selections.len();
    let mut done = 0;
    let mut results: Vec<Option<UpdateResult>> = vec![None; total];
    let stopped = || STOP_REQUESTED.load(Ordering::SeqCst);
    // One `ps` for the whole run: which app bundles have a process running right now.
    let running = running_executables();

    if selections.iter().any(|s| s.kind == "cask") && !stopped() {
        progress(progress_event("Downloading apps", None, done, total));
        let started = std::time::Instant::now();
        prefetch_casks(selections);
        tracing::info!(elapsed_ms = elapsed_ms(started), "prefetch casks");
    }

    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    for (i, sel) in selections.iter().enumerate() {
        match build_update_command(&sel.kind, &sel.id) {
            None => {
                results[i] = Some(failed_result(
                    sel,
                    format!("unsupported or invalid update: {} / {}", sel.kind, sel.id),
                ));
            }
            Some(_) if sel.kind == "brew" => formulae.push(i),
            Some(_) if sel.kind == "cask" => casks.push(i),
            Some((cmd, args)) => {
                if stopped() {
                    results[i] = Some(not_started(sel));
                    continue;
                }
                // App Store apps are bundles in /Applications named after the app.
                if sel.kind == "mas" {
                    let bundle = Path::new("/Applications").join(format!("{}.app", sel.name));
                    if bundle_is_running(&bundle, &running) {
                        results[i] = Some(skipped_open(sel, &bundle));
                        done += 1;
                        continue;
                    }
                }
                progress(progress_event("Installing", Some(&sel.name), done, total));
                done += 1;
                let started = std::time::Instant::now();
                let (cmd, args) = with_root_if_needed(cmd, args);
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let (success, output) = run_update_cmd(&cmd, &arg_refs);
                results[i] = Some(UpdateResult {
                    kind: sel.kind.clone(),
                    name: sel.name.clone(),
                    success,
                    output,
                    action: None,
                    elapsed_ms: Some(elapsed_ms(started)),
                    skipped: false,
                });
            }
        }
    }

    // Casks: skip ones that are open (upgrading can quit them and lose unsaved work), refuse any
    // whose download is signed by a different publisher, and snapshot the rest for rollback.
    let mut cleared_casks = Vec::new();
    let tokens: Vec<String> = casks.iter().map(|&i| selections[i].id.clone()).collect();
    let apps_by_token = cask_apps(&tokens);
    for i in casks {
        let sel = &selections[i];
        if stopped() {
            results[i] = Some(not_started(sel));
            continue;
        }
        let installed =
            installed_cask_apps(apps_by_token.get(&sel.id).map_or(&[][..], Vec::as_slice));
        if let Some(open) = installed.iter().find(|b| bundle_is_running(b, &running)) {
            results[i] = Some(skipped_open(sel, open));
            done += 1;
            continue;
        }
        progress(progress_event(
            "Checking publisher",
            Some(&sel.name),
            done,
            total,
        ));
        match preverify::check_cask(&sel.id, &installed) {
            preverify::Verdict::Blocked {
                reason,
                bundle_id,
                team,
            } => {
                tracing::warn!(cask = %sel.id, %reason, "blocked before install");
                results[i] = Some(UpdateResult {
                    action: bundle_id
                        .map(|bundle_id| ResultAction::TrustSigner { bundle_id, team }),
                    ..failed_result(sel, reason)
                });
                done += 1;
            }
            preverify::Verdict::Allowed { note } => {
                let snapshotted = rollback::snapshot_cask(&sel.id, &installed);
                tracing::info!(cask = %sel.id, ?note, snapshotted, "cleared for install");
                cleared_casks.push((i, note, snapshotted));
            }
        }
    }

    let formula_results = upgrade_batch("--formula", &formulae, selections, progress, done, total);
    done += formulae.len();
    for (i, r) in formulae.iter().zip(formula_results) {
        results[*i] = Some(r);
    }
    let cask_indices: Vec<usize> = cleared_casks.iter().map(|(i, _, _)| *i).collect();
    let cask_results = upgrade_batch("--cask", &cask_indices, selections, progress, done, total);
    for ((i, note, snapshotted), mut r) in cleared_casks.into_iter().zip(cask_results) {
        if let Some(note) = note {
            r.output = format!("{note}\n{}", r.output).trim().to_string();
        }
        if snapshotted && r.success {
            r.action = Some(ResultAction::Rollback {
                token: selections[i].id.clone(),
            });
        }
        results[i] = Some(r);
    }

    results
        .into_iter()
        .zip(selections)
        .map(|(r, sel)| r.unwrap_or_else(|| failed_result(sel, "not attempted".into())))
        .collect()
}

fn failed_result(sel: &UpdateSelection, output: String) -> UpdateResult {
    UpdateResult {
        kind: sel.kind.clone(),
        name: sel.name.clone(),
        success: false,
        output,
        action: None,
        elapsed_ms: None,
        skipped: false,
    }
}

const NOT_STARTED: &str = "Not started: you stopped the update.";

fn not_started(sel: &UpdateSelection) -> UpdateResult {
    UpdateResult {
        skipped: true,
        ..failed_result(sel, NOT_STARTED.into())
    }
}

fn skipped_open(sel: &UpdateSelection, bundle: &Path) -> UpdateResult {
    let app = bundle
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let shown = app.trim_end_matches(".app");
    UpdateResult {
        skipped: true,
        action: Some(ResultAction::QuitAndUpdate {
            kind: sel.kind.clone(),
            id: sel.id.clone(),
            name: sel.name.clone(),
            app: app.clone(),
        }),
        ..failed_result(
            sel,
            format!(
                "Skipped: {shown} is open, and updating it could quit it and lose unsaved work. \
                 Quit it and update now, or it will be updated next time."
            ),
        )
    }
}

// --- Is an app open? ---

// Full executable paths of every running process (`ps -axo comm=` prints them on macOS).
fn running_executables() -> Vec<String> {
    Command::new("/bin/ps")
        .args(["-axo", "comm="])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// Any process (the app or one of its helpers) running from inside `bundle`. Pure → tested.
fn bundle_is_running(bundle: &Path, executables: &[String]) -> bool {
    let prefix = format!("{}/Contents/", bundle.display());
    executables.iter().any(|e| e.starts_with(&prefix))
}

// One `brew upgrade <kind_flag> a b c …` for the given selections; one result per index, in order.
// `done_before`/`total` place this batch within the whole run for progress reporting.
#[tracing::instrument(skip(indices, selections, progress), fields(count = indices.len()))]
fn upgrade_batch(
    kind_flag: &str,
    indices: &[usize],
    selections: &[UpdateSelection],
    progress: ProgressFn<'_>,
    done_before: usize,
    total: usize,
) -> Vec<UpdateResult> {
    if indices.is_empty() {
        return Vec::new();
    }
    if STOP_REQUESTED.load(Ordering::SeqCst) {
        return indices
            .iter()
            .map(|&i| not_started(&selections[i]))
            .collect();
    }
    let ids: Vec<&str> = indices.iter().map(|&i| selections[i].id.as_str()).collect();
    let mut args = vec!["upgrade", kind_flag];
    args.extend(&ids);

    let started = std::time::Instant::now();
    progress(progress_event("Upgrading", None, done_before, total));
    // Report each package as brew reaches it, so a 65-package batch isn't a silent multi-minute
    // wait; and if the user asked to stop, interrupt at the next package boundary.
    let wanted: std::collections::HashSet<&str> = ids.iter().copied().collect();
    let mut seen = std::collections::HashSet::new();
    let mut interrupted = false;
    let (batch_ok, output) = run_update_cmd_streaming("brew", &args, &mut |line| {
        if interrupted {
            return;
        }
        if is_package_boundary(line) && STOP_REQUESTED.load(Ordering::SeqCst) {
            interrupt_running_child();
            interrupted = true;
            return;
        }
        if let Some(name) = upgrading_name(line, &wanted) {
            if seen.insert(name.to_string()) {
                let done = done_before + seen.len() - 1;
                progress(progress_event("Upgrading", Some(name), done, total));
            }
        }
    });
    let elapsed = elapsed_ms(started);
    tracing::info!(
        elapsed_ms = elapsed,
        batch_ok,
        interrupted,
        "brew upgrade batch"
    );

    // brew exits non-zero if *any* item failed. The truth per item is whether it's still outdated.
    let still_outdated: Option<std::collections::HashSet<String>> = if batch_ok {
        Some(std::collections::HashSet::new())
    } else {
        run_cmd("brew", &["outdated", "--json=v2", "--greedy"])
            .ok()
            .map(|json| outdated_names(&json))
    };
    let sections = split_brew_sections(&output, &ids);

    indices
        .iter()
        .zip(&ids)
        .map(|(&i, id)| {
            let sel = &selections[i];
            // If even the re-check failed, fall back to the batch's overall outcome.
            let success = still_outdated
                .as_ref()
                .map_or(batch_ok, |outdated| !outdated.contains(*id));
            if interrupted && !success && !seen.contains(*id) {
                return not_started(sel);
            }
            let own = sections.get(*id).cloned().unwrap_or_default();
            let item_output = if !own.is_empty() {
                own
            } else if success {
                format!("Upgraded in one batch of {}.", ids.len())
            } else {
                output.clone()
            };
            UpdateResult {
                kind: sel.kind.clone(),
                name: sel.name.clone(),
                success,
                output: item_output,
                action: None,
                elapsed_ms: Some(elapsed),
                skipped: false,
            }
        })
        .collect()
}

// Names of everything (formulae + casks) in `brew outdated --json=v2 --greedy` output.
fn outdated_names(json: &str) -> std::collections::HashSet<String> {
    parse_brew_outdated(json)
        .into_iter()
        .map(|i| i.id)
        .chain(parse_cask_names(json))
        .collect()
}

// Split a batched `brew upgrade` log into per-package sections. brew starts each package with
// `==> Upgrading <name>` (formula and cask alike; see Homebrew's upgrade.rb / cask/upgrade.rb),
// and reports failures as `Error: <name>: …` lines, often at the end, so those are attributed to
// their package too. Pure → unit-tested.
fn split_brew_sections(output: &str, names: &[&str]) -> HashMap<String, String> {
    let wanted: std::collections::HashSet<&str> = names.iter().copied().collect();
    let mut sections: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("==> Upgrading ") {
            let name = rest.split_whitespace().next().unwrap_or("");
            current = wanted.contains(name).then(|| name.to_string());
        }
        if let Some(rest) = line.strip_prefix("Error: ") {
            let name = rest.split(':').next().unwrap_or("").trim();
            if wanted.contains(name) {
                let entry = sections.entry(name.to_string()).or_default();
                entry.push_str(line);
                entry.push('\n');
                continue;
            }
        }
        if let Some(name) = &current {
            let entry = sections.entry(name.clone()).or_default();
            entry.push_str(line);
            entry.push('\n');
        }
    }
    sections
        .into_iter()
        .map(|(k, v)| (k, v.trim().to_string()))
        .collect()
}

// --- Signature / provenance verification ---
// Inspects installed apps' code signatures and flags any whose signing certificate (Team ID)
// changed since the last check — the signal of a hijacked release that Gatekeeper alone misses.

struct AppSig {
    name: String,
    // CFBundleIdentifier from the signature; the signer store's key (stable across renames).
    bundle_id: Option<String>,
    team_id: Option<String>,
    authority: Option<String>,
    signed: bool,
    notarized: bool,
}

fn list_applications() -> Vec<PathBuf> {
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

// Pull (team_id, authority) out of `codesign -dvv` output. Team ID is "not set" for some apps.
fn parse_signing(codesign_output: &str) -> (Option<String>, Option<String>) {
    let mut team_id = None;
    let mut authority = None;
    for line in codesign_output.lines() {
        if let Some(rest) = line.strip_prefix("TeamIdentifier=") {
            let t = rest.trim();
            if !t.is_empty() && t != "not set" {
                team_id = Some(t.to_string());
            }
        } else if authority.is_none() {
            if let Some(rest) = line.strip_prefix("Authority=") {
                authority = Some(rest.trim().to_string());
            }
        }
    }
    (team_id, authority)
}

// `spctl --assess` accepts a notarized app and prints "source=Notarized Developer ID".
// Match the source line specifically: a plain substring "notarized" also matches the
// "source=Unnotarized Developer ID" that spctl prints for signed-but-not-notarized apps.
fn parse_notarized(spctl_accepted: bool, spctl_output: &str) -> bool {
    spctl_accepted
        && spctl_output.contains("accepted")
        && spctl_output.to_lowercase().contains("source=notarized")
}

// `Identifier=` line of `codesign -dvv`: the signed bundle identifier.
fn parse_identifier(codesign_output: &str) -> Option<String> {
    codesign_output.lines().find_map(|l| {
        let id = l.strip_prefix("Identifier=")?.trim();
        (!id.is_empty()).then(|| id.to_string())
    })
}

// Just who signed a bundle (no spctl/notarization), for the pre-install comparison.
fn read_signer(path: &Path) -> preverify::Signer {
    Command::new("/usr/bin/codesign")
        .arg("-dvv")
        .arg(path)
        .output()
        .map_or(
            preverify::Signer {
                bundle_id: None,
                team: None,
            },
            |out| {
                let text = combine_output(&out.stdout, &out.stderr);
                preverify::Signer {
                    bundle_id: parse_identifier(&text),
                    team: parse_signing(&text).0,
                }
            },
        )
}

// A signer change (different Team ID than last seen) is the hijacked-release signal.
fn cert_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    matches!((previous, current), (Some(p), Some(c)) if p != c)
}

fn inspect_app(path: &Path, known_notarized: Option<bool>) -> AppSig {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let path_str = path.to_str().unwrap_or("");

    let mut team_id = None;
    let mut authority = None;
    let mut bundle_id = None;
    let mut signed = false;
    if let Ok(out) = command("/usr/bin/codesign")
        .args(["-dvv", path_str])
        .output()
    {
        signed = out.status.success();
        // codesign writes its details to stderr.
        let text = combine_output(&out.stdout, &out.stderr);
        let (t, a) = parse_signing(&text);
        team_id = t;
        authority = a;
        bundle_id = parse_identifier(&text);
    }

    // spctl is the slow part (~2.7s/app, and serialized system-wide by syspolicyd, so
    // parallelism barely helps); skip it when the caller already knows the answer.
    let notarized = known_notarized.unwrap_or_else(|| {
        command("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute", "-v", path_str])
            .output()
            .is_ok_and(|out| {
                let text = combine_output(&out.stdout, &out.stderr);
                parse_notarized(out.status.success(), &text)
            })
    });

    AppSig {
        name,
        bundle_id,
        team_id,
        authority,
        signed,
        notarized,
    }
}

fn signing_store_path() -> Option<PathBuf> {
    let mut p = freshet_dir()?;
    p.push("signing.json");
    Some(p)
}

fn load_signing_store() -> HashMap<String, String> {
    signing_store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_signing_store(map: &HashMap<String, String>) {
    if let (Some(p), Ok(s)) = (signing_store_path(), serde_json::to_string_pretty(map)) {
        // 0600: this is the trust baseline that signer-drift detection diffs against. A file an
        // attacker can rewrite lets them pre-seed a hijacker's Team ID and suppress the warning.
        // The dir is already 0700, but write it owner-only too (defense in depth, not umask-dependent).
        let _ = write_private(&p, &s, false);
    }
}

// Serializes the load → mutate → save sequence so two concurrent verify_signatures runs (e.g. a
// future scheduled background scan racing the manual button) can't lose updates to signing.json.
// Cross-process races aren't covered — that's a "last writer wins" we accept.
fn signing_store_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[derive(Clone, Serialize)]
struct VerifyProgress {
    done: usize,
    total: usize,
}

// Filter the verify_signatures output. When `only_changed` is true, return only entries that
// were flagged because the *signer changed* — the specific signal a hijacked update produces.
// Unsigned-but-stable apps stay out of the post-update view (they belong in the full report).
fn select_verify_results(
    entries: Vec<(UpdateResult, bool)>,
    only_changed: bool,
) -> Vec<UpdateResult> {
    if only_changed {
        entries
            .into_iter()
            .filter(|(_, c)| *c)
            .map(|(r, _)| r)
            .collect()
    } else {
        entries.into_iter().map(|(r, _)| r).collect()
    }
}

// Build (UpdateResult, was_cert_change) entries from inspected signatures, diffing against the
// caller-provided baseline store (which is mutated in place to record the new Team IDs). The
// caller is responsible for holding signing_store_lock across load → here → save so two verify
// runs can't race the signing.json file.
fn build_verify_entries(
    sigs: &[AppSig],
    store: &mut HashMap<String, String>,
) -> Vec<(UpdateResult, bool)> {
    let mut entries: Vec<(UpdateResult, bool)> = Vec::new();
    for sig in sigs {
        // Keyed by bundle id (survives renames, no collisions between same-named apps). Older
        // stores were keyed by app name; those entries are read as a fallback and migrated.
        let key = sig.bundle_id.clone().unwrap_or_else(|| sig.name.clone());
        let prev = store.get(&key).or_else(|| store.get(&sig.name)).cloned();
        let changed = cert_changed(prev.as_deref(), sig.team_id.as_deref());
        // The baseline only moves when nothing changed. On drift it stays put, so the warning
        // repeats on every check until the user explicitly trusts the new publisher
        // (trust_signer) — previously the first sighting of a new signer silently became the
        // baseline and the alarm fired exactly once.
        if !changed {
            if let Some(team) = &sig.team_id {
                store.insert(key.clone(), team.clone());
                if key != sig.name {
                    store.remove(&sig.name);
                }
            }
        }
        // Flagged = a changed signer or an unsigned app. Everything else is a "verified publisher".
        let ok = sig.signed && !changed;

        let mut out = String::new();
        if changed {
            let was = prev.clone().unwrap_or_default();
            let now = sig.team_id.clone().unwrap_or_default();
            let _ = writeln!(
                out,
                "⚠ Developer certificate CHANGED: was Team {was}, now {now}. A different publisher signed this version — possible hijacked release. Verify before trusting."
            );
        }
        if !sig.signed {
            out.push_str("Not code-signed.\n");
        }
        if let Some(a) = &sig.authority {
            let _ = writeln!(out, "Signer: {a}");
        }
        if let Some(t) = &sig.team_id {
            let _ = writeln!(out, "Team ID: {t}");
        }
        out.push_str(if sig.notarized {
            "Notarized by Apple: yes"
        } else {
            "Notarized by Apple: no"
        });

        let action = match (changed, &sig.team_id) {
            (true, Some(team)) => Some(ResultAction::TrustSigner {
                bundle_id: key.clone(),
                team: team.clone(),
            }),
            _ => None,
        };
        entries.push((
            UpdateResult {
                kind: "verify".into(),
                name: sig.name.clone(),
                success: ok,
                output: out.trim().to_string(),
                action,
                elapsed_ms: None,
                skipped: false,
            },
            changed,
        ));
    }
    entries
}

// The trusted Team ID for a bundle id, if one has been recorded.
fn trusted_team(bundle_id: &str) -> Option<String> {
    let _guard = signing_store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    load_signing_store().get(bundle_id).cloned()
}

// Shapes we accept from the frontend before writing them into the trust baseline.
fn is_valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn is_valid_team_id(team: &str) -> bool {
    team.len() == 10
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

// The user vouches for a publisher change: record `team` as the baseline for `bundle_id`. Clears
// the repeating drift warning, and lets a pre-install check that blocked on it go through.
#[tauri::command]
fn trust_signer(bundle_id: String, team: String) -> Result<(), String> {
    if !is_valid_bundle_id(&bundle_id) || !is_valid_team_id(&team) {
        return Err("invalid bundle id or Team ID".into());
    }
    let _guard = signing_store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = load_signing_store();
    store.insert(bundle_id, team);
    save_signing_store(&store);
    Ok(())
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
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|c| {
            let token = c["token"].as_str()?.to_string();
            let apps = c["artifacts"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|a| a["app"].as_array())
                .flatten()
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            Some((token, apps))
        })
        .collect()
}

fn cask_apps(tokens: &[String]) -> HashMap<String, Vec<String>> {
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
fn installed_cask_apps(app_names: &[String]) -> Vec<PathBuf> {
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

// --- Notarization cache ---
//
// An app's notarization status only changes when the app does, so `spctl`'s answer is cached per
// bundle path, keyed on its Info.plist mtime (rewritten by every update). A rescan of an
// unchanged /Applications skips spctl entirely: ~31s → about a second on 33 apps (measured).

type NotarizationCache = HashMap<String, (u64, bool)>;

fn notarization_cache_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("notarization.json"))
}

fn info_plist_mtime(app: &Path) -> Option<u64> {
    modified(&app.join("Contents/Info.plist"))?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

// The cached answer, only if the app hasn't changed since. Pure → unit-tested.
fn cached_notarization(cache: &NotarizationCache, path: &str, mtime: Option<u64>) -> Option<bool> {
    let (cached_mtime, notarized) = cache.get(path)?;
    (Some(*cached_mtime) == mtime).then_some(*notarized)
}

// Inspect every app in /Applications: at most one worker per core (spctl is serialized by the
// system anyway, so more processes just contend), notarization served from cache when the app
// hasn't changed. `progress(done, total)` is called as each app finishes.
fn inspect_all_apps(progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<AppSig> {
    let apps = list_applications();
    let total = apps.len();
    let mut cache: NotarizationCache = notarization_cache_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 8);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let found = std::sync::Mutex::new(Vec::with_capacity(total));
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(path) = apps.get(i) else { break };
                let key = path.to_string_lossy().into_owned();
                let mtime = info_plist_mtime(path);
                let sig = inspect_app(path, cached_notarization(&cache, &key, mtime));
                found
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((key, mtime, sig));
                progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
            });
        }
    });
    let found = found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.clear(); // drop apps that were removed
    let mut sigs = Vec::with_capacity(found.len());
    for (key, mtime, sig) in found {
        if let Some(m) = mtime {
            cache.insert(key, (m, sig.notarized));
        }
        sigs.push(sig);
    }
    if let (Some(p), Ok(json)) = (notarization_cache_path(), serde_json::to_string(&cache)) {
        let _ = write_private(&p, &json, false);
    }
    tracing::info!(
        apps = total,
        elapsed_ms = elapsed_ms(started),
        "signature scan"
    );
    sigs
}

// Full scan diffed against (and updating) the signer baseline.
fn verify_all_blocking(progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<(UpdateResult, bool)> {
    let sigs = inspect_all_apps(progress);
    let _guard = signing_store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = load_signing_store();
    let entries = build_verify_entries(&sigs, &mut store);
    save_signing_store(&store);
    entries
}

#[tauri::command]
async fn verify_signatures(app: tauri::AppHandle, only_changed: Option<bool>) -> Vec<UpdateResult> {
    let only_changed = only_changed.unwrap_or(false);
    let entries = tauri::async_runtime::spawn_blocking(move || {
        let _ = app.emit("verify-progress", VerifyProgress { done: 0, total: 0 });
        verify_all_blocking(&|done, total| {
            let _ = app.emit("verify-progress", VerifyProgress { done, total });
        })
    })
    .await
    .unwrap_or_default();
    let mut results = select_verify_results(entries, only_changed);
    // Flagged apps first (they auto-expand and need attention); verified publishers after.
    results.sort_by_key(|r| r.success);
    results
}

// Targeted post-update signature check: inspect only the `.app` bundles owned by the casks
// that were just upgraded. Returns just the cert-drift entries (the hijack signal). Skips:
// non-cask updates (formulae, mas, npm have no `.app` bundle to inspect), unknown tokens,
// and tokens whose `.app` isn't actually installed in /Applications.
#[tauri::command]
async fn verify_updated_casks(tokens: Vec<String>) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || verify_updated_casks_blocking(&tokens))
        .await
        .unwrap_or_default()
}

fn verify_updated_casks_blocking(tokens: &[String]) -> Vec<UpdateResult> {
    if tokens.is_empty() {
        return vec![];
    }
    let names: Vec<String> = cask_apps(tokens).into_values().flatten().collect();
    let paths = installed_cask_apps(&names);
    if paths.is_empty() {
        return vec![];
    }
    // Targeted check is small (1–5 apps in practice); serial inspection is fine.
    let sigs: Vec<AppSig> = paths.iter().map(|p| inspect_app(p, None)).collect();

    let entries = {
        let _guard = signing_store_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut store = load_signing_store();
        let entries = build_verify_entries(&sigs, &mut store);
        save_signing_store(&store);
        entries
    };

    // Post-update mode is always only-changed; unsigned-but-stable is for the full inventory report.
    let mut results = select_verify_results(entries, true);
    results.sort_by_key(|r| r.success);
    results
}

// --- Update history: ~/.freshet/history.jsonl, one line per package per run ---

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct HistoryEntry {
    // Unix seconds.
    at: u64,
    // "manual" or "scheduled".
    source: String,
    kind: String,
    id: String,
    name: String,
    from: String,
    to: String,
    // "updated" | "failed" | "needs_admin" | "blocked" | "skipped_open"
    outcome: String,
    #[serde(default)]
    elapsed_ms: Option<u64>,
}

const HISTORY_KEEP: usize = 2000;

fn history_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("history.jsonl"))
}

fn outcome_of(r: &UpdateResult) -> &'static str {
    match &r.action {
        Some(ResultAction::TrustSigner { .. }) => "blocked",
        Some(ResultAction::QuitAndUpdate { .. }) => "skipped_open",
        _ if r.success => "updated",
        _ if needs_admin(&r.output) => "needs_admin",
        _ => "failed",
    }
}

// (kind, id) → (installed, latest), from a check's categories.
type Versions = HashMap<(String, String), (String, String)>;

fn versions_of(categories: &[CategoryStatus]) -> Versions {
    categories
        .iter()
        .flat_map(|c| {
            c.items.iter().map(|i| {
                (
                    (c.kind.clone(), i.id.clone()),
                    (i.current.clone(), i.latest.clone()),
                )
            })
        })
        .collect()
}

// One entry per attempted package. Items never started (the user stopped the run) aren't
// history. `results` must align 1:1 with `selections`. Pure → unit-tested.
fn history_entries(
    selections: &[UpdateSelection],
    results: &[UpdateResult],
    versions: &Versions,
    source: &str,
    at: u64,
) -> Vec<HistoryEntry> {
    selections
        .iter()
        .zip(results)
        .filter(|(_, r)| !(r.skipped && r.action.is_none()))
        .map(|(sel, r)| {
            let (from, to) = versions
                .get(&(sel.kind.clone(), sel.id.clone()))
                .cloned()
                .unwrap_or_default();
            HistoryEntry {
                at,
                source: source.into(),
                kind: sel.kind.clone(),
                id: sel.id.clone(),
                name: sel.name.clone(),
                from,
                to,
                outcome: outcome_of(r).into(),
                elapsed_ms: r.elapsed_ms,
            }
        })
        .collect()
}

fn append_history(entries: &[HistoryEntry]) {
    let Some(path) = history_path() else { return };
    if entries.is_empty() {
        return;
    }
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default();
    lines.extend(entries.iter().filter_map(|e| serde_json::to_string(e).ok()));
    let start = lines.len().saturating_sub(HISTORY_KEEP);
    let _ = write_private_atomic(&path, &(lines[start..].join("\n") + "\n"));
}

// The update results come back with the optional "brew update" row first; history wants the
// rows that line up with the selections.
fn record_manual_history(selections: &[UpdateSelection], results: &[UpdateResult]) {
    let offset = results.len().saturating_sub(selections.len());
    // The versions the user saw when they pressed Update — even if a scheduled run has run since.
    let versions = cached_check_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<CachedCheck>(&t).ok())
        .map(|c| versions_of(&c.categories))
        .unwrap_or_default();
    append_history(&history_entries(
        selections,
        &results[offset..],
        &versions,
        "manual",
        unix_now(),
    ));
}

// Newest first.
#[tauri::command]
fn get_history(limit: Option<usize>) -> Vec<HistoryEntry> {
    let text = history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l).ok())
        .take(limit.unwrap_or(200).min(HISTORY_KEEP))
        .collect()
}

// --- Quit an open app, then update just that item ---

#[allow(clippy::needless_pass_by_value)] // Tauri hands command arguments over by value.
#[tauri::command]
async fn quit_and_update(
    app: tauri::AppHandle,
    kind: String,
    id: String,
    name: String,
    app_name: String,
) -> Result<Vec<UpdateResult>, String> {
    if !is_valid_app_name(&app_name) || !matches!(kind.as_str(), "cask" | "mas") {
        return Err("invalid app".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let bundle = Path::new("/Applications").join(&app_name);
        let shown = app_name.trim_end_matches(".app").to_string();
        // A polite quit: the app gets to ask about unsaved work, exactly as with ⌘Q.
        let script = format!("tell application {} to quit", applescript_string(&shown));
        let _ = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while bundle_is_running(&bundle, &running_executables()) {
            if std::time::Instant::now() > deadline {
                return Err(format!(
                    "{shown} didn't quit; it may be asking you to save something. Nothing was updated."
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let selections = [UpdateSelection { kind, id, name }];
        let results = run_selected_updates_blocking(&selections, &|p| {
            let _ = app.emit("update-progress", p);
        });
        record_manual_history(&selections, &results);
        Ok(results)
    })
    .await
    .map_err(|e| e.to_string())?
}

// --- Freshet updating itself (tauri-plugin-updater) ---
//
// Each release publishes latest.json beside the DMG; the update archive is signed with the key
// whose public half is in tauri.conf.json (plugins.updater.pubkey), and the plugin refuses
// anything that doesn't verify against it.

#[derive(Serialize)]
struct AppUpdate {
    version: String,
    notes: Option<String>,
}

#[tauri::command]
async fn check_app_update(app: tauri::AppHandle) -> Result<Option<AppUpdate>, String> {
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
async fn install_app_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt as _;
    if RUNNING_CHILD.load(Ordering::SeqCst) != 0 {
        return Err("Finish or stop the running update first.".into());
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
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

// --- Launch at login (tauri-plugin-autostart, a LaunchAgent in ~/Library/LaunchAgents) ---

#[allow(clippy::needless_pass_by_value)] // AppHandle is a cheap clone; Tauri passes it by value.
#[tauri::command]
fn get_launch_at_login(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt as _;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[allow(clippy::needless_pass_by_value)] // AppHandle is a cheap clone; Tauri passes it by value.
#[tauri::command]
fn set_launch_at_login(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt as _;
    let launcher = app.autolaunch();
    if enabled {
        launcher.enable().map_err(|e| e.to_string())?;
    } else {
        launcher.disable().map_err(|e| e.to_string())?;
    }
    launcher.is_enabled().map_err(|e| e.to_string())
}

// --- Settings shared by the app and the scheduled run ---
//
// Skipped versions, notify-only and greedy used to live in the webview's localStorage, which the
// launchd job can't read — so the scheduled run ignored all three. They now live in
// ~/.freshet/settings.json, written by the app (save_settings) and read by `--run-scheduled`.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
struct Settings {
    greedy: bool,
    notify_only: bool,
    // Opt-in: look up known vulnerabilities for npm updates on osv.dev (see vulns.rs).
    check_vulnerabilities: bool,
    // ignore_key()s of skipped versions.
    skipped: Vec<String>,
}

// Must match `ignoreKey` in src/logic.ts. Keyed by version, so a newer release is not skipped.
fn ignore_key(kind: &str, id: &str, latest: &str) -> String {
    format!("{kind}::{id}::{latest}")
}

fn settings_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("settings.json"))
}

// None when no settings file exists yet (the frontend then migrates its localStorage values).
fn load_settings() -> Option<Settings> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

// Write-then-rename, so the scheduled run never reads a half-written file.
fn write_private_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    write_private(&tmp, contents, false)?;
    std::fs::rename(&tmp, path)
}

#[tauri::command]
fn get_settings() -> Option<Settings> {
    load_settings()
}

// Tauri deserializes command arguments into owned values; a reference isn't an option.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn save_settings(settings: Settings) -> Result<(), String> {
    let path = settings_path().ok_or_else(|| "could not resolve ~/.freshet".to_string())?;
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    write_private_atomic(&path, &json).map_err(|e| e.to_string())
}

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
        .filter(|c| c.kind == "brew" || c.kind == "cask")
        .flat_map(|c| {
            c.items
                .iter()
                .filter(|i| settings.greedy || !i.self_updating)
                .filter(|i| !skipped.contains(ignore_key(&c.kind, &i.id, &i.latest).as_str()))
                .map(|i| UpdateSelection {
                    kind: c.kind.clone(),
                    id: i.id.clone(),
                    name: i.name.clone(),
                })
        })
        .collect()
}

// sudo's messages when it can't ask for a password (no terminal, no askpass) — what a cask that
// needs admin rights produces in the headless run. Those are "needs you", not "broken".
fn needs_admin(output: &str) -> bool {
    output.contains("a terminal is required to read the password")
        || output.contains("sudo: a password is required")
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
struct ScheduledRunSummary {
    // Unix seconds.
    finished_at: u64,
    notify_only: bool,
    // Updates found after filtering (what would be / was attempted).
    available: usize,
    updated: Vec<String>,
    // Need an admin password: left for the user to run from the app.
    needs_admin: Vec<String>,
    failed: Vec<String>,
    // Casks whose code-signing Team ID changed with this update.
    signer_changed: Vec<String>,
}

// Sort install results into the summary's buckets. Pure → unit-tested.
fn summarize_results(summary: &mut ScheduledRunSummary, results: &[UpdateResult]) {
    for r in results {
        if matches!(r.action, Some(ResultAction::TrustSigner { .. })) {
            summary.signer_changed.push(r.name.clone());
        } else if r.success {
            summary.updated.push(r.name.clone());
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
    if parts.is_empty() {
        "Homebrew is up to date.".into()
    } else {
        parts.join(" ")
    }
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
fn get_last_scheduled_run() -> Option<ScheduledRunSummary> {
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
fn scheduled_run_active() -> bool {
    scheduled_pid_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(pid_alive)
}

#[derive(Serialize)]
struct ScheduledState {
    running: bool,
    last: Option<ScheduledRunSummary>,
}

// Cheap enough to poll (a stat, a tiny file, one `kill -0`).
#[tauri::command]
fn get_scheduled_state() -> ScheduledState {
    ScheduledState {
        running: scheduled_run_active(),
        last: get_last_scheduled_run(),
    }
}

// Returns the process exit code: 0 if everything that was attempted succeeded.
fn run_scheduled() -> i32 {
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
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
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
        None => log.line(">>> brew update: skipped, ran within the last hour"),
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
            kind: "brew".into(),
            name: "brew cleanup".into(),
            success: cleanup.0,
            output: cleanup.1,
            action: None,
            elapsed_ms: None,
            skipped: false,
        };
        log.result(&cleanup);
        if !cleanup.success {
            summary.failed.push(cleanup.name);
        }

        let updated_casks: Vec<String> = selections
            .iter()
            .zip(&results)
            .filter(|(s, r)| s.kind == "cask" && r.success)
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
            || !summary.signer_changed.is_empty(),
    )
}

// --- Launchd schedule management ---
//
// The daily job is a launchd LaunchAgent that runs this app's own binary with --run-scheduled,
// so it works whether or not Freshet is open.
const SCHEDULE_LABEL: &str = "com.freshet.daily";
const RUN_SCHEDULED_FLAG: &str = "--run-scheduled";

const fn valid_schedule_time(hour: u32, minute: u32) -> bool {
    hour <= 23 && minute <= 59
}

fn find_brew() -> Option<PathBuf> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

// macOS runs a quarantined app opened from Downloads from a randomized read-only mount that
// vanishes later; a schedule pointing there would break. Pure → unit-tested.
fn is_translocated(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/AppTranslocation/")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn render_plist(label: &str, program: &[String], log_dir: &Path, hour: u32, minute: u32) -> String {
    let args = program.iter().fold(String::new(), |mut out, a| {
        let _ = write!(out, "\n        <string>{}</string>", xml_escape(a));
        out
    });
    let log_dir = xml_escape(&log_dir.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>{args}
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>{hour}</integer>
        <key>Minute</key><integer>{minute}</integer>
    </dict>
    <key>StandardOutPath</key><string>{log_dir}/launchd.out.log</string>
    <key>StandardErrorPath</key><string>{log_dir}/launchd.err.log</string>
</dict>
</plist>
"#
    )
}

// Run launchctl and turn a non-zero exit into an Err carrying its message. `Command::status()`
// alone only fails if launchctl can't be spawned, which is how a failed load used to go unnoticed.
fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "launchctl {} failed: {}",
            args.first().unwrap_or(&""),
            msg.trim()
        ))
    }
}

// The logged-in user's launchd domain, e.g. "gui/501".
fn gui_domain() -> Result<String, String> {
    let out = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err("could not determine user id".into());
    }
    Ok(format!("gui/{uid}"))
}

// One launchd job: its label, plist location, and the program it runs. The real one is
// `ScheduleJob::daily()`; tests use a temp dir and a throwaway label so they never touch the
// user's installed schedule.
struct ScheduleJob {
    label: String,
    plist: PathBuf,
    program: Vec<String>,
    log_dir: PathBuf,
}

impl ScheduleJob {
    fn daily() -> Result<Self, String> {
        let home =
            dirs::home_dir().ok_or_else(|| "could not resolve home directory".to_string())?;
        let log_dir = freshet_dir().ok_or_else(|| "could not resolve ~/.freshet".to_string())?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Ok(Self {
            label: SCHEDULE_LABEL.into(),
            plist: home.join(format!("Library/LaunchAgents/{SCHEDULE_LABEL}.plist")),
            program: vec![
                exe.to_string_lossy().into_owned(),
                RUN_SCHEDULED_FLAG.into(),
            ],
            log_dir,
        })
    }

    fn service(&self) -> Result<String, String> {
        Ok(format!("{}/{}", gui_domain()?, self.label))
    }

    fn is_loaded(&self) -> bool {
        self.service()
            .is_ok_and(|svc| launchctl(&["print", &svc]).is_ok())
    }

    fn install(&self, hour: u32, minute: u32) -> Result<(), String> {
        if !valid_schedule_time(hour, minute) {
            return Err(format!("invalid schedule time {hour}:{minute}"));
        }
        if let Some(parent) = self.plist.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let plist = render_plist(&self.label, &self.program, &self.log_dir, hour, minute);
        std::fs::write(&self.plist, plist).map_err(|e| e.to_string())?;

        // Replace any previous load (e.g. the old time); "not loaded" is the expected error here.
        let domain = gui_domain()?;
        let _ = launchctl(&["bootout", &self.service()?]);
        let plist_str = self.plist.to_string_lossy();
        // bootout completes asynchronously; an immediate bootstrap can hit "Input/output error"
        // while the old instance is still being torn down, so retry briefly.
        let mut result = Err(String::new());
        for _ in 0..10 {
            result = launchctl(&["bootstrap", &domain, &plist_str]);
            if result.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        result?;
        // Trust launchd, not our own bookkeeping: the job must actually be registered.
        if self.is_loaded() {
            Ok(())
        } else {
            Err(format!("launchd did not register {}", self.label))
        }
    }

    fn uninstall(&self) -> Result<(), String> {
        let _ = self.service().map(|svc| launchctl(&["bootout", &svc]));
        if self.plist.exists() {
            std::fs::remove_file(&self.plist).map_err(|e| e.to_string())?;
        }
        if self.is_loaded() {
            return Err(format!("launchd still has {} loaded", self.label));
        }
        Ok(())
    }

    fn status(&self) -> ScheduleStatus {
        // A plist on disk that launchd hasn't loaded runs nothing, so it doesn't count.
        let installed = self.plist.exists() && self.is_loaded();
        let time = installed
            .then(|| std::fs::read_to_string(&self.plist).ok())
            .flatten()
            .and_then(|s| parse_schedule_time(&s));
        ScheduleStatus {
            installed,
            hour: time.map(|(h, _)| h),
            minute: time.map(|(_, m)| m),
        }
    }
}

// Earlier versions ran ~/.freshet/run.sh; the job no longer uses it.
fn remove_legacy_run_script() {
    if let Some(dir) = freshet_dir() {
        let _ = std::fs::remove_file(dir.join("run.sh"));
    }
}

#[tauri::command]
fn install_schedule(hour: u32, minute: u32) -> Result<(), String> {
    if find_brew().is_none() {
        return Err("Homebrew not found in /opt/homebrew or /usr/local; install it first".into());
    }
    let job = ScheduleJob::daily()?;
    if job
        .program
        .first()
        .is_some_and(|exe| is_translocated(Path::new(exe)))
    {
        return Err("Move Freshet to your Applications folder before scheduling updates".into());
    }
    remove_legacy_run_script();
    job.install(hour, minute)
}

#[tauri::command]
fn uninstall_schedule() -> Result<(), String> {
    remove_legacy_run_script();
    ScheduleJob::daily()?.uninstall()
}

// Read Hour/Minute back out of the launchd plist so the UI reflects the schedule that's
// actually installed after a relaunch, instead of a hardcoded default. Tolerates whitespace
// between the <key> and <integer> tags (a hand-edited plist). Pure → unit-tested.
fn parse_schedule_time(plist: &str) -> Option<(u32, u32)> {
    fn int_after(plist: &str, key: &str) -> Option<u32> {
        let marker = format!("<key>{key}</key>");
        let after = &plist[plist.find(&marker)? + marker.len()..];
        let open = after.find("<integer>")? + "<integer>".len();
        let rest = &after[open..];
        let end = rest.find("</integer>")?;
        rest[..end].trim().parse().ok()
    }
    let hour = int_after(plist, "Hour")?;
    let minute = int_after(plist, "Minute")?;
    valid_schedule_time(hour, minute).then_some((hour, minute))
}

#[derive(Serialize)]
struct ScheduleStatus {
    installed: bool,
    hour: Option<u32>,
    minute: Option<u32>,
}

#[tauri::command]
fn get_schedule() -> ScheduleStatus {
    ScheduleJob::daily().map_or(
        ScheduleStatus {
            installed: false,
            hour: None,
            minute: None,
        },
        |job| job.status(),
    )
}

fn main() {
    // Timings for every check/update phase: `RUST_LOG=freshet=info pnpm tauri dev`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    augment_path();
    if std::env::args().any(|a| a == RUN_SCHEDULED_FLAG) {
        std::process::exit(run_scheduled());
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // Build the menu bar tray
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;

            let icon = app
                .default_window_icon()
                .ok_or("no default window icon")?
                .clone();
            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(icon)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    let app = tray.app_handle();
                    // Tell the positioner plugin where the tray icon currently is, so the next
                    // `move_window(Position::TrayCenter)` can anchor accurately.
                    tauri_plugin_positioner::on_tray_event(app, &event);
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(win) = app.get_webview_window("main") {
                            if win.is_visible().unwrap_or(false) {
                                let _ = win.hide();
                            } else {
                                // Anchor under the tray icon on every open. Drags during the
                                // session don't persist after the window is closed — standard
                                // menu-bar UX (1Password, Bartender, etc.).
                                let _ = win.move_window(Position::TrayCenter);
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_outdated,
            check_sparkle_apps,
            search_casks,
            suggest_adoptions,
            cask_info,
            open_url,
            set_badge,
            adopt_cask,
            adopt_casks,
            run_selected_updates,
            verify_signatures,
            verify_updated_casks,
            install_schedule,
            uninstall_schedule,
            get_schedule,
            get_settings,
            save_settings,
            package_details,
            app_icon,
            get_cached_check,
            trust_signer,
            rollback::list_snapshots,
            rollback::restore_snapshot,
            vulns::check_vulnerabilities,
            stop_updates,
            get_history,
            get_scheduled_state,
            quit_and_update,
            check_app_update,
            install_app_update,
            app_version,
            get_launch_at_login,
            set_launch_at_login,
            get_last_scheduled_run
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Quitting mid-update: interrupt brew politely so it cleans up (see stop_updates),
            // rather than leaving it to die mid-install.
            if matches!(event, tauri::RunEvent::Exit) {
                interrupt_running_child();
            }
        });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    // npm: packages whose installed version is >= the registry's "latest" must be filtered out
    // (corepack/pnpm here are ahead of latest); genuinely-behind packages are kept.
    #[test]
    fn npm_skips_packages_at_or_ahead_of_latest() {
        let json = r#"{
            "corepack": {"current": "0.35.0", "wanted": "0.34.7", "latest": "0.34.7"},
            "pnpm":     {"current": "11.4.0", "wanted": "10.34.1", "latest": "10.34.1"},
            "typescript": {"current": "5.0.0", "wanted": "5.8.0", "latest": "5.8.0"}
        }"#;
        let items = parse_npm_outdated(json);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["typescript"],
            "only the genuinely-behind package is kept"
        );
        assert_eq!(items[0].current, "5.0.0");
        assert_eq!(items[0].latest, "5.8.0");
    }

    #[test]
    fn npm_handles_empty() {
        assert_eq!(parse_npm_outdated("{}"), Vec::<OutdatedItem>::new());
        assert_eq!(parse_npm_outdated("not json"), Vec::<OutdatedItem>::new());
    }

    // mas: the id is followed by multiple spaces and the name contains spaces, so the name must
    // be parsed correctly (the old splitn(3,' ') produced empty names).
    #[test]
    fn mas_parses_id_name_and_versions() {
        let out = "462058435  Microsoft Excel       (16.71 -> 16.109.2)\n\
                   462054704  Microsoft Word        (16.71 -> 16.109.2)\n";
        let items = parse_mas_outdated(out);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "462058435");
        assert_eq!(items[0].name, "Microsoft Excel");
        assert_eq!(items[0].current, "16.71");
        assert_eq!(items[0].latest, "16.109.2");
        assert_eq!(items[1].name, "Microsoft Word");
    }

    #[test]
    fn brew_parses_formulae() {
        let json = r#"{"formulae":[{"name":"wget","installed_versions":["1.21.3"],"current_version":"1.24.5"}],"casks":[]}"#;
        let items = parse_brew_outdated(json);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "wget");
        assert_eq!(items[0].current, "1.21.3");
        assert_eq!(items[0].latest, "1.24.5");
        assert!(!items[0].self_updating);
    }

    // casks: present in greedy but not in the normal set => self_updating (hidden by default).
    #[test]
    fn casks_tag_self_updating() {
        let greedy = r#"{"casks":[
            {"name":"firefox","installed_versions":["1.0"],"current_version":"2.0"},
            {"name":"google-chrome","installed_versions":["120"],"current_version":"121"}
        ]}"#;
        let mut normal = std::collections::HashSet::new();
        normal.insert("firefox".to_string()); // firefox is a "normal" outdated cask
        let items = parse_casks(greedy, &normal);
        let firefox = items.iter().find(|i| i.name == "firefox").unwrap();
        let chrome = items.iter().find(|i| i.name == "google-chrome").unwrap();
        assert!(!firefox.self_updating, "in normal set => not self-updating");
        assert!(
            chrome.self_updating,
            "greedy-only => self-updating (hidden by default)"
        );
    }

    // codesign parsing against real `codesign -dvv` output (Google Chrome).
    #[test]
    fn signing_extracts_team_id_and_authority() {
        let out = "Identifier=com.google.Chrome\n\
                   Authority=Developer ID Application: Google LLC (EQHXZ8M8AV)\n\
                   Authority=Developer ID Certification Authority\n\
                   Authority=Apple Root CA\n\
                   TeamIdentifier=EQHXZ8M8AV\n";
        let (team, authority) = parse_signing(out);
        assert_eq!(team.as_deref(), Some("EQHXZ8M8AV"));
        assert_eq!(
            authority.as_deref(),
            Some("Developer ID Application: Google LLC (EQHXZ8M8AV)")
        );
    }

    #[test]
    fn signing_handles_unsigned_and_not_set() {
        assert_eq!(parse_signing(""), (None, None));
        assert_eq!(parse_signing("TeamIdentifier=not set\n"), (None, None));
    }

    #[test]
    fn notarized_requires_accepted_and_notarized() {
        let accepted = "/Applications/OBS.app: accepted\nsource=Notarized Developer ID\n";
        assert!(parse_notarized(true, accepted));
        assert!(
            !parse_notarized(false, accepted),
            "rejected status => not notarized"
        );
        assert!(
            !parse_notarized(true, "/Applications/X.app: accepted\nsource=Unnotarized"),
            "accepted but not notarized => false"
        );
    }

    // The hijacked-release signal: a different Team ID than last seen.
    #[test]
    fn cert_change_detection() {
        assert!(cert_changed(Some("AAA"), Some("BBB")));
        assert!(!cert_changed(Some("AAA"), Some("AAA")));
        assert!(
            !cert_changed(None, Some("AAA")),
            "first sighting is not a change"
        );
        assert!(!cert_changed(Some("AAA"), None));
    }

    // Post-update mode (only_changed=true) must surface only signer-drift entries, NOT
    // unsigned-but-stable apps. The latter are real concerns for the full inventory report
    // but shouldn't fire on every `brew upgrade` if nothing actually changed.
    #[test]
    fn select_verify_results_only_changed_keeps_cert_drift_only() {
        let stable_ok = UpdateResult {
            kind: "verify".into(),
            name: "stable".into(),
            success: true,
            output: "Team ID: AAA".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
        };
        let unsigned_unchanged = UpdateResult {
            kind: "verify".into(),
            name: "always-unsigned".into(),
            success: false,
            output: "Not code-signed.".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
        };
        let drifted = UpdateResult {
            kind: "verify".into(),
            name: "drifted".into(),
            success: false,
            output: "⚠ Developer certificate CHANGED: was Team AAA, now BBB.".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
        };
        let entries = vec![
            (stable_ok.clone(), false),
            (unsigned_unchanged.clone(), false), // unsigned, but cert didn't move => not a hijack signal
            (drifted.clone(), true),             // the actual drift
        ];
        // Default: full report includes everything.
        let all = select_verify_results(entries.clone(), false);
        assert_eq!(all, vec![stable_ok, unsigned_unchanged, drifted.clone()]);
        // Post-update mode: only drift.
        let filtered = select_verify_results(entries, true);
        assert_eq!(filtered, vec![drifted]);
    }

    #[test]
    fn update_command_mapping() {
        assert_eq!(
            build_update_command("brew", "wget"),
            Some(("brew", vec!["upgrade".into(), "wget".into()]))
        );
        assert_eq!(
            build_update_command("cask", "firefox"),
            Some((
                "brew",
                vec!["upgrade".into(), "--cask".into(), "firefox".into()]
            ))
        );
        assert_eq!(
            build_update_command("mas", "462058435"),
            Some(("mas", vec!["upgrade".into(), "462058435".into()]))
        );
        assert_eq!(
            build_update_command("npm", "typescript"),
            Some((
                "npm",
                vec!["install".into(), "-g".into(), "typescript@latest".into()]
            ))
        );
        assert_eq!(
            build_update_command("npm", "@scope/pkg"),
            Some((
                "npm",
                vec!["install".into(), "-g".into(), "@scope/pkg@latest".into()]
            ))
        );
        assert_eq!(build_update_command("bogus", "x"), None);
        // crafted ids must be rejected (no flag / install-spec injection)
        assert_eq!(build_update_command("brew", "--force"), None);
        assert_eq!(build_update_command("mas", "not-a-number"), None);
        assert_eq!(
            build_update_command("npm", "https://evil.example/x.tgz"),
            None
        );
        assert_eq!(build_update_command("npm", "-g"), None);
    }

    // The concurrent pre-fetch partitions selections into formula/cask batches, reusing the same
    // id validation as the install path so a crafted token can't smuggle a flag into fetch either.
    #[test]
    fn fetch_batches_partitions_and_validates() {
        let sels = vec![
            UpdateSelection {
                kind: "brew".into(),
                id: "wget".into(),
                name: "wget".into(),
            },
            UpdateSelection {
                kind: "cask".into(),
                id: "slack".into(),
                name: "Slack".into(),
            },
            // mas/npm have no fetch-without-install step → excluded.
            UpdateSelection {
                kind: "mas".into(),
                id: "462058435".into(),
                name: "Excel".into(),
            },
            UpdateSelection {
                kind: "npm".into(),
                id: "typescript".into(),
                name: "ts".into(),
            },
            // crafted ids are rejected (no flag / path injection into fetch).
            UpdateSelection {
                kind: "brew".into(),
                id: "--force".into(),
                name: "x".into(),
            },
            UpdateSelection {
                kind: "cask".into(),
                id: "a/../b".into(),
                name: "y".into(),
            },
        ];
        let (formulae, casks) = fetch_batches(&sels);
        assert_eq!(formulae, vec!["wget"]);
        assert_eq!(casks, vec!["slack"]);
    }

    // Round-trip: the plist install_schedule writes must parse back to the same time, so the
    // settings UI shows the real installed schedule after a relaunch (not the 10:00 default).
    #[test]
    fn schedule_time_parses_from_generated_plist() {
        let plist = r#"<?xml version="1.0"?>
<plist version="1.0">
<dict>
    <key>Label</key><string>com.freshet.daily</string>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>14</integer>
        <key>Minute</key><integer>45</integer>
    </dict>
</dict>
</plist>"#;
        assert_eq!(parse_schedule_time(plist), Some((14, 45)));
        // Whitespace between key and integer (hand-edited plist) still parses.
        let spaced =
            "<key>Hour</key>\n  <integer>7</integer>\n<key>Minute</key> <integer>0</integer>";
        assert_eq!(parse_schedule_time(spaced), Some((7, 0)));
        // Missing keys, junk values, and out-of-range times are None, not garbage.
        assert_eq!(
            parse_schedule_time("<key>Hour</key><integer>9</integer>"),
            None
        );
        assert_eq!(
            parse_schedule_time(
                "<key>Hour</key><integer>x</integer><key>Minute</key><integer>0</integer>"
            ),
            None
        );
        assert_eq!(
            parse_schedule_time(
                "<key>Hour</key><integer>24</integer><key>Minute</key><integer>0</integer>"
            ),
            None
        );
        assert_eq!(parse_schedule_time(""), None);
    }

    #[test]
    fn schedule_time_range() {
        assert!(valid_schedule_time(10, 30));
        assert!(valid_schedule_time(0, 0));
        assert!(valid_schedule_time(23, 59));
        assert!(!valid_schedule_time(24, 0));
        assert!(!valid_schedule_time(10, 60));
    }

    // --- PATH for child processes ---

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

    // --- settings + scheduled run planning ---

    fn item(id: &str, latest: &str, self_updating: bool) -> OutdatedItem {
        OutdatedItem {
            id: id.into(),
            name: id.into(),
            current: "1".into(),
            latest: latest.into(),
            self_updating,
            notes_url: String::new(),
        }
    }

    fn category(kind: &str, items: Vec<OutdatedItem>) -> CategoryStatus {
        CategoryStatus {
            category: kind.into(),
            kind: kind.into(),
            items,
            error: None,
        }
    }

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
                "brew",
                vec![item("git", "2.50", false), item("node", "24", false)],
            ),
            category(
                "cask",
                vec![item("iterm2", "3.6", false), item("chrome", "140", true)],
            ),
            category("npm", vec![item("typescript", "6", false)]),
        ];
        let mut settings = Settings {
            skipped: vec![ignore_key("brew", "node", "24")],
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
        let categories = [category("brew", vec![item("node", "25", false)])];
        let settings = Settings {
            skipped: vec![ignore_key("brew", "node", "24")],
            ..Settings::default()
        };
        assert_eq!(planned_ids(&categories, &settings), ["brew:node"]);
    }

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
            kind: "cask".into(),
            name: name.into(),
            success,
            output: output.into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
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

    #[test]
    fn applescript_string_escapes_quotes() {
        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn translocated_exe_is_detected() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/Freshet.app/Contents/MacOS/freshet"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/Freshet.app/Contents/MacOS/freshet"
        )));
    }

    // --- launchd job ---

    fn schedule_test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("freshet test's {name} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn rendered_plist_is_valid_and_round_trips() {
        let dir = schedule_test_dir("plist");
        let program = [
            "/Applications/A&B.app/Contents/MacOS/freshet".to_string(),
            RUN_SCHEDULED_FLAG.into(),
        ];
        let plist = render_plist("com.freshet.daily", &program, &dir, 7, 45);
        assert_eq!(parse_schedule_time(&plist), Some((7, 45)));
        assert!(plist.contains("<string>/Applications/A&amp;B.app/Contents/MacOS/freshet</string>"));
        assert!(plist.contains("<string>--run-scheduled</string>"));
        let path = dir.join("job.plist");
        std::fs::write(&path, &plist).unwrap();
        let lint = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            lint.status.success(),
            "{}",
            String::from_utf8_lossy(&lint.stdout)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // End-to-end through the real launchd: install → registered → kickstart → launchd ran the
    // program with its args → uninstall → gone. Uses a throwaway label and a stand-in program
    // in a temp dir, so it never touches the user's real schedule or packages. Ignored by
    // default because it registers a (short-lived) job with the user's launchd; run it with:
    //   cargo test -- --ignored launchd_runs_installed_job
    #[test]
    #[ignore = "registers a throwaway job with the user's launchd"]
    fn launchd_runs_installed_job() {
        struct Uninstall<'a>(&'a ScheduleJob);
        impl Drop for Uninstall<'_> {
            fn drop(&mut self) {
                let _ = self.0.uninstall();
            }
        }

        let dir = schedule_test_dir("launchd");
        let program = dir.join("program");
        let calls = dir.join("calls");
        write_private(
            &program,
            &format!(
                "#!/bin/bash\necho \"$* PATH=$PATH\" >> '{}'\n",
                calls.display().to_string().replace('\'', r"'\''")
            ),
            true,
        )
        .unwrap();
        let job = ScheduleJob {
            label: format!("com.freshet.test.{}", std::process::id()),
            plist: dir.join("job.plist"),
            program: vec![
                program.to_string_lossy().into_owned(),
                RUN_SCHEDULED_FLAG.into(),
            ],
            log_dir: dir.clone(),
        };
        let guard = Uninstall(&job);

        job.install(3, 15).unwrap();
        let status = job.status();
        assert!(status.installed);
        assert_eq!((status.hour, status.minute), (Some(3), Some(15)));
        // Re-installing (changing the time while enabled) must replace the loaded job cleanly.
        job.install(4, 30).unwrap();
        assert_eq!(job.status().hour, Some(4));

        launchctl(&["kickstart", &job.service().unwrap()]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while read(&calls).is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // launchd invoked it with --run-scheduled, and with its bare PATH — which is why the
        // binary calls augment_path() before anything else.
        assert_eq!(
            read(&calls),
            "--run-scheduled PATH=/usr/bin:/bin:/usr/sbin:/sbin\n"
        );

        drop(guard);
        assert!(!job.is_loaded());
        assert!(!job.plist.exists());
        assert!(!job.status().installed);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn numeric_id_validation() {
        assert!(is_numeric_id("462058435"));
        assert!(!is_numeric_id("12a"));
        assert!(!is_numeric_id(""));
        assert!(!is_numeric_id("-5"));
    }

    #[test]
    fn npm_package_validation() {
        assert!(is_valid_npm_package("typescript"));
        assert!(is_valid_npm_package("@angular/cli"));
        assert!(is_valid_npm_package("lodash.merge"));
        assert!(!is_valid_npm_package("-g"), "flag");
        assert!(!is_valid_npm_package("https://evil/x.tgz"), "url spec");
        assert!(!is_valid_npm_package("git+ssh://x"), "url spec");
        assert!(!is_valid_npm_package(".hidden"), "leading dot");
        assert!(!is_valid_npm_package("UPPER"), "npm names are lowercase");
        assert!(!is_valid_npm_package(""));
    }

    #[test]
    fn lenient_version_compare() {
        assert!(version_is_older("30.2.3", "32.1.2"));
        assert!(!version_is_older("32.1.2", "32.1.2"));
        assert!(version_is_older("2.9", "2.10"), "9 < 10 component-wise");
        assert!(!version_is_older("2.10", "2.9"));
        assert!(!version_is_older("33.0", "32.1.2"));
    }

    #[test]
    fn appcast_picks_newest_version() {
        // items intentionally out of order; element + attribute forms both present
        let xml = r#"<rss><channel>
            <item><sparkle:shortVersionString>32.1.0</sparkle:shortVersionString></item>
            <item><sparkle:shortVersionString>32.1.2</sparkle:shortVersionString></item>
            <item><enclosure sparkle:shortVersionString="32.1.1" url="x.dmg"/></item>
        </channel></rss>"#;
        assert_eq!(parse_appcast_latest(xml).as_deref(), Some("32.1.2"));
    }

    #[test]
    fn appcast_empty_is_none() {
        assert_eq!(parse_appcast_latest("<rss></rss>"), None);
    }

    #[test]
    fn appcast_release_notes_link() {
        let xml = r#"<item><sparkle:releaseNotesLink>https://obsproject.com/notes.html</sparkle:releaseNotesLink></item>"#;
        assert_eq!(
            parse_appcast_notes(xml).as_deref(),
            Some("https://obsproject.com/notes.html")
        );
        assert_eq!(parse_appcast_notes("<item></item>"), None);
        // a non-http notes link is rejected
        let bad = r#"<sparkle:releaseNotesLink>file:///etc/passwd</sparkle:releaseNotesLink>"#;
        assert_eq!(parse_appcast_notes(bad), None);
    }

    #[test]
    fn badge_shows_count_or_nothing() {
        assert_eq!(badge_title(0), None);
        assert_eq!(badge_title(3).as_deref(), Some("3"));
    }

    #[test]
    fn cask_tokens_skip_headers_and_prose() {
        let out = "==> Casks\ngoogle-chrome\ngoogle-chrome@beta\nchromedriver\n";
        assert_eq!(
            parse_cask_tokens(out),
            vec!["google-chrome", "google-chrome@beta", "chromedriver"]
        );
        // brew list --cask form (no header) and a prose line are handled too
        let listed = "sublime-text\nintellij-idea-ce\nIf you meant a formula...\n";
        assert_eq!(
            parse_cask_tokens(listed),
            vec!["sublime-text", "intellij-idea-ce"]
        );
        assert_eq!(parse_cask_tokens(""), Vec::<String>::new());
    }

    #[test]
    fn cask_token_validation_rejects_flags_and_junk() {
        assert!(is_valid_cask_token("google-chrome"));
        assert!(is_valid_cask_token("google-chrome@beta"));
        assert!(is_valid_cask_token("1password"));
        assert!(
            !is_valid_cask_token("--force"),
            "leading dash = flag injection"
        );
        assert!(!is_valid_cask_token(""), "empty");
        assert!(!is_valid_cask_token("a b"), "space");
        assert!(!is_valid_cask_token("a/../b"), "slash/path chars");
        assert!(!is_valid_cask_token(&"a".repeat(200)), "too long");
    }

    #[test]
    fn search_query_validation() {
        assert!(is_safe_search_query("chrome"));
        assert!(is_safe_search_query("google chrome"));
        assert!(!is_safe_search_query("-x"), "leading dash = flag injection");
        assert!(!is_safe_search_query("   "), "blank");
        assert!(!is_safe_search_query("a\nb"), "control char");
    }

    #[test]
    fn url_validation_only_allows_http() {
        assert!(is_safe_url("https://example.com/path"));
        assert!(is_safe_url("http://example.com"));
        assert!(!is_safe_url("file:///etc/passwd"), "no file://");
        assert!(!is_safe_url("javascript:alert(1)"), "no js scheme");
        assert!(!is_safe_url("https://e.com/ x"), "no spaces");
        assert!(!is_safe_url(""), "empty");
    }

    #[test]
    fn github_releases_url_from_homepage_or_repo() {
        assert_eq!(
            github_releases_url(["https://github.com/awslabs/aws-c-auth"]).as_deref(),
            Some("https://github.com/awslabs/aws-c-auth/releases")
        );
        // Falls through non-GitHub URLs to the source repo; strips .git and extra path.
        assert_eq!(
            github_releases_url(["https://git-scm.com", "https://github.com/git/git.git"])
                .as_deref(),
            Some("https://github.com/git/git/releases")
        );
        assert_eq!(
            github_releases_url(["https://github.com/o/r/archive/v1.tar.gz"]).as_deref(),
            Some("https://github.com/o/r/releases")
        );
        assert_eq!(github_releases_url(["https://abseil.io", ""]), None);
        assert_eq!(github_releases_url(["https://github.com/onlyowner"]), None);
        assert_eq!(
            github_releases_url(["https://github.com/o/r?x=<script>"]),
            None
        );
    }

    // Trimmed from real `brew info --json=v2 --formula abseil git` output.
    #[test]
    fn formula_infos_parse_real_output() {
        let json = r#"{"formulae":[
          {"name":"abseil","desc":"C++ Common Libraries","homepage":"https://abseil.io",
           "license":"Apache-2.0",
           "urls":{"stable":{"url":"https://github.com/abseil/abseil-cpp/archive/refs/tags/20250814.1.tar.gz"},
                   "head":{"url":"https://github.com/abseil/abseil-cpp.git","branch":"master"}},
           "installed":[{"version":"20250512.1","installed_on_request":false}]},
          {"name":"git","desc":"Distributed revision control system","homepage":"https://git-scm.com",
           "license":"GPL-2.0-only","urls":{"stable":{"url":"https://mirrors.edge.kernel.org/pub/software/scm/git/git-2.56.0.tar.xz"},
           "head":{"url":"https://github.com/git/git.git"}},
           "installed":[{"version":"2.49.0","installed_on_request":true}]}],"casks":[]}"#;
        let infos = parse_formula_infos(json);
        let abseil = &infos["abseil"];
        assert_eq!(abseil.desc, "C++ Common Libraries");
        assert!(abseil.dependency);
        assert_eq!(
            abseil.release_notes,
            "https://github.com/abseil/abseil-cpp/releases"
        );
        let git = &infos["git"];
        assert!(!git.dependency);
        assert_eq!(git.homepage, "https://git-scm.com");
        assert_eq!(git.license, "GPL-2.0-only");
        assert_eq!(git.release_notes, "https://github.com/git/git/releases");
        assert!(parse_formula_infos("not json").is_empty());
    }

    // Trimmed from real `brew info --json=v2 --cask slack` output.
    #[test]
    fn cask_infos_parse_real_output() {
        let json = r#"{"formulae":[],"casks":[{"token":"slack","desc":"Team communication and collaboration software",
          "homepage":"https://slack.com/","url":"https://downloads.slack-edge.com/desktop-releases/macos/arm64/4.46.101/Slack-4.46.101-macOS.dmg",
          "artifacts":[{"uninstall":[{"quit":"com.tinyspeck.slackmacgap"}]},{"app":["Slack.app"]},{"zap":[]}],
          "auto_updates":true}]}"#;
        let slack = &parse_cask_infos(json)["slack"];
        assert_eq!(slack.desc, "Team communication and collaboration software");
        assert_eq!(slack.app, "Slack.app");
        assert!(slack.auto_updates);
        assert!(!slack.dependency);
        assert_eq!(slack.release_notes, "");
    }

    // Real I/O against this Mac: Safari ships in /Applications on every macOS install.
    //   cargo test -- --ignored app_icon_from_real_bundle
    #[test]
    #[ignore = "reads /Applications and writes ~/.freshet/icons"]
    fn app_icon_from_real_bundle() {
        let png = app_icon_png("Safari.app").expect("Safari icon");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(app_icon_png("No Such App.app").is_none());
    }

    // Real I/O: full /Applications scan twice; the second is served from the notarization cache.
    // Updates ~/.freshet/signing.json and notarization.json exactly as the app does.
    //   cargo test -- --ignored full_scan_timing --nocapture
    #[test]
    #[ignore = "scans /Applications (~30s first time)"]
    fn full_scan_timing() {
        for pass in ["first", "second"] {
            let started = std::time::Instant::now();
            let entries = verify_all_blocking(&|_, _| {});
            eprintln!(
                "{pass} scan: {} apps in {}ms",
                entries.len(),
                elapsed_ms(started)
            );
        }
    }

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

    #[test]
    fn app_name_validation() {
        assert!(is_valid_app_name("Slack.app"));
        assert!(is_valid_app_name("IntelliJ IDEA CE.app"));
        assert!(!is_valid_app_name("../../etc.app"));
        assert!(!is_valid_app_name("Foo/Bar.app"));
        assert!(!is_valid_app_name(".app"));
        assert!(!is_valid_app_name(".hidden.app"));
        assert!(!is_valid_app_name("Slack"));
    }

    #[test]
    fn cask_info_parses_fields() {
        let json = r#"{"casks":[{"token":"google-chrome","name":["Google Chrome"],"desc":"Web browser","homepage":"https://www.google.com/chrome/","version":"148.0","auto_updates":true}]}"#;
        let info = parse_cask_info(json).unwrap();
        assert_eq!(info.token, "google-chrome");
        assert_eq!(info.name, "Google Chrome");
        assert_eq!(info.desc, "Web browser");
        assert_eq!(info.version, "148.0");
        assert!(info.auto_updates);
        assert!(parse_cask_info("{}").is_none());
    }

    #[test]
    fn adopt_args_uses_adopt_flag() {
        assert_eq!(
            adopt_args("google-chrome"),
            vec!["install", "--cask", "google-chrome", "--adopt"]
        );
    }

    #[test]
    fn mas_hint_only_on_that_error() {
        let with_err =
            maybe_append_mas_hint("Error: No downloads initiated for ADAM ID 462058435.".into());
        assert!(with_err.contains("different Apple ID"));
        let clean = maybe_append_mas_hint("Installing Microsoft Excel... done".into());
        assert_eq!(clean, "Installing Microsoft Excel... done");
    }

    // ---- deep-audit edge cases / random flows ----

    #[test]
    fn combine_output_merges_streams() {
        assert_eq!(combine_output(b"out", b""), "out");
        assert_eq!(combine_output(b"", b"err"), "err");
        assert_eq!(combine_output(b"out", b"err"), "out\nerr"); // separator inserted
        assert_eq!(combine_output(b"out\n", b"err"), "out\nerr"); // no double newline
        assert_eq!(combine_output(b"", b"   "), ""); // whitespace-only stderr ignored
        assert_eq!(combine_output(b"", b""), "");
    }

    #[test]
    fn brew_handles_malformed_and_missing() {
        assert_eq!(parse_brew_outdated("not json"), Vec::<OutdatedItem>::new());
        assert_eq!(
            parse_brew_outdated(r#"{"casks":[]}"#),
            Vec::<OutdatedItem>::new()
        ); // no formulae key
        let items = parse_brew_outdated(r#"{"formulae":[{"name":"x"}]}"#);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].current, "");
        assert_eq!(items[0].latest, "");
        // empty installed_versions array must not panic
        let i2 = parse_brew_outdated(r#"{"formulae":[{"name":"y","installed_versions":[]}]}"#);
        assert_eq!(i2[0].current, "");
    }

    #[test]
    fn casks_empty_normal_set_all_self_updating() {
        let greedy =
            r#"{"casks":[{"name":"firefox","installed_versions":["1"],"current_version":"2"}]}"#;
        let none = std::collections::HashSet::new();
        assert!(parse_casks(greedy, &none)[0].self_updating);
        assert_eq!(parse_casks("garbage", &none), Vec::<OutdatedItem>::new());
        assert_eq!(
            parse_cask_names(r#"{"casks":[{"name":"a"},{"name":"b"}]}"#).len(),
            2
        );
        assert!(parse_cask_names("nope").is_empty());
    }

    #[test]
    fn mas_handles_weird_lines() {
        let items = parse_mas_outdated("123  Some App"); // no parens
        assert_eq!(items[0].id, "123");
        assert_eq!(items[0].name, "Some App");
        assert_eq!(items[0].latest, "");
        assert_eq!(parse_mas_outdated("462058435"), Vec::<OutdatedItem>::new()); // no whitespace -> can't split
        assert_eq!(parse_mas_outdated("\n\n  \n").len(), 0); // blank lines
        let t = parse_mas_outdated("999\tCoolApp (1.0 -> 2.0)"); // tab separator
        assert_eq!(t[0].id, "999");
        assert_eq!(t[0].current, "1.0");
        assert_eq!(t[0].latest, "2.0");
    }

    #[test]
    fn npm_keeps_unparseable_versions() {
        // non-semver versions can't be compared -> kept, not silently dropped
        let items = parse_npm_outdated(r#"{"weird":{"current":"1.2","latest":"1.3"}}"#);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "weird");
        assert_eq!(parse_npm_outdated("[1,2,3]"), Vec::<OutdatedItem>::new()); // non-object json
        assert_eq!(parse_npm_outdated(""), Vec::<OutdatedItem>::new());
    }

    #[test]
    fn version_compare_edge_cases() {
        assert!(!version_is_older("1.2", "1.2.0")); // equal across differing lengths
        assert!(!version_is_older("1.2.0", "1.2"));
        assert!(version_is_older("1.2", "1.2.1"));
        assert!(!version_is_older("1.2.0-beta", "1.2.0")); // pre-release suffix loose
        assert!(version_is_older("1.09", "1.10")); // numeric, not lexical
        assert!(!version_is_older("", ""));
        assert!(version_is_older("", "1.0")); // empty treated as 0
    }

    #[test]
    fn appcast_both_forms_and_max() {
        assert_eq!(
            parse_appcast_latest(
                "<sparkle:shortVersionString>1.0</sparkle:shortVersionString>\
                 <sparkle:shortVersionString>1.10</sparkle:shortVersionString>\
                 <sparkle:shortVersionString>1.2</sparkle:shortVersionString>"
            )
            .as_deref(),
            Some("1.10")
        );
        assert_eq!(
            parse_appcast_latest(r#"<enclosure sparkle:shortVersionString="3.1" />"#).as_deref(),
            Some("3.1")
        );
        assert_eq!(parse_appcast_latest("nothing here"), None);
    }

    #[test]
    fn cask_info_tolerates_missing_fields() {
        let info = parse_cask_info(r#"{"casks":[{"token":"x"}]}"#).unwrap();
        assert_eq!(info.token, "x");
        assert_eq!(info.name, "");
        assert!(!info.auto_updates);
        assert!(parse_cask_info(r#"{"casks":[]}"#).is_none());
    }

    #[test]
    fn signing_takes_first_authority() {
        let out = "Authority=Leaf Signer\nAuthority=Intermediate\nTeamIdentifier=ABC123\n";
        let (team, auth) = parse_signing(out);
        assert_eq!(team.as_deref(), Some("ABC123"));
        assert_eq!(auth.as_deref(), Some("Leaf Signer"));
    }

    #[test]
    fn url_host_extracts_hostname() {
        assert_eq!(
            url_host("https://example.com/path").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("https://Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("http://example.com:8080/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("https://user:pass@example.com/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(url_host("https://[::1]:443/x").as_deref(), Some("::1"));
        assert_eq!(url_host("https://[fe80::1]/x").as_deref(), Some("fe80::1"));
        // Path / query / fragment terminate the host.
        assert_eq!(
            url_host("https://example.com?q=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("https://example.com#frag").as_deref(),
            Some("example.com")
        );
        // Non-http(s) and malformed cases.
        assert_eq!(url_host("file:///etc/passwd"), None);
        assert_eq!(url_host("https://"), None);
        assert_eq!(url_host("https://[unclosed"), None);
    }

    #[test]
    fn private_host_rejection() {
        // Loopback / private / link-local / unspecified / broadcast / CGNAT.
        assert!(is_local_or_private_host("localhost"));
        assert!(is_local_or_private_host("foo.local"));
        assert!(is_local_or_private_host("intranet.internal"));
        assert!(is_local_or_private_host("box.localhost"));
        assert!(is_local_or_private_host("127.0.0.1"));
        assert!(is_local_or_private_host("10.0.0.1"));
        assert!(is_local_or_private_host("192.168.1.1"));
        assert!(is_local_or_private_host("172.16.0.1"));
        assert!(is_local_or_private_host("172.31.255.255"));
        assert!(is_local_or_private_host("169.254.169.254"), "AWS metadata");
        assert!(is_local_or_private_host("0.0.0.0"));
        assert!(is_local_or_private_host("255.255.255.255"));
        assert!(is_local_or_private_host("100.64.0.1"), "carrier-grade NAT");
        assert!(is_local_or_private_host("100.127.255.255"), "CGNAT upper");
        assert!(is_local_or_private_host("::1"));
        assert!(is_local_or_private_host("::"), "IPv6 unspecified");
        assert!(is_local_or_private_host("fc00::1"), "ULA");
        assert!(is_local_or_private_host("fd12:3456:789a::1"), "ULA");
        assert!(is_local_or_private_host("fe80::1"), "link-local v6");
        // Public — must NOT be flagged.
        assert!(!is_local_or_private_host("example.com"));
        assert!(!is_local_or_private_host("github.com"));
        assert!(!is_local_or_private_host("8.8.8.8"));
        assert!(
            !is_local_or_private_host("172.32.0.1"),
            "just outside private range"
        );
        assert!(
            !is_local_or_private_host("100.63.255.255"),
            "just outside CGNAT"
        );
        assert!(
            !is_local_or_private_host("100.128.0.1"),
            "just outside CGNAT"
        );
        assert!(
            !is_local_or_private_host("2606:4700:4700::1111"),
            "Cloudflare v6"
        );
    }

    #[test]
    fn feed_url_requires_https_and_public_host() {
        assert!(is_safe_feed_url(
            "https://obsproject.com/osx_update/updates.xml"
        ));
        // http is rejected outright — Sparkle 2 requires https; older feeds can MITM us.
        assert!(!is_safe_feed_url(
            "http://obsproject.com/osx_update/updates.xml"
        ));
        assert!(!is_safe_feed_url("file:///etc/passwd"));
        assert!(!is_safe_feed_url("javascript:alert(1)"));
        // SSRF-style hostile feeds.
        assert!(!is_safe_feed_url("https://localhost/x.xml"));
        assert!(!is_safe_feed_url("https://127.0.0.1/x.xml"));
        assert!(!is_safe_feed_url("https://192.168.1.1/x.xml"));
        assert!(!is_safe_feed_url(
            "https://169.254.169.254/latest/meta-data/"
        ));
        assert!(!is_safe_feed_url("https://[::1]/x.xml"));
        assert!(!is_safe_feed_url("https://router.local/x.xml"));
        // Junk.
        assert!(!is_safe_feed_url(""));
        assert!(!is_safe_feed_url("https://e.com/ x"));
        assert!(!is_safe_feed_url(&format!(
            "https://e.com/{}",
            "a".repeat(2050)
        )));
    }

    #[test]
    fn validator_boundaries() {
        assert!(is_valid_cask_token(&"a".repeat(128)));
        assert!(!is_valid_cask_token(&"a".repeat(129)));
        assert!(is_valid_cask_token("node@18"));
        assert!(is_numeric_id("007"));
        assert!(!is_numeric_id(&"9".repeat(21)));
        assert!(is_valid_npm_package("lodash.merge"));
        assert!(is_valid_npm_package("@scope/pkg.name"));
        assert!(!is_valid_npm_package(&"a".repeat(215)));
        assert!(!is_safe_url("HTTPS://example.com")); // case-sensitive scheme (conservative)
        assert!(is_safe_url("https://example.com"));
        assert!(!is_safe_url(&format!("https://{}", "a".repeat(2050))));
        assert!(!is_safe_search_query(&"a".repeat(65)));
        assert!(is_safe_search_query("visual studio code"));
    }

    // Story 2 (manual check resilience): when one source fails, it returns a category with the
    // error attached and an empty items list — the aggregator doesn't lose the other sources.
    #[test]
    fn failed_category_err_carries_error_and_empty_items() {
        let s = failed_category_err("Homebrew", "brew", "brew not found".into());
        assert_eq!(s.category, "Homebrew");
        assert_eq!(s.kind, "brew");
        assert_eq!(s.items, Vec::<OutdatedItem>::new());
        assert_eq!(s.error.as_deref(), Some("brew not found"));
    }

    // Stories 1 + 8: first sighting writes a baseline silently (no CHANGED warning); a stable
    // second sighting stays silent; a Team-ID change between sightings fires the hijack message
    // with both old and new Team IDs and downgrades success.
    #[test]
    fn build_verify_entries_baseline_then_drift() {
        let mut store: HashMap<String, String> = HashMap::new();

        // First sighting — silent adoption.
        let sigs1 = vec![AppSig {
            name: "App.app".into(),
            bundle_id: None,
            team_id: Some("AAA".into()),
            authority: Some("Developer ID Application: Acme (AAA)".into()),
            signed: true,
            notarized: true,
        }];
        let entries1 = build_verify_entries(&sigs1, &mut store);
        assert_eq!(entries1.len(), 1);
        let (r1, changed1) = &entries1[0];
        assert!(!changed1, "first sighting is not a change");
        assert!(r1.success, "signed + unchanged = verified");
        assert!(!r1.output.contains("CHANGED"));
        assert_eq!(store.get("App.app").map(String::as_str), Some("AAA"));

        // Same Team ID on the next call — quiet.
        let entries2 = build_verify_entries(&sigs1, &mut store);
        let (_, changed2) = &entries2[0];
        assert!(!changed2, "unchanged Team ID is silent");

        // Hijack signal: a different Team ID for the same app.
        let sigs3 = vec![AppSig {
            name: "App.app".into(),
            bundle_id: None,
            team_id: Some("BBB".into()),
            authority: Some("Developer ID Application: Notacme (BBB)".into()),
            signed: true,
            notarized: true,
        }];
        let entries3 = build_verify_entries(&sigs3, &mut store);
        let (r3, changed3) = &entries3[0];
        assert!(changed3, "Team ID drift = change");
        assert!(!r3.success, "drift downgrades success");
        assert!(r3.output.contains("CHANGED"));
        assert!(r3.output.contains("AAA"));
        assert!(r3.output.contains("BBB"));
        // The baseline does NOT move on drift: the warning repeats until the user trusts it.
        assert_eq!(store.get("App.app").map(String::as_str), Some("AAA"));
        assert!(
            matches!(r3.action, Some(ResultAction::TrustSigner { ref team, .. }) if team == "BBB")
        );
        let (_, still_changed) = &build_verify_entries(&sigs3, &mut store)[0];
        assert!(
            still_changed,
            "a second scan still flags the unacknowledged change"
        );
    }

    // Bundle-id keying: an entry stored under the old app-name key is honoured as the baseline
    // and migrated to the bundle id; trusting a new publisher is what moves the baseline.
    #[test]
    fn signer_store_migrates_name_keys_and_trust_moves_baseline() {
        let mut store: HashMap<String, String> =
            HashMap::from([("Slack".into(), "BQR82RBBHL".into())]);
        let sig = |team: &str| AppSig {
            name: "Slack".into(),
            bundle_id: Some("com.tinyspeck.slackmacgap".into()),
            team_id: Some(team.into()),
            authority: None,
            signed: true,
            notarized: true,
        };
        let (_, changed) = &build_verify_entries(&[sig("BQR82RBBHL")], &mut store)[0];
        assert!(!changed);
        assert_eq!(
            store.get("com.tinyspeck.slackmacgap").map(String::as_str),
            Some("BQR82RBBHL")
        );
        assert!(!store.contains_key("Slack"), "legacy name key migrated");

        let (_, changed) = &build_verify_entries(&[sig("EVILEVIL99")], &mut store)[0];
        assert!(changed);
        // What trust_signer does:
        store.insert("com.tinyspeck.slackmacgap".into(), "EVILEVIL99".into());
        let (_, changed) = &build_verify_entries(&[sig("EVILEVIL99")], &mut store)[0];
        assert!(!changed, "trusted publisher is quiet");
    }

    #[test]
    fn identifier_parsing_from_real_codesign_output() {
        let out = "Executable=/Applications/Slack.app/Contents/MacOS/Slack\nIdentifier=com.tinyspeck.slackmacgap\nFormat=app bundle with Mach-O universal (x86_64 arm64)\nAuthority=Developer ID Application: SLACK TECHNOLOGIES L.L.C. (BQR82RBBHL)\nTeamIdentifier=BQR82RBBHL\n";
        assert_eq!(
            parse_identifier(out).as_deref(),
            Some("com.tinyspeck.slackmacgap")
        );
        assert_eq!(parse_identifier("TeamIdentifier=X\n"), None);
    }

    #[test]
    fn trust_inputs_are_validated() {
        assert!(is_valid_bundle_id("com.tinyspeck.slackmacgap"));
        assert!(!is_valid_bundle_id("com.x/../../etc"));
        assert!(!is_valid_bundle_id(""));
        assert!(is_valid_team_id("BQR82RBBHL"));
        assert!(!is_valid_team_id("bqr82rbbhl"));
        assert!(!is_valid_team_id("SHORT"));
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

    #[test]
    fn brew_update_staleness() {
        assert!(update_is_stale(None, 1000, 3600));
        assert!(!update_is_stale(Some(1000), 1000 + 3599, 3600));
        assert!(update_is_stale(Some(1000), 1000 + 3600, 3600));
        assert!(
            update_is_stale(Some(5000), 1000, 3600),
            "future stamp (clock moved back) is stale"
        );
    }

    // brew's batched output: "==> Upgrading <name>" opens each package's section (Homebrew
    // upgrade.rb / cask/upgrade.rb); failures are "Error: <name>: …", often at the very end.
    #[test]
    fn batch_output_splits_per_package() {
        let out = "==> Fetching downloads for: git and wget\n==> Upgrading git\n  2.55.0 -> 2.56.0\n==> Pouring git--2.56.0.arm64_tahoe.bottle.tar.gz\n🍺  /opt/homebrew/Cellar/git/2.56.0: 1,700 files\n==> Upgrading wget\n  1.25.0 -> 1.25.1\n==> Pouring wget--1.25.1.arm64_tahoe.bottle.tar.gz\nError: wget: post-install step failed\n";
        let sections = split_brew_sections(out, &["git", "wget", "absent"]);
        assert!(sections["git"].starts_with("==> Upgrading git"));
        assert!(sections["git"].contains("1,700 files"));
        assert!(!sections["git"].contains("wget"));
        assert!(
            sections["wget"].contains("1.25.1")
                && sections["wget"].ends_with("Error: wget: post-install step failed")
        );
        assert!(!sections.contains_key("absent"));
    }

    #[test]
    fn upgrading_lines_name_only_requested_packages() {
        let wanted: std::collections::HashSet<&str> = ["git", "ffmpeg"].into_iter().collect();
        assert_eq!(upgrading_name("==> Upgrading git", &wanted), Some("git"));
        assert_eq!(
            upgrading_name("==> Upgrading ffmpeg 7.1 -> 8.0", &wanted),
            Some("ffmpeg")
        );
        // A dependency brew upgrades along the way, and the cask-batch header, aren't counted.
        assert_eq!(upgrading_name("==> Upgrading x265", &wanted), None);
        assert_eq!(
            upgrading_name("==> Upgrading 2 outdated packages:", &wanted),
            None
        );
        assert_eq!(
            upgrading_name("==> Pouring git--2.56.0.bottle.tar.gz", &wanted),
            None
        );
    }

    // Tests that spawn through run_update_cmd_streaming share the global RUNNING_CHILD slot.
    static CHILD_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Lines arrive one at a time while the process runs, stdout and stderr interleaved in order.
    #[test]
    fn streaming_runner_delivers_lines_in_order() {
        let _slot = CHILD_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = Vec::new();
        let (ok, all) = run_update_cmd_streaming(
            "/bin/sh",
            &[
                "-c",
                "echo '==> Upgrading a'; echo oops >&2; echo '==> Upgrading b'; exit 3",
            ],
            &mut |l| lines.push(l.to_string()),
        );
        assert!(!ok);
        assert_eq!(lines, ["==> Upgrading a", "oops", "==> Upgrading b"]);
        assert_eq!(all, "==> Upgrading a\noops\n==> Upgrading b");
    }

    #[test]
    fn cask_token_candidates_follow_homebrew_naming() {
        assert_eq!(cask_token_candidates("Google Chrome")[0], "google-chrome");
        assert_eq!(
            cask_token_candidates("Visual Studio Code")[0],
            "visual-studio-code"
        );
        assert!(cask_token_candidates("zoom.us").contains(&"zoom".to_string()));
        assert!(cask_token_candidates("Notepad++")[0].starts_with("notepad-plus-plus"));
        assert!(cask_token_candidates("IntelliJ IDEA CE").contains(&"intellij-idea-ce".to_string()));
        assert_eq!(cask_token_candidates("  "), Vec::<String>::new());
    }

    // Only a cask whose own `app` artifact is this exact bundle counts: a candidate token that
    // merely exists (here "zoom" installing something else) is rejected.
    #[test]
    fn adoptions_require_an_exact_app_match() {
        let apps = vec![
            ("Google Chrome.app".to_string(), "Google Chrome".to_string()),
            ("zoom.us.app".to_string(), "zoom.us".to_string()),
        ];
        let candidates = HashMap::from([
            (
                "Google Chrome.app".to_string(),
                vec!["google-chrome".to_string()],
            ),
            (
                "zoom.us.app".to_string(),
                vec!["zoom-us".to_string(), "zoom".to_string()],
            ),
        ]);
        let cask_apps = HashMap::from([
            (
                "google-chrome".to_string(),
                vec!["Google Chrome.app".to_string()],
            ),
            ("zoom".to_string(), vec!["zoom.us.app".to_string()]),
            (
                "zoom-us".to_string(),
                vec!["Something Else.app".to_string()],
            ),
        ]);
        let found = match_adoptions(&apps, &candidates, &cask_apps);
        let pairs: Vec<(&str, &str)> = found
            .iter()
            .map(|a| (a.name.as_str(), a.token.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("Google Chrome", "google-chrome"), ("zoom.us", "zoom")]
        );
    }

    // Stop's mechanism end to end: at a package boundary the running process gets SIGINT, and
    // gets to run its interrupt handler (as brew's Ruby `Interrupt` → cleanup/relink does) —
    // so the next package never starts.
    #[test]
    fn stop_interrupts_at_a_boundary_and_lets_the_process_clean_up() {
        let _slot = CHILD_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Traps both: wherever SIGINT is ignored at start (as in many CI/background contexts),
        // this exercises the SIGTERM fallback.
        let script = "trap 'echo cleaned-up; exit 130' INT TERM; \
                      echo '==> Upgrading a'; sleep 0.2; \
                      echo '==> Upgrading b'; sleep 10 & wait $!; \
                      echo '==> Upgrading c'";
        let started = std::time::Instant::now();
        let mut lines = Vec::new();
        let (ok, _) = run_update_cmd_streaming("/bin/bash", &["-c", script], &mut |l| {
            lines.push(l.to_string());
            if l == "==> Upgrading b" {
                interrupt_running_child();
            }
        });
        assert!(!ok);
        assert_eq!(lines, ["==> Upgrading a", "==> Upgrading b", "cleaned-up"]);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(8),
            "didn't wait out the sleep"
        );
        assert_eq!(
            RUNNING_CHILD.load(Ordering::SeqCst),
            0,
            "child cleared after exit"
        );
    }

    // Real Homebrew + /Applications, read-only (brew casks / list / info).
    //   cargo test -- --ignored adoption_live --nocapture
    #[test]
    #[ignore = "reads the real /Applications and Homebrew"]
    fn adoption_live() {
        let started = std::time::Instant::now();
        let found = suggest_adoptions_blocking();
        eprintln!("{} suggestions in {}ms:", found.len(), elapsed_ms(started));
        for a in &found {
            eprintln!("  {} → {}", a.name, a.token);
        }
    }

    #[test]
    fn cached_list_is_dropped_after_a_newer_scheduled_run() {
        assert!(cache_is_current(1000, None), "no scheduled run ever");
        assert!(
            cache_is_current(1000, Some(900)),
            "run finished before the check"
        );
        assert!(
            !cache_is_current(1000, Some(1000)),
            "same second: assume stale"
        );
        assert!(
            !cache_is_current(1000, Some(1500)),
            "run finished after: list is stale"
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

    // App Store updates run as `sudo -A <absolute mas> upgrade <id>`, so the password comes from
    // Freshet's dialog (mas's own sudo call can't use one).
    #[test]
    fn app_store_updates_get_root_through_the_askpass_dialog() {
        let (cmd, args) = sudo_askpass_command(
            Path::new("/opt/homebrew/bin/mas"),
            vec!["upgrade".into(), "462058435".into()],
        );
        assert_eq!(cmd, "/usr/bin/sudo");
        assert_eq!(
            args,
            ["-A", "/opt/homebrew/bin/mas", "upgrade", "462058435"]
        );
    }

    #[test]
    fn find_in_path_returns_the_first_executable() {
        let dir = std::env::temp_dir().join(format!("freshet-path-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("tool"), "").unwrap(); // not executable: skipped
        write_private(&b.join("tool"), "#!/bin/sh\n", true).unwrap();
        let path = format!("{}:{}", a.display(), b.display());
        assert_eq!(find_in_path("tool", &path), Some(b.join("tool")));
        assert_eq!(find_in_path("absent", &path), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn package_boundaries() {
        assert!(is_package_boundary("==> Upgrading ffmpeg"));
        assert!(is_package_boundary(
            "==> Installing dependencies for ffmpeg: x265"
        ));
        assert!(!is_package_boundary("==> Upgrading 2 outdated packages:"));
        assert!(!is_package_boundary(
            "==> Pouring ffmpeg--8.0.bottle.tar.gz"
        ));
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

    fn sel(kind: &str, id: &str) -> UpdateSelection {
        UpdateSelection {
            kind: kind.into(),
            id: id.into(),
            name: id.into(),
        }
    }

    #[test]
    fn history_records_outcomes_and_versions_but_not_unstarted_items() {
        let selections = [
            sel("brew", "git"),
            sel("cask", "slack"),
            sel("cask", "zoom"),
            sel("brew", "go"),
        ];
        let ok = UpdateResult {
            kind: "brew".into(),
            name: "git".into(),
            success: true,
            output: String::new(),
            action: None,
            elapsed_ms: Some(1200),
            skipped: false,
        };
        let open = skipped_open(&selections[1], Path::new("/Applications/Slack.app"));
        let admin = failed_result(&selections[2], "sudo: a password is required".into());
        let stopped = not_started(&selections[3]);
        let versions = Versions::from([(
            ("brew".into(), "git".into()),
            ("2.55.0".into(), "2.56.0".into()),
        )]);
        let entries = history_entries(
            &selections,
            &[ok, open, admin, stopped],
            &versions,
            "manual",
            100,
        );
        let summary: Vec<(&str, &str, &str, &str)> = entries
            .iter()
            .map(|e| {
                (
                    e.id.as_str(),
                    e.outcome.as_str(),
                    e.from.as_str(),
                    e.to.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("git", "updated", "2.55.0", "2.56.0"),
                ("slack", "skipped_open", "", ""),
                ("zoom", "needs_admin", "", "")
            ]
        );
        assert_eq!(entries[0].elapsed_ms, Some(1200));
    }

    #[test]
    fn outdated_names_cover_formulae_and_casks() {
        let json = r#"{"formulae":[{"name":"git","installed_versions":["2.55.0"],"current_version":"2.56.0"}],"casks":[{"name":"discord","installed_versions":["0.0.407"],"current_version":"0.0.414"}]}"#;
        let names = outdated_names(json);
        assert!(names.contains("git") && names.contains("discord") && names.len() == 2);
    }

    #[test]
    fn notarization_cache_only_hits_for_unchanged_apps() {
        let cache: NotarizationCache = HashMap::from([("/Applications/A.app".into(), (100, true))]);
        assert_eq!(
            cached_notarization(&cache, "/Applications/A.app", Some(100)),
            Some(true)
        );
        assert_eq!(
            cached_notarization(&cache, "/Applications/A.app", Some(101)),
            None,
            "app changed"
        );
        assert_eq!(
            cached_notarization(&cache, "/Applications/A.app", None),
            None
        );
        assert_eq!(
            cached_notarization(&cache, "/Applications/B.app", Some(100)),
            None
        );
    }

    #[test]
    fn blocked_installs_count_as_signer_changes_in_the_summary() {
        let mut summary = ScheduledRunSummary::default();
        summarize_results(
            &mut summary,
            &[UpdateResult {
                kind: "cask".into(),
                name: "discord".into(),
                success: false,
                output: "Blocked before install".into(),
                action: Some(ResultAction::TrustSigner {
                    bundle_id: "com.hnc.Discord".into(),
                    team: "EVILEVIL99".into(),
                }),
                elapsed_ms: None,
                skipped: false,
            }],
        );
        assert_eq!(summary.signer_changed, ["discord"]);
        assert_eq!(summary.failed, Vec::<String>::new());
    }

    // Story 9: unsigned apps are flagged (success=false) in the full inventory, but they are
    // NOT drift (changed=false) — so the post-update filter (only_changed=true) excludes them.
    // That keeps the post-update warning channel reserved for actual hijack signals.
    #[test]
    fn build_verify_entries_unsigned_is_flagged_but_not_drift() {
        let mut store = HashMap::new();
        let sigs = vec![AppSig {
            name: "Sketchy.app".into(),
            bundle_id: None,
            team_id: None,
            authority: None,
            signed: false,
            notarized: false,
        }];
        let entries = build_verify_entries(&sigs, &mut store);
        let (r, changed) = &entries[0];
        assert!(!changed, "unsigned-but-stable is not drift");
        assert!(!r.success, "still flagged in the full report");
        assert!(r.output.contains("Not code-signed"));
        // No Team ID to remember, so the store is untouched.
        assert!(store.is_empty());
    }

    // Story 20: when an item in a batch can't be mapped to a real command (bogus kind, crafted
    // id), the batch records a per-item failure and keeps going. With no brew/cask in the
    // selection list, the `brew update` preamble is skipped too — this test runs entirely
    // in-process with no subprocess spawns.
    #[test]
    fn run_selected_updates_skips_unsupported_and_continues() {
        let bad = vec![
            UpdateSelection {
                kind: "bogus".into(),
                id: "x".into(),
                name: "X".into(),
            },
            UpdateSelection {
                kind: "mas".into(),
                id: "not-numeric".into(),
                name: "Bad MAS".into(),
            },
            UpdateSelection {
                kind: "npm".into(),
                id: "-g".into(),
                name: "Bad NPM".into(),
            },
        ];
        let results = run_selected_updates_blocking(&bad, &|_| {});
        assert_eq!(results.len(), 3, "every selection contributes a result row");
        for r in &results {
            assert!(!r.success);
            assert!(r.output.contains("unsupported or invalid update"));
        }
        // Names round-trip so the UI can attribute each failure.
        assert_eq!(results[0].name, "X");
        assert_eq!(results[1].name, "Bad MAS");
        assert_eq!(results[2].name, "Bad NPM");
    }
}
