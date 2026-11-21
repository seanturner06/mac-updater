// Finding what is outdated: Homebrew formulae and casks, the App Store and npm globals; the
// hourly `brew update`; and the cached last check.

use crate::model::{CategoryStatus, Kind, OutdatedItem, UpdateResult};
use crate::process::{elapsed_ms, run_cmd, run_update_cmd};
use crate::scheduled::get_last_scheduled_run;
use crate::store::{freshet_dir, unix_now, write_private, write_private_atomic};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

// --- Check functions (thin I/O wrappers around the pure parsers above) ---

// Formulae and casks from two `brew outdated` processes (each pays ~0.5s of Ruby start-up), run
// in parallel. The --greedy call lists formulae plus every outdated cask, self-updating ones
// included; the plain cask call tells us which of those are *not* self-updating. Casks are tagged
// self_updating so the frontend can hide them without a re-fetch.
#[tracing::instrument(skip_all)]
pub(crate) fn check_homebrew() -> (CategoryStatus, CategoryStatus) {
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
            let normal = normal.map_or_default(|n| parse_cask_names(&n));
            (
                CategoryStatus {
                    category: "Homebrew".into(),
                    kind: Kind::Brew,
                    items: parse_brew_outdated(&stdout),
                    error: None,
                },
                CategoryStatus {
                    category: "Applications".into(),
                    kind: Kind::Cask,
                    items: parse_casks(&stdout, &normal),
                    error: None,
                },
            )
        }
        Err(e) => (
            failed_category_err("Homebrew", Kind::Brew, e.clone()),
            failed_category_err("Applications", Kind::Cask, e),
        ),
    }
}

fn check_mas() -> CategoryStatus {
    match run_cmd("mas", &["outdated"]) {
        Ok(stdout) => CategoryStatus {
            category: "App Store".into(),
            kind: Kind::Mas,
            items: parse_mas_outdated(&stdout),
            error: None,
        },
        Err(e) => failed_category_err("App Store", Kind::Mas, e),
    }
}

fn check_npm() -> CategoryStatus {
    // npm outdated -g returns JSON of outdated globals; exits 1 when there are any
    match run_cmd("npm", &["outdated", "-g", "--json"]) {
        Ok(stdout) => CategoryStatus {
            category: "npm globals".into(),
            kind: Kind::Npm,
            items: parse_npm_outdated(&stdout),
            error: None,
        },
        Err(e) => failed_category_err("npm globals", Kind::Npm, e),
    }
}

fn failed_category_err(category: &str, kind: Kind, error: String) -> CategoryStatus {
    CategoryStatus {
        category: category.into(),
        kind,
        items: vec![],
        error: Some(error),
    }
}

pub(crate) fn failed_category(category: &str, kind: Kind) -> CategoryStatus {
    CategoryStatus {
        category: category.into(),
        kind,
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
pub(crate) async fn check_outdated() -> Vec<CategoryStatus> {
    tauri::async_runtime::spawn_blocking(check_outdated_blocking)
        .await
        .unwrap_or_default()
}

#[tracing::instrument]
fn check_outdated_blocking() -> Vec<CategoryStatus> {
    let ((brew, casks), mas, npm) = std::thread::scope(|scope| {
        let mas = scope.spawn(check_mas);
        let npm = scope.spawn(check_npm);
        // Without a recent `brew update`, `brew outdated` compares against stale metadata and
        // misses new releases. Throttled to once an hour, so most checks pay nothing for it; when
        // it does run (often 5-30s), only the Homebrew check waits on it, not mas and npm.
        if let Some(r) = brew_update_if_stale() {
            if !r.success {
                tracing::warn!(output = %r.output, "brew update failed; checking against cached metadata");
            }
        }
        let homebrew = check_homebrew();
        (
            homebrew,
            mas.join()
                .unwrap_or_else(|_| failed_category("App Store", Kind::Mas)),
            npm.join()
                .unwrap_or_else(|_| failed_category("npm globals", Kind::Npm)),
        )
    });
    let categories = vec![brew, casks, mas, npm];
    save_cached_check(&categories);
    categories
}

// --- Last check, cached so the window can show it instantly while a fresh check runs ---

#[derive(Serialize, Deserialize)]
pub(crate) struct CachedCheck {
    // Unix seconds.
    pub(crate) checked_at: u64,
    pub(crate) categories: Vec<CategoryStatus>,
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
pub(crate) fn get_cached_check() -> Option<CachedCheck> {
    let text = std::fs::read_to_string(cached_check_path()?).ok()?;
    let cached: CachedCheck = serde_json::from_str(&text).ok()?;
    let last_run = get_last_scheduled_run().map(|r| r.finished_at);
    cache_is_current(cached.checked_at, last_run).then_some(cached)
}

// --- `brew update`, at most once an hour ---

const BREW_UPDATE_MAX_AGE_SECS: u64 = 3600;

fn brew_update_stamp_path() -> Option<PathBuf> {
    freshet_dir().map(|d| d.join("brew-update.stamp"))
}

// Stale if never run, older than max_age, or stamped in the future (clock change). Pure → tested.
pub(crate) fn update_is_stale(last: Option<u64>, now: u64, max_age: u64) -> bool {
    last.is_none_or(|l| l > now || now - l >= max_age)
}

// Runs `brew update` if the last successful one is over an hour old; None if skipped.
pub(crate) fn brew_update_if_stale() -> Option<UpdateResult> {
    brew_update_if_stale_with(
        brew_update_stamp_path(),
        &mut || run_update_cmd("brew", &["update"]),
        &mut std::thread::sleep,
    )
}

// One `brew update` at a time within Freshet: a check, an update and the scheduled run can all
// ask for one at once (and React runs startup effects twice in dev). The second caller waits
// here, then finds the stamp fresh and skips — instead of hitting Homebrew's lock and reporting
// a failure.
static BREW_UPDATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// How long to wait for someone else's `brew update` (a terminal, another Freshet) to finish.
const BREW_UPDATE_WAIT: std::time::Duration = std::time::Duration::from_mins(1);

const BREW_UPDATE_RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

// brew_update_if_stale with the stamp file, the command and sleeping passed in, for tests.
fn brew_update_if_stale_with(
    stamp: Option<PathBuf>,
    run: &mut dyn FnMut() -> (bool, String),
    sleep: &mut dyn FnMut(std::time::Duration),
) -> Option<UpdateResult> {
    let _one_at_a_time = BREW_UPDATE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Read only once holding the lock, so a caller that waited sees the update it waited for.
    let last = stamp
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse().ok());
    if !update_is_stale(last, unix_now(), BREW_UPDATE_MAX_AGE_SECS) {
        tracing::info!("brew update skipped: ran within the last hour");
        return None;
    }
    let started = std::time::Instant::now();
    let Some((success, output)) =
        update_waiting_for_lock(run, sleep, BREW_UPDATE_WAIT, BREW_UPDATE_RETRY_EVERY)
    else {
        // Someone else is refreshing the same metadata: not a failure, and nothing to report.
        tracing::info!("brew update skipped: another `brew update` kept the lock");
        return None;
    };
    let elapsed_ms = elapsed_ms(started);
    tracing::info!(elapsed_ms, success, "brew update");
    if success {
        if let Some(p) = stamp {
            let _ = write_private(&p, &unix_now().to_string(), false);
        }
    }
    Some(UpdateResult {
        kind: Kind::Brew,
        name: "brew update".into(),
        success,
        output,
        action: None,
        elapsed_ms: Some(elapsed_ms),
        skipped: false,
        steps: Vec::new(),
        blocked: false,
    })
}

// Homebrew's refusal when another `brew update` holds its lock (cmd/update.sh). Pure.
fn brew_update_locked(output: &str) -> bool {
    output.contains("Another `brew update` process is already running")
}

// Run `brew update`, and while another process holds Homebrew's lock, retry every `every` for up
// to `max_wait` (brew refuses immediately, so each retry is cheap). None if it was still locked
// at the end. Other failures return at once. Pure apart from `run` and `sleep`.
fn update_waiting_for_lock(
    run: &mut dyn FnMut() -> (bool, String),
    sleep: &mut dyn FnMut(std::time::Duration),
    max_wait: std::time::Duration,
    every: std::time::Duration,
) -> Option<(bool, String)> {
    let mut waited = std::time::Duration::ZERO;
    loop {
        let (ok, output) = run();
        if ok || !brew_update_locked(&output) {
            return Some((ok, output));
        }
        if waited >= max_wait {
            return None;
        }
        sleep(every);
        waited += every;
    }
}

// Names of everything (formulae + casks) in `brew outdated --json=v2 --greedy` output.
pub(crate) fn outdated_names(json: &str) -> std::collections::HashSet<String> {
    parse_brew_outdated(json)
        .into_iter()
        .map(|i| i.id)
        .chain(parse_cask_names(json))
        .collect()
}

// (kind, id) → (installed, latest), from a check's categories.
pub(crate) type Versions = HashMap<(Kind, String), (String, String)>;

pub(crate) fn versions_of(categories: &[CategoryStatus]) -> Versions {
    categories
        .iter()
        .flat_map(|c| {
            c.items.iter().map(|i| {
                (
                    (c.kind, i.id.clone()),
                    (i.current.clone(), i.latest.clone()),
                )
            })
        })
        .collect()
}

// The versions the user saw when they pressed Update — from the last check, even if a scheduled
// run has run since (so not get_cached_check, which drops a stale list).
pub(crate) fn versions_last_shown() -> Versions {
    cached_check_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<CachedCheck>(&t).ok())
        .map_or_default(|c| versions_of(&c.categories))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::model::{Kind, OutdatedItem};
    use crate::store::unix_now;

    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    // Story 2 (manual check resilience): when one source fails, it returns a category with the
    // error attached and an empty items list — the aggregator doesn't lose the other sources.
    #[test]
    fn failed_category_err_carries_error_and_empty_items() {
        let s = failed_category_err("Homebrew", Kind::Brew, "brew not found".into());
        assert_eq!(s.category, "Homebrew");
        assert_eq!(s.kind, Kind::Brew);
        assert_eq!(s.items, Vec::<OutdatedItem>::new());
        assert_eq!(s.error.as_deref(), Some("brew not found"));
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

    // ---------- one `brew update` at a time ----------

    // Homebrew's exact output when another `brew update` holds its lock (seen in `tauri dev`).
    const BREW_LOCKED: &str =
        "lockf: 200: already locked\nError: Another `brew update` process is \
                               already running.\nPlease wait for it to finish or terminate it to \
                               continue.";

    #[test]
    fn recognises_homebrews_update_lock() {
        assert!(brew_update_locked(BREW_LOCKED));
        assert!(!brew_update_locked("Already up-to-date."));
        assert!(!brew_update_locked(
            "fatal: unable to access 'https://github.com/Homebrew/brew/'"
        ));
    }

    // A fake `brew update`: plays back `outcomes` in order and counts calls and sleeps.
    fn scripted(outcomes: &[(bool, &str)]) -> (impl FnMut() -> (bool, String), Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        let queue: Vec<(bool, String)> = outcomes
            .iter()
            .map(|(ok, o)| (*ok, (*o).to_string()))
            .collect();
        let counter = Rc::clone(&calls);
        let run = move || {
            let i = counter.get();
            counter.set(i + 1);
            queue[i.min(queue.len() - 1)].clone()
        };
        (run, calls)
    }

    const SEC: std::time::Duration = std::time::Duration::from_secs(1);

    #[test]
    fn waits_out_another_brew_update_then_runs() {
        let (mut run, calls) = scripted(&[
            (false, BREW_LOCKED),
            (false, BREW_LOCKED),
            (true, "Updated 2 taps."),
        ]);
        let mut slept = Vec::new();
        let got = update_waiting_for_lock(&mut run, &mut |d| slept.push(d), 60 * SEC, 2 * SEC);
        assert_eq!(got, Some((true, "Updated 2 taps.".to_string())));
        assert_eq!(calls.get(), 3);
        assert_eq!(slept, [2 * SEC, 2 * SEC]);
    }

    #[test]
    fn gives_up_quietly_when_the_lock_outlasts_the_wait() {
        let (mut run, calls) = scripted(&[(false, BREW_LOCKED)]);
        let mut slept = std::time::Duration::ZERO;
        let got = update_waiting_for_lock(&mut run, &mut |d| slept += d, 10 * SEC, 2 * SEC);
        assert_eq!(got, None, "still locked: not reported as a failure");
        assert_eq!(slept, 10 * SEC, "waited the full time, no longer");
        assert_eq!(calls.get(), 6, "the first try plus one every 2s for 10s");
    }

    #[test]
    fn real_failures_and_successes_return_at_once() {
        for outcome in [
            (false, "fatal: unable to access github.com"),
            (true, "Already up-to-date."),
        ] {
            let (mut run, calls) = scripted(&[outcome]);
            let mut slept = 0;
            let got = update_waiting_for_lock(&mut run, &mut |_| slept += 1, 60 * SEC, 2 * SEC);
            assert_eq!(got, Some((outcome.0, outcome.1.to_string())));
            assert_eq!((calls.get(), slept), (1, 0), "no retry for {outcome:?}");
        }
    }

    fn temp_stamp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freshet-stamp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("brew-update.stamp")
    }

    #[test]
    fn stale_stamp_runs_and_records_success() {
        let stamp = temp_stamp("stale");
        let (mut run, calls) = scripted(&[(true, "Updated 1 tap.")]);
        let r = brew_update_if_stale_with(Some(stamp.clone()), &mut run, &mut |_| {}).unwrap();
        assert!(r.success);
        assert_eq!(calls.get(), 1);
        let written: u64 = std::fs::read_to_string(&stamp)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(unix_now() - written < 5);
        // Within the hour: skipped without running brew.
        let (mut run, calls) = scripted(&[(true, "")]);
        assert!(brew_update_if_stale_with(Some(stamp.clone()), &mut run, &mut |_| {}).is_none());
        assert_eq!(calls.get(), 0);
        let _ = std::fs::remove_dir_all(stamp.parent().unwrap());
    }

    #[test]
    fn locked_out_or_failed_leaves_the_stamp_alone() {
        let stamp = temp_stamp("locked");
        let (mut run, _) = scripted(&[(false, BREW_LOCKED)]);
        assert!(
            brew_update_if_stale_with(Some(stamp.clone()), &mut run, &mut |_| {}).is_none(),
            "someone else's update is not our failure"
        );
        assert!(!stamp.exists(), "but it isn't our success either");
        let (mut run, _) = scripted(&[(false, "fatal: unable to access github.com")]);
        let r = brew_update_if_stale_with(Some(stamp.clone()), &mut run, &mut |_| {}).unwrap();
        assert!(!r.success && r.output.contains("unable to access"));
        assert!(!stamp.exists());
        let _ = std::fs::remove_dir_all(stamp.parent().unwrap());
    }

    // Two callers at once (a check and an update, or React's doubled dev startup): brew runs once,
    // the second waits for it and skips instead of hitting Homebrew's lock.
    #[test]
    fn concurrent_callers_run_brew_update_once() {
        let stamp = temp_stamp("concurrent");
        let runs = std::sync::Arc::new(AtomicUsize::new(0));
        let spawn = || {
            let (stamp, runs) = (stamp.clone(), std::sync::Arc::clone(&runs));
            std::thread::spawn(move || {
                let mut run = || {
                    runs.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    (true, "Updated 1 tap.".to_string())
                };
                brew_update_if_stale_with(Some(stamp), &mut run, &mut |_| {})
            })
        };
        // Both started before either is joined, so they really overlap.
        let callers = [spawn(), spawn()];
        let results: Vec<_> = callers.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(runs.load(Ordering::SeqCst), 1, "brew update ran once");
        assert_eq!(results.iter().filter(|r| r.is_some()).count(), 1);
        assert!(results.iter().flatten().all(|r| r.success));
        let _ = std::fs::remove_dir_all(stamp.parent().unwrap());
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

    #[test]
    fn outdated_names_cover_formulae_and_casks() {
        let json = r#"{"formulae":[{"name":"git","installed_versions":["2.55.0"],"current_version":"2.56.0"}],"casks":[{"name":"discord","installed_versions":["0.0.407"],"current_version":"0.0.414"}]}"#;
        let names = outdated_names(json);
        assert!(names.contains("git") && names.contains("discord") && names.len() == 2);
    }
}
