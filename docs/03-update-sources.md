# 03 — Update Sources

Five sources ship today. This document specifies the contract every source satisfies, documents
each one's quirks, and walks through adding a sixth.

## The source contract

A source is anything that can answer *"what of this kind is installed and outdated?"* Concretely
it must supply:

**1. A detection function** returning a `CategoryStatus`:

```rust
struct CategoryStatus {
    category: String,        // user-facing heading, e.g. "App Store"
    kind: String,            // stable machine key the frontend echoes back: "brew"|"cask"|"mas"|"npm"|"sparkle"
    items: Vec<OutdatedItem>,
    error: Option<String>,   // Some(msg) => the row shows the error instead of items
}

struct OutdatedItem {
    id: String,              // what the update command takes: token, ADAM ID, package name
    name: String,            // what the user sees
    current: String,
    latest: String,
    self_updating: bool,     // hidden unless the greedy toggle is on; false for non-casks
    notes_url: String,       // release notes link, "" when unavailable
}
```

**2. A pure parser** that turns the tool's raw stdout into `Vec<OutdatedItem>`, with unit tests
against output captured from the real tool.

**3. Either an update arm or read-only status.**
- Updatable: an arm in `build_update_command(kind, id)` returning `(program, argv)`, **gated on an
  id validator**. No validator, no arm — this is the command-injection boundary.
- Read-only: nothing in `build_update_command`; instead add the `kind` to `isReadOnly()` in
  `src/logic.ts` so the UI renders rows without checkboxes.

**4. Graceful absence.** If the tool isn't installed, the category reports an error and the other
four still work. `run_cmd` returning `Err` becomes `failed_category_err(...)`, never a panic and
never an IPC rejection.

**Optional:** an arm in `fetch_batches()` if the source supports download-without-install, so its
artifacts join the concurrent prefetch.

## The five sources

### 1. Homebrew formulae — `kind: "brew"`, category "Homebrew"

```
detect   brew outdated --json=v2        → parse_brew_outdated  (reads .formulae[])
update   brew upgrade <name>
prefetch brew fetch --formula <a> <b> …  (one batched process)
validate is_valid_cask_token            (name is misleading; the rule set is right)
```

Straightforward. `installed_versions[0]` is taken as current, `current_version` as latest.

### 2. Homebrew casks — `kind: "cask"`, category "Applications"

```
detect   brew outdated --cask --json=v2            → the "normal" name set
         brew outdated --cask --greedy --json=v2   → the full set
update   brew upgrade --cask <token>
prefetch brew fetch --cask <a> <b> …
validate is_valid_cask_token
```

The only source that runs **two** commands, because of the greedy problem.

`brew outdated --cask` deliberately omits casks marked `auto_updates true` — apps that update
themselves, like Chrome or IntelliJ. Omitting them entirely is wrong (the user can't see they're
behind); including them in "Update all" is also wrong (brew will happily reinstall an app that
was about to update itself, and it's slow and pointless).

The resolution: **always fetch `--greedy`, and tag the difference.** `parse_casks` marks an item
`self_updating: true` when it appears in the greedy list but not the normal one. The frontend
hides those unless the greedy toggle is on, which means flipping the toggle is instant with no
backend round-trip — and turning it *off* prunes any now-hidden items out of the selection
(`prunedSelfUpdating`) so nothing gets updated after it disappeared from view.

The cost is one extra `brew outdated` process per check. That was judged worth it for an instant
toggle.

### 3. Mac App Store — `kind: "mas"`, category "App Store"

```
detect   mas outdated       → parse_mas_outdated
update   mas upgrade <id>
validate is_numeric_id      (ADAM IDs are numeric)
```

`mas outdated` emits plain text, not JSON:

```
462058435  Microsoft Excel       (16.71 -> 16.109.2)
```

Naive whitespace splitting yields an empty name, because the id is followed by *multiple* spaces
and the name itself contains spaces. The parser takes the first whitespace-delimited token as the
id, then finds the **last** `(` and reads `old -> new` from inside it. This was a real bug caught
by a test written against genuine `mas` output — the case study for the fixture discipline in
[10 — Testing](10-testing-and-quality.md).

**`mas` is the least reliable source, and knowingly so.** Apple has been deprecating the private
APIs it uses. Two failure modes are worth knowing:

- **"No downloads initiated for ADAM ID \<id\>"** — the app has an App Store receipt but was
  acquired under a *different* Apple ID, or installed outside the App Store. `mas` cannot update
  it, ever. `maybe_append_mas_hint()` detects this exact string and appends a plain-English
  explanation with the actual fix (use the vendor's own updater, or reinstall from the App Store
  under your own Apple ID). This is the template for how tool errors should be handled: detect the
  cryptic string, keep it, and append what to do about it.
- **Elevation.** `mas` calls `sudo` **without** `-A`, so it ignores the `SUDO_ASKPASS` helper that
  Homebrew honours. In a spawned process with no TTY it fails with "sudo: a password is required."
  The net behaviour is safe (that item fails cleanly, the batch continues) but the native password
  dialog never appears for App Store apps. Open issue — see [12 — Roadmap](12-roadmap.md).

### 4. npm globals — `kind: "npm"`, category "npm globals"

```
detect   npm outdated -g --json     → parse_npm_outdated
update   npm install -g <name>@latest
validate is_valid_npm_package
```

Two things to know.

**`npm outdated` exits non-zero when anything is outdated.** `run_cmd` handles this generally: a
non-zero exit with non-empty stdout is treated as success, and only an empty stdout produces an
`Err`.

**npm reports "outdated" whenever installed ≠ latest — including when installed is *newer*.**
This happens routinely with `corepack` and `pnpm`. Acting on those would be a downgrade, and they
would never clear from the list. `parse_npm_outdated` parses both versions with `semver` and drops
the item when `current >= latest`. If either side isn't valid semver the item is kept, on the
principle that showing something questionable beats hiding something real.

`is_valid_npm_package` is the strictest validator in the codebase for a specific reason: `npm
install -g` accepts far more than package names. A git URL, a tarball URL, or a local path is a
valid install spec, so a permissive validator here is arbitrary code execution. It rejects
anything containing `://`, anything starting with `-`, `.`, or `_`, and allows only lowercase
alphanumerics plus `- _ . /` after an optional single leading `@`.

No prefetch arm: npm has no meaningful download-without-install step.

### 5. Sparkle apps — `kind: "sparkle"`, category "Other apps" — **detection only**

```
detect   scan /Applications → read Info.plist SUFeedURL + CFBundleShortVersionString
         curl each feed (parallel, https-only) → parse_appcast_latest
update   none — read-only in the UI
```

The "Latest-style" source, and the only one that touches the network directly.

For each `.app` in `/Applications`, `defaults read <app>/Contents/Info SUFeedURL` gives the
appcast URL if the app uses Sparkle. Each feed is fetched on its own thread and scanned for
version strings, which Sparkle emits in two shapes — `<sparkle:shortVersionString>X</...>`
elements and `sparkle:shortVersionString="X"` attributes. Items are not guaranteed to be ordered,
so both forms are collected and the **maximum** is taken.

Comparison uses `version_is_older`, a lenient dotted-numeric compare rather than semver, because
Sparkle short-version strings (`2.9`, `32.1.2`, `1.0.3b2`) frequently aren't valid three-part
semver. It splits on `. - +`, takes leading digits of each component, and compares
component-wise with missing components treated as zero.

`<sparkle:releaseNotesLink>` from the first item becomes `notes_url` — **after passing
`is_safe_url`**, because this is a URL supplied by a third-party file on disk that we will hand to
`open`.

**Three architectural points:**

1. **Separate command.** `check_sparkle_apps` is its own Tauri command, not part of
   `check_outdated`. One unreachable appcast must not make the local brew/mas/npm results wait on
   a network timeout. The frontend fires both and renders whichever lands first.
2. **XML is parsed by hand, not with an XML library.** Two string scans over a small document,
   with no dependency and no parser attack surface. Note that the project's `cargo audit` history
   includes two high-severity DoS advisories against `quick-xml` (reached transitively through
   `plist`) — which is a decent argument for not adding an XML parser to handle hostile input.
3. **Every feed URL is validated twice** — once when harvested in `check_sparkle`, once again
   inside `fetch_url` at the actual fetch boundary. The URL comes from a file any installed app
   controls; see [06 — Security](06-security.md) for the SSRF reasoning.

**Why detection-only:** installing a Sparkle update means downloading and executing a DMG or PKG.
That is exactly the thing the architecture is built to avoid. The category renders read-only with
a note: *"these apps update themselves — open them to update."*

## Homebrew adoption — a source *multiplier*

Adoption is not a source; it converts unmanaged apps into cask-managed ones, which makes them
detectable by source 2 forever after.

```
suggest_adoptions()  read brew's local cask metadata → build app-filename → token index
                     scan /Applications, match, exclude already-managed → suggestions
search_casks(query)  brew search --cask <query>, flagging already-installed tokens
cask_info(token)     brew info --cask --json=v2 <token> → desc, homepage, version, auto_updates
adopt_cask(token)    brew install --cask <token> --adopt
```

The index comes from `<brew --cache>/api/cask.jws.json`, Homebrew's bulk cask metadata. It's a
JWS envelope whose `payload` field is a JSON *string* containing the cask array
(`extract_jws_payload` pulls it out). Each cask's `artifacts[].app[]` lists the `.app` bundles it
installs, giving a lowercased filename → token index. On collision the bare token wins over an
`@`-variant, so `Google Chrome.app` maps to `google-chrome`, not `google-chrome@beta`.

This is a **local file read, no network**, and `brew --cache` is memoised in a `OnceLock` because
it shells out and is stable for the process lifetime.

`--adopt` is why this works at all: Homebrew takes over the existing installation in place rather
than re-downloading a 200 MB app. `adopt_args()` is a pure function with its own test, purely so
that flag can't silently disappear in a refactor.

The same index, reversed, powers targeted post-update verification
([07 — Provenance](07-provenance.md)).

## Adding a source — worked example: GitHub Releases

GitHub Releases is the most-requested missing source. Here is what implementing it would involve,
as a template for any new source.

**1. Decide what it is.** GitHub Releases is *detectable* and, since it distributes DMGs/ZIPs
directly, **not installable within the architecture**. So: read-only, like Sparkle.

**2. Solve the mapping problem first.** This is the hard part, and it's a product decision more
than a code one. Given an installed `.app`, which repo is it? Options, in preference order:
   - Its Sparkle `SUFeedURL` already points at `github.com` — free, no new mechanism.
   - The user adds a repo manually per app — honest, zero maintenance, some friction.
   - A curated app→repo map — **this violates the core scope rule** in
     [01 — Product](01-product-design.md). Don't.

**3. Write the pure parser and its tests first.**

```rust
fn parse_github_release_tag(json: &str) -> Option<String>   // tag_name, minus a leading "v"
```
Fixture: real `GET /repos/<owner>/<repo>/releases/latest` output. Cover a draft release, a
prerelease, a tag with no `v`, a 404 body, and an API rate-limit body.

**4. Write the validator before the fetch.** `owner/repo` must match a strict pattern, and the
constructed URL must pass `is_safe_feed_url` — even though `api.github.com` is a fixed host,
because the owner/repo segments are attacker-influenced. Reuse `fetch_url`, which already
enforces https across redirects, a timeout, and a size cap.

**5. Add the detection function** returning `CategoryStatus { kind: "github", … }`, following
`check_sparkle`'s parallel-fetch shape.

**6. Wire the command.** A separate `check_github_apps` command like Sparkle's — it's
network-bound and must not block local checks.

**7. Frontend.** Add `"github"` to `isReadOnly()` in `src/logic.ts`, merge the category into
`displayCategories`, and add a test asserting the rows render without checkboxes.

**8. Rate limits.** Unauthenticated GitHub API is 60 requests/hour per IP. A machine with 40
GitHub-distributed apps exhausts that in under two checks. This needs an answer — caching with
`ETag`/`If-None-Match`, or an optional user token — before the feature is usable. Design it in;
don't discover it after shipping.

### The checklist

- [ ] Pure parser + tests against real tool/API output
- [ ] Id validator, and the `build_update_command` arm gated on it (or read-only registration)
- [ ] `CategoryStatus` with a stable `kind` string
- [ ] Missing-tool / unreachable path returns `error`, never panics, never blocks other sources
- [ ] Network sources: separate command, `is_safe_feed_url`, size cap, timeout
- [ ] Frontend: category merged, read-only registered if applicable, `logic.ts` tests updated
- [ ] Prefetch arm in `fetch_batches` if download-without-install exists
- [ ] Documented here, with its quirks and failure modes
