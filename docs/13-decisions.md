# 13 — Decision Log

The load-bearing decisions, each with the alternative that was rejected and what the choice
costs. This exists so a future contributor can tell the difference between a decision and an
accident — and so that overturning one requires engaging with the reasoning rather than
rediscovering it.

Format: **Decision · Alternative rejected · Why · What it costs · When to revisit.**

---

## D1 — Reuse existing registries; never build a version database

**Alternative:** a curated database of apps, versions, and download URLs, like MacUpdater's.

**Why:** that treadmill is what ended MacUpdater — eight years, 100k+ apps maintained by hand,
daily updates, and an ongoing cost the product could not carry. Homebrew Cask is the
community-maintained equivalent and it is free. Coverage stays current because thousands of
other people keep it current.

**Cost:** coverage is bounded by what brew, the App Store, npm, and Sparkle know about. Apps
outside all four are invisible until adopted into Homebrew.

**Revisit:** never, realistically. This is the project's defining constraint. Consuming an
*additional* registry someone else maintains is always in scope; curating one is not.

---

## D2 — Orchestrate package managers; never download or execute artifacts

**Alternative:** fetch and install DMGs and PKGs directly, which would allow Sparkle and GitHub
Releases to be installable rather than detection-only.

**Why:** it eliminates an entire vulnerability class. There is no artifact in Freshet's hands to
verify, tamper with, or race. brew, mas, and npm already do checksum and signature verification
and have been doing it for years.

**Cost:** Sparkle and (eventually) GitHub sources are detection-only. Coverage is narrower than
a tool willing to run installers.

**Revisit:** this is the security architecture. Changing it is not a feature decision; it
relocates the whole threat model and needs a design discussion first.

---

## D3 — Tauri, not Electron or native Swift

**Alternative:** Electron (one language, huge ecosystem) or SwiftUI (fully native, no webview).

**Why:** a menu-bar utility is always resident, so footprint is a product attribute. Electron at
150 MB+ RSS is the wrong thing to leave running forever on a machine whose selling point is
hygiene, and a large bundled runtime is a poor look for an admin-rights tool. Tauri uses the
system WKWebView and ships a single static binary. Swift would be the most native option but
loses the web stack for UI iteration and the Rust safety properties for the process-spawning
layer.

**Cost:** two languages, two toolchains, two lint configurations, and Tauri-specific concepts
(capabilities, async runtime, command threading) for every contributor. A higher barrier to a
first contribution than a single-language project.

**Revisit:** only if Rust genuinely blocks velocity. The mitigation is D4.

---

## D4 — Keep the Rust layer thin; put logic in TypeScript

**Alternative:** a rich Rust core with a dumb view layer.

**Why:** Rust holds what only it can do — process execution, OS integration, parsing, the
security boundary. Decision logic is cheaper to change and test in TypeScript, and pushing it
there keeps the harder-to-modify half small.

**Cost:** some logic is split across the boundary in ways that can feel arbitrary (the greedy
filter is applied in TypeScript, but the tagging that makes it possible happens in Rust).

**Revisit:** if a decision needs data only Rust has, put it in Rust and test it there. The rule
is a default, not a law.

---

## D5 — Pure core, I/O shell, in both languages

**Alternative:** test through the real commands, or mock `Command`.

**Why:** a menu-bar GUI driven by five external tools is nearly untestable end-to-end. Extracting
every decision into a pure function makes the part that can be wrong also the part that can be
asserted. Mocking `Command` would mean building an abstraction solely to be mocked, then testing
the mock.

**Cost:** I/O wrappers are untested by construction and are verified by hand. The discipline has
to be maintained — the moment a branch appears inside an I/O function, it is untested logic.

**Revisit:** no. This caught two real bugs, one of them a security check reporting the exact
opposite of the truth.

---

## D6 — Always fetch `--greedy`, filter client-side

**Alternative:** re-run `brew outdated` with and without `--greedy` when the toggle changes.

**Why:** the toggle becomes instant with zero backend round-trips. `parse_casks` tags each item
by comparing the greedy and non-greedy sets.

**Cost:** one extra `brew outdated --cask` process on every check, to compute the non-greedy
name set.

**Revisit:** if the extra process ever shows up as a meaningful fraction of check time. It does
not today.

---

## D7 — "Update all" means "update what is displayed"

**Alternative:** `brew upgrade && brew upgrade --cask && mas upgrade && npm update -g`.

**Why:** a blanket upgrade would update things the user had explicitly hidden — a skipped
version, a self-updating cask filtered out of view. For a tool running with admin rights, doing
something the user actively hid is the worst possible failure. `allDisplayedSelections` enumerates
exactly the visible rows and runs them individually.

**Cost:** slower than a single blanket command (N invocations instead of one), and more code.
Mitigated by the batched prefetch.

**Revisit:** no. This is the trust posture in one function.

---

## D8 — Sparkle is a separate command, not a fifth category in `check_outdated`

**Alternative:** one command returning all five sources.

**Why:** Sparkle is N network round-trips to servers we do not control. One unreachable appcast
would make every refresh wait out an 8-second timeout. Splitting it means local results always
paint at local speed.

**Cost:** two commands, two loading states, and a placeholder category so the section does not
appear late and shift the layout.

**Revisit:** no — and this is the pattern for every future network-bound source.

---

## D9 — Skip a *version*, not an app

**Alternative:** a persistent per-app ignore list, as the original user stories described.

**Why:** `ignoreKey` includes the version, so a skip clears itself when a newer version ships.
"Not this one" is almost always what people actually mean, and an ignore list is a thing you
forget you have — which silently stops delivering the updates the tool exists to deliver.

**Cost:** you cannot permanently ignore an app. Someone who genuinely wants that has to skip each
version.

**Revisit:** if people ask for permanent ignore. It would be additive, not a replacement.

---

## D10 — No frontend framework beyond React

**Alternative:** Redux or Zustand, React Router, shadcn/Radix, Tailwind.

**Why:** one 420 × 680 window, four components, four mutually exclusive overlays, a list of a few
dozen rows. Every one of those libraries would be more machinery than product, and each is a
dependency in a security-sensitive supply chain.

**Cost:** all state lives in one component (about 25 `useState` calls in `App`), some
prop-drilling into `AdoptPanel`, and hand-written CSS for controls a library would provide.

**Revisit:** when a fifth overlay appears, or when prop-drilling becomes genuinely painful. The
answer then is `useReducer` plus one context — not a library.

---

## D11 — Hand-rolled URL host parsing

**Alternative:** the `url` crate.

**Why:** one small function against a dependency with its own attack surface and update burden,
in a tool whose dependency tree is deliberately tiny.

**Cost:** it is security-critical code we own. It has to handle userinfo, ports, and bracketed
IPv6 correctly, and it has dedicated tests for each because a bug here is an SSRF bypass.

**Revisit:** if URL handling ever needs more than host extraction. Then take the crate.

---

## D12 — Hand-rolled Sparkle XML parsing

**Alternative:** `quick-xml`, `roxmltree`, or similar.

**Why:** two string scans over a small document, with no dependency and no parser attack surface.
The project's own `cargo audit` history includes two high-severity DoS advisories against
`quick-xml` reached transitively through `plist` — a decent argument against adding an XML parser
to handle input from third-party servers.

**Cost:** it handles the two shapes Sparkle actually emits. An unusual appcast could defeat it,
and the failure mode is "no version found," i.e. the app silently does not appear as outdated.

**Revisit:** if real-world appcasts turn out to be more varied than the two forms handled.

---

## D13 — The install loop is serial

**Alternative:** update several items concurrently.

**Why:** concurrent `brew upgrade` invocations contend on Homebrew's own locks, and serial
execution keeps each item's output cleanly attributable to that item in the results panel — which
is the feature. The actually-slow part, downloading, is already overlapped by the prefetch.

**Cost:** a large batch takes the sum of its install times.

**Revisit:** if install time (not download time) becomes the visible bottleneck, and only for
sources that tolerate concurrency.

---

## D14 — Batched `brew fetch` per kind, not per token

**Alternative:** one `brew fetch` process per token in a bounded thread pool.

**Why:** measured. Homebrew loads metadata once and parallelises downloads internally (16-way by
default); spawning per token pays ~1s of Ruby startup 24 times. Identical 463 MB, cache cleared:
**17.80 s → 11.11 s, a 1.60× improvement**, with the remaining time being genuine network
transfer at the link ceiling.

**Cost:** less granular progress information — you get a per-kind result, not a per-token one.
The prefetch is best-effort and silent anyway.

**Revisit:** if Homebrew changes its internal download concurrency.

---

## D15 — Post-update verification is targeted, not a full scan

**Alternative:** re-scan all of `/Applications` after every update.

**Why:** ~150 apps × two process spawns after every update, to check apps that did not change.
Resolving upgraded cask tokens to their `.app` bundles via brew's local metadata makes it
**O(casks upgraded)** instead of **O(apps installed)**.

**Cost:** only cask updates are covered post-update. Formulae, mas apps, and npm packages have
no `.app` bundle; software changed by other means is caught only by the manual full scan.

**Revisit:** the gap closes with a scheduled background full verification, not by widening the
post-update scan.

---

## D16 — Drift requires a known previous *and* a present current Team ID

**Alternative:** also flag `None → Some` (first sighting) and `Some → None` (lost signature).

**Why:** a first sighting has nothing to compare against — flagging it would mean every app is
suspicious on first run, which is noise that teaches people to dismiss the warning. A lost
signature surfaces through the separate `signed: false` path with its own message. **Every false
alarm makes the next real one less effective.**

**Cost:** an app installed for the first time is baselined silently, whatever it is. The
first-run trust gap is real and documented in [07](07-provenance.md).

**Revisit:** only with an external source of truth — a curated allowlist of known Team IDs for
high-value apps.

---

## D17 — launchd for scheduling, not an in-app timer

**Alternative:** a timer inside the running app.

**Why:** a menu-bar app only runs while it is running. launchd runs regardless, and it is the
platform's supported mechanism. A user `LaunchAgent` needs no elevation and no installer.

**Cost:** the scheduled job is a separate process. Originally it ran a bash script sharing no
code with the app, which caused correctness issues C1–C3 in [12 — Roadmap](12-roadmap.md). It
now runs the app's own binary in a headless mode (`--run-scheduled`), with settings in
`~/.freshet/settings.json` so both processes read the same ones.

**Revisit:** no.

---

## D18 — `SUDO_ASKPASS` and a native dialog; never handle the password

**Alternative:** prompt in-app and pipe to `sudo -S`, or install a privileged helper tool.

**Why:** Freshet never sees, handles, or stores the password — `sudo` reads it from the helper's
stdout directly. Piping a password through our own process is a category of risk with no
upside. A privileged helper is the sanctioned approach for real background elevation and is a
substantial security undertaking.

**Cost:** it only works for tools that call `sudo -A`. Homebrew does; **`mas` does not**, so App
Store apps needing elevation fail without a dialog (C4).

**Revisit:** a privileged helper only if unattended elevation becomes a requirement, which it is
not.

---

## D19 — Strictest lint presets, relaxed narrowly with a written reason

**Alternative:** the "recommended" preset in each tool.

**Why:** a strict floor catches drift mechanically instead of relying on review discipline, which
matters most on a project with one maintainer. Setting it cost 49 Rust findings and 54
TypeScript findings up front, including real bugs — `unwrap()` in production paths, floating
promises, an unvalidated `JSON.parse`. CI now rarely says anything the local tools did not.

**Cost:** a higher bar for a first-time contributor, and occasional fights with a lint that is
wrong about a specific line.

**Revisit:** no. But relax individual rules freely when justified — narrowly, at the site, with
a comment, and preferring `warn` over `off` for rules that are useful signal but bad gates.

---

## D20 — One file for the backend, for now

**Alternative:** split `main.rs` into modules from the start.

**Why:** the pure/IO split already does the work module boundaries usually do, and keeping the
pure functions and their tests in one file avoids `pub` plumbing for a single-maintainer project.

**Cost:** 2,550 lines in one file. Navigation depends on the comment banners, and two people
editing it simultaneously will conflict.

**Revisit:** when a second contributor is regularly editing a different concern. Do it as a
mechanical move with no behaviour change, in its own commit. Seams are listed in
[04 — Backend](04-backend.md).

---

## D21 — Open-source the client

**Alternative:** keep it closed and rely on the binary as the product.

**Why:** the client is not defensible — a competent developer or an AI can scaffold an updater
like this in a weekend, and a polished competitor already exists. Treating the binary as the moat
would be a losing position. An auditable client is also the strongest possible statement of the
local-first, privacy-respecting posture this audience explicitly values, and it forces the
competition onto ground — trust earned over time, operational know-how, service — where the
binary is irrelevant.

**Cost:** anyone can fork it. That was always true; the source only makes it cheaper.

**Revisit:** no.

---

## D22 — AGPL-3.0 plus a CLA, not a permissive licence

**Alternative:** MIT or Apache-2.0 (maximum adoption), MPL-2.0 (middle ground), or a
source-available licence like BUSL (maximum protection, not open source).

**Why:** the goal is maximum protection *and* maximum future monetisation, and those only
combine through dual licensing — which requires two things together:

- **AGPL-3.0** makes a closed-source commercial fork legally impossible. Anyone who wants to
  ship Freshet's code inside a proprietary product has to come and buy a licence. The network
  clause additionally covers the v3 hosted service, so a competitor cannot run this code as a
  competing SaaS without publishing. The strength of the copyleft is what creates the commercial
  licence market: organisations with blanket AGPL bans are exactly the ones who pay.
- **A CLA granting the right to relicense** is what stops that copyleft from becoming a cage for
  the maintainer too. Holding the rights to the whole codebase is what permits proprietary
  builds for Setapp and the Mac App Store, a closed Pro tier linked against the AGPL core, and
  commercial licences for MDM vendors. Without it, every contributor holds a veto over every
  future licensing decision.

The App Store objection to GPL-family licences — Apple's terms conflict with §6, which is why
VLC was pulled in 2011 — **binds licensees, not the copyright holder**. With the CLA in place,
Freshet ships its own proprietary-licensed build to those channels while the public repository
stays AGPL. This is the VLC resolution, and the Qt / MySQL / Sidekiq model generally.

Permissive licences were rejected because they permit precisely the thing being protected
against: a competitor taking the code closed. MPL-2.0 was the earlier recommendation and remains
the better choice if adoption is weighted above protection; it was displaced once protection and
monetisation became the explicit criteria. BUSL was rejected because it is not open source, and
the open-source status *is* the trust asset per D21 — a source-available launch would forfeit
the credibility the licence decision is supposed to buy.

**Cost:** real and worth naming. A CLA measurably reduces drive-by contributions, and some
developers refuse to sign on principle. AGPL on a desktop utility reads as aggressive to part of
the target audience. The mitigation is to be explicit about why, and to make issues, bug
reports, and design discussion require no agreement at all.

**Revisit:** if contribution volume turns out to matter more than licensing optionality, the
maintainer can unilaterally relax to MPL or Apache at any time — that is the asymmetry the CLA
buys. The reverse is not possible. Start restrictive.
