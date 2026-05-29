// src-tauri/src/main.rs
// Backend commands for Freshet
// - check_outdated: queries each package manager for what's outdated
// - run_updates: invokes the upgrade commands
// - install_schedule / uninstall_schedule: manages a launchd plist
// - is_schedule_installed: checks current state

use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::Emitter;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

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

#[derive(Deserialize)]
struct UpdateSelection {
    kind: String,
    id: String,
    name: String,
}

#[derive(Serialize)]
struct UpdateResult {
    kind: String,
    name: String,
    success: bool,
    output: String,
}

// --- Helper: run a shell command and capture stdout ---
fn run_cmd(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {}: {}", cmd, e))?;
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
fn run_update_cmd(cmd: &str, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(cmd);
    command.args(args);
    if let Some(askpass) = askpass_program() {
        command.env("SUDO_ASKPASS", askpass);
    }
    match command.output() {
        Ok(out) => {
            let combined = maybe_append_mas_hint(combine_output(&out.stdout, &out.stderr));
            (out.status.success(), combined.trim().to_string())
        }
        Err(e) => (false, format!("Failed to run {}: {}", cmd, e)),
    }
}

// --- Pure parsers (no I/O) so they can be unit-tested against real CLI output ---

fn parse_brew_outdated(stdout: &str) -> Vec<OutdatedItem> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or(serde_json::json!({}));
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
                current: f["installed_versions"][0].as_str().unwrap_or("").to_string(),
                latest: f["current_version"].as_str().unwrap_or("").to_string(),
                self_updating: false,
                notes_url: String::new(),
            }
        })
        .collect()
}

fn parse_cask_names(stdout: &str) -> std::collections::HashSet<String> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or(serde_json::json!({}));
    parsed["casks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c["name"].as_str().map(|s| s.to_string()))
        .collect()
}

// Parse the greedy cask list, tagging self_updating = present in greedy but not in the normal set.
fn parse_casks(stdout: &str, normal: &std::collections::HashSet<String>) -> Vec<OutdatedItem> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or(serde_json::json!({}));
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
                current: c["installed_versions"][0].as_str().unwrap_or("").to_string(),
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
            let (name, current, latest) = match rest.rfind('(') {
                Some(idx) => {
                    let name = rest[..idx].trim().to_string();
                    let inner = rest[idx..].trim_matches(|c| c == '(' || c == ')');
                    let mut vers = inner.split("->").map(|s| s.trim());
                    let current = vers.next().unwrap_or("").to_string();
                    let latest = vers.next().unwrap_or("").to_string();
                    (name, current, latest)
                }
                None => (rest.to_string(), String::new(), String::new()),
            };
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
    let parsed: serde_json::Value = serde_json::from_str(stdout).unwrap_or(serde_json::json!({}));
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
        s.split(|c: char| c == '.' || c == '-' || c == '+')
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
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
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("==>") && !l.contains(char::is_whitespace))
        .map(|l| l.to_string())
        .collect()
}

// --- Input validation (defense-in-depth) ---
// Commands receive these strings from the webview; validate at the boundary so a crafted value
// (e.g. a leading "-") can't be smuggled in as a flag to brew/open, even though we never use a shell.

fn is_valid_cask_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 128
        && token.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
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
    let end = rest
        .find(|c: char| matches!(c, '/' | '?' | '#'))
        .unwrap_or(rest.len());
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
    match url_host(url) {
        Some(host) => !is_local_or_private_host(&host),
        None => false,
    }
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
    body.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}

// Homebrew's bulk cask metadata is a JWS envelope: { "payload": "<json string>", "signatures": ... }.
// The cask array lives inside the payload string.
fn extract_jws_payload(jws: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(jws).ok()?;
    v["payload"].as_str().map(|s| s.to_string())
}

// Build a lowercased ".app filename" -> cask token index from the cask array JSON, so an installed
// app can be matched to the cask that provides it. On name collisions, prefer the bare token over
// an @-variant (e.g. "google-chrome" over "google-chrome@beta").
fn build_cask_app_index(payload_json: &str) -> HashMap<String, String> {
    let casks: Vec<serde_json::Value> = serde_json::from_str(payload_json).unwrap_or_default();
    let mut idx: HashMap<String, String> = HashMap::new();
    for cask in &casks {
        let token = match cask["token"].as_str() {
            Some(t) => t,
            None => continue,
        };
        let artifacts = match cask["artifacts"].as_array() {
            Some(a) => a,
            None => continue,
        };
        for art in artifacts {
            if let Some(apps) = art["app"].as_array() {
                for app in apps.iter().filter_map(|a| a.as_str()) {
                    let key = app.to_lowercase();
                    let replace = match idx.get(&key) {
                        None => true,
                        Some(existing) => existing.contains('@') && !token.contains('@'),
                    };
                    if replace {
                        idx.insert(key, token.to_string());
                    }
                }
            }
        }
    }
    idx
}

// --- Check functions (thin I/O wrappers around the pure parsers above) ---

fn check_brew() -> CategoryStatus {
    match run_cmd("brew", &["outdated", "--json=v2"]) {
        Ok(stdout) => CategoryStatus {
            category: "Homebrew".into(),
            kind: "brew".into(),
            items: parse_brew_outdated(&stdout),
            error: None,
        },
        Err(e) => failed_category_err("Homebrew", "brew", e),
    }
}

fn normal_cask_names() -> std::collections::HashSet<String> {
    match run_cmd("brew", &["outdated", "--cask", "--json=v2"]) {
        Ok(stdout) => parse_cask_names(&stdout),
        Err(_) => std::collections::HashSet::new(),
    }
}

// Always runs --greedy to get the full set (including self-updating casks); parse_casks tags each.
// The frontend hides self-updating casks unless the user opts in, so toggling is instant.
fn check_casks() -> CategoryStatus {
    let normal = normal_cask_names();
    match run_cmd("brew", &["outdated", "--cask", "--greedy", "--json=v2"]) {
        Ok(stdout) => CategoryStatus {
            category: "Applications".into(),
            kind: "cask".into(),
            items: parse_casks(&stdout, &normal),
            error: None,
        },
        Err(e) => failed_category_err("Applications", "cask", e),
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
    if !is_safe_url(url) {
        return None;
    }
    let out = Command::new("curl")
        // Restrict to http(s) and don't let a redirect escape to file:// or other schemes.
        .args([
            "-sL",
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--max-time",
            "8",
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
            let current = read_plist_string(&info, "CFBundleShortVersionString").unwrap_or_default();
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
    let brew = tauri::async_runtime::spawn_blocking(check_brew);
    let casks = tauri::async_runtime::spawn_blocking(check_casks);
    let mas = tauri::async_runtime::spawn_blocking(check_mas);
    let npm = tauri::async_runtime::spawn_blocking(check_npm);
    vec![
        brew.await.unwrap_or_else(|_| failed_category("Homebrew", "brew")),
        casks.await.unwrap_or_else(|_| failed_category("Applications", "cask")),
        mas.await.unwrap_or_else(|_| failed_category("App Store", "mas")),
        npm.await.unwrap_or_else(|_| failed_category("npm globals", "npm")),
    ]
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
        match run_cmd("brew", &["search", "--cask", query.trim()]) {
            Ok(out) => parse_cask_tokens(&out)
                .into_iter()
                .map(|token| CaskSuggestion {
                    installed: installed.contains(&token),
                    token,
                })
                .collect(),
            Err(_) => vec![],
        }
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

#[tauri::command]
fn set_badge(app: tauri::AppHandle, count: u32) {
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_title(badge_title(count));
    }
}

// `brew --cache` shells out and is stable for the life of the process, so memoize it.
// Returning &'static Path so callers don't have to clone for the common read-only use.
fn brew_cache_dir() -> Option<&'static Path> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let out = Command::new("brew").arg("--cache").output().ok()?;
            if !out.status.success() {
                return None;
            }
            let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if base.is_empty() {
                None
            } else {
                Some(PathBuf::from(base))
            }
        })
        .as_deref()
}

fn cask_jws_path() -> Option<PathBuf> {
    let mut p = brew_cache_dir()?.to_path_buf();
    p.push("api");
    p.push("cask.jws.json");
    Some(p)
}

// Scan /Applications and suggest apps that aren't brew-managed but have a matching cask, so the
// user can adopt them in one click. Uses Homebrew's local cask metadata (no network).
#[tauri::command]
async fn suggest_adoptions() -> Vec<AdoptableApp> {
    tauri::async_runtime::spawn_blocking(|| {
        let index = cask_jws_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|jws| extract_jws_payload(&jws))
            .map(|payload| build_cask_app_index(&payload))
            .unwrap_or_default();
        if index.is_empty() {
            return vec![];
        }
        let managed: std::collections::HashSet<String> = run_cmd("brew", &["list", "--cask"])
            .map(|s| parse_cask_tokens(&s).into_iter().collect())
            .unwrap_or_default();

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for path in list_applications() {
            let filename = match path.file_name().and_then(|s| s.to_str()) {
                Some(f) => f.to_lowercase(),
                None => continue,
            };
            if let Some(token) = index.get(&filename) {
                if managed.contains(token) || !seen.insert(token.clone()) {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                out.push(AdoptableApp {
                    name,
                    token: token.clone(),
                });
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    })
    .await
    .unwrap_or_default()
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
        };
    }
    let args = adopt_args(token);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (success, output) = run_update_cmd("brew", &arg_refs);
    UpdateResult {
        kind: "cask".into(),
        name: token.to_string(),
        success,
        output,
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
        })
}

// Adopt several apps in one batch, returning a result per token.
#[tauri::command]
async fn adopt_casks(tokens: Vec<String>) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || tokens.iter().map(|t| adopt_one(t)).collect())
        .await
        .unwrap_or_default()
}

// --- Run the full upgrade sequence, returning a result per step ---
#[tauri::command]
async fn run_updates() -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(run_updates_blocking)
        .await
        .unwrap_or_default()
}

fn run_updates_blocking() -> Vec<UpdateResult> {
    let steps: Vec<(&str, Vec<&str>)> = vec![
        ("brew", vec!["update"]),
        ("brew", vec!["upgrade"]),
        ("brew", vec!["upgrade", "--cask"]),
        ("brew", vec!["cleanup"]),
        ("mas", vec!["upgrade"]),
        ("npm", vec!["update", "-g"]),
    ];
    steps
        .into_iter()
        .map(|(cmd, args)| {
            let (success, output) = run_update_cmd(cmd, &args);
            UpdateResult {
                kind: cmd.to_string(),
                name: format!("{} {}", cmd, args.join(" ")),
                success,
                output,
            }
        })
        .collect()
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
        "npm" if is_valid_npm_package(id) => {
            Some(("npm", vec!["install".into(), "-g".into(), format!("{}@latest", id)]))
        }
        _ => None,
    }
}

// --- Upgrade only the items the user selected, returning a result per item ---
#[tauri::command]
async fn run_selected_updates(selections: Vec<UpdateSelection>) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || run_selected_updates_blocking(selections))
        .await
        .unwrap_or_default()
}

fn run_selected_updates_blocking(selections: Vec<UpdateSelection>) -> Vec<UpdateResult> {
    let mut results = Vec::new();

    // Refresh Homebrew's metadata once up front if any brew/cask item is selected.
    if selections.iter().any(|s| s.kind == "brew" || s.kind == "cask") {
        let (success, output) = run_update_cmd("brew", &["update"]);
        results.push(UpdateResult {
            kind: "brew".into(),
            name: "brew update".into(),
            success,
            output,
        });
    }

    for sel in &selections {
        let (cmd, args) = match build_update_command(&sel.kind, &sel.id) {
            Some(c) => c,
            None => {
                results.push(UpdateResult {
                    kind: sel.kind.clone(),
                    name: sel.name.clone(),
                    success: false,
                    output: format!("unsupported or invalid update: {} / {}", sel.kind, sel.id),
                });
                continue;
            }
        };
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let (success, output) = run_update_cmd(cmd, &arg_refs);
        results.push(UpdateResult {
            kind: sel.kind.clone(),
            name: sel.name.clone(),
            success,
            output,
        });
    }
    results
}

// --- Signature / provenance verification ---
// Inspects installed apps' code signatures and flags any whose signing certificate (Team ID)
// changed since the last check — the signal of a hijacked release that Gatekeeper alone misses.

struct AppSig {
    name: String,
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
            if p.extension().and_then(|s| s.to_str()) == Some("app") {
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

// A signer change (different Team ID than last seen) is the hijacked-release signal.
fn cert_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    matches!((previous, current), (Some(p), Some(c)) if p != c)
}

fn inspect_app(path: &Path) -> AppSig {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let path_str = path.to_str().unwrap_or("");

    let mut team_id = None;
    let mut authority = None;
    let mut signed = false;
    if let Ok(out) = Command::new("codesign").args(["-dvv", path_str]).output() {
        signed = out.status.success();
        // codesign writes its details to stderr.
        let text = combine_output(&out.stdout, &out.stderr);
        let (t, a) = parse_signing(&text);
        team_id = t;
        authority = a;
    }

    let mut notarized = false;
    if let Ok(out) = Command::new("spctl")
        .args(["--assess", "--type", "execute", "-v", path_str])
        .output()
    {
        let text = combine_output(&out.stdout, &out.stderr);
        notarized = parse_notarized(out.status.success(), &text);
    }

    AppSig {
        name,
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
        let _ = std::fs::write(p, s);
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

#[tauri::command]
async fn verify_signatures(app: tauri::AppHandle) -> Vec<UpdateResult> {
    let apps = tauri::async_runtime::spawn_blocking(list_applications)
        .await
        .unwrap_or_default();

    let total = apps.len();
    let done = Arc::new(AtomicUsize::new(0));
    let _ = app.emit("verify-progress", VerifyProgress { done: 0, total });

    // Each app is inspected on its own blocking thread and emits a progress tick when it finishes,
    // so the frontend's progress ring reflects real completion.
    let mut handles = Vec::new();
    for path in apps {
        let app = app.clone();
        let done = done.clone();
        handles.push(tauri::async_runtime::spawn_blocking(move || {
            let sig = inspect_app(&path);
            let d = done.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app.emit("verify-progress", VerifyProgress { done: d, total });
            sig
        }));
    }
    let mut sigs = Vec::new();
    for h in handles {
        if let Ok(sig) = h.await {
            sigs.push(sig);
        }
    }

    // Held across the load → mutate → save so concurrent verify_signatures runs can't lose
    // updates. Lock poisoning is harmless here (store is just a serializable HashMap), so
    // recover instead of panicking on a previous panic.
    let guard = signing_store_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut store = load_signing_store();
    let mut results = Vec::new();
    for sig in &sigs {
        let prev = store.get(&sig.name).cloned();
        let changed = cert_changed(prev.as_deref(), sig.team_id.as_deref());
        if let Some(c) = &sig.team_id {
            store.insert(sig.name.clone(), c.clone());
        }
        // Flagged = a changed signer or an unsigned app. Everything else is a "verified publisher".
        let ok = sig.signed && !changed;

        let mut out = String::new();
        if changed {
            out.push_str(&format!(
                "⚠ Developer certificate CHANGED: was Team {}, now {}. A different publisher signed this version — possible hijacked release. Verify before trusting.\n",
                prev.clone().unwrap_or_default(),
                sig.team_id.clone().unwrap_or_default()
            ));
        }
        if !sig.signed {
            out.push_str("Not code-signed.\n");
        }
        if let Some(a) = &sig.authority {
            out.push_str(&format!("Signer: {}\n", a));
        }
        if let Some(t) = &sig.team_id {
            out.push_str(&format!("Team ID: {}\n", t));
        }
        out.push_str(if sig.notarized {
            "Notarized by Apple: yes"
        } else {
            "Notarized by Apple: no"
        });

        results.push(UpdateResult {
            kind: "verify".into(),
            name: sig.name.clone(),
            success: ok,
            output: out.trim().to_string(),
        });
    }
    save_signing_store(&store);
    drop(guard);

    // Flagged apps first (they auto-expand and need attention); verified publishers after.
    results.sort_by_key(|r| r.success);
    results
}

// --- Launchd schedule management ---
fn valid_schedule_time(hour: u32, minute: u32) -> bool {
    hour <= 23 && minute <= 59
}

fn plist_path() -> std::path::PathBuf {
    let mut p = dirs::home_dir().expect("home dir");
    p.push("Library/LaunchAgents/com.freshet.daily.plist");
    p
}

fn script_path() -> std::path::PathBuf {
    let mut p = dirs::home_dir().expect("home dir");
    p.push(".freshet/run.sh");
    p
}

fn write_run_script() -> Result<(), String> {
    let path = freshet_dir()
        .map(|d| d.join("run.sh"))
        .ok_or_else(|| "could not resolve ~/.freshet".to_string())?;
    let script = r#"#!/usr/bin/env bash
# Auto-generated by Freshet
set -uo pipefail
LOG="$HOME/.freshet/run.log"
echo "===== $(date) =====" >> "$LOG"
run() { echo ">>> $*" >> "$LOG"; "$@" >> "$LOG" 2>&1 || echo "!!! $* failed" >> "$LOG"; }

if command -v brew >/dev/null 2>&1; then
  run brew update
  run brew upgrade
  run brew upgrade --cask
  run brew cleanup
fi
if command -v mas >/dev/null 2>&1; then run mas upgrade; fi
export NVM_DIR="$HOME/.nvm"
if [ -s "$NVM_DIR/nvm.sh" ]; then
  . "$NVM_DIR/nvm.sh"
  run nvm install --lts --reinstall-packages-from=current --latest-npm
fi
if command -v npm >/dev/null 2>&1; then run npm update -g; fi
if command -v pnpm >/dev/null 2>&1; then run pnpm self-update; run pnpm update -g; fi
echo "===== done =====" >> "$LOG"
"#;
    write_private(&path, script, true).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn install_schedule(hour: u32, minute: u32) -> Result<(), String> {
    if !valid_schedule_time(hour, minute) {
        return Err(format!("invalid schedule time {hour}:{minute}"));
    }
    write_run_script()?;
    let script = script_path();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.freshet.daily</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>{}</string>
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>{}</integer>
        <key>Minute</key><integer>{}</integer>
    </dict>
    <key>StandardOutPath</key><string>{}/.freshet/launchd.out.log</string>
    <key>StandardErrorPath</key><string>{}/.freshet/launchd.err.log</string>
</dict>
</plist>
"#,
        script.display(),
        hour,
        minute,
        dirs::home_dir().unwrap().display(),
        dirs::home_dir().unwrap().display(),
    );
    let p = plist_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, plist).map_err(|e| e.to_string())?;
    // Reload: unload first (ignore errors) then load
    let _ = Command::new("launchctl")
        .args(["unload", p.to_str().unwrap()])
        .status();
    Command::new("launchctl")
        .args(["load", p.to_str().unwrap()])
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn uninstall_schedule() -> Result<(), String> {
    let p = plist_path();
    if p.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", p.to_str().unwrap()])
            .status();
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn is_schedule_installed() -> bool {
    plist_path().exists()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Build the menu bar tray
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;

            let _tray = TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .menu(&menu)
                .menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(win) = app.get_webview_window("main") {
                            if win.is_visible().unwrap_or(false) {
                                let _ = win.hide();
                            } else {
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
            run_updates,
            run_selected_updates,
            verify_signatures,
            install_schedule,
            uninstall_schedule,
            is_schedule_installed
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
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
        assert_eq!(names, vec!["typescript"], "only the genuinely-behind package is kept");
        assert_eq!(items[0].current, "5.0.0");
        assert_eq!(items[0].latest, "5.8.0");
    }

    #[test]
    fn npm_handles_empty() {
        assert!(parse_npm_outdated("{}").is_empty());
        assert!(parse_npm_outdated("not json").is_empty());
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
        assert!(chrome.self_updating, "greedy-only => self-updating (hidden by default)");
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
        assert!(!parse_notarized(false, accepted), "rejected status => not notarized");
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
        assert!(!cert_changed(None, Some("AAA")), "first sighting is not a change");
        assert!(!cert_changed(Some("AAA"), None));
    }

    #[test]
    fn update_command_mapping() {
        assert_eq!(
            build_update_command("brew", "wget"),
            Some(("brew", vec!["upgrade".into(), "wget".into()]))
        );
        assert_eq!(
            build_update_command("cask", "firefox"),
            Some(("brew", vec!["upgrade".into(), "--cask".into(), "firefox".into()]))
        );
        assert_eq!(
            build_update_command("mas", "462058435"),
            Some(("mas", vec!["upgrade".into(), "462058435".into()]))
        );
        assert_eq!(
            build_update_command("npm", "typescript"),
            Some(("npm", vec!["install".into(), "-g".into(), "typescript@latest".into()]))
        );
        assert_eq!(
            build_update_command("npm", "@scope/pkg"),
            Some(("npm", vec!["install".into(), "-g".into(), "@scope/pkg@latest".into()]))
        );
        assert_eq!(build_update_command("bogus", "x"), None);
        // crafted ids must be rejected (no flag / install-spec injection)
        assert_eq!(build_update_command("brew", "--force"), None);
        assert_eq!(build_update_command("mas", "not-a-number"), None);
        assert_eq!(build_update_command("npm", "https://evil.example/x.tgz"), None);
        assert_eq!(build_update_command("npm", "-g"), None);
    }

    #[test]
    fn schedule_time_range() {
        assert!(valid_schedule_time(10, 30));
        assert!(valid_schedule_time(0, 0));
        assert!(valid_schedule_time(23, 59));
        assert!(!valid_schedule_time(24, 0));
        assert!(!valid_schedule_time(10, 60));
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
        assert_eq!(parse_appcast_notes(xml).as_deref(), Some("https://obsproject.com/notes.html"));
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
        assert_eq!(parse_cask_tokens(listed), vec!["sublime-text", "intellij-idea-ce"]);
        assert!(parse_cask_tokens("").is_empty());
    }

    #[test]
    fn cask_token_validation_rejects_flags_and_junk() {
        assert!(is_valid_cask_token("google-chrome"));
        assert!(is_valid_cask_token("google-chrome@beta"));
        assert!(is_valid_cask_token("1password"));
        assert!(!is_valid_cask_token("--force"), "leading dash = flag injection");
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
    fn jws_payload_extraction() {
        assert_eq!(
            extract_jws_payload(r#"{"payload":"[1,2]","signatures":[]}"#).as_deref(),
            Some("[1,2]")
        );
        assert_eq!(extract_jws_payload("not json"), None);
    }

    #[test]
    fn cask_app_index_maps_and_prefers_bare_token() {
        // @beta listed FIRST, but the bare token must win the "Foo.app" collision
        let payload = r#"[
            {"token":"foo@beta","artifacts":[{"app":["Foo.app"]}]},
            {"token":"foo","artifacts":[{"app":["Foo.app"]}]},
            {"token":"google-chrome","artifacts":[{"app":["Google Chrome.app"]},{"zap":[{}]}]},
            {"token":"slack","artifacts":[{"app":["Slack.app"]}]}
        ]"#;
        let idx = build_cask_app_index(payload);
        assert_eq!(idx.get("foo.app").map(String::as_str), Some("foo"));
        assert_eq!(idx.get("google chrome.app").map(String::as_str), Some("google-chrome"));
        assert_eq!(idx.get("slack.app").map(String::as_str), Some("slack"));
        assert_eq!(idx.get("missing.app"), None);
    }

    #[test]
    fn mas_hint_only_on_that_error() {
        let with_err = maybe_append_mas_hint("Error: No downloads initiated for ADAM ID 462058435.".into());
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
        assert!(parse_brew_outdated("not json").is_empty());
        assert!(parse_brew_outdated(r#"{"casks":[]}"#).is_empty()); // no formulae key
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
        let greedy = r#"{"casks":[{"name":"firefox","installed_versions":["1"],"current_version":"2"}]}"#;
        let none = std::collections::HashSet::new();
        assert!(parse_casks(greedy, &none)[0].self_updating);
        assert!(parse_casks("garbage", &none).is_empty());
        assert_eq!(parse_cask_names(r#"{"casks":[{"name":"a"},{"name":"b"}]}"#).len(), 2);
        assert!(parse_cask_names("nope").is_empty());
    }

    #[test]
    fn mas_handles_weird_lines() {
        let items = parse_mas_outdated("123  Some App"); // no parens
        assert_eq!(items[0].id, "123");
        assert_eq!(items[0].name, "Some App");
        assert_eq!(items[0].latest, "");
        assert!(parse_mas_outdated("462058435").is_empty()); // no whitespace -> can't split
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
        assert!(parse_npm_outdated("[1,2,3]").is_empty()); // non-object json
        assert!(parse_npm_outdated("").is_empty());
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
    fn cask_index_handles_missing_and_nonstring() {
        let payload = r#"[
            {"token":"noart"},
            {"token":"objapp","artifacts":[{"app":[{"target":"x"}]}]},
            {"token":"good","artifacts":[{"app":["Good.app"]}]}
        ]"#;
        let idx = build_cask_app_index(payload);
        assert_eq!(idx.get("good.app").map(String::as_str), Some("good"));
        assert_eq!(idx.len(), 1); // missing-artifacts + non-string-app contribute nothing
        assert!(build_cask_app_index("not json").is_empty());
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
        assert_eq!(url_host("https://example.com/path").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://Example.COM").as_deref(), Some("example.com"));
        assert_eq!(url_host("http://example.com:8080/x").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://user:pass@example.com/x").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://[::1]:443/x").as_deref(), Some("::1"));
        assert_eq!(url_host("https://[fe80::1]/x").as_deref(), Some("fe80::1"));
        // Path / query / fragment terminate the host.
        assert_eq!(url_host("https://example.com?q=1").as_deref(), Some("example.com"));
        assert_eq!(url_host("https://example.com#frag").as_deref(), Some("example.com"));
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
        assert!(!is_local_or_private_host("172.32.0.1"), "just outside private range");
        assert!(!is_local_or_private_host("100.63.255.255"), "just outside CGNAT");
        assert!(!is_local_or_private_host("100.128.0.1"), "just outside CGNAT");
        assert!(!is_local_or_private_host("2606:4700:4700::1111"), "Cloudflare v6");
    }

    #[test]
    fn feed_url_requires_https_and_public_host() {
        assert!(is_safe_feed_url("https://obsproject.com/osx_update/updates.xml"));
        // http is rejected outright — Sparkle 2 requires https; older feeds can MITM us.
        assert!(!is_safe_feed_url("http://obsproject.com/osx_update/updates.xml"));
        assert!(!is_safe_feed_url("file:///etc/passwd"));
        assert!(!is_safe_feed_url("javascript:alert(1)"));
        // SSRF-style hostile feeds.
        assert!(!is_safe_feed_url("https://localhost/x.xml"));
        assert!(!is_safe_feed_url("https://127.0.0.1/x.xml"));
        assert!(!is_safe_feed_url("https://192.168.1.1/x.xml"));
        assert!(!is_safe_feed_url("https://169.254.169.254/latest/meta-data/"));
        assert!(!is_safe_feed_url("https://[::1]/x.xml"));
        assert!(!is_safe_feed_url("https://router.local/x.xml"));
        // Junk.
        assert!(!is_safe_feed_url(""));
        assert!(!is_safe_feed_url("https://e.com/ x"));
        assert!(!is_safe_feed_url(&format!("https://e.com/{}", "a".repeat(2050))));
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
}
