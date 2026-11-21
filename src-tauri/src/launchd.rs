// The launchd job that starts the scheduled run.

use crate::process::find_brew;
use crate::store::{freshet_dir, remove_legacy_run_script};
use serde::Serialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

// --- Launchd schedule management ---
//
// The daily job is a launchd LaunchAgent that runs this app's own binary with --run-scheduled,
// so it works whether or not Freshet is open.
const SCHEDULE_LABEL: &str = "com.freshet.daily";

pub(crate) const RUN_SCHEDULED_FLAG: &str = "--run-scheduled";

const fn valid_schedule_time(hour: u32, minute: u32) -> bool {
    hour <= 23 && minute <= 59
}

// macOS runs a quarantined app opened from Downloads from a randomized read-only mount that
// vanishes later; a schedule pointing there would break. Pure → unit-tested.
fn is_translocated(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/AppTranslocation/")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn render_plist(label: &str, program: &[String], log_dir: &Path, hour: u32, minute: u32) -> String {
    let args = program.iter().fold(String::new(), |mut out, a| {
        let _ = write!(out, "\n        <string>{}</string>", xml_escape(a));
        out
    });
    let log_dir = xml_escape(&log_dir.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>{args}
    </array>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>{hour}</integer>
        <key>Minute</key><integer>{minute}</integer>
    </dict>
    <key>StandardOutPath</key><string>{log_dir}/launchd.out.log</string>
    <key>StandardErrorPath</key><string>{log_dir}/launchd.err.log</string>
</dict>
</plist>
"#
    )
}

// Run launchctl and turn a non-zero exit into an Err carrying its message. `Command::status()`
// alone only fails if launchctl can't be spawned, which is how a failed load used to go unnoticed.
fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "launchctl {} failed: {}",
            args.first().unwrap_or(&""),
            msg.trim()
        ))
    }
}

// The logged-in user's launchd domain, e.g. "gui/501".
fn gui_domain() -> Result<String, String> {
    let out = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err("could not determine user id".into());
    }
    Ok(format!("gui/{uid}"))
}

// One launchd job: its label, plist location, and the program it runs. The real one is
// `ScheduleJob::daily()`; tests use a temp dir and a throwaway label so they never touch the
// user's installed schedule.
struct ScheduleJob {
    label: String,
    plist: PathBuf,
    program: Vec<String>,
    log_dir: PathBuf,
}

impl ScheduleJob {
    fn daily() -> Result<Self, String> {
        let home =
            dirs::home_dir().ok_or_else(|| "could not resolve home directory".to_string())?;
        let log_dir = freshet_dir().ok_or_else(|| "could not resolve ~/.freshet".to_string())?;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Ok(Self {
            label: SCHEDULE_LABEL.into(),
            plist: home.join(format!("Library/LaunchAgents/{SCHEDULE_LABEL}.plist")),
            program: vec![
                exe.to_string_lossy().into_owned(),
                RUN_SCHEDULED_FLAG.into(),
            ],
            log_dir,
        })
    }

    fn service(&self) -> Result<String, String> {
        Ok(format!("{}/{}", gui_domain()?, self.label))
    }

    fn is_loaded(&self) -> bool {
        self.service()
            .is_ok_and(|svc| launchctl(&["print", &svc]).is_ok())
    }

    fn install(&self, hour: u32, minute: u32) -> Result<(), String> {
        if !valid_schedule_time(hour, minute) {
            return Err(format!("invalid schedule time {hour}:{minute}"));
        }
        if let Some(parent) = self.plist.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let plist = render_plist(&self.label, &self.program, &self.log_dir, hour, minute);
        std::fs::write(&self.plist, plist).map_err(|e| e.to_string())?;

        // Replace any previous load (e.g. the old time); "not loaded" is the expected error here.
        let domain = gui_domain()?;
        let _ = launchctl(&["bootout", &self.service()?]);
        let plist_str = self.plist.to_string_lossy();
        // bootout completes asynchronously; an immediate bootstrap can hit "Input/output error"
        // while the old instance is still being torn down, so retry briefly.
        let mut result = Err(String::new());
        for _ in 0..10 {
            result = launchctl(&["bootstrap", &domain, &plist_str]);
            if result.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        result?;
        // Trust launchd, not our own bookkeeping: the job must actually be registered.
        if self.is_loaded() {
            Ok(())
        } else {
            Err(format!("launchd did not register {}", self.label))
        }
    }

    fn uninstall(&self) -> Result<(), String> {
        let _ = self.service().map(|svc| launchctl(&["bootout", &svc]));
        if self.plist.exists() {
            std::fs::remove_file(&self.plist).map_err(|e| e.to_string())?;
        }
        if self.is_loaded() {
            return Err(format!("launchd still has {} loaded", self.label));
        }
        Ok(())
    }

    fn status(&self) -> ScheduleStatus {
        // A plist on disk that launchd hasn't loaded runs nothing, so it doesn't count.
        let installed = self.plist.exists() && self.is_loaded();
        let time = installed
            .then(|| std::fs::read_to_string(&self.plist).ok())
            .flatten()
            .and_then(|s| parse_schedule_time(&s));
        ScheduleStatus {
            installed,
            hour: time.map(|(h, _)| h),
            minute: time.map(|(_, m)| m),
        }
    }
}

// launchctl can take a couple of seconds (see ScheduleJob::install), so these run on the thread
// pool, not the main thread; the lock keeps an install and an uninstall from interleaving there.
static SCHEDULE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tauri::command(async)]
pub(crate) fn install_schedule(hour: u32, minute: u32) -> Result<(), String> {
    let _guard = SCHEDULE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if find_brew().is_none() {
        return Err("Homebrew not found in /opt/homebrew or /usr/local; install it first".into());
    }
    let job = ScheduleJob::daily()?;
    if job
        .program
        .first()
        .is_some_and(|exe| is_translocated(Path::new(exe)))
    {
        return Err("Move Freshet to your Applications folder before scheduling updates".into());
    }
    remove_legacy_run_script();
    job.install(hour, minute)
}

#[tauri::command(async)]
pub(crate) fn uninstall_schedule() -> Result<(), String> {
    let _guard = SCHEDULE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    remove_legacy_run_script();
    ScheduleJob::daily()?.uninstall()
}

// Read Hour/Minute back out of the launchd plist so the UI reflects the schedule that's
// actually installed after a relaunch, instead of a hardcoded default. Tolerates whitespace
// between the <key> and <integer> tags (a hand-edited plist). Pure → unit-tested.
fn parse_schedule_time(plist: &str) -> Option<(u32, u32)> {
    fn int_after(plist: &str, key: &str) -> Option<u32> {
        let marker = format!("<key>{key}</key>");
        let after = &plist[plist.find(&marker)? + marker.len()..];
        let open = after.find("<integer>")? + "<integer>".len();
        let rest = &after[open..];
        let end = rest.find("</integer>")?;
        rest[..end].trim().parse().ok()
    }
    let hour = int_after(plist, "Hour")?;
    let minute = int_after(plist, "Minute")?;
    valid_schedule_time(hour, minute).then_some((hour, minute))
}

#[derive(Serialize)]
pub(crate) struct ScheduleStatus {
    pub(crate) installed: bool,
    pub(crate) hour: Option<u32>,
    pub(crate) minute: Option<u32>,
}

#[tauri::command(async)]
pub(crate) fn get_schedule() -> ScheduleStatus {
    ScheduleJob::daily().map_or(
        ScheduleStatus {
            installed: false,
            hour: None,
            minute: None,
        },
        |job| job.status(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]
mod tests {
    use super::*;
    use crate::store::write_private;
    use crate::test_support::read;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // Round-trip: the plist install_schedule writes must parse back to the same time, so the
    // settings UI shows the real installed schedule after a relaunch (not the 10:00 default).
    #[test]
    fn schedule_time_parses_from_generated_plist() {
        let plist = r#"<?xml version="1.0"?>
<plist version="1.0">
<dict>
    <key>Label</key><string>com.freshet.daily</string>
    <key>StartCalendarInterval</key>
    <dict>
        <key>Hour</key><integer>14</integer>
        <key>Minute</key><integer>45</integer>
    </dict>
</dict>
</plist>"#;
        assert_eq!(parse_schedule_time(plist), Some((14, 45)));
        // Whitespace between key and integer (hand-edited plist) still parses.
        let spaced =
            "<key>Hour</key>\n  <integer>7</integer>\n<key>Minute</key> <integer>0</integer>";
        assert_eq!(parse_schedule_time(spaced), Some((7, 0)));
        // Missing keys, junk values, and out-of-range times are None, not garbage.
        assert_eq!(
            parse_schedule_time("<key>Hour</key><integer>9</integer>"),
            None
        );
        assert_eq!(
            parse_schedule_time(
                "<key>Hour</key><integer>x</integer><key>Minute</key><integer>0</integer>"
            ),
            None
        );
        assert_eq!(
            parse_schedule_time(
                "<key>Hour</key><integer>24</integer><key>Minute</key><integer>0</integer>"
            ),
            None
        );
        assert_eq!(parse_schedule_time(""), None);
    }

    #[test]
    fn schedule_time_range() {
        assert!(valid_schedule_time(10, 30));
        assert!(valid_schedule_time(0, 0));
        assert!(valid_schedule_time(23, 59));
        assert!(!valid_schedule_time(24, 0));
        assert!(!valid_schedule_time(10, 60));
    }

    #[test]
    fn translocated_exe_is_detected() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/Freshet.app/Contents/MacOS/freshet"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/Freshet.app/Contents/MacOS/freshet"
        )));
    }

    // --- launchd job ---

    fn schedule_test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("freshet test's {name} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rendered_plist_is_valid_and_round_trips() {
        let dir = schedule_test_dir("plist");
        let program = [
            "/Applications/A&B.app/Contents/MacOS/freshet".to_string(),
            RUN_SCHEDULED_FLAG.into(),
        ];
        let plist = render_plist("com.freshet.daily", &program, &dir, 7, 45);
        assert_eq!(parse_schedule_time(&plist), Some((7, 45)));
        assert!(plist.contains("<string>/Applications/A&amp;B.app/Contents/MacOS/freshet</string>"));
        assert!(plist.contains("<string>--run-scheduled</string>"));
        let path = dir.join("job.plist");
        std::fs::write(&path, &plist).unwrap();
        let lint = Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            lint.status.success(),
            "{}",
            String::from_utf8_lossy(&lint.stdout)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // End-to-end through the real launchd: install → registered → kickstart → launchd ran the
    // program with its args → uninstall → gone. Uses a throwaway label and a stand-in program
    // in a temp dir, so it never touches the user's real schedule or packages. Ignored by
    // default because it registers a (short-lived) job with the user's launchd; run it with:
    //   cargo test -- --ignored launchd_runs_installed_job
    #[test]
    #[ignore = "registers a throwaway job with the user's launchd"]
    fn launchd_runs_installed_job() {
        struct Uninstall<'a>(&'a ScheduleJob);
        impl Drop for Uninstall<'_> {
            fn drop(&mut self) {
                let _ = self.0.uninstall();
            }
        }

        let dir = schedule_test_dir("launchd");
        let program = dir.join("program");
        let calls = dir.join("calls");
        write_private(
            &program,
            &format!(
                "#!/bin/bash\necho \"$* PATH=$PATH\" >> '{}'\n",
                calls.display().to_string().replace('\'', r"'\''")
            ),
            true,
        )
        .unwrap();
        let job = ScheduleJob {
            label: format!("com.freshet.test.{}", std::process::id()),
            plist: dir.join("job.plist"),
            program: vec![
                program.to_string_lossy().into_owned(),
                RUN_SCHEDULED_FLAG.into(),
            ],
            log_dir: dir.clone(),
        };
        let guard = Uninstall(&job);

        job.install(3, 15).unwrap();
        let status = job.status();
        assert!(status.installed);
        assert_eq!((status.hour, status.minute), (Some(3), Some(15)));
        // Re-installing (changing the time while enabled) must replace the loaded job cleanly.
        job.install(4, 30).unwrap();
        assert_eq!(job.status().hour, Some(4));

        launchctl(&["kickstart", &job.service().unwrap()]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while read(&calls).is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // launchd invoked it with --run-scheduled, and with its bare PATH — which is why the
        // binary calls augment_path() before anything else.
        assert_eq!(
            read(&calls),
            "--run-scheduled PATH=/usr/bin:/bin:/usr/sbin:/sbin\n"
        );

        drop(guard);
        assert!(!job.is_loaded());
        assert!(!job.plist.exists());
        assert!(!job.status().installed);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
