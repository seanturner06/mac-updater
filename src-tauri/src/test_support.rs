// Fixtures shared by more than one module's tests.
#![allow(clippy::unwrap_used)]

use crate::model::{CategoryStatus, Kind, OutdatedItem, UpdateSelection};
use std::path::Path;

pub(crate) fn item(id: &str, latest: &str, self_updating: bool) -> OutdatedItem {
    OutdatedItem {
        id: id.into(),
        name: id.into(),
        current: "1".into(),
        latest: latest.into(),
        self_updating,
        notes_url: String::new(),
    }
}

pub(crate) fn category(kind: Kind, items: Vec<OutdatedItem>) -> CategoryStatus {
    CategoryStatus {
        category: kind.to_string(),
        kind,
        items,
        error: None,
    }
}

pub(crate) fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

// Tests that spawn through run_update_cmd_streaming share the global RUNNING_CHILD slot.
pub(crate) static CHILD_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn sel(kind: Kind, id: &str) -> UpdateSelection {
    UpdateSelection {
        kind,
        id: id.into(),
        name: id.into(),
    }
}
