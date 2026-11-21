// Who signed an app: reading and proving code signatures, the trusted-publisher baseline, and
// the checks that flag a change of publisher.

use crate::apps::{
    app_store_bundles, cask_apps, info_plist_mtime, installed_cask_apps, list_applications,
};
use crate::model::{Kind, ResultAction, UpdateResult};
use crate::preverify;
use crate::process::{combine_output, command, elapsed_ms};
use crate::store::{freshet_dir, write_private_atomic};
use crate::validate::{is_valid_bundle_id, is_valid_team_id};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use tauri::Emitter;

pub(crate) const FULL_SCAN_INTERVAL_SECS: u64 = 7 * 86_400;

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
pub(crate) fn read_signer(path: &Path) -> preverify::Signer {
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

// The code requirement a bundle must satisfy for its Team ID to count as proven, not just
// claimed: a signature that chains to Apple, and either a leaf certificate issued to that team
// (Developer ID, Apple Development) or Apple's own App Store signing certificate — an App Store
// signature covers the Team ID it states, so there the claim is Apple's, not the developer's.
// None for anything that isn't a Team ID (which also keeps the string from being injected into).
// Pure → unit-tested.
fn team_requirement(team: &str) -> Option<String> {
    is_valid_team_id(team).then(|| {
        format!(
            "=anchor apple generic and (certificate leaf[subject.OU] = \"{team}\" \
             or certificate leaf[field.1.2.840.113635.100.6.1.9])"
        )
    })
}

// Does the bundle's signature actually verify, as `team`? `codesign -dvv` (read_signer) only
// prints what a signature says about itself, and a hand-built one can say anything; this checks
// the certificate chain and the sealed contents. Costs from a fraction of a second to a few
// seconds, by app size.
pub(crate) fn signature_proves_team(path: &Path, team: &str) -> bool {
    team_requirement(team).is_some_and(|requirement| {
        Command::new("/usr/bin/codesign")
            .arg("--verify")
            .arg(format!("-R{requirement}"))
            .arg(path)
            .output()
            .is_ok_and(|o| o.status.success())
    })
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
        // Written to a temp file and renamed into place: the app and the scheduled run both read
        // and write this, and a reader that caught a half-written file would see no baseline at
        // all — then save that back, silently trusting whatever is installed now.
        let _ = write_private_atomic(&p, &s);
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
pub(crate) struct VerifyProgress {
    pub(crate) done: usize,
    pub(crate) total: usize,
}

// Filter the verify_signatures output. When `only_changed` is true, return only entries that
// were flagged because the *signer changed* — the specific signal a hijacked update produces.
// Unsigned-but-stable apps stay out of the post-update view (they belong in the full report).
pub(crate) fn select_verify_results(
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
// --- Who should have signed an app ---
//
// Signer drift is judged against a baseline, normally the first Team ID Freshet saw for the app
// ("trust on first use"). That misses an app that was already compromised before Freshet was
// installed. For widely used, often-targeted apps, Freshet instead knows the publisher up front:
// these pins apply from the very first sighting. Each was confirmed two independent ways — the
// app's real signature and the publisher's known identity — and only apps verified that way are
// listed. An explicit "Trust new publisher" from the user still overrides a pin (a real change of
// ownership), recorded under an `explicit:` key.
pub(crate) const PINNED_PUBLISHERS: &[(&str, &str, &str)] = &[
    (
        "com.tinyspeck.slackmacgap",
        "BQR82RBBHL",
        "Slack Technologies",
    ),
    ("com.hnc.Discord", "53Q6R32WPB", "Discord"),
    ("com.google.Chrome", "EQHXZ8M8AV", "Google"),
    ("com.microsoft.VSCode", "UBF8T346G9", "Microsoft"),
    ("com.microsoft.Excel", "UBF8T346G9", "Microsoft"),
    ("com.microsoft.Word", "UBF8T346G9", "Microsoft"),
    ("com.microsoft.Powerpoint", "UBF8T346G9", "Microsoft"),
    ("us.zoom.xos", "BJ4HAAB9B3", "Zoom"),
    ("com.spotify.client", "2FNC3A47ZF", "Spotify"),
    ("com.figma.Desktop", "T8RA8NE3B7", "Figma"),
    ("com.docker.docker", "9BNSXJN65R", "Docker"),
    ("com.postmanlabs.mac", "H7H8Q7M5CK", "Postman"),
    ("WhatsApp", "57T9237FN3", "WhatsApp"),
    ("com.anthropic.claudefordesktop", "Q6L2SF6YDW", "Anthropic"),
    ("com.github.GitHubClient", "VEKTX9H2N7", "GitHub"),
    ("com.obsproject.obs-studio", "2MMRE5MTB8", "OBS Project"),
    ("ch.protonmail.desktop", "6UN54H93QT", "Proton"),
];

const EXPLICIT_PREFIX: &str = "explicit:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Basis {
    // The user pressed "Trust new publisher" for this team.
    UserTrusted,
    // Freshet's pinned publisher for this app.
    Pinned(&'static str),
    // The first Team ID Freshet saw.
    FirstSeen,
}

// The Team ID an app is expected to be signed by, and why. Explicit user trust beats a pin; a
// pin beats whatever was first seen. Pure → unit-tested.
fn expected_signer(
    key: &str,
    legacy_name: &str,
    store: &HashMap<String, String>,
) -> Option<(String, Basis)> {
    if let Some(team) = store.get(&format!("{EXPLICIT_PREFIX}{key}")) {
        return Some((team.clone(), Basis::UserTrusted));
    }
    if let Some((_, team, publisher)) = PINNED_PUBLISHERS.iter().find(|(id, ..)| *id == key) {
        return Some(((*team).to_string(), Basis::Pinned(publisher)));
    }
    store
        .get(key)
        .or_else(|| store.get(legacy_name))
        .map(|team| (team.clone(), Basis::FirstSeen))
}

fn build_verify_entries(
    sigs: &[AppSig],
    store: &mut HashMap<String, String>,
) -> Vec<(UpdateResult, bool)> {
    let mut entries: Vec<(UpdateResult, bool)> = Vec::new();
    for sig in sigs {
        // Keyed by bundle id (survives renames, no collisions between same-named apps). Older
        // stores were keyed by app name; those entries are read as a fallback and migrated.
        let key = sig.bundle_id.clone().unwrap_or_else(|| sig.name.clone());
        let expected = expected_signer(&key, &sig.name, store);
        let prev = expected.as_ref().map(|(team, _)| team.clone());
        let pinned_publisher = match &expected {
            Some((_, Basis::Pinned(publisher))) => Some(*publisher),
            _ => None,
        };
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
        if let (true, Some(publisher)) = (changed, pinned_publisher) {
            let was = prev.unwrap_or_default();
            let now = sig.team_id.clone().unwrap_or_default();
            let _ = writeln!(
                out,
                "⚠ Not signed by {publisher}: this copy is signed by Team {now}, but {} is published by {publisher} (Team {was}). Don't open it until you know why.",
                sig.name
            );
        } else if changed {
            let was = prev.unwrap_or_default();
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
                bundle_id: key,
                team: team.clone(),
            }),
            _ => None,
        };
        entries.push((
            UpdateResult {
                kind: Kind::Verify,
                name: sig.name.clone(),
                success: ok,
                output: out.trim().to_string(),
                action,
                elapsed_ms: None,
                skipped: false,
                steps: Vec::new(),
                blocked: false,
            },
            changed,
        ));
    }
    entries
}

// The trusted Team ID for a bundle id, if one has been recorded.
// The Team ID an app should be signed by: explicit trust, else a pin, else first seen.
pub(crate) fn trusted_team(bundle_id: &str) -> Option<String> {
    trusted_signer(bundle_id).map(|(team, _)| team)
}

// The same, with why: the pre-install check is stricter for a pinned publisher.
pub(crate) fn trusted_signer(bundle_id: &str) -> Option<(String, Basis)> {
    let _guard = signing_store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    expected_signer(bundle_id, bundle_id, &load_signing_store())
}

// The user vouches for a publisher change: record `team` as the baseline for `bundle_id`. Clears
// the repeating drift warning, and lets a pre-install check that blocked on it go through.
#[tauri::command]
pub(crate) fn trust_signer(bundle_id: String, team: String) -> Result<(), String> {
    if !is_valid_bundle_id(&bundle_id) || !is_valid_team_id(&team) {
        return Err("invalid bundle id or Team ID".into());
    }
    let _guard = signing_store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = load_signing_store();
    // The explicit entry is what lets a user's decision override a pin.
    store.insert(format!("{EXPLICIT_PREFIX}{bundle_id}"), team.clone());
    store.insert(bundle_id, team);
    save_signing_store(&store);
    Ok(())
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
        let _ = write_private_atomic(&p, &json);
    }
    tracing::info!(
        apps = total,
        elapsed_ms = elapsed_ms(started),
        "signature scan"
    );
    sigs
}

// Full scan diffed against (and updating) the signer baseline.
pub(crate) fn verify_all_blocking(
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Vec<(UpdateResult, bool)> {
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
pub(crate) async fn verify_signatures(
    app: tauri::AppHandle,
    only_changed: Option<bool>,
) -> Vec<UpdateResult> {
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
// `apps`: App Store apps just updated, by bundle name ("Notability.app"). They're signed by the
// developer's Team ID like any other app, so the same publisher-change check applies.
pub(crate) async fn verify_updated_casks(
    tokens: Vec<String>,
    apps: Option<Vec<String>>,
) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || {
        verify_updated_apps_blocking(&tokens, &apps.unwrap_or_default())
    })
    .await
    .unwrap_or_default()
}

fn verify_updated_apps_blocking(tokens: &[String], app_store_apps: &[String]) -> Vec<UpdateResult> {
    let mut results = verify_updated_casks_blocking(tokens);
    let paths = app_store_bundles(Path::new("/Applications"), app_store_apps);
    if paths.is_empty() {
        return results;
    }
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
    results.extend(select_verify_results(entries, true));
    results.sort_by_key(|r| r.success);
    results
}

pub(crate) fn verify_updated_casks_blocking(tokens: &[String]) -> Vec<UpdateResult> {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::apps::{app_store_bundles, list_applications};
    use crate::model::{Kind, ResultAction, UpdateResult};
    use crate::process::elapsed_ms;
    use crate::validate::{is_valid_bundle_id, is_valid_team_id};
    use std::collections::HashMap;
    use std::path::Path;

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
            kind: Kind::Verify,
            name: "stable".into(),
            success: true,
            output: "Team ID: AAA".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        };
        let unsigned_unchanged = UpdateResult {
            kind: Kind::Verify,
            name: "always-unsigned".into(),
            success: false,
            output: "Not code-signed.".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        };
        let drifted = UpdateResult {
            kind: Kind::Verify,
            name: "drifted".into(),
            success: false,
            output: "⚠ Developer certificate CHANGED: was Team AAA, now BBB.".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
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

    #[test]
    fn signing_takes_first_authority() {
        let out = "Authority=Leaf Signer\nAuthority=Intermediate\nTeamIdentifier=ABC123\n";
        let (team, auth) = parse_signing(out);
        assert_eq!(team.as_deref(), Some("ABC123"));
        assert_eq!(auth.as_deref(), Some("Leaf Signer"));
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
        // Overwriting the baseline (what tampering with signing.json would do) doesn't beat
        // Slack's pin...
        store.insert("com.tinyspeck.slackmacgap".into(), "EVILEVIL99".into());
        let (_, changed) = &build_verify_entries(&[sig("EVILEVIL99")], &mut store)[0];
        assert!(changed, "a rewritten baseline can't vouch for a pinned app");
        // ...only an explicit trust decision does (what trust_signer records).
        store.insert(
            format!("{EXPLICIT_PREFIX}com.tinyspeck.slackmacgap"),
            "EVILEVIL99".into(),
        );
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

    // ---------- #4 pinned publishers ----------

    #[test]
    fn pins_are_well_formed_and_unique() {
        let mut ids = std::collections::HashSet::new();
        for (id, team, publisher) in PINNED_PUBLISHERS {
            assert!(is_valid_team_id(team), "{id}: {team}");
            assert!(is_valid_bundle_id(id), "{id}");
            assert_ne!(*publisher, "");
            assert!(ids.insert(*id), "duplicate pin for {id}");
        }
    }

    #[test]
    fn expected_signer_precedence() {
        let slack = "com.tinyspeck.slackmacgap";
        let mut store: HashMap<String, String> = HashMap::new();
        // Pinned, never seen: the pin applies from the start.
        assert_eq!(
            expected_signer(slack, "Slack", &store),
            Some(("BQR82RBBHL".into(), Basis::Pinned("Slack Technologies")))
        );
        // A first-seen baseline doesn't override the pin...
        store.insert(slack.into(), "EVILEVIL99".into());
        assert_eq!(
            expected_signer(slack, "Slack", &store).unwrap().0,
            "BQR82RBBHL"
        );
        // ...but the user's explicit trust does.
        store.insert(format!("{EXPLICIT_PREFIX}{slack}"), "NEWOWNER11".into());
        assert_eq!(
            expected_signer(slack, "Slack", &store),
            Some(("NEWOWNER11".into(), Basis::UserTrusted))
        );
        // Unpinned apps keep trust-on-first-use, including legacy name keys.
        let other: HashMap<String, String> = HashMap::from([("Anki".into(), "7ZM8SLJM4P".into())]);
        assert_eq!(
            expected_signer("net.ankiweb.dtop", "Anki", &other),
            Some(("7ZM8SLJM4P".into(), Basis::FirstSeen))
        );
        assert_eq!(expected_signer("com.unknown.app", "Unknown", &other), None);
    }

    fn slack_sig(team: &str) -> AppSig {
        AppSig {
            name: "Slack".into(),
            bundle_id: Some("com.tinyspeck.slackmacgap".into()),
            team_id: Some(team.into()),
            authority: None,
            signed: true,
            notarized: true,
        }
    }

    // The trust-on-first-use gap: a copy that was already wrong when Freshet first saw it is
    // flagged anyway, because the pin knows who should have signed it.
    #[test]
    fn pinned_app_with_wrong_signer_is_flagged_on_first_sight() {
        let mut store = HashMap::new();
        let (r, changed) = build_verify_entries(&[slack_sig("EVILEVIL99")], &mut store).remove(0);
        assert!(changed && !r.success);
        assert!(
            r.output.contains("Not signed by Slack Technologies"),
            "{}",
            r.output
        );
        assert!(r.output.contains("EVILEVIL99") && r.output.contains("BQR82RBBHL"));
        assert!(
            !store.values().any(|t| t == "EVILEVIL99"),
            "the wrong signer never becomes a baseline"
        );

        let (r, changed) = build_verify_entries(&[slack_sig("BQR82RBBHL")], &mut store).remove(0);
        assert!(!changed && r.success, "the real publisher passes");
    }

    #[test]
    fn explicit_trust_overrides_a_pin() {
        let mut store = HashMap::from([(
            format!("{EXPLICIT_PREFIX}com.tinyspeck.slackmacgap"),
            "NEWOWNER11".to_string(),
        )]);
        let (_, changed) = build_verify_entries(&[slack_sig("NEWOWNER11")], &mut store).remove(0);
        assert!(!changed);
    }

    // Real /Applications, read-only: every installed pinned app must match its pin (no false
    // alarms), and App Store apps resolve and carry a developer Team ID.
    //   cargo test -- --ignored security_live --nocapture
    #[test]
    #[ignore = "reads the real /Applications"]
    fn security_live_pins_and_app_store() {
        let mut checked = 0;
        for app in list_applications() {
            let signer = read_signer(&app);
            let Some(id) = signer.bundle_id.as_deref() else {
                continue;
            };
            if let Some((_, team, publisher)) = PINNED_PUBLISHERS.iter().find(|(p, ..)| *p == id) {
                eprintln!(
                    "pinned {:32} expects {team} ({publisher}), signed {:?}",
                    id, signer.team
                );
                assert_eq!(signer.team.as_deref(), Some(*team), "{id}");
                checked += 1;
            }
        }
        eprintln!("{checked} installed pinned apps match their pins");
        let mas = app_store_bundles(
            Path::new("/Applications"),
            &["Notability.app".into(), "Microsoft Excel.app".into()],
        );
        for app in &mas {
            let s = read_signer(app);
            eprintln!(
                "App Store {} → {:?} team {:?}",
                app.display(),
                s.bundle_id,
                s.team
            );
            assert!(s.team.is_some());
        }
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

    // The requirement that turns "says it's signed by X" into "is signed by X".
    #[test]
    fn team_requirement_names_the_team_and_rejects_anything_else() {
        let r = team_requirement("BQR82RBBHL").unwrap();
        assert!(r.starts_with("=anchor apple generic and "), "{r}");
        assert!(
            r.contains(r#"certificate leaf[subject.OU] = "BQR82RBBHL""#),
            "{r}"
        );
        assert_eq!(
            team_requirement(r#"X" or anchor trusted or "Y"#),
            None,
            "injection"
        );
        assert_eq!(team_requirement("not set"), None);
        assert_eq!(team_requirement(""), None);
    }

    // Real codesign against real installed apps: the right team verifies, a wrong one doesn't —
    // for a Developer ID app and (if present) an App Store one.
    //   cargo test -- --ignored signature_proof_live --nocapture
    #[test]
    #[ignore = "verifies real apps in /Applications"]
    fn signature_proof_live() {
        for app in [
            "Slack.app",
            "Discord.app",
            "Microsoft Excel.app",
            "Xcode.app",
        ] {
            let path = Path::new("/Applications").join(app);
            let Some(team) = read_signer(&path).team else {
                eprintln!("{app}: not installed or no team");
                continue;
            };
            let started = std::time::Instant::now();
            let proven = signature_proves_team(&path, &team);
            eprintln!(
                "{app}: team {team} proven={proven} in {}ms",
                elapsed_ms(started)
            );
            assert!(proven, "{app} should verify as its own team");
        }
        assert!(!signature_proves_team(
            Path::new("/Applications/Slack.app"),
            "AAAAAAAAAA"
        ));
    }
}
