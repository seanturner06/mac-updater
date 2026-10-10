// Pre-install publisher check for casks.
//
// The post-update signature check catches a hijacked release only after it's installed. This
// runs *before*: after `brew fetch` has downloaded the new version, open the download (DMG, ZIP
// or PKG), read who signed it, and compare that Team ID with the installed app's (or the trusted
// baseline in signing.json). A different publisher — or an unsigned download replacing a signed
// app — blocks the install. That's the 3CX / HandBrake / Transmission scenario: validly signed,
// sometimes notarized, by the wrong people.
//
// Only the claimed signature is read (`codesign -dvv`), not a full `--verify --deep`: tampered
// contents under a copied signature fail Gatekeeper on first launch anyway (casks keep the
// quarantine flag), while the hijack Gatekeeper *doesn't* catch is a valid signature by a
// different team — which is exactly what this compares. It keeps the check to ~1s per cask.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    Signed(String),
    Unsigned,
    // Couldn't be inspected (format, mount failure, app not found inside); the reason is shown.
    Unknown(String),
}

// The decision, separated from all I/O. `baseline` is the trusted Team ID for this app (stored
// baseline, else the installed app's own). Pure → unit-tested.
pub(crate) fn decide(
    token: &str,
    installed: &Signer,
    baseline: Option<&str>,
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
        Incoming::Unknown(why) => allowed(Some(format!("Pre-install check skipped: {why}."))),
    }
}

// `pkgutil --check-signature` lists the certificate chain; the leaf is
// "1. Developer ID Installer: Name (TEAMID)". Pure → unit-tested.
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
pub(crate) fn check_cask(token: &str, installed_apps: &[PathBuf]) -> Verdict {
    let started = Instant::now();
    let verdict = check_cask_inner(token, installed_apps);
    tracing::info!(
        cask = token,
        elapsed_ms = crate::elapsed_ms(started),
        ?verdict,
        "pre-install check"
    );
    verdict
}

fn check_cask_inner(token: &str, installed_apps: &[PathBuf]) -> Verdict {
    let Some(installed_app) = installed_apps.first() else {
        // Not installed in /Applications (or a cask with no app): nothing to compare against.
        return Verdict::Allowed { note: None };
    };
    let installed = crate::read_signer(installed_app);
    let baseline = installed
        .bundle_id
        .as_deref()
        .and_then(crate::trusted_team)
        .or_else(|| installed.team.clone());

    let app_name = installed_app
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let incoming = cached_download(token).map_or_else(
        || Incoming::Unknown("the download wasn't in Homebrew's cache".into()),
        |path| inspect_download(&path, &app_name),
    );
    decide(token, &installed, baseline.as_deref(), incoming)
}

fn cached_download(token: &str) -> Option<PathBuf> {
    let out = crate::run_cmd("brew", &["--cache", "--cask", token]).ok()?;
    let path = PathBuf::from(out.trim());
    path.is_file().then_some(path)
}

fn inspect_download(path: &Path, app_name: &str) -> Incoming {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "dmg" => inspect_dmg(path, app_name),
        "zip" => inspect_zip(path, app_name),
        "pkg" => inspect_pkg(path),
        other => Incoming::Unknown(format!("can't inspect .{other} downloads")),
    }
}

fn scratch_dir(label: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "freshet-{label}-{}-{}",
        std::process::id(),
        crate::unix_now()
    ));
    std::fs::create_dir_all(&dir).ok()?;
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
        Ok(out) => {
            let text = crate::combine_output(&out.stdout, &out.stderr);
            parse_pkgutil_team(&text).map_or(Incoming::Unsigned, Incoming::Signed)
        }
        Err(e) => Incoming::Unknown(format!("pkgutil failed: {e}")),
    }
}

fn incoming_from_bundle(app: &Path) -> Incoming {
    let signer = crate::read_signer(app);
    signer.team.map_or(Incoming::Unsigned, Incoming::Signed)
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
            Incoming::Signed("AAAAAAAAAA".into()),
        );
        assert!(matches!(v, Verdict::Allowed { note: Some(_) }));
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

    // Real I/O: mounts the cached Discord DMG (if present) and reads its signer. Same-publisher
    // vs the installed Discord → allowed; compared against Slack's Team ID → blocked.
    //   cargo test -- --ignored real_dmg
    #[test]
    #[ignore = "mounts a real cached download; needs Discord installed and its update fetched"]
    fn real_dmg_preinstall_check() {
        let dmg = cached_download("discord").expect("run `brew fetch --cask discord` first");
        let started = Instant::now();
        let incoming = inspect_download(&dmg, "Discord.app");
        eprintln!(
            "inspected {} in {}ms: {incoming:?}",
            dmg.display(),
            crate::elapsed_ms(started)
        );
        let Incoming::Signed(new_team) = &incoming else {
            panic!("expected a signed download")
        };

        let discord = crate::read_signer(Path::new("/Applications/Discord.app"));
        let same = decide(
            "discord",
            &discord,
            discord.team.as_deref(),
            Incoming::Signed(new_team.clone()),
        );
        assert_eq!(same, Verdict::Allowed { note: None });

        let slack = crate::read_signer(Path::new("/Applications/Slack.app"));
        let other = decide(
            "discord",
            &slack,
            slack.team.as_deref(),
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
