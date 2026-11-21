// Apps that update themselves through a Sparkle appcast: detection only.

use crate::apps::{cask_apps, list_applications, read_plist_string};
use crate::check::failed_category;
use crate::model::{CategoryStatus, Kind, OutdatedItem};
use crate::validate::{is_safe_feed_url, is_safe_url};
use std::collections::HashSet;
use std::process::Command;

// Lenient dotted-numeric version compare — Sparkle shortVersionStrings (e.g. "2.9", "32.1.2")
// aren't always valid 3-part semver, so we compare numeric components directly.
fn version_is_older(installed: &str, latest: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        s.split(['.', '-', '+'])
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect()
    }
    let a = parts(installed);
    let b = parts(latest);
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x < y;
        }
    }
    false
}

// One release in an appcast, with the fields that decide whether this Mac would be offered it.
#[derive(Debug, Default, PartialEq, Eq)]
struct AppcastItem {
    version: String,
    channel: Option<String>,
    min_os: Option<String>,
    max_os: Option<String>,
    notes: Option<String>,
}

// The text of `<tag>…</tag>`, if present and non-empty.
fn element<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let after = &xml[xml.find(&open)? + open.len()..];
    let v = after[..after.find('<')?].trim();
    (!v.is_empty()).then_some(v)
}

// The value of `name="…"` (Sparkle puts some fields on the <enclosure> as attributes).
fn attribute<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("{name}=\"");
    let after = &xml[xml.find(&open)? + open.len()..];
    let v = after[..after.find('"')?].trim();
    (!v.is_empty()).then_some(v)
}

fn field<'a>(item: &'a str, name: &str) -> Option<&'a str> {
    element(item, name).or_else(|| attribute(item, name))
}

// Each <item> that names a display version. Items without one are skipped: comparing a build
// number against CFBundleShortVersionString would be apples to oranges.
fn parse_appcast_items(xml: &str) -> Vec<AppcastItem> {
    let mut items = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<item") {
        let after = &rest[i..];
        let end = after.find("</item>").unwrap_or(after.len());
        let item = &after[..end];
        if let Some(version) = field(item, "sparkle:shortVersionString") {
            items.push(AppcastItem {
                version: version.to_string(),
                channel: element(item, "sparkle:channel").map(str::to_lowercase),
                min_os: field(item, "sparkle:minimumSystemVersion").map(String::from),
                max_os: field(item, "sparkle:maximumSystemVersion").map(String::from),
                notes: element(item, "sparkle:releaseNotesLink")
                    .filter(|u| is_safe_url(u))
                    .map(String::from),
            });
        }
        rest = &after[end..];
    }
    items
}

// Channels whose items Sparkle only offers to users who opted in. An item with no channel, or a
// release-like one ("stable", which OBS uses), is for everyone.
fn is_prerelease_channel(channel: &str) -> bool {
    const WORDS: [&str; 12] = [
        "alpha",
        "beta",
        "rc",
        "nightly",
        "dev",
        "preview",
        "canary",
        "insider",
        "test",
        "experimental",
        "edge",
        "unstable",
    ];
    WORDS.iter().any(|w| channel.contains(w))
}

// Older feeds mark betas in the version itself ("2.0b3", "5.1-beta.2", "33.0.0-rc1").
fn is_prerelease_version(version: &str) -> bool {
    let v = version.to_lowercase();
    if [
        "alpha", "beta", "rc", "pre", "dev", "nightly", "preview", "canary",
    ]
    .iter()
    .any(|w| v.contains(w))
    {
        return true;
    }
    let c: Vec<char> = v.chars().collect();
    c.windows(2).enumerate().any(|(i, w)| {
        w[0].is_ascii_digit()
            && (w[1] == 'a' || w[1] == 'b')
            && c.get(i + 2).is_none_or(char::is_ascii_digit)
    })
}

// The newest release this Mac would actually be offered: no opt-in channels, no betas (unless
// the installed version is itself a beta), and only builds that run on `os` when it's known.
fn newest_offer(xml: &str, installed: &str, os: Option<&str>) -> Option<AppcastItem> {
    let on_beta = is_prerelease_version(installed);
    parse_appcast_items(xml)
        .into_iter()
        .filter(|it| !it.channel.as_deref().is_some_and(is_prerelease_channel))
        .filter(|it| on_beta || !is_prerelease_version(&it.version))
        .filter(|it| os.is_none_or(|os| runs_on(it, os)))
        .reduce(|best, it| {
            if version_is_older(&best.version, &it.version) {
                it
            } else {
                best
            }
        })
}

// Whether the release's declared macOS range includes `os`.
fn runs_on(item: &AppcastItem, os: &str) -> bool {
    !item
        .min_os
        .as_deref()
        .is_some_and(|min| version_is_older(os, min))
        && !item
            .max_os
            .as_deref()
            .is_some_and(|max| version_is_older(max, os))
}

fn macos_version() -> Option<String> {
    read_plist_string(
        "/System/Library/CoreServices/SystemVersion",
        "ProductVersion",
    )
}

// Bundle names (lowercased "Foo.app") that a Homebrew cask owns. Homebrew is the source of truth
// for those — its greedy check already sees self-updating casks — so they're left out here
// rather than listed twice.
fn cask_owned_apps() -> HashSet<String> {
    let Some(caskroom) = crate::process::find_brew()
        .and_then(|brew| Some(brew.parent()?.parent()?.join("Caskroom")))
    else {
        return HashSet::new();
    };
    let tokens: Vec<String> = std::fs::read_dir(caskroom).map_or_default(|rd| {
        rd.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect()
    });
    cask_apps(&tokens)
        .into_values()
        .flatten()
        .map(|a| a.to_lowercase())
        .collect()
}

fn fetch_url(url: &str) -> Option<String> {
    // https-only and no private/loopback host, enforced again at the fetch boundary (not just at
    // the caller): a plaintext feed can be MITM'd, and a redirect to http would silently downgrade.
    if !is_safe_feed_url(url) {
        return None;
    }
    let out = Command::new("/usr/bin/curl")
        // https only, on the initial request AND across every redirect hop — no plaintext fallback,
        // no escape to file:// or other schemes. TLS cert validation stays on (we never pass -k).
        .args([
            "-sL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "5",
            "--max-time",
            "8",
            // Cap the body a hostile feed can make us buffer. Best-effort: curl can only
            // enforce this when the server declares a Content-Length, but appcasts are tiny
            // and a well-behaved CDN always sends one.
            "--max-filesize",
            "5242880",
            url,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

// --- Detect updates for apps that ship a Sparkle appcast (the "Latest" approach) ---
// These apps update themselves, so this is detection-only; the frontend shows them without a
// checkbox. Feeds are fetched in parallel since each is a network round-trip.
// The handles `.collect::<Vec<_>>()` below is intentional: without it, spawn+join interleaves
// and the network fetches run serially instead of in parallel.
#[allow(clippy::needless_collect)]
fn check_sparkle() -> CategoryStatus {
    let owned = cask_owned_apps();
    let os = macos_version();
    let feeds: Vec<(String, String, String)> = list_applications()
        .iter()
        .filter(|path| {
            path.file_name()
                .is_none_or(|f| !owned.contains(&f.to_string_lossy().to_lowercase()))
        })
        .filter_map(|path| {
            let info = format!("{}/Contents/Info", path.to_str()?);
            let feed = read_plist_string(&info, "SUFeedURL")?;
            // Don't follow a hostile app's appcast into private/loopback hosts.
            if !is_safe_feed_url(&feed) {
                return None;
            }
            let current =
                read_plist_string(&info, "CFBundleShortVersionString").unwrap_or_default();
            let name = path.file_stem()?.to_str()?.to_string();
            Some((name, feed, current))
        })
        .collect();

    let items: Vec<OutdatedItem> = std::thread::scope(|s| {
        let os = &os;
        let handles: Vec<_> = feeds
            .iter()
            .map(|(name, feed, current)| {
                s.spawn(move || {
                    let xml = fetch_url(feed)?;
                    let offer = newest_offer(&xml, current, os.as_deref())?;
                    if !current.is_empty() && version_is_older(current, &offer.version) {
                        Some(OutdatedItem {
                            id: name.clone(),
                            name: name.clone(),
                            current: current.clone(),
                            latest: offer.version,
                            self_updating: false,
                            notes_url: offer.notes.unwrap_or_default(),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });

    CategoryStatus {
        category: "Other apps".into(),
        kind: Kind::Sparkle,
        items,
        error: None,
    }
}

// Network-bound Sparkle check, loaded separately/in the background by the frontend.
#[tauri::command]
pub(crate) async fn check_sparkle_apps() -> CategoryStatus {
    tauri::async_runtime::spawn_blocking(check_sparkle)
        .await
        .unwrap_or_else(|_| failed_category("Other apps", Kind::Sparkle))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    #[test]
    fn lenient_version_compare() {
        assert!(version_is_older("30.2.3", "32.1.2"));
        assert!(!version_is_older("32.1.2", "32.1.2"));
        assert!(version_is_older("2.9", "2.10"), "9 < 10 component-wise");
        assert!(!version_is_older("2.10", "2.9"));
        assert!(!version_is_older("33.0", "32.1.2"));
    }

    // Trimmed from OBS's real feed (updates_arm64_v2.xml, 2026-11): a stable channel, plus a beta
    // channel item. The beta here is bumped to 33.1.0 to model a beta period, when it's the newest.
    const OBS: &str = r#"<rss><channel>
        <item>
          <title>33.1.0</title>
          <sparkle:releaseNotesLink>https://obsproject.com/osx_update/notes_beta.html</sparkle:releaseNotesLink>
          <sparkle:channel>beta</sparkle:channel>
          <sparkle:version>41513185113</sparkle:version>
          <sparkle:shortVersionString>33.1.0</sparkle:shortVersionString>
          <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
        </item>
        <item>
          <title>33.0.1</title>
          <sparkle:releaseNotesLink>https://obsproject.com/osx_update/notes_stable.html</sparkle:releaseNotesLink>
          <sparkle:channel>stable</sparkle:channel>
          <sparkle:version>40954849292</sparkle:version>
          <sparkle:shortVersionString>33.0.1</sparkle:shortVersionString>
          <sparkle:minimumSystemVersion>13.0</sparkle:minimumSystemVersion>
        </item>
        <item>
          <title>32.2.2</title>
          <sparkle:channel>stable</sparkle:channel>
          <sparkle:shortVersionString>32.2.2</sparkle:shortVersionString>
          <sparkle:minimumSystemVersion>12.0</sparkle:minimumSystemVersion>
        </item>
    </channel></rss>"#;

    // Trimmed from iMazing's real feed: the version lives on the <enclosure> as an attribute.
    const IMAZING: &str = r#"<rss><channel>
        <item>
            <title>Version 3.6.5</title>
            <sparkle:releaseNotesLink>https://downloads.imazing.com/mac/iMazing/3.6.5.24380/release-notes.html</sparkle:releaseNotesLink>
            <enclosure length="323713213" sparkle:shortVersionString="3.6.5" sparkle:version="24380" type="application/octet-stream" url="https://downloads.imazing.com/mac/iMazing/3.6.5.24380/iMazing_3.6.5.24380.dmg" />
            <sparkle:minimumSystemVersion>10.12.0</sparkle:minimumSystemVersion>
        </item>
        <item>
            <enclosure sparkle:shortVersionString="3.6.4" sparkle:version="24330" url="x.dmg" />
        </item>
    </channel></rss>"#;

    fn offer(xml: &str, installed: &str, os: Option<&str>) -> Option<String> {
        newest_offer(xml, installed, os).map(|it| it.version)
    }

    #[test]
    fn appcast_reads_element_and_attribute_forms() {
        let obs = parse_appcast_items(OBS);
        assert_eq!(obs.len(), 3);
        assert_eq!(obs[0].channel.as_deref(), Some("beta"));
        assert_eq!(obs[1].min_os.as_deref(), Some("13.0"));
        assert_eq!(
            offer(IMAZING, "3.4.0", Some("26.1")).as_deref(),
            Some("3.6.5")
        );
        assert_eq!(parse_appcast_items("<rss></rss>"), vec![]);
        assert_eq!(
            parse_appcast_items("<item><title>no version</title></item>"),
            vec![]
        );
    }

    #[test]
    fn appcast_items_out_of_order_take_the_newest() {
        let xml = r#"<item><sparkle:shortVersionString>1.2</sparkle:shortVersionString></item>
            <item><sparkle:shortVersionString>1.10</sparkle:shortVersionString></item>
            <item><enclosure sparkle:shortVersionString="1.9" /></item>"#;
        assert_eq!(offer(xml, "1.0", None).as_deref(), Some("1.10"));
    }

    #[test]
    fn beta_channel_is_not_offered_to_stable_users() {
        // Bug: the newest version anywhere in the feed was taken, so stable OBS users were told
        // to "update" to the beta.
        assert_eq!(
            offer(OBS, "32.1.2", Some("26.1")).as_deref(),
            Some("33.0.1")
        );
        assert!(is_prerelease_channel("beta"));
        assert!(is_prerelease_channel("nightly"));
        assert!(!is_prerelease_channel("stable"));
    }

    #[test]
    fn beta_versions_without_a_channel_are_skipped() {
        let xml = r#"<item><sparkle:shortVersionString>2.1b3</sparkle:shortVersionString></item>
            <item><sparkle:shortVersionString>2.0.4</sparkle:shortVersionString></item>
            <item><sparkle:shortVersionString>2.1.0-rc1</sparkle:shortVersionString></item>"#;
        assert_eq!(offer(xml, "2.0.1", None).as_deref(), Some("2.0.4"));
        // …unless the user is already on a beta, in which case they're following that track. (The
        // lenient compare can't order "2.1b3" against "2.1.0-rc1", so only check a beta is offered.)
        assert!(offer(xml, "2.1b1", None).is_some_and(|v| is_prerelease_version(&v)));
        assert!(is_prerelease_version("5.1-beta.2"));
        assert!(!is_prerelease_version("33.0.1"));
        assert!(!is_prerelease_version("1.0")); // no letters
    }

    #[test]
    fn releases_this_mac_cannot_run_are_skipped() {
        // Bug: minimumSystemVersion was ignored, so a Monterey user was shown 33.0.1 (needs 13.0).
        assert_eq!(
            offer(OBS, "32.1.2", Some("12.7.6")).as_deref(),
            Some("32.2.2")
        );
        assert_eq!(offer(OBS, "32.1.2", Some("11.7")), None);
        let capped = r#"<item><sparkle:shortVersionString>4.0</sparkle:shortVersionString>
                <sparkle:maximumSystemVersion>14.99</sparkle:maximumSystemVersion></item>
            <item><sparkle:shortVersionString>5.0</sparkle:shortVersionString></item>"#;
        assert_eq!(offer(capped, "3.0", Some("26.1")).as_deref(), Some("5.0"));
        assert_eq!(
            offer(
                r#"<item><sparkle:shortVersionString>4.0</sparkle:shortVersionString>
                <sparkle:maximumSystemVersion>14.99</sparkle:maximumSystemVersion></item>"#,
                "3.0",
                Some("26.1")
            ),
            None
        );
        // Unknown OS: don't filter on it.
        assert_eq!(offer(OBS, "32.1.2", None).as_deref(), Some("33.0.1"));
    }

    #[test]
    fn notes_come_from_the_offered_release() {
        let it = newest_offer(OBS, "32.1.2", Some("26.1")).unwrap();
        assert_eq!(
            it.notes.as_deref(),
            Some("https://obsproject.com/osx_update/notes_stable.html")
        );
        // a non-http notes link is rejected
        let bad = r#"<item><sparkle:shortVersionString>2.0</sparkle:shortVersionString>
            <sparkle:releaseNotesLink>file:///etc/passwd</sparkle:releaseNotesLink></item>"#;
        assert_eq!(newest_offer(bad, "1.0", None).unwrap().notes, None);
    }

    #[test]
    fn version_compare_edge_cases() {
        assert!(!version_is_older("1.2", "1.2.0")); // equal across differing lengths
        assert!(!version_is_older("1.2.0", "1.2"));
        assert!(version_is_older("1.2", "1.2.1"));
        assert!(!version_is_older("1.2.0-beta", "1.2.0")); // pre-release suffix loose
        assert!(version_is_older("1.09", "1.10")); // numeric, not lexical
        assert!(!version_is_older("", ""));
        assert!(version_is_older("", "1.0")); // empty treated as 0
    }

    // Hits the network and this Mac's /Applications: `cargo test sparkle::tests::live -- --ignored --nocapture`.
    #[test]
    #[ignore = "network + installed apps"]
    fn live_check_against_installed_apps() {
        let status = check_sparkle();
        for item in &status.items {
            println!(
                "{}: {} -> {}  notes={}",
                item.name, item.current, item.latest, item.notes_url
            );
        }
        println!("{} outdated", status.items.len());
    }
}
