// Input validation (defense-in-depth).

use std::path::Path;

// --- Input validation (defense-in-depth) ---
// Commands receive these strings from the webview; validate at the boundary so a crafted value
// (e.g. a leading "-") can't be smuggled in as a flag to brew/open, even though we never use a shell.

pub(crate) fn is_valid_cask_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 128
        && token
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'@' | b'.' | b'_' | b'+'))
}

pub(crate) fn is_safe_search_query(query: &str) -> bool {
    let q = query.trim();
    !q.is_empty() && q.len() <= 64 && !q.starts_with('-') && q.chars().all(|c| !c.is_control())
}

pub(crate) fn is_safe_url(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://"))
        && url.len() <= 2048
        && url.chars().all(|c| !c.is_control() && c != ' ')
}

// Pull the host out of an http(s) URL (no scheme, no port, no path, no userinfo). Returns
// None for anything that doesn't look like an http(s) URL with a non-empty host. Hand-rolled
// because std doesn't ship a URL parser and a dep just for this is overkill.
fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    // Strip any "user:pass@" prefix.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literals look like "[::1]" or "[fe80::1]:443".
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        let close = stripped.find(']')?;
        &stripped[..close]
    } else {
        // IPv4 / DNS — strip the optional ":port".
        host_port.split(':').next().unwrap_or(host_port)
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

// Hostnames and IP ranges we refuse to talk to when fetching app-supplied URLs (Sparkle feeds).
// Mitigates SSRF from a hostile local app that points its SUFeedURL at internal infrastructure
// (e.g. 169.254.169.254 cloud-metadata, 192.168/16 routers, localhost services). DNS rebinding
// can bypass an IP-literal check; we accept that as a known limitation.
// `.local` / `.internal` are mDNS / private hostname suffixes (RFC 6762), not file extensions —
// clippy's case_sensitive_file_extension_comparisons false-positives here. Callers pass a
// lowercased host from url_host(), so a plain ends_with is correct.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_local_or_private_host(host: &str) -> bool {
    if host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".localhost")
        || host.ends_with(".internal")
    {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return ip.is_private()
            || ip.is_loopback()
            || ip.is_link_local()
            || ip.is_unspecified()
            || ip.is_broadcast()
            // Carrier-grade NAT (RFC 6598) — not in std's helpers.
            || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64);
    }
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        let first = ip.segments()[0];
        return ip.is_loopback()
            || ip.is_unspecified()
            // Unique-local fc00::/7
            || (first & 0xfe00) == 0xfc00
            // Link-local fe80::/10
            || (first & 0xffc0) == 0xfe80;
    }
    false
}

// Stricter than is_safe_url: used for app-supplied Sparkle feed URLs that we (Freshet, not the
// user's browser) will fetch over the network. https only and no internal/private hosts.
pub(crate) fn is_safe_feed_url(url: &str) -> bool {
    if !url.starts_with("https://") || url.len() > 2048 {
        return false;
    }
    if url.chars().any(|c| c.is_control() || c == ' ') {
        return false;
    }
    url_host(url).is_some_and(|host| !is_local_or_private_host(&host))
}

// Mac App Store ADAM IDs are numeric.
pub(crate) fn is_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())
}

// Validate an npm package name so a selection can't smuggle a flag or an arbitrary install spec
// (e.g. a git/tarball URL) into `npm install -g`.
pub(crate) fn is_valid_npm_package(name: &str) -> bool {
    if name.is_empty() || name.len() > 214 {
        return false;
    }
    if name.starts_with('-') || name.starts_with('.') || name.starts_with('_') {
        return false;
    }
    if name.contains("://") {
        return false;
    }
    // A '/' only ever separates a scope from its name ("@scope/name"). An unscoped "owner/repo"
    // is npm's GitHub shorthand: an install from an arbitrary repository, not the registry.
    let body = match name.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, pkg)) if !scope.is_empty() && !pkg.is_empty() && !pkg.contains('/') => {
                scoped
            }
            _ => return false,
        },
        None if name.contains('/') => return false,
        None => name,
    };
    body.bytes().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.' | b'/')
    })
}

// A bare "Something.app" bundle name — no path separators, so it can only name a bundle directly
// inside an Applications folder. Pure → unit-tested.
pub(crate) fn is_valid_app_name(app: &str) -> bool {
    app.len() <= 255
        && Path::new(app)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        && !app.contains('/')
        && !app.starts_with('.')
        && !app.chars().any(char::is_control)
}

// A single path component: no separators, not "." / "..", nothing hidden. Pure → unit-tested.
pub(crate) fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.starts_with('.')
}

// Shapes we accept from the frontend before writing them into the trust baseline.
pub(crate) fn is_valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

pub(crate) fn is_valid_team_id(team: &str) -> bool {
    team.len() == 10
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;

    #[test]
    fn numeric_id_validation() {
        assert!(is_numeric_id("462058435"));
        assert!(!is_numeric_id("12a"));
        assert!(!is_numeric_id(""));
        assert!(!is_numeric_id("-5"));
    }

    #[test]
    fn npm_package_validation() {
        assert!(is_valid_npm_package("typescript"));
        assert!(is_valid_npm_package("@angular/cli"));
        assert!(is_valid_npm_package("lodash.merge"));
        assert!(!is_valid_npm_package("-g"), "flag");
        assert!(!is_valid_npm_package("https://evil/x.tgz"), "url spec");
        assert!(!is_valid_npm_package("git+ssh://x"), "url spec");
        assert!(!is_valid_npm_package(".hidden"), "leading dot");
        assert!(!is_valid_npm_package("UPPER"), "npm names are lowercase");
        assert!(!is_valid_npm_package(""));
    }

    #[test]
    fn cask_token_validation_rejects_flags_and_junk() {
        assert!(is_valid_cask_token("google-chrome"));
        assert!(is_valid_cask_token("google-chrome@beta"));
        assert!(is_valid_cask_token("1password"));
        assert!(
            !is_valid_cask_token("--force"),
            "leading dash = flag injection"
        );
        assert!(!is_valid_cask_token(""), "empty");
        assert!(!is_valid_cask_token("a b"), "space");
        assert!(!is_valid_cask_token("a/../b"), "slash/path chars");
        assert!(!is_valid_cask_token(&"a".repeat(200)), "too long");
    }

    #[test]
    fn search_query_validation() {
        assert!(is_safe_search_query("chrome"));
        assert!(is_safe_search_query("google chrome"));
        assert!(!is_safe_search_query("-x"), "leading dash = flag injection");
        assert!(!is_safe_search_query("   "), "blank");
        assert!(!is_safe_search_query("a\nb"), "control char");
    }

    #[test]
    fn url_validation_only_allows_http() {
        assert!(is_safe_url("https://example.com/path"));
        assert!(is_safe_url("http://example.com"));
        assert!(!is_safe_url("file:///etc/passwd"), "no file://");
        assert!(!is_safe_url("javascript:alert(1)"), "no js scheme");
        assert!(!is_safe_url("https://e.com/ x"), "no spaces");
        assert!(!is_safe_url(""), "empty");
    }

    #[test]
    fn app_name_validation() {
        assert!(is_valid_app_name("Slack.app"));
        assert!(is_valid_app_name("IntelliJ IDEA CE.app"));
        assert!(!is_valid_app_name("../../etc.app"));
        assert!(!is_valid_app_name("Foo/Bar.app"));
        assert!(!is_valid_app_name(".app"));
        assert!(!is_valid_app_name(".hidden.app"));
        assert!(!is_valid_app_name("Slack"));
    }

    #[test]
    fn url_host_extracts_hostname() {
        assert_eq!(
            url_host("https://example.com/path").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("https://Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("http://example.com:8080/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("https://user:pass@example.com/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(url_host("https://[::1]:443/x").as_deref(), Some("::1"));
        assert_eq!(url_host("https://[fe80::1]/x").as_deref(), Some("fe80::1"));
        // Path / query / fragment terminate the host.
        assert_eq!(
            url_host("https://example.com?q=1").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            url_host("https://example.com#frag").as_deref(),
            Some("example.com")
        );
        // Non-http(s) and malformed cases.
        assert_eq!(url_host("file:///etc/passwd"), None);
        assert_eq!(url_host("https://"), None);
        assert_eq!(url_host("https://[unclosed"), None);
    }

    #[test]
    fn private_host_rejection() {
        // Loopback / private / link-local / unspecified / broadcast / CGNAT.
        assert!(is_local_or_private_host("localhost"));
        assert!(is_local_or_private_host("foo.local"));
        assert!(is_local_or_private_host("intranet.internal"));
        assert!(is_local_or_private_host("box.localhost"));
        assert!(is_local_or_private_host("127.0.0.1"));
        assert!(is_local_or_private_host("10.0.0.1"));
        assert!(is_local_or_private_host("192.168.1.1"));
        assert!(is_local_or_private_host("172.16.0.1"));
        assert!(is_local_or_private_host("172.31.255.255"));
        assert!(is_local_or_private_host("169.254.169.254"), "AWS metadata");
        assert!(is_local_or_private_host("0.0.0.0"));
        assert!(is_local_or_private_host("255.255.255.255"));
        assert!(is_local_or_private_host("100.64.0.1"), "carrier-grade NAT");
        assert!(is_local_or_private_host("100.127.255.255"), "CGNAT upper");
        assert!(is_local_or_private_host("::1"));
        assert!(is_local_or_private_host("::"), "IPv6 unspecified");
        assert!(is_local_or_private_host("fc00::1"), "ULA");
        assert!(is_local_or_private_host("fd12:3456:789a::1"), "ULA");
        assert!(is_local_or_private_host("fe80::1"), "link-local v6");
        // Public — must NOT be flagged.
        assert!(!is_local_or_private_host("example.com"));
        assert!(!is_local_or_private_host("github.com"));
        assert!(!is_local_or_private_host("8.8.8.8"));
        assert!(
            !is_local_or_private_host("172.32.0.1"),
            "just outside private range"
        );
        assert!(
            !is_local_or_private_host("100.63.255.255"),
            "just outside CGNAT"
        );
        assert!(
            !is_local_or_private_host("100.128.0.1"),
            "just outside CGNAT"
        );
        assert!(
            !is_local_or_private_host("2606:4700:4700::1111"),
            "Cloudflare v6"
        );
    }

    #[test]
    fn feed_url_requires_https_and_public_host() {
        assert!(is_safe_feed_url(
            "https://obsproject.com/osx_update/updates.xml"
        ));
        // http is rejected outright — Sparkle 2 requires https; older feeds can MITM us.
        assert!(!is_safe_feed_url(
            "http://obsproject.com/osx_update/updates.xml"
        ));
        assert!(!is_safe_feed_url("file:///etc/passwd"));
        assert!(!is_safe_feed_url("javascript:alert(1)"));
        // SSRF-style hostile feeds.
        assert!(!is_safe_feed_url("https://localhost/x.xml"));
        assert!(!is_safe_feed_url("https://127.0.0.1/x.xml"));
        assert!(!is_safe_feed_url("https://192.168.1.1/x.xml"));
        assert!(!is_safe_feed_url(
            "https://169.254.169.254/latest/meta-data/"
        ));
        assert!(!is_safe_feed_url("https://[::1]/x.xml"));
        assert!(!is_safe_feed_url("https://router.local/x.xml"));
        // Junk.
        assert!(!is_safe_feed_url(""));
        assert!(!is_safe_feed_url("https://e.com/ x"));
        assert!(!is_safe_feed_url(&format!(
            "https://e.com/{}",
            "a".repeat(2050)
        )));
    }

    #[test]
    fn validator_boundaries() {
        assert!(is_valid_cask_token(&"a".repeat(128)));
        assert!(!is_valid_cask_token(&"a".repeat(129)));
        assert!(is_valid_cask_token("node@18"));
        assert!(is_numeric_id("007"));
        assert!(!is_numeric_id(&"9".repeat(21)));
        assert!(is_valid_npm_package("lodash.merge"));
        assert!(is_valid_npm_package("@scope/pkg.name"));
        assert!(!is_valid_npm_package(&"a".repeat(215)));
        assert!(!is_safe_url("HTTPS://example.com")); // case-sensitive scheme (conservative)
        assert!(is_safe_url("https://example.com"));
        assert!(!is_safe_url(&format!("https://{}", "a".repeat(2050))));
        assert!(!is_safe_search_query(&"a".repeat(65)));
        assert!(is_safe_search_query("visual studio code"));
    }

    #[test]
    fn trust_inputs_are_validated() {
        assert!(is_valid_bundle_id("com.tinyspeck.slackmacgap"));
        assert!(!is_valid_bundle_id("com.x/../../etc"));
        assert!(!is_valid_bundle_id(""));
        assert!(is_valid_team_id("BQR82RBBHL"));
        assert!(!is_valid_team_id("bqr82rbbhl"));
        assert!(!is_valid_team_id("SHORT"));
    }

    // "owner/repo" is npm's GitHub shorthand, not a registry package: a '/' is only valid
    // between a scope and its name.
    #[test]
    fn npm_names_allow_a_slash_only_after_a_scope() {
        assert!(is_valid_npm_package("@angular/cli"));
        assert!(!is_valid_npm_package("evil/repo"), "GitHub shorthand");
        assert!(!is_valid_npm_package("@scope/pkg/extra"));
        assert!(!is_valid_npm_package("@scope"), "scope without a name");
        assert!(!is_valid_npm_package("@/pkg"), "empty scope");
        assert!(!is_valid_npm_package("@scope/"), "empty name");
    }

    // CFBundleIconFile comes from the app's own Info.plist; it must stay inside Resources.
    #[test]
    fn icon_file_names_cannot_leave_the_bundle() {
        assert!(is_plain_file_name("AppIcon"));
        assert!(is_plain_file_name("electron.icns"));
        assert!(!is_plain_file_name("../../../../etc/secret.png"));
        assert!(!is_plain_file_name("/Users/someone/Pictures/private.png"));
        assert!(!is_plain_file_name(".."));
        assert!(!is_plain_file_name(""));
    }
}
