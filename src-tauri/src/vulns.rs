// Known-vulnerability lookup for npm global updates, via OSV.dev (free, no API key). Each package
// is queried at its installed *and* latest version, so the UI can say "this update fixes N known
// vulnerabilities" rather than just "N exist". Opt-in (Settings → check_vulnerabilities) because
// it sends package names and versions to osv.dev; enforced here, not only in the UI.
//
// Homebrew isn't covered: OSV has no Homebrew ecosystem, and mapping formulae to upstream
// projects would mean maintaining our own database.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::process::{Command, Stdio};

const OSV_BATCH_URL: &str = "https://api.osv.dev/v1/querybatch";

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct VulnQuery {
    pub(crate) name: String,
    pub(crate) current: String,
    pub(crate) latest: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct VulnReport {
    // OSV ids affecting the installed version.
    pub(crate) current: Vec<String>,
    // Of those, the ones the latest version no longer has: what updating fixes.
    pub(crate) fixed: Vec<String>,
}

fn is_valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

// Two queries per package (installed, then latest), in input order. Pure → unit-tested.
pub(crate) fn batch_body(queries: &[VulnQuery]) -> String {
    let qs: Vec<serde_json::Value> = queries
        .iter()
        .flat_map(|q| {
            [&q.current, &q.latest].map(|version| {
                serde_json::json!({
                    "package": { "name": q.name, "ecosystem": "npm" },
                    "version": version,
                })
            })
        })
        .collect();
    serde_json::json!({ "queries": qs }).to_string()
}

// OSV answers `{"results":[{"vulns":[{"id":…}]}, {}, …]}` in query order; an entry with no
// vulnerabilities is `{}`. Pure → unit-tested.
pub(crate) fn parse_batch(json: &str) -> Option<Vec<Vec<String>>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(
        v["results"]
            .as_array()?
            .iter()
            .map(|r| {
                r["vulns"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|x| x["id"].as_str().map(String::from))
                    .collect()
            })
            .collect(),
    )
}

pub(crate) fn report(current: &[String], latest: &[String]) -> VulnReport {
    let still: HashSet<&String> = latest.iter().collect();
    let mut current = current.to_vec();
    current.sort();
    VulnReport {
        fixed: current
            .iter()
            .filter(|id| !still.contains(id))
            .cloned()
            .collect(),
        current,
    }
}

fn post_json(url: &str, body: &str) -> Result<String, String> {
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "-sS",
            "--fail",
            "--proto",
            "=https",
            "--max-time",
            "15",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("curl: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "osv.dev request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// name → report, for packages with at least one known vulnerability in the installed version.
#[tauri::command]
pub(crate) async fn check_vulnerabilities(
    packages: Vec<VulnQuery>,
) -> Result<HashMap<String, VulnReport>, String> {
    if !crate::load_settings()
        .unwrap_or_default()
        .check_vulnerabilities
    {
        return Err("vulnerability checks are turned off in Settings".into());
    }
    let queries: Vec<VulnQuery> = packages
        .into_iter()
        .filter(|q| {
            crate::is_valid_npm_package(&q.name)
                && is_valid_version(&q.current)
                && is_valid_version(&q.latest)
        })
        .take(500) // OSV's batch limit is 1000 queries; we send two per package.
        .collect();
    if queries.is_empty() {
        return Ok(HashMap::new());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let json = post_json(OSV_BATCH_URL, &batch_body(&queries))?;
        let results = parse_batch(&json).ok_or("unexpected response from osv.dev")?;
        tracing::info!(
            packages = queries.len(),
            elapsed_ms = crate::elapsed_ms(started),
            "osv lookup"
        );
        Ok(queries
            .iter()
            .enumerate()
            .filter_map(|(i, q)| {
                let current = results.get(2 * i)?;
                let latest = results.get(2 * i + 1).map_or(&[][..], Vec::as_slice);
                (!current.is_empty()).then(|| (q.name.clone(), report(current, latest)))
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn body_has_installed_then_latest_per_package() {
        let body = batch_body(&[VulnQuery {
            name: "lodash".into(),
            current: "4.17.15".into(),
            latest: "4.17.21".into(),
        }]);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let qs = v["queries"].as_array().unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0]["package"]["ecosystem"], "npm");
        assert_eq!(qs[0]["version"], "4.17.15");
        assert_eq!(qs[1]["version"], "4.17.21");
    }

    #[test]
    fn parses_batch_results_including_empty_entries() {
        let json = r#"{"results":[{"vulns":[{"id":"GHSA-p6mc-m468-83gw","modified":"2024-01-01T00:00:00Z"},{"id":"GHSA-35jh-r3h4-6jhm","modified":"x"}]},{}]}"#;
        assert_eq!(
            parse_batch(json).unwrap(),
            vec![
                vec![
                    "GHSA-p6mc-m468-83gw".to_string(),
                    "GHSA-35jh-r3h4-6jhm".to_string()
                ],
                vec![]
            ]
        );
        assert!(parse_batch("nope").is_none());
    }

    #[test]
    fn report_lists_what_the_update_fixes() {
        let r = report(&["B".into(), "A".into()], &["B".into()]);
        assert_eq!(r.current, ["A", "B"]);
        assert_eq!(r.fixed, ["A"]);
    }

    #[test]
    fn version_validation() {
        assert!(is_valid_version("6.0.0-beta.1+build"));
        assert!(!is_valid_version("1.0; rm -rf"));
        assert!(!is_valid_version(""));
    }

    // Live call to osv.dev. lodash 4.17.15 has well-known advisories fixed by 4.17.21.
    //   cargo test -- --ignored osv_live
    #[test]
    #[ignore = "network: queries api.osv.dev"]
    fn osv_live_lookup() {
        let q = [VulnQuery {
            name: "lodash".into(),
            current: "4.17.15".into(),
            latest: "4.17.21".into(),
        }];
        let json = post_json(OSV_BATCH_URL, &batch_body(&q)).unwrap();
        let results = parse_batch(&json).unwrap();
        let r = report(&results[0], &results[1]);
        eprintln!("{r:?}");
        assert!(!r.current.is_empty() && !r.fixed.is_empty());
    }
}
