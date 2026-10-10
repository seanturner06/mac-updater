# 12 — Roadmap & Deferred Work

Status as of this writing: **v0.1, early development.** Core update flows, signer-drift
detection, adoption, and scheduling work today.

The sequencing principle is **useful-and-trusted first, defensible-later**. Anything that depends
on having users is worthless before there are users, so the order is: ship something genuinely
useful, earn trust with it, and let the harder-to-copy things accumulate underneath.

## v1 — Useful & trusted

Goal: people install it, and trust it with admin rights.

**Done:** the four local sources plus Sparkle detection; selective and "update all" flows; the
greedy toggle; notify-only mode; per-version skip; the results panel with real tool output;
Homebrew adoption (suggestions + search + batch); signer-drift detection in all three modes; the
launchd schedule; the native sudo dialog; the tray badge; the strict lint floor and CI.

**Remaining, in priority order:**

1. **Apple Developer ID signing + notarization.** This is the floor of the trust story, not a
   release chore. An unsigned app asking for your admin password is not a thing a
   security-conscious user should install, and this app's entire pitch is aimed at
   security-conscious users. Nothing else in v1 matters as much.
2. ~~**Fix the scheduled-run divergence.**~~ Done: the scheduled job is now
   `freshet --run-scheduled`, sharing the app's code path and settings (C1–C3, C5, C7 below).
3. **Per-item update progress.** A twelve-item batch shows "updating…" and nothing else for
   several minutes. The `verify-progress` event pattern already exists to copy.
4. **Deploy the landing page and open the waitlist.** `landing/` is built (self-contained static
   HTML/CSS/JS, no build step, reusing the app's palette and fonts) but not deployed, and the
   waitlist form still points at a placeholder endpoint.

## v2 — Safety features

Goal: the reasons to choose Freshet over a shell alias. All client-side, all valuable with zero
other users.

- **One-click rollback.** Snapshot the `.app` bundle before replacing it; restore on one click.
  This is the cure to signer-drift's detection: "an update broke my app" is the single most
  common reason people abandon an updater, and an undo button removes the fear that makes
  updating feel risky. **Explicitly deferred** — revisit only when the user does. Two real
  caveats to design around: bundle-level rollback is reliable but already-migrated app *data* and
  preferences generally are not reversible, and snapshots of large bundles cost real disk. The
  token → installed `.app` mapping this needs already exists (reverse the adoption index).
- **Usage-aware scheduling.** Don't replace an app while it is running — don't update Docker
  mid-build.
- **Background scheduled verification.** Today nothing fires unless the user runs an update or
  presses the button. A daily or weekly background scan would catch software that arrived by
  paths Freshet didn't drive: direct DMGs, sideloaded bundles, malware.
- **"I trust this change" acknowledgement.** Legitimate signer changes — acquisitions, developer
  account migrations, certificate reissues — currently look identical to a hijack, and the only
  way to clear one is to run verification again, which accepts the new ID silently. This is the
  most valuable unbuilt piece of the provenance feature: **every false alarm trains the user to
  ignore the real one.**
- **Opt-in, anonymous telemetry.** Strictly opt-in and invisible by default. This audience values
  local-first and privacy explicitly, and the leading competitor markets on exactly that, so
  telemetry can never be the headline. It is here because data has to start accumulating long
  before it can serve anything.

## v3 — Flywheel & service

Requires v2 telemetry running and a v1 userbase. **The dependency chain is hard; this cannot be
pulled forward.**

- **Update-health warnings** — "3 of 40 users saw this version fail on macOS 15" at the moment of
  decision.
- **Curated safe-to-auto-update tiers and a known-bad-version blocklist.**
- **A hosted service** — health database, security advisories, optional cross-device sync. Kept
  thin, and the client must degrade gracefully offline.

## Correctness work outstanding

Higher priority than most features, because each of these is a promise the product currently
breaks.

| # | Issue | Impact |
|---|---|---|
| ~~C1~~ | **Fixed.** ~~The scheduled run ignores notify-only.~~ A user who turned notify-only on specifically so nothing gets installed will have things installed. | High — a broken promise, in the security-conscious direction |
| ~~C2~~ | **Fixed.** ~~The scheduled run ignores skipped versions.~~ A version you explicitly skipped gets installed overnight. | High |
| ~~C3~~ | **Fixed** (scope is now Homebrew + `brew cleanup`). ~~The scheduled run does extra things.~~ `nvm install --lts`, `pnpm self-update`, `brew cleanup` — changing the machine's Node version is not implied by "auto-update daily." | Medium |
| C4 | **`mas` elevation fails silently.** `mas` calls `sudo` without `-A`, so it ignores `SUDO_ASKPASS` and never shows the password dialog. The item fails cleanly but the user sees a cryptic message. Fix: either a mas-specific hint alongside `maybe_append_mas_hint`, or invoke `mas` under `sudo -A`. | Medium |
| ~~C5~~ | **Fixed** (no script any more; a legacy one is removed). ~~`uninstall_schedule` leaves `~/.freshet/run.sh` behind.** Inert, but it is residue an inspecting user will find. | Low |
| C6 | **`run.log` has no rotation.** Grows without bound. | Low |
| ~~C7~~ | **Fixed.** ~~`.expect("home dir")` in `plist_path`/`script_path`** panics where everything else degrades. | Low |
| ~~C8~~ | **Fixed** (keyed by bundle id; name keys migrate on first scan). ~~The signing store is keyed by app name.~~ A rename silently re-seeds the baseline; identical file stems collide. Bundle identifier would be strictly better. | Low, but it weakens the headline feature |
| ~~C9~~ | **Fixed.** ~~Stale header comment in `main.rs`.~~ | Trivial |

| ~~C10~~ | **Fixed** (public `brew casks` + name-derived candidates confirmed by one `brew info`; per-cask fallback when one cask fails to load). ~~"Suggest adoptions" finds nothing on Homebrew 7.~~ It reads `api/cask.jws.json`, which Homebrew 7 replaced with an internal indexed `api/internal/packages.*.jws.json` (and disabled `--eval-all`). The signature features were moved to the public `brew info --cask`; adoption needs *all* casks, which no public command lists any more. | Medium |

C1–C3 were fixed together by reimplementing the scheduled job as a headless mode of the app
binary. See [08 — Scheduling](08-scheduling.md).

## Deferred sources

- **GitHub Releases.** The most-requested missing source and table stakes against competitors.
  The hard part is mapping an installed app to its repo, and the only acceptable answers are
  "its Sparkle feed already points at GitHub" or "the user tells us" — a curated app→repo map
  violates the core scope rule. Also needs a rate-limit answer (60 req/hour unauthenticated) 
  before it is usable. Full design sketch in [03 — Update Sources](03-update-sources.md).
- **Electron app detection.** Competitors do it; the method is undocumented.
- **Tauri app detection.** Nobody does it. Genuinely open niche.

## Engineering debt

- **`tracing` instrumentation.** The highest-value performance work: `#[instrument]` on the
  command entry points so a slow check can be attributed to a specific subprocess rather than
  guessed at. See [11 — Performance](11-performance.md).
- **A pre-commit hook (lefthook).** The missing quality gate. Fast feedback on the checks CI
  already runs.
- **Absolute paths for system tools** — `/usr/bin/codesign`, `/usr/bin/curl`, `/usr/bin/spctl`,
  `/usr/bin/defaults`, `/bin/launchctl`. Cheap hardening against `PATH` manipulation.
- **Split `main.rs`** into modules — when a second contributor is regularly editing a different
  concern, not before. Seams identified in [04 — Backend](04-backend.md).
- **Overlay accessibility** — `Escape` to close, focus trap, `aria-modal`, `aria-label` coverage.
  Small, well-defined, and a good first contribution.
- **Light mode.** The token structure supports it; see
  [09 — UI Design System](09-ui-design-system.md) for the gotchas.
- **Extend `list_applications()`** beyond `/Applications` to `~/Applications` and nested bundles.

## Explicitly out of scope

Not "not yet" — **no**. Proposing one of these needs an argument against the reasoning in
[01 — Product Design](01-product-design.md), not just a use case.

- **A curated app-version database.** The trap that killed the category leader.
- **Downloading or executing update artifacts directly.** This is the load-bearing security
  boundary, not a limitation.
- **Cross-platform support.** Every mechanism in the app is macOS-specific.
- **Becoming a general Homebrew GUI.** Browsing, installing, uninstalling.
- **Silent unattended updating by default.**
- **Affiliate or referral revenue tied to update recommendations.** It would corrupt the only
  thing the product actually sells.

## Good first contributions

Small, well-scoped, and genuinely wanted:

- C5, C6, C7, C9 from the correctness table — each is a handful of lines.
- `aria-label` coverage and `Escape`-to-close on the overlays.
- Absolute paths for the system tools.
- Tokenise the two hardcoded colours in `.toggle-slider`.
- Additional parser fixtures from tool output shapes not yet covered — especially `mas` and
  Sparkle appcasts, where real-world output varies more than you would expect.
