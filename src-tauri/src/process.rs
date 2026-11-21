// Running other programs: which binary is trusted, how it is elevated, how its output is
// streamed, and how a running update is stopped safely.

use crate::store::{freshet_dir, unix_now};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

// --- Stopping a running update safely ---
//
// The batch is one `brew` process. Killing it outright (what quitting the app used to do) skips
// Homebrew's cleanup. SIGINT — Ctrl-C — doesn't: brew raises Interrupt, removes a half-poured
// new version, and its `ensure` relinks the old one (Homebrew install.rb `install_formula`,
// formula_installer.rb `pour`). So "Stop" waits for the next package boundary and sends SIGINT
// there; everything already upgraded stays upgraded, the package that was about to start
// doesn't, and nothing is left half-installed.
pub(crate) static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

// pid of the update process currently streaming output (0 = none).
pub(crate) static RUNNING_CHILD: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

// SIGINT first. A process can start with SIGINT *ignored* (inherited from whatever launched us,
// e.g. any background job) and then can't un-ignore it; if it hasn't exited 3s later, send one
// SIGTERM, which Ruby turns into a SignalException that unwinds through the same cleanup and
// relink code. One signal only: Homebrew guards its cleanup against a second interrupt.
pub(crate) fn interrupt_running_child() {
    let pid = RUNNING_CHILD.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    // The child leads its own process group (see run_update_cmd_streaming), so `-pid` signals it
    // and everything it started (curl downloads, installers) — what Ctrl-C does in a terminal —
    // and nothing is left holding the output pipe open.
    let signal = move |sig: &str| {
        tracing::info!(pid, sig, "signalling update process group");
        let _ = Command::new("/bin/kill")
            .args([sig, "--", &format!("-{pid}")])
            .output();
    };
    signal("-INT");
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if RUNNING_CHILD.load(Ordering::SeqCst) == pid {
            signal("-TERM");
        }
    });
}

// Set while the tracked child is only downloading (the cask prefetch): nothing is installed yet,
// so Stop can interrupt it at once instead of waiting for a package boundary.
pub(crate) static DOWNLOADING: AtomicBool = AtomicBool::new(false);

// "Stop after this package": takes effect at the next package boundary — or straight away while
// the run is still downloading, where there is nothing half-installed to wait for.
#[tauri::command]
pub(crate) fn stop_updates() {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
    if DOWNLOADING.load(Ordering::SeqCst) {
        interrupt_running_child();
    }
}

// Set by the headless scheduled run. Nobody is at the keyboard, so update commands must never
// put up the askpass password dialog; anything needing admin fails fast and is reported instead.
pub(crate) static HEADLESS: AtomicBool = AtomicBool::new(false);

// --- One operation that changes the system at a time ---
//
// An update, an adoption, a rollback and Freshet updating itself must not overlap: they share
// STOP_REQUESTED and RUNNING_CHILD, two brews fight over Homebrew's locks, and a self-update
// restarts the app under whatever is running. The window disables its own buttons while one
// runs; this is the backend holding the same line, whatever the window does. Taken by the
// commands, not the functions they call, so tests can run those side by side.
static BUSY: AtomicBool = AtomicBool::new(false);

pub(crate) const BUSY_MESSAGE: &str =
    "Another update is already running. Try again when it finishes.";

pub(crate) const SCHEDULED_BUSY_MESSAGE: &str =
    "A scheduled update is running. Try again when it finishes.";

// Held for the length of an operation; dropping it lets the next one start.
pub(crate) struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

// None if another operation holds it.
pub(crate) fn begin_operation() -> Option<Busy> {
    BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .ok()
        .map(|_| Busy)
}

// --- Helper: run a shell command and capture stdout ---
// A Command for `cmd`. In the headless scheduled run it's wrapped in `taskpolicy -c background`:
// macOS then keeps it (and everything it starts — brew's curl, codesign, spctl) on the efficiency
// cores at low priority. Slower, but nobody is waiting on an unattended run, and it's what keeps
// a 40-package upgrade from heating the Mac up while you work.
pub(crate) fn command(cmd: &str) -> Command {
    let program = resolve_program(cmd);
    if HEADLESS.load(Ordering::Relaxed) {
        let mut c = Command::new("/usr/sbin/taskpolicy");
        c.args(["-c", "background", &program]);
        c
    } else {
        Command::new(program)
    }
}

// `brew` always runs from a fixed, trusted location, never "whatever is first on PATH": a fake
// `brew` planted earlier on PATH would otherwise be handed Freshet's password dialog (askpass)
// and every update. Other commands are looked up normally.
fn resolve_program(cmd: &str) -> String {
    if cmd != "brew" {
        return cmd.to_string();
    }
    // Tests point this at a stand-in brew. Debug builds only: a release build ignores it, so it
    // can't be used to redirect the real app.
    if cfg!(debug_assertions) {
        if let Some(path) = std::env::var_os("FRESHET_BREW") {
            return path.to_string_lossy().into_owned();
        }
    }
    trusted_brew().map_or_else(|| cmd.to_string(), |p| p.to_string_lossy().into_owned())
}

// The standard Homebrew prefixes, where the executable belongs to this user or root and isn't
// writable by anyone else.
fn trusted_brew() -> Option<PathBuf> {
    let me = own_uid()?;
    BREW_LOCATIONS.iter().map(PathBuf::from).find(|p| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && owned_safely(m.uid(), m.mode(), me))
    })
}

const BREW_LOCATIONS: [&str; 2] = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];

// Owned by `me` or root, and not group/world-writable. Pure → unit-tested.
const fn owned_safely(uid: u32, mode: u32, me: u32) -> bool {
    (uid == me || uid == 0) && mode & 0o022 == 0
}

// The current user's uid: the owner of their home directory.
pub(crate) fn own_uid() -> Option<u32> {
    static UID: OnceLock<Option<u32>> = OnceLock::new();
    *UID.get_or_init(|| std::fs::metadata(dirs::home_dir()?).ok().map(|m| m.uid()))
}

pub(crate) fn run_cmd(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = command(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run {cmd}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // Some tools (npm outdated) exit non-zero when items are outdated; treat stdout as truth
        if output.stdout.is_empty() {
            return Err(stderr);
        }
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// The GUI password helper sudo runs (SUDO_ASKPASS) when it needs admin rights and has no
// terminal: it shows a native macOS password dialog. It's created fresh for every command that
// might need it and deleted when that command finishes, rather than living in ~/.freshet where
// anything running as the user could swap in a version that keeps a copy of the password.
// Created with O_EXCL (create_new) and 0700 so an existing file or planted symlink can't be
// reused, then checked before use.
struct Askpass(PathBuf);

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

const ASKPASS_SCRIPT: &str = r#"#!/bin/bash
/usr/bin/osascript -e 'display dialog "Freshet needs your administrator password to update one or more apps." with title "Freshet" default answer "" with hidden answer with icon caution' -e 'text returned of result' 2>/dev/null
"#;

fn fresh_askpass() -> Option<Askpass> {
    fresh_askpass_in(&freshet_dir()?)
}

fn fresh_askpass_in(dir: &Path) -> Option<Askpass> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let path = dir.join(format!(
        "askpass-{}-{}-{}.sh",
        std::process::id(),
        unix_now(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)
        .ok()?;
    file.write_all(ASKPASS_SCRIPT.as_bytes()).ok()?;
    drop(file);
    let guard = Askpass(path);
    let meta = std::fs::symlink_metadata(&guard.0).ok()?;
    let ok = meta.is_file()
        && meta.mode() & 0o777 == 0o700
        && own_uid() == Some(meta.uid())
        && std::fs::read_to_string(&guard.0).ok()? == ASKPASS_SCRIPT;
    ok.then_some(guard)
}

// mas can't update App Store apps acquired under a different Apple ID (or installed outside the
// App Store). Translate its cryptic "No downloads initiated" error into something actionable.
fn maybe_append_mas_hint(mut output: String) -> String {
    if output.contains("No downloads initiated for ADAM ID") {
        output.push_str(
            "\n\nThis App Store app was purchased under a different Apple ID, or installed \
             outside the App Store, so it can't be updated here. Update it via the app's own \
             updater (e.g. Microsoft AutoUpdate: Help → Check for Updates), or delete it and \
             reinstall from the App Store while signed in with your Apple ID.",
        );
    }
    output
}

// Merge a process's stdout and stderr into one log blob.
pub(crate) fn combine_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut combined = String::from_utf8_lossy(stdout).to_string();
    let err = String::from_utf8_lossy(stderr);
    if !err.trim().is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
    combined
}

// --- Helper: run an upgrade command, returning whether it succeeded plus combined output ---
// The askpass guard must outlive the command: dropping it deletes the helper.
fn update_command(cmd: &str, args: &[&str]) -> (Command, Option<Askpass>) {
    let mut command = command(cmd);
    command.args(args);
    command.env_remove("SUDO_ASKPASS");
    let askpass = if HEADLESS.load(Ordering::Relaxed) {
        None
    } else {
        fresh_askpass()
    };
    if let Some(a) = &askpass {
        command.env("SUDO_ASKPASS", &a.0);
    }
    (command, askpass)
}

// Like run_update_cmd, but stdout and stderr share one pipe (as in a terminal), so a batch's
// error lines stay next to the package they belong to; and each line is handed to `on_line` as
// it arrives, so progress can be reported while a long batch runs.
pub(crate) fn run_update_cmd_streaming(
    cmd: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    fn read_lines(
        reader: std::io::PipeReader,
        all: &mut String,
        on_line: &mut dyn FnMut(&str),
    ) -> std::io::Result<()> {
        use std::io::BufRead as _;
        let mut reader = std::io::BufReader::new(reader);
        let mut buf = Vec::new();
        while reader.read_until(b'\n', &mut buf)? > 0 {
            let line = String::from_utf8_lossy(&buf);
            on_line(line.trim_end());
            all.push_str(&line);
            buf.clear();
        }
        Ok(())
    }
    let mut run = || -> std::io::Result<(bool, String)> {
        let (reader, writer) = std::io::pipe()?;
        let (mut command, _askpass) = update_command(cmd, args);
        command
            .envs(envs.iter().copied())
            .stdin(std::process::Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            // Its own process group, so Stop can signal brew and all its children together.
            .process_group(0);
        let mut child = command.spawn()?;
        RUNNING_CHILD.store(child.id(), Ordering::SeqCst);
        // The Command still holds write ends of the pipe; drop it so EOF arrives at child exit.
        drop(command);
        let mut all = String::new();
        // The reader is dropped before waiting, so a child still writing after a read error gets
        // EPIPE instead of blocking on a full pipe; and the child is always reaped and the slot
        // cleared, whatever the read did.
        let read = read_lines(reader, &mut all, on_line);
        let status = child.wait();
        RUNNING_CHILD.store(0, Ordering::SeqCst);
        read?;
        Ok((status?.success(), all.trim().to_string()))
    };
    run().unwrap_or_else(|e| (false, format!("Failed to run {cmd}: {e}")))
}

pub(crate) fn run_update_cmd(cmd: &str, args: &[&str]) -> (bool, String) {
    let (mut command, _askpass) = update_command(cmd, args);
    match command.output() {
        Ok(out) => {
            let combined = maybe_append_mas_hint(combine_output(&out.stdout, &out.stderr));
            (out.status.success(), combined.trim().to_string())
        }
        Err(e) => (false, format!("Failed to run {cmd}: {e}")),
    }
}

pub(crate) fn elapsed_ms(since: std::time::Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

// mas 7 needs root to install App Store updates and gets it by running `sudo` itself — without
// `-A`, so it ignores SUDO_ASKPASS and, with no terminal, fails instantly ("a terminal is required
// to read the password"). Its README's supported route is to start it as root: so Freshet runs
// `sudo -A <mas> …` itself, and `-A` makes sudo ask through Freshet's native password dialog. The
// absolute path is needed because sudo replaces PATH with its secure_path (no /opt/homebrew/bin).
// sudo remembers the password per parent process for a few minutes, so several App Store updates
// in one run ask once. The headless scheduled run never updates App Store apps, so it never gets
// here; elsewhere commands pass through unchanged.
//
// Because it runs as root, `mas` — like brew — is only ever taken from a standard Homebrew bin
// directory, never "whatever is first on PATH". PATH comes from the login shell and usually has
// user-writable directories ahead of Homebrew's (nvm's global bin, ~/.local/bin): a global npm
// package that ships a binary called `mas` would otherwise be run as root the moment the
// password is typed. A `mas` anywhere else isn't elevated; it runs as the user and fails the way
// it does in a terminal-less shell.
pub(crate) fn with_root_if_needed(cmd: &str, args: Vec<String>) -> (String, Vec<String>) {
    if cmd != "mas" || HEADLESS.load(Ordering::Relaxed) {
        return (cmd.to_string(), args);
    }
    if let Some(mas) = trusted_mas() {
        sudo_askpass_command(&mas, args)
    } else {
        tracing::warn!("no mas in a Homebrew bin directory; not running it as root");
        (cmd.to_string(), args)
    }
}

const MAS_LOCATIONS: [&str; 2] = ["/opt/homebrew/bin/mas", "/usr/local/bin/mas"];

fn trusted_mas() -> Option<PathBuf> {
    first_trusted_executable(&MAS_LOCATIONS.map(PathBuf::from), own_uid()?)
}

// The first candidate that is an executable file owned by `me` or root and not writable by
// anyone else (see owned_safely). Pure apart from the file checks → tested.
fn first_trusted_executable(candidates: &[PathBuf], me: u32) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| {
                m.is_file() && m.mode() & 0o111 != 0 && owned_safely(m.uid(), m.mode(), me)
            })
        })
        .cloned()
}

// `/usr/bin/sudo -A <program> <args…>`. Pure → unit-tested.
fn sudo_askpass_command(program: &Path, args: Vec<String>) -> (String, Vec<String>) {
    let mut full = vec!["-A".to_string(), program.to_string_lossy().into_owned()];
    full.extend(args);
    ("/usr/bin/sudo".to_string(), full)
}

pub(crate) fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(crate) fn find_brew() -> Option<PathBuf> {
    trusted_brew()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::store::freshet_dir;
    use crate::test_support::CHILD_SLOT;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;

    #[test]
    fn applescript_string_escapes_quotes() {
        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn mas_hint_only_on_that_error() {
        let with_err =
            maybe_append_mas_hint("Error: No downloads initiated for ADAM ID 462058435.".into());
        assert!(with_err.contains("different Apple ID"));
        let clean = maybe_append_mas_hint("Installing Microsoft Excel... done".into());
        assert_eq!(clean, "Installing Microsoft Excel... done");
    }

    // ---- deep-audit edge cases / random flows ----

    #[test]
    fn combine_output_merges_streams() {
        assert_eq!(combine_output(b"out", b""), "out");
        assert_eq!(combine_output(b"", b"err"), "err");
        assert_eq!(combine_output(b"out", b"err"), "out\nerr"); // separator inserted
        assert_eq!(combine_output(b"out\n", b"err"), "out\nerr"); // no double newline
        assert_eq!(combine_output(b"", b"   "), ""); // whitespace-only stderr ignored
        assert_eq!(combine_output(b"", b""), "");
    }

    // Lines arrive one at a time while the process runs, stdout and stderr interleaved in order.
    #[test]
    fn streaming_runner_delivers_lines_in_order() {
        let _slot = CHILD_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut lines = Vec::new();
        let (ok, all) = run_update_cmd_streaming(
            "/bin/sh",
            &[
                "-c",
                "echo '==> Upgrading a'; echo oops >&2; echo '==> Upgrading b'; exit 3",
            ],
            &[],
            &mut |l| lines.push(l.to_string()),
        );
        assert!(!ok);
        assert_eq!(lines, ["==> Upgrading a", "oops", "==> Upgrading b"]);
        assert_eq!(all, "==> Upgrading a\noops\n==> Upgrading b");
    }

    // Stop's mechanism end to end: at a package boundary the running process gets SIGINT, and
    // gets to run its interrupt handler (as brew's Ruby `Interrupt` → cleanup/relink does) —
    // so the next package never starts.
    #[test]
    fn stop_interrupts_at_a_boundary_and_lets_the_process_clean_up() {
        let _slot = CHILD_SLOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Traps both: wherever SIGINT is ignored at start (as in many CI/background contexts),
        // this exercises the SIGTERM fallback.
        let script = "trap 'echo cleaned-up; exit 130' INT TERM; \
                      echo '==> Upgrading a'; sleep 0.2; \
                      echo '==> Upgrading b'; sleep 10 & wait $!; \
                      echo '==> Upgrading c'";
        let started = std::time::Instant::now();
        let mut lines = Vec::new();
        let (ok, _) = run_update_cmd_streaming("/bin/bash", &["-c", script], &[], &mut |l| {
            lines.push(l.to_string());
            if l == "==> Upgrading b" {
                interrupt_running_child();
            }
        });
        assert!(!ok);
        assert_eq!(lines, ["==> Upgrading a", "==> Upgrading b", "cleaned-up"]);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(8),
            "didn't wait out the sleep"
        );
        assert_eq!(
            RUNNING_CHILD.load(Ordering::SeqCst),
            0,
            "child cleared after exit"
        );
    }

    // App Store updates run as `sudo -A <absolute mas> upgrade <id>`, so the password comes from
    // Freshet's dialog (mas's own sudo call can't use one).
    #[test]
    fn app_store_updates_get_root_through_the_askpass_dialog() {
        let (cmd, args) = sudo_askpass_command(
            Path::new("/opt/homebrew/bin/mas"),
            vec!["upgrade".into(), "462058435".into()],
        );
        assert_eq!(cmd, "/usr/bin/sudo");
        assert_eq!(
            args,
            ["-A", "/opt/homebrew/bin/mas", "upgrade", "462058435"]
        );
    }

    // What may be run as root: an executable that is ours (or root's) and that nobody else can
    // rewrite — from the fixed list of places only, in order.
    #[test]
    fn only_a_safely_owned_executable_is_trusted_to_run_as_root() {
        let dir = std::env::temp_dir().join(format!("freshet-trusted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::fs::metadata(&dir).unwrap().uid();
        let file = |name: &str, mode: u32| {
            let p = dir.join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            p
        };
        let not_executable = file("plain", 0o644);
        let world_writable = file("loose", 0o777);
        let group_writable = file("shared", 0o775);
        let good = file("good", 0o755);
        let candidates = [
            dir.join("absent"),
            not_executable,
            world_writable,
            group_writable,
            good.clone(),
        ];
        assert_eq!(first_trusted_executable(&candidates, me), Some(good));
        assert_eq!(
            first_trusted_executable(&candidates, me + 1),
            None,
            "someone else's file is not trusted"
        );
        assert_eq!(
            first_trusted_executable(std::slice::from_ref(&dir), me),
            None,
            "a directory"
        );
        // Only the fixed Homebrew locations are ever considered, whatever PATH says.
        assert!(MAS_LOCATIONS
            .iter()
            .all(|p| p.starts_with("/opt/homebrew/bin/") || p.starts_with("/usr/local/bin/")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---------- #5 same-user tampering ----------

    #[test]
    fn executable_ownership_rules() {
        let me = 501;
        assert!(owned_safely(me, 0o100_755, me));
        assert!(owned_safely(0, 0o100_755, me), "root-owned is fine");
        assert!(!owned_safely(502, 0o100_755, me), "another user's brew");
        assert!(!owned_safely(me, 0o100_775, me), "group-writable");
        assert!(!owned_safely(me, 0o100_757, me), "world-writable");
    }

    // The password helper is created per use: owner-only, exactly Freshet's script, a new file each
    // time, and gone as soon as the command that used it is done.
    #[test]
    fn askpass_is_fresh_private_and_removed_after_use() {
        let dir = std::env::temp_dir().join(format!("freshet-askpass-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = fresh_askpass_in(&dir).unwrap();
        let b = fresh_askpass_in(&dir).unwrap();
        assert_ne!(a.0, b.0, "a new file for every use");
        let meta = std::fs::metadata(&a.0).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700);
        assert_eq!(std::fs::read_to_string(&a.0).unwrap(), ASKPASS_SCRIPT);
        let path = a.0.clone();
        drop(a);
        assert!(!path.exists(), "deleted when the command is done");
        drop(b);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "nothing left behind"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_commands_get_a_live_askpass_that_disappears_afterwards() {
        let (cmd, guard) = update_command("/usr/bin/true", &[]);
        let askpass = cmd
            .get_envs()
            .find(|(k, _)| *k == "SUDO_ASKPASS")
            .and_then(|(_, v)| v)
            .map(PathBuf::from)
            .expect("SUDO_ASKPASS set");
        assert!(askpass.is_file());
        assert!(askpass.starts_with(freshet_dir().unwrap()));
        drop(guard);
        assert!(!askpass.exists());
    }

    // brew never comes from PATH: a fixed Homebrew location (or, in debug builds only, the
    // FRESHET_BREW test override). Other programs pass through.
    #[test]
    fn brew_is_resolved_to_a_fixed_location() {
        assert_eq!(resolve_program("npm"), "npm");
        assert_eq!(resolve_program("/usr/bin/true"), "/usr/bin/true");
        if std::env::var_os("FRESHET_BREW").is_none() {
            let brew = resolve_program("brew");
            if let Some(trusted) = trusted_brew() {
                assert_eq!(brew, trusted.to_string_lossy());
                assert!(BREW_LOCATIONS.contains(&brew.as_str()));
            }
        }
    }

    // The backend's own "one at a time": a second operation is refused while the first holds
    // the guard, and can start once it's dropped.
    #[test]
    fn only_one_operation_at_a_time() {
        let first = begin_operation().expect("nothing else is running");
        assert!(begin_operation().is_none(), "second operation is refused");
        drop(first);
        let again = begin_operation();
        assert!(again.is_some(), "free again once the first finished");
    }
}
