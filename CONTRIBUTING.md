# Contributing to Freshet

Thanks for considering it. Freshet is a macOS menu-bar app that keeps everything on your Mac up
to date from one place, and warns you when an update is signed by a different developer than the
last one.

This guide covers what you need to know to make a change that gets merged. The reasoning behind
the architecture lives in [`docs/`](docs/) — this document is the process.

## Before you start

**Freshet runs commands that replace software on your machine, and sometimes asks for your admin
password.** That shapes how changes are reviewed. Two things matter more here than in most
projects:

1. **Nothing should surprise the user.** No silent installs, no hidden behaviour, nothing updated
   that the user hid from view.
2. **Every value that becomes a command argument is validated.** See
   [`docs/06-security.md`](docs/06-security.md).

If a change touches either, expect close review. That is not distrust — it is the same bar the
existing code was held to.

## Setup

**Requirements:** macOS 11+, Rust (stable), Node 20+, pnpm.

```sh
git clone https://github.com/seanturner06/mac-updater
cd mac-updater
pnpm install
pnpm tauri dev
```

The first Rust build takes a few minutes. After that, `pnpm tauri dev` gives you hot reload on
the frontend and a rebuild on Rust changes.

The app starts hidden — **look for the icon in your menu bar**, not for a window. Left-click it
to open; right-click for Quit.

Optional but useful:

```sh
cargo install cargo-audit    # dependency advisories, as CI runs them
```

To exercise the real update paths, you need something genuinely outdated. `brew outdated`,
`mas outdated`, and `npm outdated -g` will tell you what you have to work with.

## Before you open a pull request

Everything CI runs, you can run locally. There is no CI-only step.

```sh
# Frontend
pnpm lint
pnpm exec tsc --noEmit
pnpm test
pnpm type-coverage

# Backend
cd src-tauri
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo audit
```

`cargo test` should report **50 passed**; `pnpm test` should report **24 passed** (plus whatever
you added). Include the counts in your PR description — it is the cheapest evidence the change
does what it says.

## The two rules that shape the codebase

### 1. Pure logic is separated from I/O, in both languages

This is the core convention. Every decision lives in a function that takes values and returns
values, with no process spawning, no filesystem, no network — and every one of those functions
has tests.

**Rust** (`src-tauri/src/main.rs`) is three layers:

```
Layer 3  #[tauri::command]              thin: spawn_blocking + a safe fallback
Layer 2  fn check_brew(), fetch_url()   I/O only. Kept trivial. Not unit-tested.
Layer 1  fn parse_brew_outdated(&str)   PURE. Tested. All the logic lives here.
```

**TypeScript** mirrors it: `src/logic.ts` is pure and tested by `src/logic.test.ts` with no
rendering; `src/App.tsx` holds state and JSX.

> **If your function makes a decision, it goes in Layer 1 and needs a test. If it touches the
> world, it goes in Layer 2 and should be as boring as possible.**

The moment a branch appears inside an I/O function, that is untested logic. Extract it.

### 2. Test fixtures come from real tool output

**Never invent the string.** Run the tool, capture what it actually printed, trim it to the
smallest excerpt that preserves the shape, and use that.

Two real bugs came from output nobody would have guessed:

- `mas outdated` separates the id from the name with *multiple* spaces, and the name contains
  spaces — naive splitting produced empty names.
- `spctl` prints `source=Unnotarized Developer ID` for signed-but-not-notarized apps, so a
  substring match on `"notarized"` reported the **exact opposite of the truth** in a security
  check.

Capture fixtures with:

```sh
brew outdated --json=v2
brew outdated --cask --greedy --json=v2
mas outdated
npm outdated -g --json
brew info --cask --json=v2 slack
codesign -dvv /Applications/Safari.app 2>&1
spctl --assess --type execute -v /Applications/Safari.app 2>&1
```

Preserve odd whitespace exactly. That is usually the part that breaks parsers.

## What different changes need

| Change | Required |
|---|---|
| New parser | Pure function + tests against real tool output, including a malformed input |
| New validator | Pure function + tests, **including adversarial inputs** |
| New update source | The checklist in [`docs/03-update-sources.md`](docs/03-update-sources.md) |
| New UI decision logic | Goes in `src/logic.ts`, with tests |
| Pure-CSS change | No test; include a before/after screenshot |
| Bug fix | A test that fails before your fix |
| New dependency | Justify it in the PR; `cargo audit` must stay clean |
| Loosened lint | An inline comment with the reason; prefer `warn` over `off` |

### Security-relevant changes

Anything in this list gets close review — please flag it in your PR description:

- Building an argv, adding a `Command::new`, or adding an arm to `build_update_command`
- Fetching a URL
- Writing to `~/.freshet`
- Adding a Tauri capability or plugin
- Loosening a validator
- Adding a dependency

And one architectural line: **a change that makes Freshet download or execute an update artifact
itself is not an incremental feature.** The whole security model rests on delegating that to
brew, mas, and npm. Open an issue to discuss before writing code.

## Style

**Rust** — `cargo fmt`, non-negotiable. Clippy runs at `pedantic = deny`. When a lint is wrong
about a specific line, allow it *at that site* with a one-line comment saying why:

```rust
#[allow(clippy::needless_collect)] // collect() is what makes the fetches parallel
```

Never loosen it project-wide. `unwrap()` in production code is a warning; use `?`,
`unwrap_or_else`, `unwrap_or_default`, or let-else.

**TypeScript** — `strictTypeChecked` + `stylisticTypeChecked`, type coverage ≥99% (currently
100%). Use `type`, not `interface`. Always parameterise `invoke<T>(…)`. Use `void invoke(…)` for
deliberate fire-and-forget so `no-floating-promises` stays useful everywhere else.

**CSS** — use the tokens in `:root`. A literal colour in a rule is a bug unless it is genuinely
non-thematic. Keep hit targets ≥24px. Don't add a CSS framework.

**Comments** — the codebase explains *why*, not *what*. Match that. Comments that record a
non-obvious constraint (`collect() is load-bearing here`, `codesign writes to stderr`) are
valuable; comments restating the line above are not.

**One thing worth knowing:** types crossing the IPC boundary are declared twice — as Rust structs
and as TypeScript types in `App.tsx` — and kept in sync by hand. There is no codegen. Renaming a
serialized field in Rust will not fail the TypeScript build; it will produce `undefined` at
runtime. **When you change a serialized struct, grep for its fields in `src/`.**

## Pull requests

- **Branch from `main`.** Keep the PR to one concern.
- **Describe the *why*.** What was wrong, what you changed, and anything you considered and
  rejected. The decision log ([`docs/13-decisions.md`](docs/13-decisions.md)) is written in that
  style; PRs in the same style are what keep it accurate.
- **Include the test counts**, and a screenshot for anything visual.
- **Call out security-relevant changes** explicitly.
- **If you changed behaviour that a design doc describes, update the doc in the same PR.** Docs
  that drift from the code are worse than no docs.
- CI must be green: `rust` (fmt, clippy, test), `web` (lint, tsc, vitest, type-coverage), and
  `audit`.

Commit messages: a short imperative subject line, and a body explaining why if it isn't obvious.

## Good first contributions

Small, well-scoped, and genuinely wanted — the full list is in
[`docs/12-roadmap.md`](docs/12-roadmap.md):

- `Escape`-to-close and focus management on the overlay panels
- `aria-label` coverage on icon buttons
- Absolute paths for system tools (`/usr/bin/codesign`, `/usr/bin/curl`, …) — cheap hardening
- `uninstall_schedule` should remove `~/.freshet/run.sh` alongside the plist
- Log rotation for `~/.freshet/run.log`
- Replace the two `.expect("home dir")` calls with graceful degradation
- Tokenise the two hardcoded colours in `.toggle-slider`
- More parser fixtures — especially `mas` and Sparkle appcasts, where real output varies more
  than you would expect

Larger, higher-impact work, if you want to take something on: per-item progress events during
updates (the `verify-progress` event pattern already exists to copy), a "trust this signer
change" acknowledgement, or a headless `--run-scheduled` mode so the background job uses the same
code path as the app.

## Things to discuss before building

Not "no" — but they need agreement on approach first, because the wrong shape is expensive to
unwind:

- **A new update source.** Read [`docs/03-update-sources.md`](docs/03-update-sources.md) first.
  The mapping problem (which installed app corresponds to which registry entry) is usually the
  hard part, and the answer must not involve hand-curated data.
- **Anything that downloads or runs an artifact.** See above.
- **A new dependency**, particularly in Rust. The tree is deliberately tiny.
- **Splitting `main.rs` into modules.** Worth doing eventually, but as a mechanical move with no
  behaviour change, in its own commit — not bundled into a feature.

## Things that are out of scope

Proposing one of these needs an argument against the reasoning in
[`docs/01-product-design.md`](docs/01-product-design.md), not just a use case:

- A curated app-version database. This is the trap that killed the last product in this space.
- Cross-platform support. Every mechanism used is macOS-specific.
- Becoming a general Homebrew GUI — browsing, installing, uninstalling.
- Silent unattended updating as a default.

## Reporting bugs

Please include:

- macOS version and chip (Apple Silicon or Intel)
- Which source (brew formula, cask, App Store, npm, Sparkle)
- **The exact output from the results panel** — expand the failed row and copy the log. That
  text is usually the whole answer.
- For scheduling issues: `~/.freshet/run.log` and `launchctl list | grep com.freshet`

## Licensing and the CLA

Freshet is licensed under the **GNU Affero General Public License v3.0** ([`LICENSE`](LICENSE)).
Anyone can use, study, modify, and redistribute it; a modified version that is distributed — or
made available over a network — must also be released under AGPL-3.0. The code stays open,
permanently.

Contributions require signing the **[Contributor License Agreement](.github/CLA.md)** — a
one-time thing, handled automatically on your first pull request.

**What it does:** you keep the copyright in your work and can reuse it however you like. Your
contribution will always remain available under AGPL-3.0. In addition, you grant the maintainer
the right to distribute it under other terms.

**Why it's needed:** Freshet aims to be commercially sustainable — a paid tier, distribution on
Setapp and the Mac App Store, and commercial licences for organisations that can't use AGPL
code. None of that is possible unless one party can license the whole codebase under more than
one set of terms. Without a CLA, every contributor would hold a veto over any future licensing
decision, including the ones that fund the work.

This is a real trade and worth being upfront about: a CLA adds friction, and some people decline
to sign them on principle. That's a legitimate position. **You are very welcome to file issues,
report bugs, suggest designs, and discuss architecture without signing anything** — none of that
requires an agreement.

New source files should carry the standard header:

```rust
// Freshet — keep everything on your Mac up to date.
// Copyright (C) 2026 Sean Turner
// SPDX-License-Identifier: AGPL-3.0-or-later
```

## Security issues

**Do not open a public issue.** See [`SECURITY.md`](SECURITY.md).

## Where to read next

| You're doing | Read |
|---|---|
| Anything | [`docs/02-architecture.md`](docs/02-architecture.md) |
| Backend work | [`docs/04-backend.md`](docs/04-backend.md) |
| Frontend work | [`docs/05-frontend.md`](docs/05-frontend.md) |
| Adding a source | [`docs/03-update-sources.md`](docs/03-update-sources.md) |
| Anything security-adjacent | [`docs/06-security.md`](docs/06-security.md) |
| UI work | [`docs/09-ui-design-system.md`](docs/09-ui-design-system.md) |
| Wondering why something is weird | [`docs/13-decisions.md`](docs/13-decisions.md) |
