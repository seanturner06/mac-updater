# 04 — Backend Design (Rust)

The backend is `src-tauri/src/`, one module per concern (split out of a single `main.rs` on
2026-11-14, as a mechanical move: the code and its tests are unchanged, only where they live).

## Module layout

```
main.rs        wiring only: the tray, the window, the command list
model.rs       data that crosses to the window: Kind, OutdatedItem, CategoryStatus,
               UpdateSelection, UpdateResult, Step, ResultAction
check.rs       what is outdated (brew, casks, mas, npm), the hourly `brew update`, the cached check
sparkle.rs     apps with a Sparkle appcast (detection only)
details.rs     descriptions, homepages, release notes
update.rs      installing selected updates: batches, progress, Stop, quit-and-update
npm.rs         npm globals without install scripts by default
adopt.rs       Homebrew adoption
signing.rs     reading and proving signatures, the trust baseline and pins, publisher-change checks
preverify.rs   the publisher check on a cask download before install
rollback.rs    app snapshots and restore
vulns.rs       osv.dev lookups (opt-in)
scheduled.rs   `freshet --run-scheduled`: plan, run, summary, notification
launchd.rs     the launchd job
settings.rs    ~/.freshet/settings.json
history.rs     ~/.freshet/history.jsonl
apps.rs        app bundles on disk: versions, icons, running processes, cask → app
process.rs     running programs: trusted brew/mas, askpass, streaming, Stop, the one-at-a-time guard
shell_path.rs  PATH for child processes
store.rs       ~/.freshet and private/atomic file writes
validate.rs    validators for everything the window sends
tray.rs        the menu-bar icon
self_update.rs Freshet's own updater, version, launch at login
test_support.rs  fixtures shared by more than one module's tests (test builds only)
```

Each module keeps its tests beside the code (`#[cfg(test)] mod tests`). An item is `pub(crate)`
only when another module uses it; `unreachable_pub` keeps plain `pub` out.

The package kind is the `Kind` enum (`model.rs`), not a string: it serializes as the same
lowercase word (`"brew"`, `"cask"`, …), and a kind the backend doesn't know deserializes to
`Kind::Unknown` and is refused per item.

## Types

Four types cross the IPC boundary. They are small on purpose: the frontend should not need to
know which tool produced a row.

```rust
struct OutdatedItem   { id, name, current, latest, self_updating, notes_url }
struct CategoryStatus { category, kind, items, error: Option<String> }
struct UpdateSelection{ kind, id, name }              // Deserialize only — comes from the webview
struct UpdateResult   { kind, name, success, output } // Serialize only
```

`UpdateResult` is reused for update results, adoption results, and signature-verification
results. That is slightly loose typing in exchange for a single results panel that renders all
three — a good trade for a UI this size. `kind: "verify"` marks provenance rows.

`notes_url` carries `#[serde(default)]` so older shapes deserialise; `self_updating` is always
`false` for non-cask sources.

## Layer 1 — pure functions

No I/O, no `Command`, no filesystem, no clock. Take `&str`, return values. **Every one of them is
unit tested.** If you are adding logic, this is where it goes.

### Parsers

| Function | Input | Notable behaviour |
|---|---|---|
| `parse_brew_outdated` | `brew outdated --json=v2` | Reads `.formulae[]`; malformed JSON → empty list, never an error |
| `parse_cask_names` | same, `--cask` | The "normal" (non-greedy) name set |
| `parse_casks` | same, `--cask --greedy` | Tags `self_updating` = in greedy but not normal |
| `parse_mas_outdated` | `mas outdated` (plain text) | Id, then last `(old -> new)`; see [03](03-update-sources.md) |
| `parse_npm_outdated` | `npm outdated -g --json` | Drops items where installed ≥ latest (semver) |
| `parse_appcast_items` | Sparkle XML | Per `<item>`: version (element or `<enclosure>` attribute), channel, min/max macOS, **validated** notes link |
| `newest_offer` | items + installed + macOS | The newest release this Mac would be offered: skips opt-in channels (beta, nightly…) and beta versions unless already on one, and releases outside its macOS range |
| `parse_cask_tokens` | `brew list/search --cask` | Skips `==>` headers and prose (real tokens have no whitespace) |
| `parse_cask_info` | `brew info --cask --json=v2` | `name` is an array in brew's schema |
| `parse_signing` | `codesign -dvv` | `(team_id, authority)`; treats `"not set"` as absent |
| `parse_notarized` | `spctl --assess` | Matches `source=notarized`, not a bare "notarized" substring |
| `parse_schedule_time` | the launchd plist | Tolerates whitespace in hand-edited plists; range-validated |
| `extract_jws_payload` | `cask.jws.json` | The payload is a JSON *string* inside the envelope |
| `build_cask_app_index` | the payload | `.app` filename → token; bare token beats `@`-variant |
| `resolve_cask_app_filenames` | payload + tokens | The reverse lookup, for targeted verification |

Two of these encode bugs that tests caught and that must not regress:

- **`parse_mas_outdated`** — naive splitting produced empty names.
- **`parse_notarized`** — a substring match on `"notarized"` also matches
  `source=Unnotarized Developer ID`, which is spctl's output for *signed but not notarized*. The
  check is anchored to `source=notarized`.

### Validators

Detailed threat reasoning is in [06 — Security](06-security.md); the rules:

| Function | Rule |
|---|---|
| `is_valid_cask_token` | Non-empty, ≤128, starts alphanumeric, then `[A-Za-z0-9-@._+]` |
| `is_numeric_id` | Non-empty, ≤20, all ASCII digits |
| `is_valid_npm_package` | ≤214, no leading `-`/`.`/`_`, **no `://`**, optional single `@`, then `[a-z0-9-_./]` |
| `is_safe_search_query` | Non-empty, ≤64, no leading `-`, no control characters |
| `is_safe_url` | `http(s)://`, ≤2048, no controls or spaces — for `open` |
| `is_safe_feed_url` | **https only**, ≤2048, no controls/spaces, host not private/local — for network fetches |
| `url_host` | Hand-rolled host extraction: strips scheme, userinfo, port, path; handles `[IPv6]` |
| `is_local_or_private_host` | `localhost`, `.local`, `.localhost`, `.internal`; RFC1918, loopback, link-local, unspecified, broadcast, CGNAT 100.64/10; IPv6 ULA `fc00::/7` and link-local `fe80::/10` |
| `valid_schedule_time` | `hour ≤ 23 && minute ≤ 59`, `const fn` |

`url_host` is hand-rolled rather than pulling in a URL crate. That is a deliberate trade: one
small, fully-tested function against a dependency with its own attack surface and update burden.
It is also where a parsing bug would be most costly, which is why it has dedicated tests for
userinfo (`https://user:pass@evil/`), ports, and bracketed IPv6.

### Decision functions

- `build_update_command(kind, id) -> Option<(&'static str, Vec<String>)>` — the single place a
  selection becomes a command. Every arm is gated on its validator; an unknown kind or a failing
  id returns `None`, which the caller turns into a per-item failure. **This function is the
  command-injection chokepoint.** Anything that runs an update must go through it.
- `fetch_batches(selections) -> (formulae, casks)` — partitions for prefetch, re-validating with
  the same rules so a crafted token can't reach `brew fetch` either.
- `adopt_args(token)` — the adopt argv, pure so `--adopt` can't silently vanish.
- `version_is_older(installed, latest)` — lenient dotted-numeric compare for Sparkle.
- `cert_changed(prev, current)` — `true` only when **both** are present and differ. A
  newly-seen app or one that lost its signature is *not* drift.
- `select_verify_results(entries, only_changed)` — post-update mode shows drift only.
- `build_verify_entries(sigs, &mut store)` — diffs against the baseline, mutates it, and renders
  the human-readable output. Takes the store by reference so the caller can hold the lock across
  load → mutate → save.
- `badge_title(count) -> Option<String>` — `None` at zero, so the tray title clears.

## Layer 2 — I/O

Kept as thin as possible. If one of these functions grows a branch, that branch probably belongs
in Layer 1.

### `run_cmd(cmd, args) -> Result<String, String>`

Captures stdout. **A non-zero exit with non-empty stdout is treated as success** — `npm outdated`
exits 1 when things are outdated, and stdout is the truth. Only an empty stdout on failure
produces `Err(stderr)`.

### `run_update_cmd(cmd, args) -> (bool, String)`

For mutating commands. Merges stdout and stderr (`combine_output`), applies
`maybe_append_mas_hint`, and sets `SUDO_ASKPASS` to the generated helper so a tool that calls
`sudo -A` gets a native macOS password dialog instead of failing on a nonexistent TTY. Never
returns `Err` — a spawn failure becomes `(false, "Failed to run …")`, because a batch must
continue.

**Both of these build an argv. Nothing in this codebase constructs a shell string.** There is no
`sh -c` anywhere, which is what makes the validators defence-in-depth rather than the only line
of defence.

### Filesystem

- `freshet_dir()` — creates `~/.freshet` and forces mode `0700`, every call.
- `write_private(path, contents, executable)` — writes then chmods `0700`/`0600`. Not
  umask-dependent.
- `askpass_program()` — writes `askpass.sh` once per process behind a `OnceLock`.
- `load_signing_store` / `save_signing_store` — JSON `HashMap<app name, team id>`; a corrupt file
  degrades to an empty baseline rather than an error.
- `brew_cache_dir()` — `OnceLock`-memoised `brew --cache`.
- `list_applications()` — non-recursive `/Applications` scan for `*.app` (case-insensitive
  extension). Does **not** cover `~/Applications` or nested bundles; see gaps.

### Process-shaped reads

- `read_plist_string(info_path, key)` — `defaults read`. Note this takes the Info.plist path
  *without* the `.plist` extension, which is what `defaults` expects.
- `fetch_url(url)` — `curl -sL --proto =https --proto-redir =https --max-time 8 --max-filesize
  5242880`. `--proto-redir` is the important flag: it enforces https across *every redirect hop*,
  so a feed can't 302 you down to plaintext or into `file://`. TLS verification is never disabled
  — `-k` appears nowhere. Re-validates the URL itself before spawning.

## Layer 3 — commands

Uniform shape:

```rust
#[tauri::command]
async fn thing(arg: T) -> R {
    tauri::async_runtime::spawn_blocking(move || thing_blocking(&arg))
        .await
        .unwrap_or_else(|_| /* safe fallback */)
}
```

Three properties hold across all of them:

1. **Blocking work never runs on the async runtime.** Every one of these shells out.
2. **A join error produces a fallback value, not a rejection** — `failed_category(...)`, an empty
   `Vec`, `None`. The frontend's `catch` blocks are belt-and-braces; the normal path never
   rejects.
3. **Validation happens before any work.** `cask_info` returns `None` for a bad token before
   spawning; `search_casks` returns `vec![]` for a bad query.

The two schedule mutators are the deliberate exception: they return `Result<(), String>` because
a launchctl failure is something the user should see, and the UI keeps the toggle reflecting
reality rather than optimistically flipping it.

### Command catalogue

| Command | Returns | Notes |
|---|---|---|
| `check_outdated` | `Vec<CategoryStatus>` | 4 parallel `spawn_blocking`; always 4 entries, errors inline |
| `check_sparkle_apps` | `CategoryStatus` | Network-bound; separate so it can't stall the rest |
| `run_selected_updates` | `Vec<UpdateResult>` | `brew update` once if any brew/cask → prefetch → serial installs |
| `verify_signatures` | `Vec<UpdateResult>` | Full `/Applications`; emits `verify-progress`; `only_changed` filter |
| `verify_updated_casks` | `Vec<UpdateResult>` | Targeted; drift only; empty when nothing resolves |
| `suggest_adoptions` | `Vec<AdoptableApp>` | Local cask metadata, no network; sorted case-insensitively |
| `search_casks` | `Vec<CaskSuggestion>` | Flags already-managed tokens |
| `cask_info` | `Option<CaskInfo>` | desc, homepage, version, `auto_updates` |
| `adopt_cask` / `adopt_casks` | `UpdateResult`(s) | `brew install --cask <t> --adopt` |
| `get_schedule` | `ScheduleStatus` | Reads the real plist so the UI reflects disk, not defaults |
| `install_schedule` / `uninstall_schedule` | `Result<(), String>` | Range-validated; writes script + plist; `launchctl` reload |
| `open_url` | `()` | `is_safe_url` then `open`. Silent no-op on failure — intentional |
| `set_badge` | `()` | Tray title; `None` at zero |

### The update pipeline

`run_selected_updates_blocking` in order:

1. **`brew update` once**, if any selection is `brew` or `cask`. Emitted as its own
   `UpdateResult` named `"brew update"` so a metadata-refresh failure is visible rather than
   mysteriously poisoning every subsequent item.
2. **`prefetch_selected`** — concurrent, batched `brew fetch` per kind. Best-effort; a miss just
   means the upgrade downloads inline.
3. **The serial loop.** Per item: `build_update_command` → run → push an `UpdateResult`. An
   invalid or unsupported selection becomes a failed result and the loop continues. **Nothing
   short-circuits the batch** — user story 20, and the behaviour people actually need.

Then, on the frontend side, `verify_updated_casks` runs against the cask tokens from the batch
and any drift is prepended to the results with a warning title.

## Error model

There are no `Result`-returning commands except the two schedule mutators, and no error type. The
model is:

- **Per-item errors are data.** `UpdateResult { success: false, output: "<the real tool output>" }`.
- **Per-source errors are data.** `CategoryStatus { error: Some(msg) }`.
- **Raw tool output is preserved verbatim**, and augmented rather than replaced when we can add
  something useful (`maybe_append_mas_hint`). Paraphrasing a `brew` conflict message destroys the
  only information that makes it actionable.
- **Optional things use `Option`** and a `None` renders as absence, not an error.

`unwrap_used` is a warn-level clippy lint; production code uses `?`, `unwrap_or_else`,
`unwrap_or_default`, and let-else instead. The two `.expect("home dir")` calls in `plist_path()`
and `script_path()` are the standing violations of this (see gaps).

## Lints

`Cargo.toml` sets `clippy::all` and `clippy::pedantic` to **deny**, `nursery` to warn, plus
`unwrap_used`, `todo`, and `dbg_macro` as warnings, and `unsafe_code = "deny"`. Roughly a dozen
noisy pedantic lints are allowed back with a one-line comment each.

**The rule for relaxing a lint: do it narrowly, at the site, with a comment saying why.** The
three in the codebase are all genuine:

```rust
#[allow(clippy::needless_collect)]                        // collect() is what makes the fetches parallel
#[allow(clippy::needless_pass_by_value)]                  // Tauri AppHandle convention; it's an Arc clone
#[allow(clippy::case_sensitive_file_extension_comparisons)] // .local/.internal are mDNS suffixes, not extensions
```

Test code gets its own block-level allow (`unwrap_used`, `needless_raw_string_hashes`) rather than
loosening the project.

## Known gaps

- **`list_applications()` only scans `/Applications`.** Apps in `~/Applications`, `/System/
  Applications`, or nested subdirectories are invisible to both Sparkle detection and signature
  verification.
- **macOS App Management can refuse a cask install or adoption.** Since macOS 13, changing another
  developer's app bundle needs the user's permission (Privacy & Security › App Management), and
  sudo doesn't get round it; Homebrew's `chgrp -hR admin` during `--adopt` is the usual trigger.
  `apps::explain_app_management` recognises the "Operation not permitted" wall, replaces it with
  an explanation and offers `open_app_management_settings`. A dev build's permission belongs to
  the terminal that launched it, not to Freshet.
- **Sparkle apps installed by a Homebrew cask are left to Homebrew** (its greedy check already
  sees them), so they aren't listed twice. Apps that set their feed URL in code rather than in
  `Info.plist` (`SUFeedURL`) aren't detected at all.
- **The signing store is keyed by app *name*.** Renaming an app loses its baseline (a silent
  re-seed, not a false alarm); two apps with the same file stem in different locations would
  collide.
- **`.expect("home dir")`** in `plist_path()`/`script_path()` panics where the rest of the code
  degrades.
- **`open_url` accepts `http://`** as well as https. It hands the URL to the OS rather than
  fetching it, so this is a considered choice, but it is a looser rule than `is_safe_feed_url`.
- **Cross-process races on `signing.json`** are last-writer-wins. The in-process mutex does not
  help if a future scheduled run writes from a second process.
