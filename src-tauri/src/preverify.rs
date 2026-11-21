// Pre-install publisher check for casks.
//
// The post-update signature check catches a hijacked release only after it's installed. This
// runs *before*: after `brew fetch` has downloaded the new version, open the download (DMG, ZIP
// or PKG), read who signed it, and compare that Team ID with the installed app's (or the trusted
// baseline in signing.json). A different publisher — or an unsigned download replacing a signed
// app — blocks the install. That's the 3CX / HandBrake / Transmission scenario: validly signed,
// sometimes notarized, by the wrong people.
//
// The download's Team ID has to be proven, not just stated: after reading what the signature
// claims (`codesign -dvv`), the bundle is verified against a requirement naming that team
// (crate::signing::signature_proves_team), so a signature hand-built to *say* the right team doesn't pass.
// That used to be left to Gatekeeper on first launch; now nothing is installed first.
//
// When the download can't be inspected at all (an archive format we don't open, a disk image
// that won't mount), the install normally goes ahead with a note. Not for an app with a pinned
// publisher (crate::signing::PINNED_PUBLISHERS): there Freshet knows who must have signed it, so "couldn't
// check" blocks — otherwise choosing awkward packaging would be a way around the check.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    // Go ahead. `note` explains when the check couldn't be done (shown in the result log).
    Allowed {
        note: Option<String>,
    },
    // Don't install. `bundle_id` + `team` let the user trust the new publisher explicitly.
    Blocked {
        reason: String,
        bundle_id: Option<String>,
        team: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Signer {
    pub(crate) bundle_id: Option<String>,
    pub(crate) team: Option<String>,
}

// What we learned about the download.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Incoming {
    // Signed by this team, and the signature verifies.
    Signed(String),
    Unsigned,
    // Claims this team, but the signature doesn't verify as it.
    Invalid(String),
    // Couldn't be inspected (format, mount failure, app not found inside); the reason is shown.
    Unknown(String),
}

// The decision, separated from all I/O. `baseline` is the trusted Team ID for this app (stored
// baseline, else the installed app's own); `pinned` the publisher's name when that baseline is
// one of Freshet's pins. Pure → unit-tested.
pub(crate) fn decide(
    token: &str,
    installed: &Signer,
    baseline: Option<&str>,
    pinned: Option<&str>,
    incoming: Incoming,
) -> Verdict {
    let allowed = |note: Option<String>| Verdict::Allowed { note };
    let Some(expected) = baseline else {
        return allowed(Some("Pre-install check: the installed app isn't signed, so there's no publisher to compare.".into()));
    };
    match incoming {
        Incoming::Signed(team) if team == expected => allowed(None),
        Incoming::Signed(team) => Verdict::Blocked {
            reason: format!(
                "Blocked before install: the new {token} download is signed by Team {team}, but the installed app is \
                 from Team {expected}. A different publisher signing an update is what a hijacked release looks \
                 like. Nothing was installed. If you know this change is legitimate (an acquisition, a new \
                 developer account), trust the new publisher and update again."
            ),
            bundle_id: installed.bundle_id.clone(),
            team,
        },
        Incoming::Unsigned => Verdict::Blocked {
            reason: format!(
                "Blocked before install: the new {token} download isn't code-signed, but the installed app is \
                 signed by Team {expected}. Nothing was installed."
            ),
            bundle_id: None,
            team: String::new(),
        },
        Incoming::Invalid(team) => Verdict::Blocked {
            reason: format!(
                "Blocked before install: the new {token} download says it is signed by Team {team}, but \
                 its signature doesn't verify. A signature that only claims a publisher is what a forged \
                 release looks like. Nothing was installed."
            ),
            bundle_id: None,
            team: String::new(),
        },
        Incoming::Unknown(why) => pinned.map_or_else(
            || allowed(Some(format!("Pre-install check skipped: {why}."))),
            |publisher| Verdict::Blocked {
                reason: format!(
                    "Blocked before install: the new {token} download couldn't be checked ({why}), and \
                     {token} must come from {publisher} (Team {expected}). Freshet only installs it when it \
                     can confirm that. Nothing was installed; try again, or update it from the app itself."
                ),
                bundle_id: None,
                team: String::new(),
            },
        ),
    }
}

// What `pkgutil --check-signature` says about an installer package. pkgutil validates the
// signature itself and reports the outcome on its "Status:" line; only a certificate issued by
// Apple counts ("signed by a developer certificate issued by Apple for distribution", "signed
// Apple Software"). Anything else that is signed — "signed by untrusted certificate", an expired
// or revoked one — is Invalid: a self-made certificate can be *named* "Developer ID Installer:
// Zoom (BJ4HAAB9B3)". Pure → unit-tested.
pub(crate) fn parse_pkgutil(output: &str) -> Incoming {
    let status = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Status: "))
        .unwrap_or("");
    if status.is_empty() || status.contains("no signature") {
        return Incoming::Unsigned;
    }
    let team = parse_pkgutil_team(output);
    let apple_issued = status.contains("issued by Apple") || status.contains("Apple Software");
    match team {
        Some(team) if apple_issued => Incoming::Signed(team),
        Some(team) => Incoming::Invalid(team),
        None => Incoming::Unknown("the installer package's signer couldn't be read".into()),
    }
}

// The certificate chain's leaf is "1. Developer ID Installer: Name (TEAMID)". Pure → unit-tested.
pub(crate) fn parse_pkgutil_team(output: &str) -> Option<String> {
    if output.contains("Status: no signature") {
        return None;
    }
    output.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("1. ")?;
        let open = rest.rfind('(')?;
        let team = rest[open + 1..].strip_suffix(')')?;
        (team.len() == 10 && team.bytes().all(|b| b.is_ascii_alphanumeric()))
            .then(|| team.to_string())
    })
}

// --- I/O ---

// `installed_apps`: this cask's bundles currently in /Applications (see installed_cask_apps).
// `download`: where Homebrew cached the new version, if it's there (see cached_downloads).
pub(crate) fn check_cask(
    token: &str,
    installed_apps: &[PathBuf],
    download: Option<&Path>,
) -> Verdict {
    let started = Instant::now();
    let verdict = check_cask_inner(token, installed_apps, download);
    tracing::info!(
        cask = token,
        elapsed_ms = crate::process::elapsed_ms(started),
        ?verdict,
        "pre-install check"
    );
    verdict
}

fn check_cask_inner(token: &str, installed_apps: &[PathBuf], download: Option<&Path>) -> Verdict {
    let Some(installed_app) = installed_apps.first() else {
        // Not installed in /Applications (or a cask with no app): nothing to compare against.
        return Verdict::Allowed { note: None };
    };
    let installed = crate::signing::read_signer(installed_app);
    let trusted = installed
        .bundle_id
        .as_deref()
        .and_then(crate::signing::trusted_signer);
    let pinned = match &trusted {
        Some((_, crate::signing::Basis::Pinned(publisher))) => Some(*publisher),
        _ => None,
    };
    let baseline = trusted
        .map(|(team, _)| team)
        .or_else(|| installed.team.clone());

    let app_name = installed_app
        .file_name()
        .map_or_default(|n| n.to_string_lossy().into_owned());
    let incoming = download.map_or_else(
        || Incoming::Unknown("the download wasn't in Homebrew's cache".into()),
        |path| inspect_download(path, &app_name),
    );
    decide(token, &installed, baseline.as_deref(), pinned, incoming)
}

// `brew --cache --cask a b c` prints one path per line, in argument order. None if the answer
// doesn't line up with the question (brew refused one of the casks). Pure → unit-tested.
pub(crate) fn parse_cache_paths(stdout: &str, tokens: &[&str]) -> Option<Vec<(String, PathBuf)>> {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    (lines.len() == tokens.len()).then(|| {
        tokens
            .iter()
            .zip(lines)
            .map(|(token, line)| ((*token).to_string(), PathBuf::from(line.trim())))
            .collect()
    })
}

// Where Homebrew cached each cask's download, for the ones that are actually there. One brew
// process for the whole batch rather than one per cask (each start-up costs about a second);
// if brew won't answer for the batch, ask per cask so one bad token only costs itself.
pub(crate) fn cached_downloads(tokens: &[String]) -> HashMap<String, PathBuf> {
    let tokens: Vec<&str> = tokens
        .iter()
        .map(String::as_str)
        .filter(|t| crate::validate::is_valid_cask_token(t))
        .collect();
    if tokens.is_empty() {
        return HashMap::new();
    }
    let ask = |tokens: &[&str]| -> Option<Vec<(String, PathBuf)>> {
        let mut args = vec!["--cache", "--cask"];
        args.extend(tokens);
        parse_cache_paths(&crate::process::run_cmd("brew", &args).ok()?, tokens)
    };
    ask(&tokens)
        .unwrap_or_else(|| tokens.iter().filter_map(|t| ask(&[t])).flatten().collect())
        .into_iter()
        .filter(|(_, path)| path.is_file())
        .collect()
}

fn inspect_download(path: &Path, app_name: &str) -> Incoming {
    let ext = path
        .extension()
        .map_or_default(|e| e.to_string_lossy().to_lowercase());
    match ext.as_str() {
        "dmg" => inspect_dmg(path, app_name),
        "zip" => inspect_zip(path, app_name),
        "pkg" => inspect_pkg(path),
        other => Incoming::Unknown(format!("can't inspect .{other} downloads")),
    }
}

// A new, empty directory of our own: unique per call (two casks checked in the same second must
// not share a mount point), and `create_dir` refuses one that already exists.
fn scratch_dir(label: &str) -> Option<PathBuf> {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "freshet-{label}-{}-{}-{}",
        std::process::id(),
        crate::store::unix_now(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).ok()?;
    Some(dir)
}

fn inspect_dmg(path: &Path, app_name: &str) -> Incoming {
    let Some(mount) = scratch_dir("dmg") else {
        return Incoming::Unknown("couldn't create a temporary mount point".into());
    };
    // Homebrew's flags, plus -noverify: brew already checked the whole download against the
    // cask's SHA-256, so hdiutil's own checksum pass (~3s on a 100MB image) is redundant. "qn"
    // answers a license-agreement prompt (quit, decline) so an SLA can't hang us; such images
    // just come back as "couldn't inspect".
    let attached = run_with_timeout(
        "/usr/bin/hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-noverify".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            path.as_os_str(),
        ],
        b"qn\n",
        Duration::from_mins(1),
    )
    .is_some_and(|ok| ok);
    let result = if attached {
        find_bundle(&mount, app_name, 2).map_or_else(
            || {
                // Some casks ship a .pkg inside the DMG.
                find_with_extension(&mount, "pkg", 1).map_or_else(
                    || Incoming::Unknown(format!("{app_name} wasn't found in the disk image")),
                    |pkg| inspect_pkg(&pkg),
                )
            },
            |app| incoming_from_bundle(&app),
        )
    } else {
        Incoming::Unknown("the disk image couldn't be mounted".into())
    };
    if attached {
        let _ = Command::new("/usr/bin/hdiutil")
            .args(["detach", "-force", "-quiet"])
            .arg(&mount)
            .output();
    }
    let _ = std::fs::remove_dir(&mount);
    result
}

fn inspect_zip(path: &Path, app_name: &str) -> Incoming {
    let Some(dir) = scratch_dir("zip") else {
        return Incoming::Unknown("couldn't create a temporary directory".into());
    };
    let ok = run_with_timeout(
        "/usr/bin/ditto",
        &[
            "-x".as_ref(),
            "-k".as_ref(),
            path.as_os_str(),
            dir.as_os_str(),
        ],
        b"",
        Duration::from_mins(2),
    )
    .is_some_and(|ok| ok);
    let result = if ok {
        find_bundle(&dir, app_name, 2).map_or_else(
            || Incoming::Unknown(format!("{app_name} wasn't found in the archive")),
            |app| incoming_from_bundle(&app),
        )
    } else {
        Incoming::Unknown("the archive couldn't be extracted".into())
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn inspect_pkg(path: &Path) -> Incoming {
    match Command::new("/usr/sbin/pkgutil")
        .arg("--check-signature")
        .arg(path)
        .output()
    {
        Ok(out) => parse_pkgutil(&crate::process::combine_output(&out.stdout, &out.stderr)),
        Err(e) => Incoming::Unknown(format!("pkgutil failed: {e}")),
    }
}

fn incoming_from_bundle(app: &Path) -> Incoming {
    match crate::signing::read_signer(app).team {
        None => Incoming::Unsigned,
        Some(team) if crate::signing::signature_proves_team(app, &team) => Incoming::Signed(team),
        Some(team) => Incoming::Invalid(team),
    }
}

// Case-insensitive search for `name` up to `depth` directories down.
fn find_bundle(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    if let Some(hit) = entries.iter().find(|p| {
        p.file_name()
            .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name))
    }) {
        return Some(hit.clone());
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|p| p.is_dir() && !p.is_symlink() && p.extension().is_none_or(|e| e != "app"))
        .find_map(|p| find_bundle(p, name, depth - 1))
}

fn find_with_extension(root: &Path, ext: &str, depth: usize) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    if let Some(hit) = entries
        .iter()
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
    {
        return Some(hit.clone());
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|p| p.is_dir() && !p.is_symlink())
        .find_map(|p| find_with_extension(p, ext, depth - 1))
}

// Run with stdin input and a deadline; Some(success) if it finished, None on timeout/spawn error.
fn run_with_timeout(
    cmd: &str,
    args: &[&std::ffi::OsStr],
    input: &[u8],
    timeout: Duration,
) -> Option<bool> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn installed(team: &str) -> Signer {
        Signer {
            bundle_id: Some("com.example.app".into()),
            team: Some(team.into()),
        }
    }

    #[test]
    fn same_publisher_is_allowed() {
        assert_eq!(
            decide(
                "app",
                &installed("AAAAAAAAAA"),
                Some("AAAAAAAAAA"),
                None,
                Incoming::Signed("AAAAAAAAAA".into())
            ),
            Verdict::Allowed { note: None }
        );
    }

    #[test]
    fn different_publisher_is_blocked_with_a_trust_action() {
        let v = decide(
            "app",
            &installed("AAAAAAAAAA"),
            Some("AAAAAAAAAA"),
            None,
            Incoming::Signed("EVILEVIL99".into()),
        );
        let Verdict::Blocked {
            reason,
            bundle_id,
            team,
        } = v
        else {
            panic!("expected Blocked")
        };
        assert!(reason.contains("Team EVILEVIL99") && reason.contains("Team AAAAAAAAAA"));
        assert_eq!(bundle_id.as_deref(), Some("com.example.app"));
        assert_eq!(team, "EVILEVIL99");
    }

    // The trusted baseline wins over the live app: once a user trusts a new publisher, the
    // update goes through even though the installed bundle still carries the old signature.
    #[test]
    fn trusted_baseline_overrides_installed_team() {
        assert_eq!(
            decide(
                "app",
                &installed("OLDOLDOLD1"),
                Some("NEWNEWNEW1"),
                None,
                Incoming::Signed("NEWNEWNEW1".into())
            ),
            Verdict::Allowed { note: None }
        );
    }

    #[test]
    fn unsigned_download_replacing_signed_app_is_blocked() {
        assert!(matches!(
            decide(
                "app",
                &installed("AAAAAAAAAA"),
                Some("AAAAAAAAAA"),
                None,
                Incoming::Unsigned
            ),
            Verdict::Blocked { .. }
        ));
    }

    #[test]
    fn uninspectable_or_unsigned_installed_is_allowed_with_a_note() {
        let v = decide(
            "app",
            &installed("AAAAAAAAAA"),
            Some("AAAAAAAAAA"),
            None,
            Incoming::Unknown("x".into()),
        );
        assert!(matches!(v, Verdict::Allowed { note: Some(_) }));
        let v = decide(
            "app",
            &Signer {
                bundle_id: None,
                team: None,
            },
            None,
            None,
            Incoming::Signed("AAAAAAAAAA".into()),
        );
        assert!(matches!(v, Verdict::Allowed { note: Some(_) }));
    }

    // A download that only *claims* the right publisher is blocked, with nothing to "trust".
    #[test]
    fn a_signature_that_does_not_verify_is_blocked() {
        let v = decide(
            "app",
            &installed("AAAAAAAAAA"),
            Some("AAAAAAAAAA"),
            None,
            Incoming::Invalid("AAAAAAAAAA".into()),
        );
        let Verdict::Blocked {
            reason, bundle_id, ..
        } = v
        else {
            panic!("expected Blocked")
        };
        assert!(reason.contains("doesn't verify"), "{reason}");
        assert_eq!(bundle_id, None, "no publisher to trust");
    }

    // "Couldn't check" lets an ordinary app through with a note, but not one whose publisher
    // Freshet pins: there it must be confirmed.
    #[test]
    fn an_uninspectable_download_is_blocked_only_for_a_pinned_publisher() {
        let unknown = || Incoming::Unknown("can't inspect .tar downloads".into());
        let ordinary = decide(
            "app",
            &installed("AAAAAAAAAA"),
            Some("AAAAAAAAAA"),
            None,
            unknown(),
        );
        assert!(matches!(ordinary, Verdict::Allowed { note: Some(_) }));
        let pinned = decide(
            "slack",
            &installed("BQR82RBBHL"),
            Some("BQR82RBBHL"),
            Some("Slack Technologies"),
            unknown(),
        );
        let Verdict::Blocked { reason, .. } = pinned else {
            panic!("expected Blocked")
        };
        assert!(
            reason.contains("Slack Technologies") && reason.contains("can't inspect .tar"),
            "{reason}"
        );
        // A pinned app whose download checks out is unaffected.
        assert_eq!(
            decide(
                "slack",
                &installed("BQR82RBBHL"),
                Some("BQR82RBBHL"),
                Some("Slack Technologies"),
                Incoming::Signed("BQR82RBBHL".into())
            ),
            Verdict::Allowed { note: None }
        );
    }

    // pkgutil's own verdict decides whether the named team counts.
    #[test]
    fn installer_packages_need_an_apple_issued_certificate() {
        let chain = "   Certificate Chain:\n    1. Developer ID Installer: Zoom Video Communications, Inc. (BJ4HAAB9B3)\n";
        let genuine = format!("Package \"z.pkg\":\n   Status: signed by a developer certificate issued by Apple for distribution\n{chain}");
        assert_eq!(
            parse_pkgutil(&genuine),
            Incoming::Signed("BJ4HAAB9B3".into())
        );
        // A self-made certificate with the right *name*.
        let forged =
            format!("Package \"z.pkg\":\n   Status: signed by untrusted certificate\n{chain}");
        assert_eq!(
            parse_pkgutil(&forged),
            Incoming::Invalid("BJ4HAAB9B3".into())
        );
        assert_eq!(
            parse_pkgutil("Package \"x.pkg\":\n   Status: no signature\n"),
            Incoming::Unsigned
        );
        assert_eq!(parse_pkgutil(""), Incoming::Unsigned);
    }

    // Format of `pkgutil --check-signature` on a Developer ID-signed installer.
    #[test]
    fn pkgutil_team_parsing() {
        let signed = "Package \"zoomusInstallerFull.pkg\":\n   Status: signed by a developer certificate issued by Apple for distribution\n   Notarization: trusted by the Apple notary service\n   Signed with a trusted timestamp on: 2025-09-01 10:00:00 +0000\n   Certificate Chain:\n    1. Developer ID Installer: Zoom Video Communications, Inc. (BJ4HAAB9B3)\n       Expires: 2027-02-01 22:12:15 +0000\n    2. Developer ID Certification Authority\n    3. Apple Root CA\n";
        assert_eq!(parse_pkgutil_team(signed).as_deref(), Some("BJ4HAAB9B3"));
        assert_eq!(
            parse_pkgutil_team("Package \"x.pkg\":\n   Status: no signature\n"),
            None
        );
    }

    // Real `brew --cache --cask firefox google-chrome` output: one path per line, in order, and
    // file names can contain spaces.
    #[test]
    fn cache_paths_pair_up_with_their_casks_in_order() {
        let out = "/Users/u/Library/Caches/Homebrew/downloads/8ee1--Firefox 160.0.dmg\n\
                   /Users/u/Library/Caches/Homebrew/downloads/8888--googlechrome.dmg\n";
        let paired = parse_cache_paths(out, &["firefox", "google-chrome"]).unwrap();
        assert_eq!(
            paired,
            [
                (
                    "firefox".to_string(),
                    PathBuf::from(
                        "/Users/u/Library/Caches/Homebrew/downloads/8ee1--Firefox 160.0.dmg"
                    )
                ),
                (
                    "google-chrome".to_string(),
                    PathBuf::from(
                        "/Users/u/Library/Caches/Homebrew/downloads/8888--googlechrome.dmg"
                    )
                ),
            ]
        );
        // A line short (brew refused a cask) or over: don't guess which path is whose.
        assert_eq!(parse_cache_paths(out, &["firefox"]), None);
        assert_eq!(parse_cache_paths(out, &["a", "b", "c"]), None);
        assert_eq!(parse_cache_paths("", &["a"]), None);
    }

    #[test]
    fn scratch_dirs_are_unique_within_a_second() {
        let a = scratch_dir("test").unwrap();
        let b = scratch_dir("test").unwrap();
        assert_ne!(a, b);
        std::fs::remove_dir(&a).unwrap();
        std::fs::remove_dir(&b).unwrap();
    }

    #[test]
    fn finds_bundles_case_insensitively_and_nested() {
        let root = std::env::temp_dir().join(format!("freshet-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Payload/Discord.app/Contents")).unwrap();
        assert_eq!(
            find_bundle(&root, "discord.app", 2),
            Some(root.join("Payload/Discord.app"))
        );
        assert_eq!(find_bundle(&root, "discord.app", 0), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    // Pinned publishers fail closed when a download can't be inspected, so their real downloads
    // had better be inspectable. Checks whichever of these are in Homebrew's cache right now.
    //   cargo test -- --ignored pinned_downloads_live --nocapture
    #[test]
    #[ignore = "inspects real cached downloads of pinned apps"]
    fn pinned_downloads_live() {
        let casks = [
            ("slack", "Slack.app", "BQR82RBBHL"),
            ("discord", "Discord.app", "53Q6R32WPB"),
            ("figma", "Figma.app", "T8RA8NE3B7"),
            ("google-chrome", "Google Chrome.app", "EQHXZ8M8AV"),
            ("visual-studio-code", "Visual Studio Code.app", "UBF8T346G9"),
            ("zoom", "zoom.us.app", "BJ4HAAB9B3"),
            ("docker-desktop", "Docker.app", "9BNSXJN65R"),
            ("spotify", "Spotify.app", "2FNC3A47ZF"),
            ("postman", "Postman.app", "H7H8Q7M5CK"),
            ("github", "GitHub Desktop.app", "VEKTX9H2N7"),
            ("obs", "OBS.app", "2MMRE5MTB8"),
        ];
        let tokens: Vec<String> = casks.iter().map(|(t, ..)| (*t).to_string()).collect();
        let cached = cached_downloads(&tokens);
        eprintln!("{} of {} in the cache", cached.len(), casks.len());
        for (token, app, team) in casks {
            let Some(path) = cached.get(token) else {
                continue;
            };
            let started = Instant::now();
            let incoming = inspect_download(path, app);
            eprintln!(
                "{token}: {incoming:?} in {}ms",
                crate::process::elapsed_ms(started)
            );
            assert_eq!(incoming, Incoming::Signed(team.into()), "{token}");
        }
    }

    // Real I/O: mounts the cached Discord DMG (if present) and reads its signer. Same-publisher
    // vs the installed Discord → allowed; compared against Slack's Team ID → blocked.
    //   cargo test -- --ignored real_dmg
    #[test]
    #[ignore = "mounts a real cached download; needs Discord installed and its update fetched"]
    fn real_dmg_preinstall_check() {
        let dmg = cached_downloads(&["discord".to_string()])
            .remove("discord")
            .expect("run `brew fetch --cask discord` first");
        let started = Instant::now();
        let incoming = inspect_download(&dmg, "Discord.app");
        eprintln!(
            "inspected {} in {}ms: {incoming:?}",
            dmg.display(),
            crate::process::elapsed_ms(started)
        );
        let Incoming::Signed(new_team) = &incoming else {
            panic!("expected a signed download")
        };

        let discord = crate::signing::read_signer(Path::new("/Applications/Discord.app"));
        let same = decide(
            "discord",
            &discord,
            discord.team.as_deref(),
            None,
            Incoming::Signed(new_team.clone()),
        );
        assert_eq!(same, Verdict::Allowed { note: None });

        let slack = crate::signing::read_signer(Path::new("/Applications/Slack.app"));
        let other = decide(
            "discord",
            &slack,
            slack.team.as_deref(),
            None,
            Incoming::Signed(new_team.clone()),
        );
        eprintln!("{other:?}");
        assert!(matches!(other, Verdict::Blocked { .. }));
        // The mount was cleaned up.
        let mounts =
            String::from_utf8(Command::new("/sbin/mount").output().unwrap().stdout).unwrap();
        assert!(!mounts.contains("freshet-dmg"), "{mounts}");
    }
}
