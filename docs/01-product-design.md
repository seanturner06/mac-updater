# 01 — Product Design

## The problem

A working Mac accumulates software from mutually ignorant channels. A developer machine typically
has all five of these at once:

| Channel | Updated by | Tells you it's outdated? |
|---|---|---|
| Homebrew formulae (CLI tools) | `brew upgrade` | Only if you run `brew outdated` |
| Homebrew casks (GUI apps) | `brew upgrade --cask` | Only if you run `brew outdated --cask` |
| Mac App Store | App Store.app / `mas` | Sometimes, in a badge you ignore |
| npm globals | `npm i -g` | Never |
| Sparkle-based apps | The app itself, on launch | Only when you open the app |

There is no single place that answers "what on this machine is out of date?" The result is a
long tail of software that is months behind — which is a security problem, not just an
untidiness problem, because unpatched local software is a standard intrusion path.

## Who it's for

The primary user is a **developer power-user**. That is a deliberate narrowing, not an accident
of the sources: npm globals and Homebrew formulae are meaningless to a general consumer, and the
competitors that target consumers do not cover them. Three secondary shapes matter for design
decisions:

- **Security-conscious user** — wants to be *told*, not acted upon. Drives notify-only mode and
  the entire provenance feature.
- **Maintainer** — many apps across many sources; needs selective updates, per-version skip, and
  real error output rather than a green tick.
- **First-timer** — must get value with zero configuration. There is no setup wizard, by design.

The twenty user stories these personas generated live in [`../context.md`](../context.md). That
file is the product-intent record; this document is the design reasoning behind it. Where a story
describes behaviour that isn't built yet, the gap is listed in [12 — Roadmap](12-roadmap.md).

## The core design decision

**Freshet reuses existing version registries. It does not build its own.**

This is the single most important constraint in the project, and most scope questions resolve
against it. The reasoning comes from comparing the two prior tools in this space:

- **MacUpdater** (commercial, shut down 1 Jan 2026) maintained a hand-curated cloud database of
  100k+ apps and their download URLs. That bought near-total coverage and true auto-install — and
  the maintenance treadmill was a permanent, growing cost that the product could not carry. It ran
  eight years and wound down.
- **Latest** (free, open source) reads each app's own Sparkle appcast and checks the App Store.
  Near-zero maintenance; coverage limited to Sparkle and MAS apps, which is a minority of a real
  machine.

Freshet takes Latest's maintenance posture and widens the coverage by leaning on **Homebrew Cask**,
which is effectively the open-source, community-maintained equivalent of MacUpdater's database —
plus `mas`, plus npm, plus Sparkle. Coverage stays current because thousands of other people keep
it current.

**The practical rule:** a proposal to hand-maintain app metadata, download URLs, or version numbers
is out of scope. A proposal to *consume another registry that someone else maintains* is in scope.

## Scope boundary

**In scope — detection:** anything a registry on the machine (or an app's own feed) can be asked
about without curation.

**In scope — installation:** anything a package manager will install on our behalf. Freshet never
downloads or executes an update artifact itself; it invokes `brew`, `mas`, or `npm` and those
tools do the downloading and integrity-checking they already do. This is the security architecture
as much as the product architecture — see [06 — Security](06-security.md).

**Deliberately detection-only:** Sparkle apps. Freshet reads the appcast to tell you a newer
version exists; the app updates itself when you open it. Installing a Sparkle update would mean
downloading and running a DMG, which crosses the line above.

## Non-goals

- **A curated app-version database.** See above. This is the one that killed the category leader.
- **"Updates everything."** The promise is bounded and stated as such: *everything brew, the App
  Store, npm, and Sparkle know about*. An unbounded promise cannot be kept and erodes trust the
  first time it's broken.
- **Being a Homebrew GUI.** Freshet is not a package browser, installer, or uninstaller. The one
  install-shaped feature — adoption — exists because it *increases what can be kept updated*.
- **Cross-platform.** macOS only. Every mechanism used (launchd, `codesign`, `spctl`, Sparkle,
  `mas`, the menu bar) is macOS-specific. The bundle targets are `app` and `dmg`;
  `minimumSystemVersion` is 11.0.
- **Silent unattended updating as the default.** Updates run when a human presses a button, or on
  a schedule that the human explicitly turned on.

## What makes it different

Four things, in rough order of defensibility:

1. **Signer-drift detection.** After an update, Freshet checks whether the upgraded app is signed
   by the same Developer Team ID as before. Gatekeeper asks "is this signed by *someone* and
   notarized?"; it does not notice that yesterday's build was signed by Microsoft and today's is
   signed by someone else. That difference is precisely what the 3CX (2023), HandBrake (2017), and
   Transmission (2016, 2018) supply-chain attacks looked like — all of them validly signed. No
   other updater in this space surfaces it. Full design in [07 — Provenance](07-provenance.md).
2. **The developer toolchain.** npm globals in particular. The app-focused competitors don't cover
   them.
3. **Real error output.** When `brew upgrade` fails on a dependency conflict, Freshet shows the
   actual brew output in an expandable log, auto-expanded for failures. Competitors show a red
   icon. "What do I do now" is answerable only from the real text.
4. **Restraint in the UI.** The consistent complaint about tools in this category is that they are
   overwhelming. A single 420px window, five collapsed categories, two buttons.

## Sustainability model

Freshet's client is open source. That is a deliberate strategic choice, not just a licensing
preference:

- The client code is not defensible. A competent developer — or an AI — can scaffold a Tauri or
  Swift updater in a weekend. Treating the binary as the moat would be a losing position.
- The audience for this tool values local-first and privacy explicitly, and an auditable client
  is the strongest possible statement of that.
- What *is* defensible compounds outside the binary: trust earned over time by a tool that runs
  with admin rights and hasn't hurt anyone, operational know-how about which updates break what,
  and — if it ever reaches scale — aggregated update-health data.

The intended split, should a paid tier exist, is **coverage is free, automation and the safety net
are paid**: every detection source stays free, including ones competitors charge for, because
paywalling coverage makes the free product look worse than the alternatives. Unattended
scheduling, rollback, and update-health warnings are the things with genuine ongoing cost.

This matters to contributors for exactly one reason: **detection sources and core update flows
belong in this repository, and contributions to them will always ship to every user.**

## Success criteria

For the open-source client, in order:

1. A first-time user gets a correct, useful list with zero configuration.
2. Nothing Freshet does surprises the user. No silent installs, no hidden behaviour, no update
   they didn't ask for.
3. When something fails, the user can tell *what* failed and *why* from the results panel alone.
4. The signer-drift alarm fires when it should and stays quiet when it shouldn't. A false alarm
   here is expensive — it trains people to dismiss the one warning that matters.
