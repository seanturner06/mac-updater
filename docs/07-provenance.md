# 07 — Provenance & Signer-Drift Detection

This is the feature that justifies Freshet's existence over a shell alias for `brew upgrade`.

## What it catches that Gatekeeper doesn't

Gatekeeper asks one question when an app runs:

> Is this signed by *a* valid Developer ID, and is it notarized by Apple?

It does not ask:

> Is this signed by the **same** developer who signed the version I had yesterday?

That second question is the whole feature. If VS Code was signed by Microsoft (`UBF8T346G9`)
last week and today's build is signed by "Anonymous LLC (`XYZ123`)", Gatekeeper is perfectly
happy — the new signature is valid, the notarization is real — and something is very wrong.

**Notarization is not a safety certification.** It is an automated malware scan plus an identity
check. An attacker with a stolen or freshly-registered Developer ID gets notarized builds.

### The attack class, with real cases

| Incident | What happened | Signed? | Notarized? | Signer changed? |
|---|---|---|---|---|
| **3CX (2023)** | Vendor's own build pipeline compromised; the trojanised client shipped through official channels | Yes | **Yes** | Yes |
| **HandBrake (2017)** | A download mirror was compromised; the replacement DMG carried a different signature | Yes | n/a (pre-notarization) | Yes |
| **Transmission (2016)** | Compromised build served from the official site; delivered KeRanger, the first macOS ransomware | Yes | n/a | Yes |
| **Transmission (2018)** | Again, two years later | Yes | n/a | Yes |

Every one of these passed the checks the OS performs. Every one of them would have shown a
changed Team ID against the previously-installed version.

That is the whole thesis: **the signer changing between versions of the same app is a
high-signal, low-false-positive indicator of a compromised release**, and nothing in the macOS
update ecosystem surfaces it.

## How it works

### Inspecting one app

`inspect_app(path) -> AppSig` runs two system tools:

```
codesign -dvv <app>                            → TeamIdentifier=, Authority=
spctl --assess --type execute -v <app>         → accepted / rejected, source=…
```

```rust
struct AppSig { name, team_id: Option<String>, authority: Option<String>, signed: bool, notarized: bool }
```

Both are parsed by pure functions:

- **`parse_signing`** reads `TeamIdentifier=` (treating the literal `"not set"` as absent — some
  apps genuinely have no Team ID) and the **first** `Authority=` line, which is the leaf
  certificate. Note that `codesign` writes its details to **stderr**, which is why `combine_output`
  merges both streams before parsing.
- **`parse_notarized`** requires `spctl` to have exited zero, the output to contain `accepted`,
  **and** the output to contain `source=notarized`. That last anchoring matters: a bare substring
  search for `"notarized"` also matches `source=Unnotarized Developer ID` — spctl's phrasing for
  *signed but not notarized* — which would report the exact opposite of the truth. A test caught
  this.

### The baseline

`~/.freshet/signing.json`, mode `0600`, a flat `{ "App Name": "TEAMID" }` map.

It is written by whichever verification path runs, and diffed on the next run.
`build_verify_entries` does the load → diff → mutate in one pass, and the caller holds
`signing_store_lock()` across load → build → save so two concurrent verifications can't lose
each other's updates. Lock poisoning is recovered from rather than panicked on — the guarded
value is just a serialisable `HashMap`.

### What counts as drift

```rust
fn cert_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    matches!((previous, current), (Some(p), Some(c)) if p != c)
}
```

Drift requires **both** a known previous Team ID and a present current one, and they must differ.
Deliberately excluded:

- **First sighting** (`None → Some`). There is nothing to compare. It seeds the baseline silently.
- **Lost signature** (`Some → None`). Suspicious, but it surfaces through the separate
  `signed: false` path instead, which produces a different message. Conflating the two would
  muddy the one alarm that is supposed to mean something specific.

An entry is **flagged** when it drifted *or* is unsigned. Everything else is a verified
publisher. In the `UpdateResult` this is inverted into `success: !flagged`, so the results panel
renders flagged rows in the failure style and sorts them to the top.

### The two modes

**Full inventory — `verify_signatures(only_changed?)`**

Triggered from Settings → "Verify app signatures". Scans every `.app` in `/Applications`, one
`spawn_blocking` per app (~100 apps × two process spawns), with an `AtomicUsize` counter emitting
a `verify-progress` event per completion so the header shows a live percentage. Reports
everything: drift, unsigned apps, and each app's signer, Team ID, and notarization status.

This is the "what is the overall state of my machine" report.

**Targeted post-update — `verify_updated_casks(tokens)`**

Runs automatically after every update batch that included casks. Steps:

1. Read `<brew --cache>/api/cask.jws.json`, extract the JWS payload.
2. `resolve_cask_app_filenames(payload, tokens)` — for each just-upgraded token, the `.app`
   filenames it installs (`artifacts[].app[]`; one token can own several bundles).
3. Filter `/Applications` to those bundles.
4. Inspect them serially — typically one to five apps, so threading would be ceremony.
5. Diff, save, and return **drift entries only**.

Everything else is skipped by construction: formulae, mas apps, and npm packages have no `.app`
bundle; unknown tokens resolve to nothing; casks whose app isn't in `/Applications` drop out.

The frontend gathers the tokens (`selections` filtered to `kind === "cask"`), calls this after
the update completes, and if anything comes back prepends it to the results with the title
`Update results — ⚠ N signer changes`. The whole call is wrapped in its own `try/catch` so a
verification failure can never stop the user seeing their update results.

### Why targeted, and the numbers

The first implementation of post-update verification re-scanned all of `/Applications`. On a
machine with ~150 apps that is ~300 process spawns after every update — several seconds of
pointless work to check apps that hadn't changed.

The targeted path is **O(casks upgraded)** instead of **O(apps installed)**. Upgrading three
casks inspects three apps.

This is the canonical example of the project's performance philosophy: the win came from *doing
less work*, not from doing the same work faster. See [11 — Performance](11-performance.md).

## Evolution

| Version | Shape | Why |
|---|---|---|
| v1 | On-demand button, full inventory | Useful, but only fires when the user remembers |
| v2 | Auto after updates, `only_changed` filter | Catches a hijacked upgrade without the user thinking about it; filters out unsigned-but-stable apps, which are noise in a post-update context |
| v3 | `verify_updated_casks`, token→bundle resolution | Targeted scope; made auto-verify cheap enough to always run |

The `only_changed` parameter is the interesting part of v2. Post-update, the user is asking "did
anything about that update look wrong?" — an unsigned app that has been unsigned for two years is
not an answer to that question. It stays in the full inventory report, where "what is unsigned on
this machine" *is* the question.

## Limits — read this before trusting it

**It is post-hoc.** Drift is detected *after* the update has been installed. Freshet does not
inspect the artifact before it runs, because it never has the artifact — brew does. This is a
detector, not a preventer. The value is reducing dwell time from months to minutes, and telling
you exactly which app to be suspicious of.

**The first run defines "normal."** Whatever Team IDs are present the first time Freshet scans
become the baseline. A machine that is *already* compromised gets baselined as clean, and that
compromise is invisible forever. Closing this needs an external source of truth — a curated
allowlist of known Team IDs for high-value apps, or cross-referencing vendor sites. Not built.

**Legitimate signer changes look identical to hijacks.** Acquisitions, developer-account
migrations, and certificate reissues under a new team all produce genuine drift. Today they are
flagged the same way, and the only remedy is running verification again, which silently accepts
the new ID. A "trust this change" acknowledgement — which persists the new Team ID deliberately
rather than accidentally — is the most valuable unbuilt piece of this feature. Without it, every
false alarm trains the user to ignore the real one.

**Non-cask updates are not covered post-update.** Formulae, mas apps, and npm packages have no
`.app` bundle to inspect. A compromised App Store app is Apple's problem; a compromised npm
global is a real gap with no obvious mechanism.

**Nothing fires without an update.** An app that was replaced by a direct DMG download, a
sideloaded bundle, or malware is only caught if the user manually runs the full inventory scan.
A scheduled background verification is the obvious fix and is not built.

**Keyed by app name.** Renaming an app loses its baseline and silently re-seeds it. Two apps
sharing a file stem in different locations would collide. Keying by bundle identifier would be
strictly better.

**Only `/Applications`.** `~/Applications`, `/System/Applications`, and nested bundles are
invisible.

**Team ID is not identity.** An attacker who compromises the vendor's *own* signing key — rather
than substituting their own — produces no drift at all. 3CX is the uncomfortable case here: the
build pipeline was compromised, and whether drift would have appeared depends on whether the
attacker had the legitimate key. The feature catches signer *substitution*, which covers
HandBrake and Transmission cleanly and is the common case, but it is not a general defence
against a compromised vendor.

## Design principles for changes here

1. **A false alarm is expensive.** Every unnecessary warning makes the next real one less
   effective. When in doubt, don't flag — `cert_changed` returning `false` for `None → Some` is
   this principle in code.
2. **Keep the alarm specific.** "Different publisher signed this version" is actionable. "Something
   about this app's signature is different" is not. Resist folding other signals into the same
   warning.
3. **Never block the update results on verification.** It is additive information, wrapped in its
   own error handling, always.
4. **Optimise for the signal, not for completeness.** v3 inspects serially because one to five
   apps is already fast. Parallelising it would be premature.
5. **The warning text carries the evidence.** `was Team X, now Y` — the user can go look both up.
   An opaque "⚠ suspicious" would be worthless.
