// What a package is: descriptions, homepages and release notes for the update list.

use crate::apps::{bundle_version, installed_cask_apps, versions_agree};
use crate::process::run_cmd;
use crate::validate::{is_safe_url, is_valid_cask_token};
use serde::Serialize;
use std::collections::HashMap;
use std::process::Command;

#[derive(Serialize)]
pub(crate) struct CaskInfo {
    pub(crate) token: String,
    pub(crate) name: String,
    pub(crate) desc: String,
    pub(crate) homepage: String,
    pub(crate) version: String,
    pub(crate) auto_updates: bool,
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
pub(crate) async fn cask_info(token: String) -> Option<CaskInfo> {
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

// --- Package details for the update list: what is this thing, and where are its release notes ---

#[derive(Serialize, Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct PackageInfo {
    pub(crate) desc: String,
    pub(crate) homepage: String,
    pub(crate) license: String,
    // Formula installed only to satisfy another formula — not something the user asked for.
    pub(crate) dependency: bool,
    // Cask that updates itself (Homebrew's `auto_updates`).
    pub(crate) auto_updates: bool,
    // GitHub releases page when the project lives on GitHub; empty otherwise.
    pub(crate) release_notes: String,
    // The cask's .app bundle name, used to look up its icon.
    pub(crate) app: String,
    // The version the installed app reports, set only when it disagrees with Homebrew's record
    // (e.g. after a rollback). The list shows this as the real "from" version.
    pub(crate) on_disk_version: String,
}

// "https://github.com/owner/repo(.git)(/anything)" → "https://github.com/owner/repo/releases".
// First GitHub URL wins. Pure → unit-tested.
fn github_releases_url<'a>(urls: impl IntoIterator<Item = &'a str>) -> Option<String> {
    urls.into_iter().find_map(|u| {
        let rest = u
            .strip_prefix("https://github.com/")
            .or_else(|| u.strip_prefix("http://github.com/"))?;
        let mut parts = rest.split('/');
        let owner = parts.next().filter(|s| !s.is_empty())?;
        let repo = parts.next()?.trim_end_matches(".git");
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        (valid(owner) && valid(repo)).then(|| format!("https://github.com/{owner}/{repo}/releases"))
    })
}

fn json_str(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

// `brew info --json=v2 --formula …` → name → info. Pure → unit-tested against real output.
fn parse_formula_infos(json: &str) -> HashMap<String, PackageInfo> {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    parsed["formulae"]
        .as_array()
        .map_or_default(Vec::as_slice)
        .iter()
        .map(|f| {
            let homepage = json_str(&f["homepage"]);
            let release_notes = github_releases_url([
                homepage.as_str(),
                f["urls"]["head"]["url"].as_str().unwrap_or(""),
                f["urls"]["stable"]["url"].as_str().unwrap_or(""),
            ])
            .unwrap_or_default();
            let info = PackageInfo {
                desc: json_str(&f["desc"]),
                license: json_str(&f["license"]),
                dependency: f["installed"][0]["installed_on_request"] == serde_json::json!(false),
                release_notes,
                homepage,
                ..PackageInfo::default()
            };
            (json_str(&f["name"]), info)
        })
        .collect()
}

// `brew info --json=v2 --cask …` → token → info. Pure → unit-tested against real output.
fn parse_cask_infos(json: &str) -> HashMap<String, PackageInfo> {
    let parsed: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    parsed["casks"]
        .as_array()
        .map_or_default(Vec::as_slice)
        .iter()
        .map(|c| {
            let homepage = json_str(&c["homepage"]);
            let app = c["artifacts"]
                .as_array()
                .and_then(|arts| arts.iter().find_map(|a| a["app"][0].as_str()))
                .unwrap_or("")
                .to_string();
            let info = PackageInfo {
                desc: json_str(&c["desc"]),
                auto_updates: c["auto_updates"].as_bool().unwrap_or(false),
                release_notes: github_releases_url([
                    homepage.as_str(),
                    c["url"].as_str().unwrap_or(""),
                ])
                .unwrap_or_default(),
                homepage,
                app,
                ..PackageInfo::default()
            };
            (json_str(&c["token"]), info)
        })
        .collect()
}

// One `brew info` per kind for the whole list (~0.4s each, run in parallel), keyed
// "brew::<name>" / "cask::<token>" to match the frontend's selKey.
#[tauri::command]
pub(crate) async fn package_details(
    formulae: Vec<String>,
    casks: Vec<String>,
    // token → the version Homebrew records as installed (the list's "from"), to compare with
    // what the app on disk reports.
    installed: Option<HashMap<String, String>>,
) -> HashMap<String, PackageInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let info = |flag: &'static str, ids: &[String]| -> Option<String> {
            let ids: Vec<&str> = ids
                .iter()
                .map(String::as_str)
                .filter(|id| is_valid_cask_token(id))
                .collect();
            if ids.is_empty() {
                return None;
            }
            let mut args = vec!["info", "--json=v2", flag];
            args.extend(ids);
            run_cmd("brew", &args).ok()
        };
        let (formula_json, cask_json) = std::thread::scope(|scope| {
            let f = scope.spawn(|| info("--formula", &formulae));
            let c = info("--cask", &casks);
            (f.join().ok().flatten(), c)
        });
        let formula = formula_json.map_or_default(|j| parse_formula_infos(&j));
        let mut cask = cask_json.map_or_default(|j| parse_cask_infos(&j));
        let installed = installed.unwrap_or_default();
        for (token, info) in &mut cask {
            let (Some(recorded), false) = (installed.get(token), info.app.is_empty()) else {
                continue;
            };
            let on_disk = installed_cask_apps(std::slice::from_ref(&info.app))
                .first()
                .and_then(|app| bundle_version(app));
            if let Some(v) = on_disk.filter(|v| !versions_agree(recorded, v)) {
                info.on_disk_version = v;
            }
        }
        formula
            .into_iter()
            .map(|(k, v)| (format!("brew::{k}"), v))
            .chain(cask.into_iter().map(|(k, v)| (format!("cask::{k}"), v)))
            .collect()
    })
    .await
    .unwrap_or_default()
}

// Open an http(s) URL in the default browser (custom command, so no extra capability needed).
#[tauri::command(async)]
pub(crate) fn open_url(url: String) {
    if is_safe_url(&url) {
        let _ = Command::new("/usr/bin/open").arg(url).status();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    #[test]
    fn github_releases_url_from_homepage_or_repo() {
        assert_eq!(
            github_releases_url(["https://github.com/awslabs/aws-c-auth"]).as_deref(),
            Some("https://github.com/awslabs/aws-c-auth/releases")
        );
        // Falls through non-GitHub URLs to the source repo; strips .git and extra path.
        assert_eq!(
            github_releases_url(["https://git-scm.com", "https://github.com/git/git.git"])
                .as_deref(),
            Some("https://github.com/git/git/releases")
        );
        assert_eq!(
            github_releases_url(["https://github.com/o/r/archive/v1.tar.gz"]).as_deref(),
            Some("https://github.com/o/r/releases")
        );
        assert_eq!(github_releases_url(["https://abseil.io", ""]), None);
        assert_eq!(github_releases_url(["https://github.com/onlyowner"]), None);
        assert_eq!(
            github_releases_url(["https://github.com/o/r?x=<script>"]),
            None
        );
    }

    // Trimmed from real `brew info --json=v2 --formula abseil git` output.
    #[test]
    fn formula_infos_parse_real_output() {
        let json = r#"{"formulae":[
          {"name":"abseil","desc":"C++ Common Libraries","homepage":"https://abseil.io",
           "license":"Apache-2.0",
           "urls":{"stable":{"url":"https://github.com/abseil/abseil-cpp/archive/refs/tags/20250814.1.tar.gz"},
                   "head":{"url":"https://github.com/abseil/abseil-cpp.git","branch":"master"}},
           "installed":[{"version":"20250512.1","installed_on_request":false}]},
          {"name":"git","desc":"Distributed revision control system","homepage":"https://git-scm.com",
           "license":"GPL-2.0-only","urls":{"stable":{"url":"https://mirrors.edge.kernel.org/pub/software/scm/git/git-2.56.0.tar.xz"},
           "head":{"url":"https://github.com/git/git.git"}},
           "installed":[{"version":"2.49.0","installed_on_request":true}]}],"casks":[]}"#;
        let infos = parse_formula_infos(json);
        let abseil = &infos["abseil"];
        assert_eq!(abseil.desc, "C++ Common Libraries");
        assert!(abseil.dependency);
        assert_eq!(
            abseil.release_notes,
            "https://github.com/abseil/abseil-cpp/releases"
        );
        let git = &infos["git"];
        assert!(!git.dependency);
        assert_eq!(git.homepage, "https://git-scm.com");
        assert_eq!(git.license, "GPL-2.0-only");
        assert_eq!(git.release_notes, "https://github.com/git/git/releases");
        assert!(parse_formula_infos("not json").is_empty());
    }

    // Trimmed from real `brew info --json=v2 --cask slack` output.
    #[test]
    fn cask_infos_parse_real_output() {
        let json = r#"{"formulae":[],"casks":[{"token":"slack","desc":"Team communication and collaboration software",
          "homepage":"https://slack.com/","url":"https://downloads.slack-edge.com/desktop-releases/macos/arm64/4.46.101/Slack-4.46.101-macOS.dmg",
          "artifacts":[{"uninstall":[{"quit":"com.tinyspeck.slackmacgap"}]},{"app":["Slack.app"]},{"zap":[]}],
          "auto_updates":true}]}"#;
        let slack = &parse_cask_infos(json)["slack"];
        assert_eq!(slack.desc, "Team communication and collaboration software");
        assert_eq!(slack.app, "Slack.app");
        assert!(slack.auto_updates);
        assert!(!slack.dependency);
        assert_eq!(slack.release_notes, "");
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
    fn cask_info_tolerates_missing_fields() {
        let info = parse_cask_info(r#"{"casks":[{"token":"x"}]}"#).unwrap();
        assert_eq!(info.token, "x");
        assert_eq!(info.name, "");
        assert!(!info.auto_updates);
        assert!(parse_cask_info(r#"{"casks":[]}"#).is_none());
    }
}
