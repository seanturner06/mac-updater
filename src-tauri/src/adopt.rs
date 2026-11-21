// Homebrew adoption: bringing a manually installed app under Homebrew, so it updates here.

use crate::apps::{cask_apps, explain_app_management, list_applications};
use crate::model::{Kind, UpdateResult};
use crate::process::{
    begin_operation, elapsed_ms, interrupt_running_child, run_cmd, run_update_cmd_streaming,
    BUSY_MESSAGE, STOP_REQUESTED,
};
use crate::validate::{is_safe_search_query, is_valid_cask_token};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;

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

// --- Homebrew adoption: bring a manually-installed app under Homebrew management ---

#[derive(Serialize)]
pub(crate) struct CaskSuggestion {
    pub(crate) token: String,
    pub(crate) installed: bool,
}

// Search Homebrew casks by name; flag ones already installed via brew so the UI can mark them.
#[tauri::command]
pub(crate) async fn search_casks(query: String) -> Vec<CaskSuggestion> {
    tauri::async_runtime::spawn_blocking(move || {
        if !is_safe_search_query(&query) {
            return vec![];
        }
        let installed: std::collections::HashSet<String> = run_cmd("brew", &["list", "--cask"])
            .map_or_default(|s| parse_cask_tokens(&s).into_iter().collect());
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
pub(crate) struct AdoptableApp {
    pub(crate) name: String,
    pub(crate) token: String,
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
pub(crate) async fn suggest_adoptions() -> Vec<AdoptableApp> {
    tauri::async_runtime::spawn_blocking(suggest_adoptions_blocking)
        .await
        .unwrap_or_default()
}

fn suggest_adoptions_blocking() -> Vec<AdoptableApp> {
    {
        let all_casks: std::collections::HashSet<String> = run_cmd("brew", &["casks"])
            .map_or_default(|s| s.lines().map(|l| l.trim().to_string()).collect());
        if all_casks.is_empty() {
            return vec![];
        }
        // Apps Homebrew already manages: whatever bundles the installed casks own.
        let managed_tokens: Vec<String> =
            run_cmd("brew", &["list", "--cask"]).map_or_default(|s| parse_cask_tokens(&s));
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

// Live progress for an adoption ("adopt-progress" events).
#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
pub(crate) struct AdoptProgress {
    pub(crate) token: String,
    // "Starting", "Downloading", "Adopting".
    pub(crate) phase: String,
    // Size of the download so far, from Homebrew's in-progress file; 0 until one appears.
    pub(crate) downloaded_mb: u64,
    // No new data for STALL_SECS: the download server has likely stopped sending.
    pub(crate) stalled: bool,
}

const STALL_SECS: u64 = 60;

// The phase a line of `brew install --cask --adopt` output starts. Pure → unit-tested.
fn adopt_phase(line: &str) -> Option<&'static str> {
    let rest = line.strip_prefix("==> ")?;
    if rest.starts_with("Downloading") || rest.starts_with("Fetching") {
        Some("Downloading")
    } else if rest.starts_with("Adopting")
        || rest.starts_with("Moving")
        || rest.starts_with("Installing")
        || rest.starts_with("Linking")
    {
        Some("Adopting")
    } else {
        None
    }
}

// Whether a download has stalled: it has started, and its size hasn't moved for STALL_SECS.
// Pure → unit-tested.
const fn download_stalled(bytes: u64, unchanged_for_secs: u64) -> bool {
    bytes > 0 && unchanged_for_secs >= STALL_SECS
}

// Size of the newest partial download in Homebrew's cache (`*.incomplete`) that started after
// `since` — i.e. the one this adoption is fetching.
fn current_download_bytes(downloads: &Path, since: std::time::SystemTime) -> u64 {
    std::fs::read_dir(downloads).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".incomplete"))
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.modified().is_ok_and(|t| t >= since))
            .map(|m| m.len())
            .max()
            .unwrap_or(0)
    })
}

// `brew install --cask <token> --adopt`. Despite the name, --adopt still downloads the whole
// app (Homebrew checks it against the installed copy); it only skips moving it into place. A
// big app on a slow or stalled server can take a long time, so: stream brew's output for the
// phase, watch the partial download's size once a second for progress and stalls, and run it
// as the tracked child so cancel_adoption can interrupt it safely.
fn adopt_one(token: &str, progress: &(dyn Fn(AdoptProgress) + Sync)) -> UpdateResult {
    if !is_valid_cask_token(token) {
        return UpdateResult {
            kind: Kind::Cask,
            name: token.to_string(),
            success: false,
            output: "invalid cask token".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        };
    }
    let event = |phase: &str, downloaded: u64, stalled: bool| AdoptProgress {
        token: token.to_string(),
        phase: phase.into(),
        downloaded_mb: downloaded / 1_048_576,
        stalled,
    };
    progress(event("Starting", 0, false));
    let phase = std::sync::Mutex::new("Starting");
    let done = AtomicBool::new(false);
    let started_at = std::time::SystemTime::now();
    let started = std::time::Instant::now();
    let downloads = run_cmd("brew", &["--cache"])
        .ok()
        .map(|c| PathBuf::from(c.trim()).join("downloads"));
    let args = adopt_args(token);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let (success, output) = std::thread::scope(|scope| {
        // Watcher: download size, and whether it has stopped growing.
        scope.spawn(|| {
            let (mut last, mut last_change) = (0_u64, std::time::Instant::now());
            while !done.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let Some(dir) = &downloads else { continue };
                let bytes = current_download_bytes(dir, started_at);
                if bytes != last {
                    (last, last_change) = (bytes, std::time::Instant::now());
                }
                let current = *phase
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if current != "Adopting" && bytes > 0 {
                    let stalled = download_stalled(bytes, last_change.elapsed().as_secs());
                    progress(event("Downloading", bytes, stalled));
                }
            }
        });
        let result = run_update_cmd_streaming("brew", &arg_refs, &[], &mut |line| {
            if let Some(p) = adopt_phase(line) {
                *phase
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = p;
                progress(event(p, 0, false));
            }
        });
        done.store(true, Ordering::SeqCst);
        result
    });
    let cancelled = STOP_REQUESTED.load(Ordering::SeqCst);
    let mut result = UpdateResult {
        kind: Kind::Cask,
        name: token.to_string(),
        success,
        output: if cancelled && !success {
            "Cancelled. Nothing was changed; the app stays as it was.".into()
        } else {
            output
        },
        action: None,
        elapsed_ms: Some(elapsed_ms(started)),
        skipped: cancelled && !success,
        steps: Vec::new(),
        blocked: false,
    };
    explain_app_management(&mut result);
    result
}

// Adopt an existing (manually-installed) app into Homebrew, so it updates from Freshet.
#[tauri::command]
pub(crate) async fn adopt_cask(app: tauri::AppHandle, token: String) -> UpdateResult {
    adopt_casks(app, vec![token])
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| UpdateResult {
            kind: Kind::Cask,
            name: "adopt".into(),
            success: false,
            output: "adoption task failed".into(),
            action: None,
            elapsed_ms: None,
            skipped: false,
            steps: Vec::new(),
            blocked: false,
        })
}

// Adopt several apps in one batch, returning a result per token. Cancelling stops the one in
// progress (safely, see interrupt_running_child) and skips the rest.
#[tauri::command]
pub(crate) async fn adopt_casks(app: tauri::AppHandle, tokens: Vec<String>) -> Vec<UpdateResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(_busy) = begin_operation() else {
            return tokens
                .iter()
                .map(|t| UpdateResult {
                    kind: Kind::Cask,
                    name: t.clone(),
                    success: false,
                    output: BUSY_MESSAGE.into(),
                    action: None,
                    elapsed_ms: None,
                    skipped: true,
                    steps: Vec::new(),
                    blocked: false,
                })
                .collect();
        };
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        let emit = |p: AdoptProgress| {
            let _ = app.emit("adopt-progress", p);
        };
        tokens
            .iter()
            .map(|t| {
                if STOP_REQUESTED.load(Ordering::SeqCst) {
                    UpdateResult {
                        kind: Kind::Cask,
                        name: t.clone(),
                        success: false,
                        output: "Not started: you cancelled the adoption.".into(),
                        action: None,
                        elapsed_ms: None,
                        skipped: true,
                        steps: Vec::new(),
                        blocked: false,
                    }
                } else {
                    adopt_one(t, &emit)
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

// Cancel a running adoption now (not at a package boundary, as Stop does for updates: an
// adoption is one package, and until brew starts adopting, all it has done is download).
#[tauri::command]
pub(crate) fn cancel_adoption() {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
    interrupt_running_child();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    use crate::process::elapsed_ms;
    use std::collections::HashMap;

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
    fn adopt_args_uses_adopt_flag() {
        assert_eq!(
            adopt_args("google-chrome"),
            vec!["install", "--cask", "google-chrome", "--adopt"]
        );
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
    fn adopt_phases_from_brew_output() {
        assert_eq!(
            adopt_phase("==> Downloading https://web.whatsapp.com/desktop/..."),
            Some("Downloading")
        );
        assert_eq!(
            adopt_phase("==> Fetching downloads for: whatsapp"),
            Some("Downloading")
        );
        assert_eq!(
            adopt_phase("==> Adopting existing App at '/Applications/WhatsApp.app'"),
            Some("Adopting")
        );
        assert_eq!(
            adopt_phase("==> Installing Cask whatsapp"),
            Some("Adopting")
        );
        assert_eq!(
            adopt_phase("🍺  whatsapp was successfully installed!"),
            None
        );
    }

    #[test]
    fn stall_detection() {
        assert!(
            !download_stalled(0, 600),
            "nothing downloaded yet: still connecting, not stalled"
        );
        assert!(!download_stalled(155_000_000, 59));
        assert!(download_stalled(155_000_000, 60));
    }

    // The newest partial download that started during this adoption is the one measured; older
    // leftovers in the cache are ignored.
    #[test]
    fn download_size_comes_from_this_adoptions_partial_file() {
        let dir = std::env::temp_dir().join(format!("freshet-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old--Other-1.0.zip.incomplete"), vec![0; 4096]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let since = std::time::SystemTime::now();
        assert_eq!(
            current_download_bytes(&dir, since),
            0,
            "old partial file is ignored"
        );
        std::fs::write(dir.join("abc--WhatsApp-2.26.zip.incomplete"), vec![0; 2048]).unwrap();
        std::fs::write(dir.join("done--Thing.zip"), vec![0; 9999]).unwrap();
        assert_eq!(
            current_download_bytes(&dir, since - std::time::Duration::from_millis(500)),
            2048
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
