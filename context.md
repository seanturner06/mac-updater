# Freshet — User Stories

Twenty flows through the app, written for product/design/test reference. Format:

> **As a** [persona], **I want** [action], **so that** [value].

Personas:
- **Dev** — developer-power-user, the primary target
- **First-timer** — fresh install, no prior state
- **Security** — security-conscious user; cares about supply-chain integrity
- **Maintainer** — someone tracking many apps across sources

---

## 1. First launch — silent adoption
**As a** First-timer, **I want** Freshet to catalogue my currently installed apps the first time I open it, **so that** I don't have to configure anything before it's useful.

- Tray icon appears on launch with no setup wizard.
- Existing Homebrew formulae, casks, mas apps, and npm globals are detected without prompts.
- Currently-installed app signatures are recorded as the trust baseline (so later drift can be detected).
- No outdated count is shown until a check has run.

## 2. Manual check from tray
**As a** Dev, **I want** to click the tray icon and press "Check now," **so that** I can see what's outdated on demand.

- Tray click opens the window anchored to the tray icon.
- "Check now" runs in the background; UI shows progress, not a frozen spinner.
- Results stream in per source (brew, casks, mas, npm, Sparkle).
- A failure in one source does not block results from others.

## 3. One-click "Update all"
**As a** Dev, **I want** to update everything outdated with a single button, **so that** I don't have to confirm each app.

- Single button runs every outdated update across all sources.
- Progress is reported per app.
- Self-updating apps (Chrome, Firefox, etc.) are skipped — not retried via brew.
- A summary view shows successes, failures, and any signer-drift warnings.

## 4. Selective update via checkboxes
**As a** Maintainer, **I want** to check off specific apps and update only those, **so that** I can defer risky upgrades.

- Each outdated row has a checkbox.
- "Update selected" only runs the checked items.
- Unchecked items remain in the outdated list afterward.
- Selection state is not persisted across sessions (intentional — fresh decision each time).

## 5. Ignore an app
**As a** Dev, **I want** to mark an app as "ignored," **so that** Freshet stops showing it as outdated.

- Ignore action is one click from the app row.
- Ignored apps disappear from outdated lists immediately.
- Ignored list persists across launches.
- Future checks do not re-add ignored apps.

## 6. Unignore an app
**As a** Dev, **I want** to view and restore ignored apps, **so that** I can change my mind later.

- A separate "Ignored" view lists everything currently ignored.
- One click restores an app to the active list.
- After restoring, the next check evaluates it normally.

## 7. Inspect app details
**As a** Maintainer, **I want** to see the current version, latest version, source, and homepage for a given app, **so that** I can decide whether to update.

- Click an app row to expand or open a detail view.
- Shows installed version, available version, source (brew/cask/mas/npm/Sparkle), and link to homepage/source.
- Information is cached and updated as part of the check, not re-fetched on every click.

## 8. Signer-drift warning after an update (the hijack alarm)
**As a** Security user, **I want** Freshet to alert me when a just-updated app is signed by a different developer than the prior version, **so that** I can catch a supply-chain hijack like 3CX / HandBrake / Transmission.

- Targeted signature verification runs automatically after every `run_updates` call.
- Only the cask tokens just upgraded are inspected (resolved token -> .app filename via Homebrew JWS metadata).
- A drift result (Team ID or CN change) is surfaced as a prominent warning in the post-update summary.
- A drift result does NOT auto-rollback; the user sees the warning and decides.

## 9. On-demand full signature inventory
**As a** Security user, **I want** an explicit "Verify signatures" button that inspects every installed app, **so that** I can run a full audit on demand.

- Button is reachable from the main window (not buried).
- Runs against the full app inventory, not just outdated.
- Shows both changed signatures (drift) and unsigned/quarantined apps.
- Does not block other UI; reports when complete.

## 10. Scheduled background check
**As a** Dev, **I want** Freshet to check for updates on a schedule (e.g., daily), **so that** I don't have to remember.

- A schedule can be configured (time of day).
- The schedule fires while the app is running in the menu bar.
- Tray badge updates silently when results land.
- Schedule range is validated (no nonsense times).

## 11. Tray badge reflects outdated count
**As a** Dev, **I want** the tray icon to show how many updates are pending, **so that** I get a peripheral cue without opening the window.

- Badge number equals the outdated count across all sources.
- Self-updating apps are excluded from the count.
- Ignored apps are excluded from the count.
- Badge clears when there are zero outdated.

## 12. Drag the window from top or bottom
**As a** Dev, **I want** to reposition the menu-bar window by dragging either the header strip or the footer strip, **so that** I can move it out of the way of other windows.

- Header is a drag region.
- Footer is a drag region.
- The actions row (Update Selected / Update All) is NOT a drag region — buttons still click.
- Drag works without holding modifier keys.

## 13. Window re-anchors to tray on reopen
**As a** Dev, **I want** the window to reappear under the tray icon after I close it, **so that** I don't have to find it again.

- Closing the window does not quit the app.
- Clicking the tray icon shows the window at tray-center position.
- Anchoring works across multiple monitors.
- Previous drag-position is not preserved on reopen (intentional — anchored is the default).

## 14. Open an app's homepage / source
**As a** Maintainer, **I want** to click through to an app's website from inside Freshet, **so that** I can read release notes before updating.

- Each app row exposes a link to its homepage or source.
- Click opens in the default browser via the OS, not in-app.
- Only URLs that pass validation (https, non-local host) are opened.

## 15. Update a Sparkle-direct app
**As a** Dev, **I want** Sparkle-based apps (those not under brew) to be updated via their appcast, **so that** I don't have to manually download DMGs.

- Outdated Sparkle apps appear in the same list as brew/mas/npm.
- Their appcast feed URL is fetched and parsed.
- The SUFeedURL is validated against the allowlist (https + non-private host) before any network call.
- The update is installed in-place; result is reported the same as any other source.

## 16. Update an npm global package
**As a** Dev, **I want** outdated npm global packages to be detected and upgraded, **so that** my CLI tools stay current alongside my Mac apps.

- `npm outdated -g --json` (or equivalent) provides the outdated list.
- Selected npm globals are upgraded via `npm i -g` per package.
- Failures (e.g., permission errors) are reported per package, not as a single all-or-nothing.

## 17. Update a Mac App Store app
**As a** Dev, **I want** Mac App Store apps to be updated through Freshet, **so that** I don't have to open the App Store app separately.

- `mas outdated` provides the list.
- `mas upgrade <id>` runs for selected items.
- If the user is not signed into the App Store, Freshet surfaces a clear "sign in required" message instead of a cryptic error.

## 18. Self-updating apps are labeled and skipped
**As a** Dev, **I want** Chrome, Firefox, and other self-updating apps to be visibly excluded from updates, **so that** I'm not confused when they don't appear in the outdated list.

- Greedy / self-updating casks are detected from Homebrew cask metadata.
- They appear in the catalogue with a "self-updating" label.
- They are excluded from the outdated count and from "Update all."
- The user can still see their installed version for inventory purposes.

## 19. Malicious appcast URL is rejected
**As a** Security user, **I want** Freshet to refuse to fetch a Sparkle feed URL that points to a private/loopback/link-local address, **so that** a poisoned cask cannot SSRF the host machine.

- Before any network fetch, `is_safe_feed_url` validates: https scheme + non-private IPv4 (not 10/8, 172.16/12, 192.168/16, 127/8, 169.254/16, 100.64/10) + non-private IPv6 (not ULA fc00::/7, not link-local fe80::/10).
- A rejected URL produces a clear error in the UI — not a silent skip.
- The check runs regardless of TLS validity (TLS alone doesn't stop internal-IP SSRF).

## 20. Partial update failure with mixed results
**As a** Dev, **I want** Freshet to keep going if one update fails mid-batch, **so that** the rest of my updates still apply and I see exactly what failed.

- If app N fails, apps N+1..M still attempt.
- The summary clearly distinguishes succeeded / failed / skipped.
- Each failure shows the underlying error (e.g., brew conflict message, mas auth error).
- Failed apps remain in the outdated list and can be retried.

---

## Coverage notes

- **Security flows**: stories 8, 9, 19 cover the differentiator (signer drift) and the SSRF defense.
- **Source coverage**: stories 15–17 hit Sparkle / npm / mas explicitly. Brew formulae and casks are exercised throughout (2, 3, 4, 8).
- **UX/window behavior**: stories 12, 13 cover the menu-bar window mechanics (drag regions + tray anchoring).
- **Lifecycle**: story 1 (cold start), 10 (scheduled), 11 (passive badge), 20 (failure recovery).
- **Out of scope here** (intentional — not yet built): rollback / snapshot-before-update, telemetry opt-in flow, first-run trust acknowledgement for unrecognized signers, Setapp distribution.
