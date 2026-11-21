// The menu-bar icon.

use serde::Deserialize;

// Text shown next to the menu-bar icon: the count, or nothing when there are no updates.
fn badge_title(count: u32) -> Option<String> {
    if count == 0 {
        None
    } else {
        Some(count.to_string())
    }
}

// What the menu-bar icon should say, from the window's point of view.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrayState {
    // Updates available.
    pub(crate) count: u32,
    // Something needs a look: a publisher change, a known vulnerability, a scheduled run that
    // failed or was blocked.
    pub(crate) attention: bool,
    // Updating now (in the window, or a scheduled run); `done` of `total` when known.
    pub(crate) running: bool,
    pub(crate) done: u32,
    pub(crate) total: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayLook {
    // The template drop: macOS tints it for a light or dark menu bar.
    Normal,
    // Outline drop (template): an update is running.
    Running,
    // Amber drop, not a template, so the colour shows: needs a look.
    Attention,
}

// Icon and title for a state. Running wins (it's what's happening now), then attention.
// Pure → unit-tested.
fn tray_look(s: &TrayState) -> (TrayLook, Option<String>) {
    if s.running {
        let title = (s.total > 0).then(|| format!("{}/{}", s.done.min(s.total), s.total));
        (TrayLook::Running, title)
    } else if s.attention {
        (TrayLook::Attention, badge_title(s.count))
    } else {
        (TrayLook::Normal, badge_title(s.count))
    }
}

// The icon last set, so a stream of progress updates only swaps the image when it changes.
static TRAY_LOOK: std::sync::Mutex<Option<TrayLook>> = std::sync::Mutex::new(None);

// AppHandle is taken by value to match the Tauri command convention; it's a cheap Arc clone.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub(crate) fn set_tray_state(app: tauri::AppHandle, state: TrayState) {
    let Some(tray) = app.tray_by_id("main-tray") else {
        return;
    };
    let (look, title) = tray_look(&state);
    let mut last = TRAY_LOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *last != Some(look) {
        let bytes: &[u8] = match look {
            TrayLook::Normal => include_bytes!("../icons/tray-template.png"),
            TrayLook::Running => include_bytes!("../icons/tray-running.png"),
            TrayLook::Attention => include_bytes!("../icons/tray-attention.png"),
        };
        if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_icon_as_template(look != TrayLook::Attention);
            *last = Some(look);
        }
    }
    drop(last);
    let _ = tray.set_title(title);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    #[test]
    fn badge_shows_count_or_nothing() {
        assert_eq!(badge_title(0), None);
        assert_eq!(badge_title(3).as_deref(), Some("3"));
    }

    #[test]
    fn tray_shows_running_then_attention_then_count() {
        let base = TrayState {
            count: 4,
            attention: false,
            running: false,
            done: 0,
            total: 0,
        };
        assert_eq!(tray_look(&base), (TrayLook::Normal, Some("4".into())));
        let idle = TrayState { count: 0, ..base };
        assert_eq!(tray_look(&idle), (TrayLook::Normal, None));
        let flagged = TrayState {
            attention: true,
            ..base
        };
        assert_eq!(tray_look(&flagged), (TrayLook::Attention, Some("4".into())));
        let running = TrayState {
            running: true,
            attention: true,
            done: 3,
            total: 12,
            ..base
        };
        assert_eq!(
            tray_look(&running),
            (TrayLook::Running, Some("3/12".into())),
            "running wins over attention"
        );
        let scheduled = TrayState {
            running: true,
            ..base
        };
        assert_eq!(
            tray_look(&scheduled),
            (TrayLook::Running, None),
            "a scheduled run has no count to show"
        );
        let overshoot = TrayState {
            running: true,
            done: 13,
            total: 12,
            ..base
        };
        assert_eq!(tray_look(&overshoot).1.as_deref(), Some("12/12"));
    }
}
