# 10 — Testing & Quality

## Why the strategy is what it is

Freshet is a menu-bar GUI whose behaviour depends on the output of five external tools, on the
contents of `/Applications`, and on the network. Almost none of that is clickable into a
reproducible test, and the parts that are would need a machine in a known state.

So the strategy is not "test the app." It is: **push every decision into a pure function and test
that function against real tool output.** What remains — process spawning, file writes, network
calls — is kept so thin that it is hard for it to be wrong, and is verified by hand.

This has already paid for itself twice, with bugs that no amount of clicking would reliably have
found:

- **`parse_mas_outdated`** produced empty names. `mas outdated` separates the id from the name
  with multiple spaces and the name itself contains spaces, so naive splitting silently lost it.
- **`parse_notarized`** returned `true` for unnotarized apps, because a substring match on
  `"notarized"` also matches spctl's `source=Unnotarized Developer ID`. That is a **security
  check reporting the exact opposite of the truth** — the worst class of bug in this codebase,
  found by a test written against real `spctl` output.

## The four gates

Each layer trades latency for authority. The redundancy is intentional.

| Layer | Latency | Authority | Purpose |
|---|---|---|---|
| `pnpm test:watch`, `cargo test` | sub-second | advisory | Inner loop while typing |
| Agent / editor checks | on demand | advisory | Catch things as you edit |
| Pre-commit hook | seconds | mandatory-ish | Commit-time guardrail (bypassable with `--no-verify`) |
| **GitHub Actions** | minutes | **un-bypassable** | The merge gate |

GitHub Actions is the only layer that cannot be skipped. Every local layer is honour-system —
anyone can `git push --no-verify`. That is why CI exists even though the local checks are
identical.

It is also why the local layers exist even though CI would catch it anyway: **a 5-second failure
gets fixed; a 4-minute failure gets argued with.** The latency difference is the entire point.

**Status:** watch mode and GitHub Actions are in place. A pre-commit hook (lefthook) is the
highest-value missing piece. When you add a new check, wire it into **both** the hook and
`ci.yml`, or the inner loop drifts from the gate.

## Backend tests — 50, in `src-tauri/src/main.rs`

They live in the same file as the code, in `#[cfg(test)] mod tests`, with a module-level
`#[allow(clippy::unwrap_used, clippy::needless_raw_string_hashes)]` — test code gets its own
relaxation rather than loosening the project.

Coverage by area:

| Area | Tested |
|---|---|
| Parsers | brew, cask (incl. greedy tagging), mas, npm (incl. the semver-ahead filter), appcast version + notes, cask tokens, cask info, signing, notarization, schedule time |
| Validators | cask token, numeric id, npm package, search query, `is_safe_url`, `is_safe_feed_url`, `url_host`, `is_local_or_private_host` |
| Decisions | `build_update_command` per kind, `fetch_batches`, `adopt_args`, `version_is_older`, `cert_changed`, `select_verify_results`, `build_verify_entries`, `badge_title` |
| Index building | `extract_jws_payload`, `build_cask_app_index` (incl. `@`-variant precedence), `resolve_cask_app_filenames` |

**Not tested**, deliberately: `run_cmd`, `run_update_cmd`, `fetch_url`, `read_plist_string`,
`list_applications`, the signing-store file I/O, `install_schedule`, and the tray setup. These
are Layer 2 and Layer 3 — they spawn processes and touch the user's machine. Testing them would
require mocking `Command`, which would mean an abstraction layer built solely to be mocked, and
would test the mock rather than the behaviour. They are kept trivial instead.

### The fixture rule

**Ground fixtures in output captured from the real tool. Never invent the string.**

Both historical bugs above came from output shapes nobody would have guessed. Invented fixtures
test your mental model of the tool; real fixtures test the tool.

```sh
brew outdated --json=v2
brew outdated --cask --greedy --json=v2
mas outdated
npm outdated -g --json
brew info --cask --json=v2 slack
codesign -dvv /Applications/Safari.app 2>&1
spctl --assess --type execute -v /Applications/Safari.app 2>&1
curl -s "$(defaults read /Applications/<App>.app/Contents/Info SUFeedURL)" | head -60
```

Trim to the smallest excerpt that preserves the shape — keys, spacing, ordering, and any header
lines. Preserve odd whitespace exactly; that is usually the part that breaks parsers.

### Adversarial cases are required for validators

Every validator test covers the hostile inputs, not just the happy path:

```rust
assert!(!is_valid_cask_token("--force"));                    // flag injection
assert!(!is_valid_npm_package("git+https://evil/x.git"));    // arbitrary install spec
assert!(!is_safe_feed_url("https://169.254.169.254/x"));     // cloud metadata
assert!(!is_safe_feed_url("http://example.com/appcast"));    // plaintext
assert!(!is_safe_feed_url("https://[::1]/x"));               // IPv6 loopback
assert_eq!(url_host("https://evil.com@127.0.0.1/"), Some("127.0.0.1".into()));  // userinfo
```

A change that loosens a validator must come with a test demonstrating that the newly-allowed
input is actually safe.

## Frontend tests — 24, in `src/logic.test.ts`

Vitest, no rendering, no DOM. Every exported function in `src/logic.ts` has a `describe` block,
plus an explicit edge-case block for empty categories and the everything-filtered case.

The reason there is no rendering harness: the value lives in the decision logic, which is
already extracted. A DOM harness would add a dependency and a pile of assertions like "the
element exists" that break on every markup change while testing nothing about behaviour.

**The consequence, stated honestly:** effects, overlay routing, the `localStorage` round-trip,
and the drift-prepending logic inside `runUpdate` are untested. When a bug turns up in one of
those, **the fix is to extract the decision into `logic.ts` and test it there** — not to add a
rendering library. If a future change makes that impossible, that is the moment to revisit, with
a reason.

## Static analysis

The posture is: **pick the strictest preset the tool offers, fix everything that fires, then
relax individual rules narrowly with a one-line reason.** Never a blanket loosening, never a TODO
list of pending cleanups.

### Rust

```toml
[lints.rust]     unsafe_code = "deny", rust_2018_idioms, unreachable_pub, unused_qualifications,
                 trivial_casts, trivial_numeric_casts
[lints.clippy]   all = deny, pedantic = deny, nursery = warn
                 + unwrap_used = warn, todo = warn, dbg_macro = warn
```

`clippy::cargo` is omitted (it targets published crates). About a dozen noisy pedantic lints are
allowed back, each with a comment. Setting this floor initially required fixing 49 findings,
including real ones — `unwrap()` in production paths, `format!`-push-string patterns.

### TypeScript

`typescript-eslint`'s `strictTypeChecked` + `stylisticTypeChecked`, with `projectService` for
type-aware linting, plus `react-hooks` and `react-refresh`. `type-coverage` is enforced at ≥99%
and currently sits at 100% — no implicit `any` anywhere.

Setting this floor required fixing 54 findings, including real bugs: floating promises and an
unvalidated `JSON.parse`.

### Relaxations, and the three-level rule

Some lints are useful as signal and bad as a gate. Those get **`warn`**, not `off`:

```js
"react-hooks/set-state-in-effect": "warn"   // flags the legitimate "reset + fetch on open" shape
```

Every `off` or narrowed rule carries its reason inline:

```js
"@typescript-eslint/no-dynamic-delete": "off"                          // logic.ts toggles Records deliberately
"@typescript-eslint/no-confusing-void-expression": [..., { ignoreArrowShorthand: true }]  // onClick={() => setX(true)}
"@typescript-eslint/no-misused-promises": [..., { checksVoidReturn: { attributes: false } }]  // async JSX handlers
```

```rust
#[allow(clippy::needless_collect)]                           // collect() is what parallelises the fetches
#[allow(clippy::needless_pass_by_value)]                     // Tauri AppHandle convention; it's an Arc clone
#[allow(clippy::case_sensitive_file_extension_comparisons)]  // .local/.internal are mDNS suffixes
```

Test code gets its own override block in both languages rather than a project-wide allowance.

## CI

`.github/workflows/ci.yml` — three parallel jobs on push to `main` and on every pull request,
with in-flight runs for the same ref cancelled.

| Job | Runner | Steps |
|---|---|---|
| `rust` | `macos-latest` | `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features` |
| `web` | `ubuntu-latest` | `pnpm lint`, `tsc --noEmit`, `pnpm test`, `pnpm type-coverage` |
| `audit` | `ubuntu-latest` | `cargo audit --ignore RUSTSEC-2024-0429` |

**Rust runs on macOS** because that is what Freshet ships to, and because Tauri on Ubuntu pulls
in the GTK/WebKitGTK chain, which would mean installing Linux-only build dependencies to compile
a macOS-only app.

The single audit ignore is documented in the workflow with its verification:
`RUSTSEC-2024-0429` is a `glib` unsoundness reachable only through that Linux-only chain, and
`cargo tree --target aarch64-apple-darwin` confirms `glib` is absent from the macOS graph. **An
ignore needs that shape of evidence** — a command whose output proves the claim — not an
assertion. It comes with a removal condition: add a Linux target, remove the ignore.

## Running things locally

```sh
# Frontend
pnpm test                 # vitest, once
pnpm test:watch           # inner loop
pnpm lint                 # eslint
pnpm exec tsc --noEmit    # types
pnpm type-coverage        # ≥99%

# Backend
cd src-tauri
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all
cargo audit               # needs: cargo install cargo-audit
```

Everything CI runs, you can run. There is no CI-only step.

## What a change needs

| Change | Required |
|---|---|
| New parser | Pure function + tests against **real** tool output, including a malformed input |
| New validator | Pure function + tests, **including adversarial inputs** |
| New source | Parser + validator + the [03](03-update-sources.md) checklist |
| New UI decision logic | Goes in `logic.ts`, with tests |
| Pure-CSS change | No test; a before/after screenshot in the PR |
| New dependency | Justification in the PR; `cargo audit` must stay clean |
| Loosened lint | Inline comment with the reason; `warn` before `off` |
| Bug fix | A test that fails before the fix |

**Report the test counts after a change** (`50 passed` / `24 passed`). It is the cheapest
evidence that the change did what it claims.

## Performance testing

Separate from correctness testing, and deliberately so. `tests/performance-harness.py` runs the
real operations against whatever is actually outdated on the machine, with nothing mocked, and
writes a timestamped report to `tests/performance-results/`. See
[11 — Performance](11-performance.md). It is not part of CI and never will be — it depends on the
machine's current state and its network.
