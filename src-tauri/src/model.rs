// The data that crosses the IPC boundary and the scheduled run: what is outdated, what was
// asked for, and what happened.

use serde::{Deserialize, Serialize};

// What a row is, and so how it's checked and updated. Travels as the lowercase word it always
// was ("brew", "cask", …), so the window, stored history, the cached check and skipped-version
// keys are unchanged; in here it's a type, so a misspelt kind is a compile error, not a
// comparison that quietly never matches.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    // Homebrew formula.
    Brew,
    // Homebrew cask.
    Cask,
    // Mac App Store app.
    Mas,
    // Global npm package.
    Npm,
    // App with its own Sparkle updater: detected, never updated from here.
    Sparkle,
    // A row of the signature report, not a package.
    Verify,
    // Anything else the webview sends. Refused per item (see build_update_command) instead of
    // failing to deserialize the whole request.
    #[serde(other)]
    Unknown,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Brew => "brew",
            Self::Cask => "cask",
            Self::Mas => "mas",
            Self::Npm => "npm",
            Self::Sparkle => "sparkle",
            Self::Verify => "verify",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct OutdatedItem {
    // Identifier used to run the per-item update: formula/cask token, mas app id, or npm package name.
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) current: String,
    pub(crate) latest: String,
    // True for casks that update themselves (only surfaced via `brew outdated --cask --greedy`).
    // The frontend hides these unless the user opts in. Always false for non-cask items.
    pub(crate) self_updating: bool,
    // Release-notes URL when the source provides one (Sparkle appcasts). Empty otherwise.
    #[serde(default)]
    pub(crate) notes_url: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct CategoryStatus {
    pub(crate) category: String,
    // The frontend echoes this back in selections.
    pub(crate) kind: Kind,
    pub(crate) items: Vec<OutdatedItem>,
    pub(crate) error: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateSelection {
    pub(crate) kind: Kind,
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateResult {
    pub(crate) kind: Kind,
    pub(crate) name: String,
    pub(crate) success: bool,
    pub(crate) output: String,
    // A follow-up the user can take from the results panel (trust a new signer, roll back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) action: Option<ResultAction>,
    // Wall time for this item: in a batch, its own steps plus the batch's shared downloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) elapsed_ms: Option<u64>,
    // Deliberately not attempted (the app was open, or the user stopped the run): neither a
    // success nor a failure.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) skipped: bool,
    // Where the time went, from brew's `==> …` lines (see brew_steps). Empty for mas/npm.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) steps: Vec<Step>,
    // Refused by the pre-install publisher check: a safety block, not an ordinary failure. Said
    // outright, because not every block comes with an action to recognise it by (an unsigned
    // download has no new publisher to trust).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) blocked: bool,
}

// One `==> …` line of brew's output and how long until the next one: "Pouring python@3.15" took
// 41s. `shared` steps belong to the whole batch (fetching every download up front), not one item.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) label: String,
    pub(crate) ms: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) shared: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResultAction {
    // The publisher (Team ID) changed; trusting records `team` as the new baseline for `bundle_id`.
    TrustSigner {
        bundle_id: String,
        team: String,
    },
    // A pre-update snapshot of this cask's app exists and can be restored.
    Rollback {
        token: String,
    },
    // An npm package wants to run install scripts: approving lets this exact version run them.
    AllowNpmScripts {
        name: String,
        version: String,
        scripts: String,
    },
    // macOS refused to let Freshet change another developer's app (App Management): open the
    // settings pane where the user can allow it.
    AllowAppManagement {
        app: String,
    },
    // Skipped because the app was open: quit it, then update just this item.
    QuitAndUpdate {
        kind: Kind,
        id: String,
        name: String,
        app: String,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::history::HistoryEntry;
    use crate::settings::ignore_key;
    use crate::update::build_update_command;

    // The kind travels as the same lowercase word as before the enum existed, both ways, so the
    // window, history.jsonl, last-check.json and skipped-version keys keep working.
    #[test]
    fn kinds_keep_their_wire_names() {
        for (kind, word) in [
            (Kind::Brew, "brew"),
            (Kind::Cask, "cask"),
            (Kind::Mas, "mas"),
            (Kind::Npm, "npm"),
            (Kind::Sparkle, "sparkle"),
            (Kind::Verify, "verify"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), word);
            assert_eq!(serde_json::from_value::<Kind>(word.into()).unwrap(), kind);
            assert_eq!(kind.to_string(), word);
        }
        assert_eq!(ignore_key(Kind::Cask, "zoom", "6.1"), "cask::zoom::6.1");
    }

    // A kind the backend doesn't know is refused for that one item, not a failed request.
    #[test]
    fn an_unknown_kind_is_refused_per_item() {
        let selections: Vec<UpdateSelection> = serde_json::from_str(
            r#"[{"kind":"pip","id":"requests","name":"requests"},{"kind":"brew","id":"git","name":"git"}]"#,
        )
        .unwrap();
        assert_eq!(selections[0].kind, Kind::Unknown);
        assert_eq!(selections[1].kind, Kind::Brew);
        assert_eq!(build_update_command(Kind::Unknown, "requests"), None);
        assert_eq!(build_update_command(Kind::Sparkle, "Some App"), None);
        // An old history line with a kind we no longer know still loads.
        let entry: HistoryEntry = serde_json::from_str(
            r#"{"at":1,"source":"manual","kind":"gem","id":"x","name":"x","from":"1","to":"2","outcome":"updated"}"#,
        )
        .unwrap();
        assert_eq!(entry.kind, Kind::Unknown);
    }
}
