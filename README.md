# mac-updater

A bash script and launchd schedule that automatically keeps your macOS dev environment up to date. Runs daily and logs everything.

## What it updates

- **Homebrew** — CLI tools, GUI applications (casks), and cleanup of old versions
- **Mac App Store** — apps installed from the App Store (requires [mas](https://github.com/mas-cli/mas))
- **nvm / Node.js** — installs the latest LTS, carries global packages over, updates npm
- **npm** — global packages on the active Node version
- **pnpm** — self-update and global packages (if installed)

Each block is independent — if a tool isn't installed, that block is skipped cleanly.

## Prerequisites

- macOS
- [Homebrew](https://brew.sh)
- [nvm](https://github.com/nvm-sh/nvm) (if you want Node updates)
- [mas](https://github.com/mas-cli/mas) (optional, for App Store updates): `brew install mas`

## Install

```bash
git clone https://github.com/seanturner06/mac-updater.git
cd mac-updater
chmod +x install.sh
./install.sh
```

This copies the update script to `~/scripts/`, installs the launchd plist, and schedules the job to run daily at 10 AM.

## Test it manually

```bash
~/scripts/update-all.sh
cat ~/scripts/update-all.log
```

## Customize the schedule

Edit the `Hour` and `Minute` values in the plist before running `install.sh`, or edit the installed copy at `~/Library/LaunchAgents/com.updateall.plist` and reload:

```bash
launchctl unload ~/Library/LaunchAgents/com.updateall.plist
launchctl load ~/Library/LaunchAgents/com.updateall.plist
```

## Useful commands

```bash
# Trigger a run right now
launchctl start com.updateall

# Check the job is registered
launchctl list | grep updateall

# Check the log
cat ~/scripts/update-all.log

# Check launchd errors
cat ~/scripts/launchd.err.log
```

## Uninstall

```bash
launchctl unload ~/Library/LaunchAgents/com.updateall.plist
rm ~/Library/LaunchAgents/com.updateall.plist
rm ~/scripts/update-all.sh
rm ~/scripts/update-all.log
rm ~/scripts/launchd.out.log
rm ~/scripts/launchd.err.log
```

## How it works

The script runs each update block in order. If any single command fails, it logs the error and continues to the next block rather than aborting. The launchd plist schedules the script to run daily — if your Mac is asleep at the scheduled time, launchd runs it at the next wake.

All output goes to `~/scripts/update-all.log` so you can review what happened on unattended runs.
