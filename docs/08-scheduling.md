# 08 — Scheduling & Background Updates

## What it is

An optional daily **Homebrew** run (formulae + casks, then `brew cleanup`), executed by
**launchd** at a time the user picks. It is off by default. It installs exactly what the app
would list — skipped versions and (unless greedy is on) self-updating casks are left alone — and
in notify-only mode it installs nothing.

```
install_schedule(hour, minute)  → Err unless Homebrew exists and the app isn't App-Translocated
                                → writes ~/Library/LaunchAgents/com.freshet.daily.plist,
                                  ProgramArguments = [<this app's binary>, --run-scheduled]
                                → launchctl bootout (ignored) ; launchctl bootstrap gui/<uid>
                                → launchctl print — Err unless launchd really has the job
uninstall_schedule()            → launchctl bootout ; delete the plist
get_schedule()                  → installed = plist exists AND launchd has it loaded;
                                  time parsed back out of the plist
get_settings / save_settings    → ~/.freshet/settings.json
get_last_scheduled_run()        → ~/.freshet/last-run.json (the settings panel's "last run" line)
```

## Why launchd, not an in-app timer

A menu-bar app can only run a timer while it is running. launchd runs whether Freshet is open,
closed, or has never been launched since boot, and it is the platform's supported mechanism for
exactly this. A `LaunchAgent` in `~/Library/LaunchAgents` runs as the user, with no elevation
and no installer. See D17 in [13 — Decisions](13-decisions.md).

## One code path: `freshet --run-scheduled`

The job runs the app's own binary in a headless mode. `main()` checks for the flag before
starting Tauri and calls `run_scheduled()`, which:

1. `brew update` — metadata only, so it runs in notify-only mode too. Skipped if any Freshet
   process ran it within the hour (`brew-update.stamp`).
2. `check_brew()` + `check_casks()` — the same checks the app's list uses.
3. `plan_scheduled_run(categories, settings)` — pure, tested; mirrors `allDisplayedSelections`
   in `src/logic.ts`, restricted to `brew`/`cask`.
4. Notify-only: log the list, install nothing, skip cleanup.
   Otherwise: `install_selections()` (the same per-item install + prefetch the app uses),
   `brew cleanup`, then `verify_updated_casks_blocking()` for signer drift.
5. Weekly (`full-scan.stamp`), a full signature scan of /Applications for publisher changes made
   outside Homebrew; results join `signer_changed`.
6. Writes `~/.freshet/last-run.json`, appends to `~/.freshet/run.log`, and posts a macOS
   notification with the outcome. Exit status is non-zero if anything failed, needs a password,
   or changed signer — `launchctl print` shows it as `last exit code`.

An earlier version ran a generated `~/.freshet/run.sh` that blanket-ran `brew upgrade`,
`mas upgrade`, `npm update -g` and `nvm install --lts`, ignoring notify-only and skipped
versions. Installing or removing the schedule deletes that legacy script if present.

### Shared settings

Skipped versions, notify-only and greedy used to live only in the webview's `localStorage`,
which a launchd job cannot read. They now live in `~/.freshet/settings.json` (written atomically,
0600). The frontend writes every change through `save_settings`; on first launch after the
upgrade, when the file doesn't exist, it migrates its `localStorage` values into it.
`ignore_key` in Rust must stay identical to `ignoreKey` in `logic.ts`.

### Before the first run

Enabling the schedule while updates are pending (and notify-only is off) asks for confirmation,
stating how many Homebrew packages the first run will upgrade (`scheduledSelections` in
`logic.ts`) — so nobody wakes up to dozens of surprise upgrades. Skipping versions or turning on
notify-only first changes that number.

## PATH — the bug that started this

launchd, and macOS when it opens an app from Finder, start processes with
`PATH=/usr/bin:/bin:/usr/sbin:/sbin`: no Homebrew, no nvm. The original script looked `brew` up
by name, so under launchd every scheduled run silently did nothing. The app had the same latent
problem when launched from Finder rather than `pnpm tauri dev`.

`augment_path()` runs first thing in `main()`. If PATH has no Homebrew prefix on it, it asks the
user's login shell for its PATH (`$SHELL -ilc`, 3-second timeout, output taken from between
markers so shell banners don't matter) and merges that in, followed by the known Homebrew
prefixes as a fallback. The result is the same tools, in the same order, as the user's terminal
— including which Homebrew wins on a machine that has both an Apple Silicon (`/opt/homebrew`)
and a leftover Intel (`/usr/local`) install.

## Elevation in the background

A scheduled run has no user present. In headless mode `run_update_cmd` removes `SUDO_ASKPASS`,
so a cask needing admin rights can't put a password dialog on an unattended screen; sudo fails
immediately ("a terminal is required to read the password"). `needs_admin()` recognises that
output and the item is reported as **needs your password** — in the notification and the
settings panel — rather than as a generic failure, so the user can finish it from the app.

**A background job should never acquire privilege silently, and should never block on a dialog
nobody is looking at.** A privileged helper tool is the macOS-sanctioned alternative and a
substantial security undertaking — not a small change.

## Validation and failure modes

`valid_schedule_time(hour, minute)` (`hour ≤ 23 && minute ≤ 59`) gates `install_schedule` and
`parse_schedule_time`. Every `launchctl` call checks its exit status — `Command::status()` alone
only fails if launchctl can't be spawned, which is how a failed `load` once went unnoticed. The
frontend shows errors under the toggle and never flips it optimistically.

- **No Homebrew.** `install_schedule` returns `Err`; toggle stays off.
- **App run from Downloads (App Translocation).** The binary's path is a temporary mount;
  `install_schedule` refuses and asks the user to move Freshet to Applications.
- **App moved or deleted after scheduling.** launchd can't start the job; the error is in
  `~/.freshet/launchd.err.log`. Re-enabling the toggle rewrites the path.
- **`bootstrap` right after `bootout`** can fail with `Input/output error` while the old instance
  is torn down; install retries for up to ~2s.
- **Machine asleep at the scheduled time.** launchd runs the job on wake.
- **Log growth.** `run.log` is append-only. The headless run logs one line per success and full
  output only for failures, so growth is slow, but there is no rotation (C6).

## Testing

- **Unit (pure):** `plan_scheduled_run` (skipped, per-version skips, greedy, kinds),
  `needs_admin` (real sudo output), `summarize_results`, `summary_message`, settings JSON
  round-trip, PATH helpers (`path_has_brew_prefix`, `extract_marked_path`, `merge_path`),
  `render_plist` (`plutil -lint` + round-trip), `is_translocated`, `valid_schedule_time`,
  `parse_schedule_time`. Frontend: `toSettings`, `scheduledSelections`, `lastRunText`.
- **Binary integration** (`src-tauri/tests/scheduled_run.rs`): runs the real
  `freshet --run-scheduled` with launchd's environment, a temp `HOME`, and a fake `brew` that
  records its calls. Covers: only listed items are upgraded, `brew update` runs once, cleanup
  runs, a sudo failure is reported as needs-admin with exit 1; greedy includes self-updating
  casks; notify-only upgrades and cleans nothing.
- **Real launchd** (ignored by default — registers a short-lived throwaway job):

  ```sh
  cd src-tauri && cargo test -- --ignored launchd_runs_installed_job
  ```

  Installs, re-installs at a new time, `kickstart`s, asserts launchd ran the program with
  `--run-scheduled` and its bare PATH, uninstalls, asserts it's gone.

By hand, against the real schedule:

```sh
launchctl print gui/$(id -u)/com.freshet.daily | grep -E 'state|last exit'
cat ~/.freshet/run.log ~/.freshet/last-run.json
launchctl kickstart gui/$(id -u)/com.freshet.daily   # run now (really upgrades unless notify-only)
```
