# Freshet

A macOS menu-bar app that keeps *everything* on your Mac up to date from one place — Homebrew formulae, casks, Mac App Store apps, npm globals, and Sparkle-based apps — and watches for the supply-chain signal most updaters miss: **a new version signed by a different developer**.

Freshet doesn't build its own version database (the trap that killed MacUpdater). It orchestrates the registries your Mac already trusts — `brew`, `mas`, `npm`, and each app's own Sparkle appcast — so coverage stays current without a team curating versions by hand.

## Features

- **One list for every source.** Outdated Homebrew formulae, casks, App Store apps, npm globals, and self-updating Sparkle apps, checked in parallel and streamed into a single menu-bar window.
- **Update all, or exactly what you pick.** Per-item checkboxes, per-category select-all, and an "Update all" that runs precisely what's displayed — skipped versions and hidden self-updating apps are never touched.
- **Signer-drift detection (the hijack alarm).** After every update, Freshet re-checks the code signature of each upgraded app and warns if the Developer Team ID changed since the last version — the fingerprint of hijacked releases like 3CX, HandBrake, and Transmission. Gatekeeper alone doesn't catch this: a hijacked build can be validly signed and notarized, just by someone else.
- **Full signature inventory on demand.** Audit every app in /Applications: signer, Team ID, notarization, and drift since the last scan.
- **Homebrew adoption.** Finds manually-installed apps that Homebrew could manage and adopts them in place (`brew install --cask --adopt`) — no re-download, and from then on they update with everything else.
- **Skip a version.** Hide an update you don't want; it reappears automatically when a newer version ships.
- **Scheduled updates.** Optional daily launchd job at a time you pick, with full logs in `~/.freshet/run.log`.
- **Notify-only mode.** See what's outdated without letting Freshet install anything.

## Security model

Freshet shells out to package managers; it never downloads or executes update artifacts itself. The hardening is focused on the boundaries that exist:

- **No shell, no injection.** Every external command runs via `Command::new` with an argument vector — nothing is interpolated into a shell string. All webview-supplied identifiers (cask tokens, npm package names, mas IDs, search queries) are validated against strict allowlist patterns before reaching an argv, so a crafted id can't smuggle a flag like `--force` or a git/tarball install spec.
- **SSRF-hardened feed fetching.** Sparkle appcast URLs supplied by installed apps are fetched only over https (enforced across every redirect hop via curl `--proto-redir =https`), with private, loopback, link-local, CGNAT, and `.local`/`.internal` hosts refused — a hostile app can't point Freshet at cloud metadata or your router. Response size is capped.
- **Tamper-resistant trust baseline.** The signer baseline (`~/.freshet/signing.json`) and helper scripts live in a `0700` directory with `0600`/`0700` file modes, so no other local user can pre-seed an attacker's Team ID or swap the askpass helper.
- **Minimal Tauri surface.** No shell plugin, no opener plugin; the webview gets only `core:default` plus window-drag. URL opening is a custom command that validates the URL first.
- **Least-privilege elevation.** When a cask needs sudo, `SUDO_ASKPASS` points at a native macOS password dialog — the app never sees or stores your password.

The full audit write-up lives in [`tests/security-audit.txt`](tests/security-audit.txt).

## Architecture

- **Backend** — Rust + Tauri 2 ([`src-tauri/src/main.rs`](src-tauri/src/main.rs)). Every parser and validator is a pure function (no I/O) with unit tests grounded in real CLI output; thin command wrappers do the process spawning. Checks run concurrently; the network-bound Sparkle scan is a separate command so a slow appcast can't stall the local results.
- **Frontend** — React 19 + TypeScript ([`src/App.tsx`](src/App.tsx)). UI decision logic is extracted into [`src/logic.ts`](src/logic.ts) as pure functions, tested with Vitest without rendering.
- **Why Tauri** — a menu-bar utility should be small and native-feeling; the webview ships with macOS and the backend is a single static binary.

## Development

Prereqs: Rust (stable), Node 20+, pnpm.

```sh
pnpm install
pnpm tauri dev      # run the app
```

Quality gates (all enforced in CI on every push/PR):

```sh
pnpm test                 # Vitest — pure UI logic
pnpm lint                 # ESLint, typescript-eslint strictTypeChecked
pnpm exec tsc --noEmit    # type-check
pnpm type-coverage        # ≥99% type coverage

cd src-tauri
cargo test                # pure parser/validator tests against real CLI fixtures
cargo clippy --all-targets --all-features -- -D warnings   # pedantic level, deny
cargo fmt --check
cargo audit               # RustSec advisories
```

## Status

Early development (v0.1). Core update flows, signer-drift detection, adoption, and scheduling work today; see [`context.md`](context.md) for the user stories driving the roadmap.
