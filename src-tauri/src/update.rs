// Installing updates: one result per selected item, with live progress.

use crate::apps::{bundle_is_running, cask_apps, installed_cask_apps, running_executables};
use crate::check::{brew_update_if_stale, outdated_names, versions_last_shown, Versions};
use crate::history::record_manual_history;
use crate::model::{Kind, ResultAction, Step, UpdateResult, UpdateSelection};
use crate::npm;
use crate::preverify;
use crate::process::{
    applescript_string, begin_operation, elapsed_ms, interrupt_running_child, run_cmd,
    run_update_cmd, run_update_cmd_streaming, with_root_if_needed, BUSY_MESSAGE, DOWNLOADING,
    SCHEDULED_BUSY_MESSAGE, STOP_REQUESTED,
};
use crate::rollback;
use crate::scheduled::scheduled_run_active;
use crate::settings::load_settings;
use crate::validate::{
    is_numeric_id, is_valid_app_name, is_valid_cask_token, is_valid_npm_package,
};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use tauri::Emitter;

// A line where brew moves on to the next package — the safe place to interrupt. Pure → tested.
fn is_package_boundary(line: &str) -> bool {
    (line.starts_with("==> Upgrading ") && !line.contains("outdated package"))
        || line.starts_with("==> Installing ")
}

// --- Homebrew build provenance (attestations) ---
//
// With HOMEBREW_VERIFY_ATTESTATIONS=1, Homebrew checks each formula bottle's GitHub build
// provenance attestation (via the `gh` CLI) before pouring it, and refuses one that wasn't built
// by Homebrew's CI. It needs GitHub credentials, and with none it refuses to install *any*
// bottle (Homebrew utils/attestation.rb: GhAuthNeeded → CannotInstallFormulaError). So Freshet
// turns it on only when credentials exist; Settings shows which, and how to enable it.

// HOMEBREW_GITHUB_API_TOKEN, or a `gh` that is signed in (what Homebrew itself looks for).
fn github_credentials_available() -> bool {
    if std::env::var("HOMEBREW_GITHUB_API_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
        return true;
    }
    Command::new("gh")
        .args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
}

// Pure → unit-tested.
fn provenance_env(has_credentials: bool) -> Vec<(&'static str, &'static str)> {
    if has_credentials {
        vec![("HOMEBREW_VERIFY_ATTESTATIONS", "1")]
    } else {
        Vec::new()
    }
}

// Homebrew's wording when a bottle fails verification ("…has an invalid build provenance
// attestation"). That's a block, not an ordinary failure. Pure → unit-tested.
pub(crate) fn provenance_failed(output: &str) -> bool {
    output.contains("invalid build provenance attestation")
}

// `(async)`: spawns `gh`, so it runs on the thread pool rather than the main (UI) thread, where
// plain commands run.
#[tauri::command(async)]
pub(crate) fn homebrew_provenance_active() -> bool {
    github_credentials_available()
}

// The requested package a `==> Upgrading <name>` line starts on, if any. Pure → unit-tested.
fn upgrading_name<'a>(line: &str, wanted: &std::collections::HashSet<&'a str>) -> Option<&'a str> {
    let name = line
        .strip_prefix("==> Upgrading ")?
        .split_whitespace()
        .next()?;
    wanted.get(name).copied()
}

// Map a selected item (kind + id) to the command that updates just it. The id is validated per
// kind so a crafted selection can't inject a flag or an arbitrary install spec. Pure → unit-tested.
pub(crate) fn build_update_command(kind: Kind, id: &str) -> Option<(&'static str, Vec<String>)> {
    match kind {
        Kind::Brew if is_valid_cask_token(id) => Some(("brew", vec!["upgrade".into(), id.into()])),
        Kind::Cask if is_valid_cask_token(id) => {
            Some(("brew", vec!["upgrade".into(), "--cask".into(), id.into()]))
        }
        Kind::Mas if is_numeric_id(id) => Some(("mas", vec!["upgrade".into(), id.into()])),
        Kind::Npm if is_valid_npm_package(id) => Some((
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
        match s.kind {
            Kind::Brew if is_valid_cask_token(&s.id) => formulae.push(s.id.clone()),
            Kind::Cask if is_valid_cask_token(&s.id) => casks.push(s.id.clone()),
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
//
// Runs as the tracked child with DOWNLOADING set, so Stop ends it immediately (a multi-gigabyte
// download shouldn't have to finish first); an interrupted `brew fetch` leaves only a partial
// file in Homebrew's cache, which the next fetch resumes. `brew` is the program to run: "brew",
// or a stand-in in tests.
fn prefetch_casks(brew: &str, selections: &[UpdateSelection]) {
    let (_, casks) = fetch_batches(selections);
    if casks.is_empty() {
        return;
    }
    let mut args: Vec<&str> = vec!["fetch", "--cask"];
    args.extend(casks.iter().map(String::as_str));
    DOWNLOADING.store(true, Ordering::SeqCst);
    // A Stop pressed just before the child existed found nothing to interrupt: catch it on the
    // next line of output. (A second SIGINT is harmless here; nothing is being installed.)
    let mut interrupted = false;
    let _ = run_update_cmd_streaming(brew, &args, &[], &mut |_| {
        if !interrupted && STOP_REQUESTED.load(Ordering::SeqCst) {
            interrupted = true;
            interrupt_running_child();
        }
    });
    DOWNLOADING.store(false, Ordering::SeqCst);
}

// Live progress for an update run, emitted to the frontend as "update-progress".
#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
pub(crate) struct UpdateProgress {
    // What's happening: "Refreshing Homebrew", "Downloading apps", "Checking publisher",
    // "Upgrading", "Installing".
    pub(crate) phase: String,
    // The package being worked on, if any.
    pub(crate) current: Option<String>,
    // Packages finished so far, out of `total` selected.
    pub(crate) done: usize,
    pub(crate) total: usize,
    // The `==> …` step brew is on (see step_label), for the live run view.
    pub(crate) step: Option<String>,
}

pub(crate) type ProgressFn<'a> = &'a (dyn Fn(UpdateProgress) + Sync);

fn progress_event(phase: &str, current: Option<&str>, done: usize, total: usize) -> UpdateProgress {
    UpdateProgress {
        phase: phase.into(),
        current: current.map(String::from),
        done,
        total,
        step: None,
    }
}

// --- Upgrade only the items the user selected, returning a result per item ---
#[tauri::command]
pub(crate) async fn run_selected_updates(
    app: tauri::AppHandle,
    selections: Vec<UpdateSelection>,
) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let refused = |why: &str| -> Vec<UpdateResult> {
            selections
                .iter()
                .map(|sel| failed_result(sel, why.into()))
                .collect()
        };
        let Some(_busy) = begin_operation() else {
            return refused(BUSY_MESSAGE);
        };
        // The launchd run is another process, so BUSY can't see it; its pid marker can.
        if scheduled_run_active() {
            return refused(SCHEDULED_BUSY_MESSAGE);
        }
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
        .any(|s| s.kind == Kind::Brew || s.kind == Kind::Cask)
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
pub(crate) fn install_selections(
    selections: &[UpdateSelection],
    progress: ProgressFn<'_>,
) -> Vec<UpdateResult> {
    let total = selections.len();
    let mut done = 0;
    let mut results: Vec<Option<UpdateResult>> = vec![None; total];
    let stopped = || STOP_REQUESTED.load(Ordering::SeqCst);
    // One `ps` for the whole run: which app bundles have a process running right now.
    let running = running_executables();

    if selections.iter().any(|s| s.kind == Kind::Cask) && !stopped() {
        progress(progress_event("Downloading apps", None, done, total));
        let started = std::time::Instant::now();
        prefetch_casks("brew", selections);
        tracing::info!(elapsed_ms = elapsed_ms(started), "prefetch casks");
    }

    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    let mut npm_versions: Option<Versions> = None;
    let npm_approved = load_settings().map_or_default(|s| s.npm_scripts_approved);
    for (i, sel) in selections.iter().enumerate() {
        match build_update_command(sel.kind, &sel.id) {
            None => {
                results[i] = Some(failed_result(
                    sel,
                    format!("unsupported or invalid update: {} / {}", sel.kind, sel.id),
                ));
            }
            Some(_) if sel.kind == Kind::Brew => formulae.push(i),
            Some(_) if sel.kind == Kind::Cask => casks.push(i),
            Some(_) if sel.kind == Kind::Npm => {
                if stopped() {
                    results[i] = Some(not_started(sel));
                    continue;
                }
                progress(progress_event("Installing", Some(&sel.name), done, total));
                done += 1;
                let versions = npm_versions.get_or_insert_with(versions_last_shown);
                let latest = versions
                    .get(&(sel.kind, sel.id.clone()))
                    .map(|(_, latest)| latest.as_str());
                results[i] = Some(npm::update(sel, latest, &npm_approved, "npm"));
            }
            Some((cmd, args)) => {
                if stopped() {
                    results[i] = Some(not_started(sel));
                    continue;
                }
                // App Store apps are bundles in /Applications named after the app.
                if sel.kind == Kind::Mas {
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
                    kind: sel.kind,
                    name: sel.name.clone(),
                    success,
                    output,
                    action: None,
                    elapsed_ms: Some(elapsed_ms(started)),
                    skipped: false,
                    steps: Vec::new(),
                    blocked: false,
                });
            }
        }
    }

    // Casks: skip ones that are open (upgrading can quit them and lose unsaved work), refuse any
    // whose download is signed by a different publisher, and snapshot the rest for rollback.
    let mut cleared_casks = Vec::new();
    let tokens: Vec<String> = casks.iter().map(|&i| selections[i].id.clone()).collect();
    let apps_by_token = cask_apps(&tokens);
    let downloads = if tokens.is_empty() || stopped() {
        HashMap::new()
    } else {
        preverify::cached_downloads(&tokens)
    };
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
        match preverify::check_cask(
            &sel.id,
            &installed,
            downloads.get(&sel.id).map(PathBuf::as_path),
        ) {
            preverify::Verdict::Blocked {
                reason,
                bundle_id,
                team,
            } => {
                tracing::warn!(cask = %sel.id, %reason, "blocked before install");
                results[i] = Some(UpdateResult {
                    action: bundle_id
                        .map(|bundle_id| ResultAction::TrustSigner { bundle_id, team }),
                    blocked: true,
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

    let formula_results = upgrade_batch(
        "brew",
        "--formula",
        &formulae,
        selections,
        progress,
        done,
        total,
    );
    done += formulae.len();
    for (i, r) in formulae.iter().zip(formula_results) {
        results[*i] = Some(r);
    }
    let cask_indices: Vec<usize> = cleared_casks.iter().map(|(i, _, _)| *i).collect();
    let cask_results = upgrade_batch(
        "brew",
        "--cask",
        &cask_indices,
        selections,
        progress,
        done,
        total,
    );
    for ((i, note, snapshotted), mut r) in cleared_casks.into_iter().zip(cask_results) {
        if let Some(note) = note {
            r.output = format!("{note}\n{}", r.output).trim().to_string();
        }
        if snapshotted && r.success {
            r.action = Some(ResultAction::Rollback {
                token: selections[i].id.clone(),
            });
        }
        crate::apps::explain_app_management(&mut r);
        results[i] = Some(r);
    }

    results
        .into_iter()
        .zip(selections)
        .map(|(r, sel)| r.unwrap_or_else(|| failed_result(sel, "not attempted".into())))
        .collect()
}

pub(crate) fn failed_result(sel: &UpdateSelection, output: String) -> UpdateResult {
    UpdateResult {
        kind: sel.kind,
        name: sel.name.clone(),
        success: false,
        output,
        action: None,
        elapsed_ms: None,
        skipped: false,
        steps: Vec::new(),
        blocked: false,
    }
}

const NOT_STARTED: &str = "Not started: you stopped the update.";

pub(crate) fn not_started(sel: &UpdateSelection) -> UpdateResult {
    UpdateResult {
        skipped: true,
        steps: Vec::new(),
        ..failed_result(sel, NOT_STARTED.into())
    }
}

pub(crate) fn skipped_open(sel: &UpdateSelection, bundle: &Path) -> UpdateResult {
    let app = bundle
        .file_name()
        .map_or_default(|n| n.to_string_lossy().into_owned());
    let shown = app.trim_end_matches(".app");
    UpdateResult {
        skipped: true,
        steps: Vec::new(),
        blocked: false,
        action: Some(ResultAction::QuitAndUpdate {
            kind: sel.kind,
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

// One `brew upgrade <kind_flag> a b c …` for the given selections; one result per index, in order.
// `done_before`/`total` place this batch within the whole run for progress reporting.
#[tracing::instrument(skip(indices, selections, progress), fields(count = indices.len()))]
// `brew` is the program to run: "brew" (resolved to the trusted Homebrew), or a stand-in in tests.
fn upgrade_batch(
    brew: &str,
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
    // Formula bottles: have Homebrew verify each one's build provenance (a signed record that it
    // was built by Homebrew's own CI from the published formula) when it can — see
    // provenance_env.
    let envs = if kind_flag == "--formula" {
        provenance_env(github_credentials_available())
    } else {
        Vec::new()
    };
    // Each line with when it arrived, for the per-step timing in the results (brew_steps).
    let mut timed: Vec<(u64, String)> = Vec::new();
    let mut current: Option<&str> = None;
    let (batch_ok, output) = run_update_cmd_streaming(brew, &args, &envs, &mut |line| {
        if line.starts_with("==> ") {
            timed.push((elapsed_ms(started), line.to_string()));
        }
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
                current = Some(name);
            }
        }
        // Every step, so the window can show what brew is doing right now and for how long.
        if let Some(header) = line.strip_prefix("==> ") {
            let done = done_before + seen.len().saturating_sub(1);
            let mut event = progress_event("Upgrading", current, done, total);
            event.step = Some(step_label(header));
            progress(event);
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
        run_cmd(brew, &["outdated", "--json=v2", "--greedy"])
            .ok()
            .map(|json| outdated_names(&json))
    };
    let sections = split_brew_sections(&output, &ids);
    let mut timings = brew_steps(&timed, elapsed, &ids);

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
            // Its own time when brew's output says where it went; else the batch's.
            let (steps, ms) = timings.remove(*id).unwrap_or((Vec::new(), elapsed));
            UpdateResult {
                kind: sel.kind,
                name: sel.name.clone(),
                success,
                output: item_output,
                action: None,
                elapsed_ms: Some(ms),
                skipped: false,
                steps,
                blocked: false,
            }
        })
        .collect()
}

// A step's label: the `==> ` line without its marker, a bottle file name shortened to the package
// ("Pouring python@3.15--3.15.1.arm64_tahoe.bottle.tar.gz" → "Pouring python@3.15"). Homebrew 5's
// download queue also unpacks each bottle and cask (mounting the DMG, copying the app out) under
// its "Fetching downloads" heading — for a big app that's most of the step, even with the file
// already cached — so it's named for both. Pure.
fn step_label(header: &str) -> String {
    let text = header.trim();
    if let Some(what) = text.strip_prefix("Fetching downloads for: ") {
        return format!("Downloading and unpacking: {what}");
    }
    text.strip_prefix("Pouring ").map_or_else(
        || text.to_string(),
        |file| format!("Pouring {}", file.split("--").next().unwrap_or(file)),
    )
}

// Per-package steps and time for a batched `brew upgrade`, from its output lines and when each
// arrived (ms since start; `total_ms` is when brew exited). A step runs from its `==> ` line to the
// next one, so "Pouring awscli" covers unpacking, relocating and linking. Lines are attributed to
// packages as split_brew_sections does; steps before the first requested package (the batch's
// up-front downloads) are shared by every item. Returns name → (steps, ms), where ms is the
// item's own steps plus the shared ones. Pure → unit-tested.
fn brew_steps(
    lines: &[(u64, String)],
    total_ms: u64,
    names: &[&str],
) -> HashMap<String, (Vec<Step>, u64)> {
    let wanted: std::collections::HashSet<&str> = names.iter().copied().collect();
    // (owner, step); owner None = before any requested package = shared.
    let mut steps: Vec<(Option<String>, Step, u64)> = Vec::new();
    let mut current: Option<String> = None;
    for (at, line) in lines {
        let Some(header) = line.strip_prefix("==> ") else {
            continue;
        };
        if let Some(rest) = header.strip_prefix("Upgrading ") {
            if let Some(name) = rest
                .split_whitespace()
                .next()
                .filter(|n| wanted.contains(n))
            {
                current = Some(name.to_string());
            }
        }
        if let Some((_, last, start)) = steps.last_mut() {
            last.ms = at.saturating_sub(*start);
        }
        let step = Step {
            label: step_label(header),
            ms: 0,
            shared: current.is_none(),
        };
        steps.push((current.clone(), step, *at));
    }
    if let Some((_, last, start)) = steps.last_mut() {
        last.ms = total_ms.saturating_sub(*start);
    }
    let shared: Vec<Step> = steps
        .iter()
        .filter(|(owner, ..)| owner.is_none())
        .map(|(_, s, _)| s.clone())
        .collect();
    let shared_ms: u64 = shared.iter().map(|s| s.ms).sum();
    let mut out: HashMap<String, (Vec<Step>, u64)> = HashMap::new();
    for (owner, step, _) in steps {
        if let Some(name) = owner {
            let entry = out
                .entry(name)
                .or_insert_with(|| (shared.clone(), shared_ms));
            entry.1 += step.ms;
            entry.0.push(step);
        }
    }
    out
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

// --- Quit an open app, then update just that item ---

#[allow(clippy::needless_pass_by_value)] // Tauri hands command arguments over by value.
#[tauri::command]
pub(crate) async fn quit_and_update(
    app: tauri::AppHandle,
    kind: Kind,
    id: String,
    name: String,
    app_name: String,
) -> Result<Vec<UpdateResult>, String> {
    if !is_valid_app_name(&app_name) || !matches!(kind, Kind::Cask | Kind::Mas) {
        return Err("invalid app".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _busy = begin_operation().ok_or(BUSY_MESSAGE)?;
        if scheduled_run_active() {
            return Err(SCHEDULED_BUSY_MESSAGE.into());
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::model::{Kind, UpdateResult, UpdateSelection};
    use crate::process::{stop_updates, DOWNLOADING, RUNNING_CHILD, STOP_REQUESTED};
    use crate::store::write_private;
    use crate::test_support::{read, CHILD_SLOT};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::Ordering;

    #[test]
    fn update_command_mapping() {
        assert_eq!(
            build_update_command(Kind::Brew, "wget"),
            Some(("brew", vec!["upgrade".into(), "wget".into()]))
        );
        assert_eq!(
            build_update_command(Kind::Cask, "firefox"),
            Some((
                "brew",
                vec!["upgrade".into(), "--cask".into(), "firefox".into()]
            ))
        );
        assert_eq!(
            build_update_command(Kind::Mas, "462058435"),
            Some(("mas", vec!["upgrade".into(), "462058435".into()]))
        );
        assert_eq!(
            build_update_command(Kind::Npm, "typescript"),
            Some((
                "npm",
                vec!["install".into(), "-g".into(), "typescript@latest".into()]
            ))
        );
        assert_eq!(
            build_update_command(Kind::Npm, "@scope/pkg"),
            Some((
                "npm",
                vec!["install".into(), "-g".into(), "@scope/pkg@latest".into()]
            ))
        );
        assert_eq!(build_update_command(Kind::Unknown, "x"), None);
        // crafted ids must be rejected (no flag / install-spec injection)
        assert_eq!(build_update_command(Kind::Brew, "--force"), None);
        assert_eq!(build_update_command(Kind::Mas, "not-a-number"), None);
        assert_eq!(
            build_update_command(Kind::Npm, "https://evil.example/x.tgz"),
            None
        );
        assert_eq!(build_update_command(Kind::Npm, "-g"), None);
    }

    // The concurrent pre-fetch partitions selections into formula/cask batches, reusing the same
    // id validation as the install path so a crafted token can't smuggle a flag into fetch either.
    #[test]
    fn fetch_batches_partitions_and_validates() {
        let sels = vec![
            UpdateSelection {
                kind: Kind::Brew,
                id: "wget".into(),
                name: "wget".into(),
            },
            UpdateSelection {
                kind: Kind::Cask,
                id: "slack".into(),
                name: "Slack".into(),
            },
            // mas/npm have no fetch-without-install step → excluded.
            UpdateSelection {
                kind: Kind::Mas,
                id: "462058435".into(),
                name: "Excel".into(),
            },
            UpdateSelection {
                kind: Kind::Npm,
                id: "typescript".into(),
                name: "ts".into(),
            },
            // crafted ids are rejected (no flag / path injection into fetch).
            UpdateSelection {
                kind: Kind::Brew,
                id: "--force".into(),
                name: "x".into(),
            },
            UpdateSelection {
                kind: Kind::Cask,
                id: "a/../b".into(),
                name: "y".into(),
            },
        ];
        let (formulae, casks) = fetch_batches(&sels);
        assert_eq!(formulae, vec!["wget"]);
        assert_eq!(casks, vec!["slack"]);
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

    // The `==> ` lines of a real awscli upgrade (2.37.12_2 → 2.38.5, Homebrew 7), with made-up
    // arrival times: dependencies' steps count toward awscli, and the slow step is visible.
    fn awscli_timed() -> Vec<(u64, String)> {
        [
            (0, "==> Upgrading awscli"),
            (900, "==> Installing dependencies for awscli: ca-certificates, s2n, aws-c-http, aws-c-mqtt, sqlite and python@3.15"),
            (1_000, "==> Upgrading awscli dependency: ca-certificates (2026-09-25 -> 2026-10-20)"),
            (1_500, "==> Pouring ca-certificates--2026-10-20.all.bottle.tar.gz"),
            (3_000, "==> Installing awscli dependency: python@3.15 (3.15.1)"),
            (4_000, "==> Pouring python@3.15--3.15.1.arm64_tahoe.bottle.tar.gz"),
            (45_000, "==> Installing awscli"),
            (45_500, "==> Pouring awscli--2.38.5.arm64_tahoe.bottle.tar.gz"),
            (100_000, "==> Cleanup"),
            (118_000, "==> Caveats"),
            (118_100, "==> awscli"),
            (118_200, "==> Upgraded 1 requested outdated package"),
        ]
        .into_iter()
        .map(|(at, l)| (at, l.to_string()))
        .collect()
    }

    #[test]
    fn steps_time_each_brew_stage_including_dependencies() {
        let timings = brew_steps(&awscli_timed(), 119_000, &["awscli"]);
        let (steps, ms) = &timings["awscli"];
        assert_eq!(*ms, 119_000, "a lone package owns all of the batch's time");
        let find = |label: &str| steps.iter().find(|s| s.label == label).map(|s| s.ms);
        assert_eq!(find("Pouring python@3.15"), Some(41_000));
        assert_eq!(find("Pouring awscli"), Some(54_500));
        assert_eq!(find("Cleanup"), Some(18_000));
        assert_eq!(
            find("Upgraded 1 requested outdated package"),
            Some(800),
            "the last step runs until brew exits"
        );
        assert!(steps.iter().all(|s| !s.shared));
        assert_eq!(steps.iter().map(|s| s.ms).sum::<u64>(), 119_000);
    }

    #[test]
    fn steps_split_a_batch_and_share_its_up_front_downloads() {
        let timed: Vec<(u64, String)> = [
            (0, "==> Fetching downloads for: git and wget"),
            (6_000, "==> Upgrading git"),
            (6_100, "==> Pouring git--2.56.0.arm64_tahoe.bottle.tar.gz"),
            (8_000, "==> Upgrading wget"),
            (8_200, "==> Pouring wget--1.25.1.arm64_tahoe.bottle.tar.gz"),
        ]
        .into_iter()
        .map(|(at, l)| (at, l.to_string()))
        .collect();
        let timings = brew_steps(&timed, 9_000, &["git", "wget", "absent"]);
        let (git, git_ms) = &timings["git"];
        let (wget, wget_ms) = &timings["wget"];
        assert_eq!(
            (*git_ms, *wget_ms),
            (8_000, 7_000),
            "own steps + the shared 6s"
        );
        assert_eq!(git[0].label, "Downloading and unpacking: git and wget");
        assert!(git[0].shared && wget[0].shared);
        assert_eq!(git[0].ms, 6_000);
        assert!(git.iter().all(|s| !s.label.contains("wget") || s.shared));
        assert_eq!(wget.last().map(|s| s.ms), Some(800));
        assert!(!timings.contains_key("absent"));
    }

    #[test]
    fn steps_need_brew_headers() {
        assert!(brew_steps(&[], 5_000, &["git"]).is_empty());
        let only_shared = vec![(0, "==> Fetching downloads for: git".to_string())];
        assert!(
            brew_steps(&only_shared, 5_000, &["git"]).is_empty(),
            "no section of its own → fall back to the batch time"
        );
    }

    #[test]
    fn step_labels_shorten_bottle_names() {
        assert_eq!(
            step_label("Pouring python@3.15--3.15.1.arm64_tahoe.bottle.tar.gz"),
            "Pouring python@3.15"
        );
        assert_eq!(
            step_label("Pouring ca-certificates--2026-10-20.all.bottle.tar.gz"),
            "Pouring ca-certificates"
        );
        assert_eq!(step_label("Installing Cask zoom"), "Installing Cask zoom");
        // Not just a download: brew unpacks there too (a cached Slack still spends ~19s on it).
        assert_eq!(
            step_label("Fetching downloads for: slack"),
            "Downloading and unpacking: slack"
        );
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

    // A whole batch through a stand-in brew that prints Homebrew's real step lines with pauses
    // between them: every step reaches the window as a live progress event (with the package it
    // belongs to), and each item's result carries its own steps and time, the up-front downloads
    // shared.
    #[test]
    fn batch_reports_live_steps_and_per_item_times() {
        let _slot = CHILD_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("freshet-steps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let brew = dir.join("brew");
        std::fs::write(
            &brew,
            "#!/bin/sh\n\
             echo '==> Fetching downloads for: slowapp and quickapp'; sleep 0.3\n\
             echo '==> Upgrading slowapp'; echo '  1.0 -> 1.1'\n\
             echo '==> Installing Cask slowapp'; sleep 0.6\n\
             echo '==> Upgrading quickapp'\n\
             echo '==> Installing Cask quickapp'; sleep 0.1\n",
        )
        .unwrap();
        std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).unwrap();

        let selections: Vec<UpdateSelection> = ["slowapp", "quickapp"]
            .iter()
            .map(|id| UpdateSelection {
                kind: Kind::Cask,
                id: (*id).into(),
                name: (*id).into(),
            })
            .collect();
        let events = std::sync::Mutex::new(Vec::new());
        let record = |p: UpdateProgress| events.lock().unwrap().push(p);
        let results = upgrade_batch(
            brew.to_str().unwrap(),
            "--cask",
            &[0, 1],
            &selections,
            &record,
            0,
            2,
        );
        let _ = std::fs::remove_dir_all(&dir);

        // Live: one event per step, in order, each naming its package and the count so far.
        let steps: Vec<(Option<String>, Option<String>, usize)> = events
            .into_inner()
            .unwrap()
            .into_iter()
            .filter(|e| e.step.is_some())
            .map(|e| (e.step, e.current, e.done))
            .collect();
        let s = |x: &str| Some(x.to_string());
        assert_eq!(
            steps,
            [
                (
                    s("Downloading and unpacking: slowapp and quickapp"),
                    None,
                    0
                ),
                (s("Upgrading slowapp"), s("slowapp"), 0),
                (s("Installing Cask slowapp"), s("slowapp"), 0),
                (s("Upgrading quickapp"), s("quickapp"), 1),
                (s("Installing Cask quickapp"), s("quickapp"), 1),
            ]
        );

        // Results: own steps after the shared download step, and times that follow the sleeps.
        let (slow, quick) = (&results[0], &results[1]);
        assert!(slow.success && quick.success);
        let labels = |r: &UpdateResult| r.steps.iter().map(|x| x.label.clone()).collect::<Vec<_>>();
        assert_eq!(
            labels(slow),
            [
                "Downloading and unpacking: slowapp and quickapp",
                "Upgrading slowapp",
                "Installing Cask slowapp"
            ]
        );
        assert_eq!(
            labels(quick),
            [
                "Downloading and unpacking: slowapp and quickapp",
                "Upgrading quickapp",
                "Installing Cask quickapp"
            ]
        );
        assert!(slow.steps[0].shared && quick.steps[0].shared);
        assert!(slow.steps[1..].iter().all(|x| !x.shared));
        let install = slow.steps[2].ms;
        assert!(
            (500..2_000).contains(&install),
            "slowapp's install slept 0.6s, measured {install}ms"
        );
        let (slow_ms, quick_ms) = (slow.elapsed_ms.unwrap(), quick.elapsed_ms.unwrap());
        assert!(
            slow_ms >= 800 && quick_ms >= 300 && slow_ms > quick_ms + 300,
            "each item's own time, plus the shared 0.3s: slow {slow_ms}ms, quick {quick_ms}ms"
        );
        for r in &results {
            assert_eq!(
                r.elapsed_ms,
                Some(r.steps.iter().map(|x| x.ms).sum()),
                "an item's time is the sum of its steps"
            );
        }
    }

    // ---------- #3 formula provenance and App Store apps ----------

    #[test]
    fn provenance_verification_only_with_github_credentials() {
        assert_eq!(
            provenance_env(true),
            [("HOMEBREW_VERIFY_ATTESTATIONS", "1")]
        );
        assert!(
            provenance_env(false).is_empty(),
            "without credentials Homebrew would refuse every bottle"
        );
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

    // Story 20: when an item in a batch can't be mapped to a real command (bogus kind, crafted
    // id), the batch records a per-item failure and keeps going. With no brew/cask in the
    // selection list, the `brew update` preamble is skipped too — this test runs entirely
    // in-process with no subprocess spawns.
    #[test]
    fn run_selected_updates_skips_unsupported_and_continues() {
        let bad = vec![
            UpdateSelection {
                kind: Kind::Unknown,
                id: "x".into(),
                name: "X".into(),
            },
            UpdateSelection {
                kind: Kind::Mas,
                id: "not-numeric".into(),
                name: "Bad MAS".into(),
            },
            UpdateSelection {
                kind: Kind::Npm,
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

    // Stop during the download phase ends it at once: nothing is being installed, so there is
    // no package boundary to wait for.
    #[test]
    fn stop_interrupts_the_download_phase_immediately() {
        let _slot = CHILD_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("freshet-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let brew = dir.join("brew");
        // A download that would take 30s, and says so when interrupted.
        let script = format!(
            "#!/bin/bash\ntrap 'echo interrupted > \"{d}/outcome\"; exit 130' INT TERM\n\
             echo \"$*\" > \"{d}/args\"\nsleep 30 & wait $!\necho finished > \"{d}/outcome\"\n",
            d = dir.display()
        );
        write_private(&brew, &script, true).unwrap();
        let selections = [UpdateSelection {
            kind: Kind::Cask,
            id: "bigapp".into(),
            name: "Big App".into(),
        }];
        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Once the download is running — the stand-in has started and set its trap, which
                // is before it writes `args` (a fixed delay raced bash start-up under a loaded
                // parallel test run) — press Stop.
                while RUNNING_CHILD.load(Ordering::SeqCst) == 0 || !dir.join("args").exists() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                stop_updates();
            });
            prefetch_casks(&brew.to_string_lossy(), &selections);
        });
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "didn't wait for the download to finish"
        );
        assert_eq!(read(&dir.join("args")).trim(), "fetch --cask bigapp");
        assert_eq!(read(&dir.join("outcome")).trim(), "interrupted");
        assert!(!DOWNLOADING.load(Ordering::SeqCst), "phase flag cleared");
        // Outside the download phase, Stop only sets the flag (the boundary logic does the rest).
        assert!(STOP_REQUESTED.load(Ordering::SeqCst));
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
