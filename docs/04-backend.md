# 04 — Backend Design (Rust)

Everything lives in `src-tauri/src/main.rs` (~2,550 lines including ~870 lines of tests).

## Why one file

It is a legitimate question and the honest answer is: it has not yet hurt. The file is
strictly ordered by concern, the pure/IO split does the work that module boundaries usually do,
and a single file keeps the pure functions and their tests visible to each other without `pub`
plumbing. The `[lints.rust] unreachable_pub` setting exists partly to keep that honest.

The point at which to split is when a **second contributor is regularly editing a different
concern** — the merge-conflict cost then exceeds the navigation benefit. The natural seams are
already visible as comment banners and map cleanly to modules:

```
parsers.rs     the pure Layer-1 parsers
validate.rs    the pure Layer-1 validators
sources/       brew.rs, cask.rs, mas.rs, npm.rs, sparkle.rs
adopt.rs       the cask index and adoption
provenance.rs  codesign/spctl, the signing store, drift
schedule.rs    launchd
tray.rs        setup() and the tray wiring
```

Do this as a mechanical move with no behaviour change, in its own commit, when the pain arrives.

## File layout as it stands

```
  1– 62   Types: OutdatedItem, CategoryStatus, UpdateSelection, UpdateResult
 64–156   Process + fs helpers: run_cmd, freshet_dir, write_private, askpass_program,
          maybe_append_mas_hint, combine_output, run_update_cmd
158–371   Pure parsers (Layer 1)
373–534   Pure validators + the JWS/cask index (Layer 1)
536–720   Check functions (Layer 2) + Sparkle
722–753   check_outdated / check_sparkle_apps commands
755–988   Adoption: search, suggest, info, adopt
990–1104  Update execution: build_update_command, fetch_batches, prefetch, the install loop
1106–1448 Provenance: inspect, signing store, drift, verify commands
1450–1601 launchd schedule
1603–1672 main(): tray, window, handler registration
1674–end  #[cfg(test)] mod tests — 50 tests
```

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
| `parse_appcast_latest` | Sparkle XML | Both element and attribute forms; takes the max |
| `parse_appcast_notes` | Sparkle XML | First `releaseNotesLink`, **validated** before returning |
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
- **The signing store is keyed by app *name*.** Renaming an app loses its baseline (a silent
  re-seed, not a false alarm); two apps with the same file stem in different locations would
  collide.
- **`.expect("home dir")`** in `plist_path()`/`script_path()` panics where the rest of the code
  degrades.
- **`open_url` accepts `http://`** as well as https. It hands the URL to the OS rather than
  fetching it, so this is a considered choice, but it is a looser rule than `is_safe_feed_url`.
- **Cross-process races on `signing.json`** are last-writer-wins. The in-process mutex does not
  help if a future scheduled run writes from a second process.
- **The stale header comment** at the top of `main.rs` names `run_updates` and
  `is_schedule_installed`, neither of which exists.
