// Freshet's backend. This file only wires things together: the menu-bar tray, the window, and
// the list of commands the window can call. The work is in the modules:
//
//   model       the data that crosses to the window: what's outdated, what was asked, what happened
//   check       what is outdated (Homebrew, App Store, npm), `brew update`, the cached last check
//   sparkle     apps with their own Sparkle updater (detection only)
//   details     descriptions, homepages and release notes for the list
//   update      installing the selected updates, with live progress and Stop
//   npm         npm globals without running install scripts by default
//   adopt       bringing a manually installed app under Homebrew
//   signing     who signed an app; the trusted-publisher baseline; publisher-change checks
//   preverify   the publisher check on a cask download, before anything is installed
//   rollback    snapshots of an app before it is updated, and restoring one
//   vulns       known vulnerabilities for npm updates (osv.dev, opt-in)
//   scheduled   `freshet --run-scheduled`, the headless daily run
//   launchd     the launchd job that starts it
//   settings    ~/.freshet/settings.json, shared by the app and the scheduled run
//   history     ~/.freshet/history.jsonl
//   apps        app bundles on disk: versions, icons, what's running, which cask owns what
//   process     running other programs: trusted binaries, elevation, streaming, stopping
//   shell_path  PATH for child processes
//   store       ~/.freshet itself and how files in it are written
//   validate    checks on everything that arrives from the window
//   tray        the menu-bar icon
//   self_update Freshet's own updates, version, launch at login

use crate::launchd::RUN_SCHEDULED_FLAG;
use crate::process::interrupt_running_child;
use crate::scheduled::run_scheduled;
use crate::shell_path::augment_path;
use crate::store::remove_legacy_askpass;
use tauri::Manager;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_positioner::{Position, WindowExt as _};

mod adopt;
mod apps;
mod check;
mod details;
mod history;
mod launchd;
mod model;
mod npm;
mod preverify;
mod process;
mod rollback;
mod scheduled;
mod self_update;
mod settings;
mod shell_path;
mod signing;
mod sparkle;
mod store;
#[cfg(test)]
mod test_support;
mod tray;
mod update;
mod validate;
mod vulns;

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
    remove_legacy_askpass();
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

            // The menu bar gets its own single-colour drop (icons/source/menubar-icon.svg), drawn
            // as a template: black on transparent, which macOS tints to match a light or dark
            // menu bar. 36px = 18pt at 2x. Reusing the full-colour app icon here would show up as
            // a solid rounded square.
            let icon =
                tauri::image::Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;
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
            check::check_outdated,
            sparkle::check_sparkle_apps,
            adopt::search_casks,
            apps::open_app_management_settings,
            adopt::suggest_adoptions,
            details::cask_info,
            details::open_url,
            tray::set_tray_state,
            adopt::adopt_cask,
            adopt::adopt_casks,
            update::run_selected_updates,
            signing::verify_signatures,
            signing::verify_updated_casks,
            launchd::install_schedule,
            launchd::uninstall_schedule,
            launchd::get_schedule,
            settings::get_settings,
            settings::save_settings,
            details::package_details,
            apps::app_icon,
            check::get_cached_check,
            signing::trust_signer,
            rollback::list_snapshots,
            rollback::restore_snapshot,
            vulns::check_vulnerabilities,
            process::stop_updates,
            adopt::cancel_adoption,
            history::get_history,
            scheduled::get_scheduled_state,
            update::homebrew_provenance_active,
            settings::approve_npm_scripts,
            update::quit_and_update,
            self_update::check_app_update,
            self_update::install_app_update,
            self_update::app_version,
            self_update::get_launch_at_login,
            self_update::set_launch_at_login,
            scheduled::get_last_scheduled_run
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
