// Update history: ~/.freshet/history.jsonl, one line per package per run.

use crate::check::{versions_last_shown, Versions};
use crate::model::{Kind, ResultAction, UpdateResult, UpdateSelection};
use crate::scheduled::needs_admin;
use crate::store::{freshet_dir, unix_now, write_private_atomic};
use crate::update::provenance_failed;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// --- Update history: ~/.freshet/history.jsonl, one line per package per run ---

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistoryEntry {
    // Unix seconds.
    pub(crate) at: u64,
    // "manual" or "scheduled".
    pub(crate) source: String,
    pub(crate) kind: Kind,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) from: String,
    pub(crate) to: String,
    // "updated" | "failed" | "needs_admin" | "blocked" | "skipped_open"
    pub(crate) outcome: String,
    #[serde(default)]
    pub(crate) elapsed_ms: Option<u64>,
}

const HISTORY_KEEP: usize = 2000;

fn history_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("history.jsonl"))
}

pub(crate) fn outcome_of(r: &UpdateResult) -> &'static str {
    match &r.action {
        _ if !r.success && provenance_failed(&r.output) => "blocked",
        _ if r.blocked => "blocked",
        Some(ResultAction::TrustSigner { .. }) => "blocked",
        Some(ResultAction::QuitAndUpdate { .. }) => "skipped_open",
        _ if r.success => "updated",
        _ if needs_admin(&r.output) => "needs_admin",
        _ => "failed",
    }
}

// One entry per attempted package. Items never started (the user stopped the run) aren't
// history. `results` must align 1:1 with `selections`. Pure → unit-tested.
pub(crate) fn history_entries(
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
                .get(&(sel.kind, sel.id.clone()))
                .cloned()
                .unwrap_or_default();
            HistoryEntry {
                at,
                source: source.into(),
                kind: sel.kind,
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

pub(crate) fn append_history(entries: &[HistoryEntry]) {
    let Some(path) = history_path() else { return };
    if entries.is_empty() {
        return;
    }
    let mut lines: Vec<String> =
        std::fs::read_to_string(&path).map_or_default(|s| s.lines().map(String::from).collect());
    lines.extend(entries.iter().filter_map(|e| serde_json::to_string(e).ok()));
    let start = lines.len().saturating_sub(HISTORY_KEEP);
    let _ = write_private_atomic(&path, &(lines[start..].join("\n") + "\n"));
}

// The update results come back with the optional "brew update" row first; history wants the
// rows that line up with the selections.
pub(crate) fn record_manual_history(selections: &[UpdateSelection], results: &[UpdateResult]) {
    let offset = results.len().saturating_sub(selections.len());
    let versions = versions_last_shown();
    append_history(&history_entries(
        selections,
        &results[offset..],
        &versions,
        "manual",
        unix_now(),
    ));
}

// Newest first.
#[tauri::command(async)]
pub(crate) fn get_history(limit: Option<usize>) -> Vec<HistoryEntry> {
    let text = history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str(l).ok())
        .take(limit.unwrap_or(200).min(HISTORY_KEEP))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::check::Versions;
    use crate::model::{Kind, UpdateResult};

    use crate::test_support::sel;
    use crate::update::{failed_result, not_started, skipped_open};
    use std::path::Path;

    #[test]
    fn history_records_outcomes_and_versions_but_not_unstarted_items() {
        let selections = [
            sel(Kind::Brew, "git"),
            sel(Kind::Cask, "slack"),
            sel(Kind::Cask, "zoom"),
            sel(Kind::Brew, "go"),
        ];
        let ok = UpdateResult {
            kind: Kind::Brew,
            name: "git".into(),
            success: true,
            output: String::new(),
            action: None,
            elapsed_ms: Some(1200),
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        };
        let open = skipped_open(&selections[1], Path::new("/Applications/Slack.app"));
        let admin = failed_result(&selections[2], "sudo: a password is required".into());
        let stopped = not_started(&selections[3]);
        let versions = Versions::from([(
            (Kind::Brew, "git".into()),
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
}
