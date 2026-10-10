# 02 — System Architecture

## Shape

Freshet is a single-process Tauri 2 desktop app: a Rust binary that owns a macOS menu-bar tray
icon and one hidden-by-default WKWebView window, rendering a React frontend.

```
┌─────────────────────────────────────────────────────────────────┐
│  Freshet.app (one process)                                      │
│                                                                 │
│  ┌───────────────────────────┐        ┌──────────────────────┐  │
│  │  WKWebView ("main")       │  IPC   │  Rust core           │  │
│  │  React 19 + TS            │◄──────►│  src-tauri/src/      │  │
│  │  src/App.tsx              │ invoke │      main.rs         │  │
│  │  src/logic.ts  (pure)     │ events │  15 commands         │  │
│  │  420 × 680, transparent   │        │  pure core + IO shell│  │
│  └───────────────────────────┘        └──────────┬───────────┘  │
│                                                   │              │
│  ┌───────────────────────────┐                    │              │
│  │  Tray icon "main-tray"    │                    │              │
│  │  left-click → toggle win  │                    │              │
│  │  right-click → Quit menu  │                    │              │
│  │  title = badge count      │                    │              │
│  └───────────────────────────┘                    │              │
└───────────────────────────────────────────────────┼──────────────┘
                                                    │ Command::new (argv, no shell)
        ┌───────────────────────┬───────────────────┼───────────────┬─────────────┐
        ▼                       ▼                   ▼               ▼             ▼
     brew / mas / npm      codesign / spctl      curl (https)    defaults    launchctl
     (package managers)    (signature check)   (Sparkle feeds)  (Info.plist) (schedule)
```

**Nothing is downloaded or executed by Freshet itself.** Every artifact fetch and integrity check
belongs to the package manager being driven. The single exception is `curl` fetching Sparkle
appcast XML, which is parsed for a version string and never acted on. This boundary is the
foundation of the security model ([06](06-security.md)).

## Why Tauri

A menu-bar utility is always resident. That makes footprint a product attribute, not an
engineering vanity: an Electron shell at 150 MB+ RSS is the wrong thing to leave running forever
on a machine whose whole selling point is hygiene. Tauri uses the system WKWebView, so the app
ships as a single static binary with no bundled runtime.

The cost is real and worth stating: two languages, two toolchains, two lint configurations, and
Tauri-specific concepts (capabilities, the async runtime, command threading) that a contributor
has to learn. The mitigation is a deliberately **thin Rust layer** — process execution, OS
integration, parsing — with decision logic pushed into TypeScript where it is cheaper to change
and test. See [13 — Decisions](13-decisions.md).

## The IPC boundary

This is the only interface between the two halves, and the only place where the shape of the
system is contractual.

### Commands (frontend → backend, request/response)

All 15 are registered in `main()` via `tauri::generate_handler!`. Full catalogue with signatures
in [04 — Backend](04-backend.md).

```
Detection     check_outdated() -> Vec<CategoryStatus>          (brew, cask, mas, npm — parallel)
              check_sparkle_apps() -> CategoryStatus            (network-bound, separate on purpose)

Updating      run_selected_updates(selections) -> Vec<UpdateResult>

Provenance    verify_signatures(only_changed?) -> Vec<UpdateResult>     (full /Applications)
              verify_updated_casks(tokens) -> Vec<UpdateResult>         (targeted, post-update)

Adoption      suggest_adoptions() -> Vec<AdoptableApp>
              search_casks(query) -> Vec<CaskSuggestion>
              cask_info(token) -> Option<CaskInfo>
              adopt_cask(token) -> UpdateResult
              adopt_casks(tokens) -> Vec<UpdateResult>

Schedule      get_schedule() -> ScheduleStatus
              install_schedule(hour, minute) -> Result<(), String>
              uninstall_schedule() -> Result<(), String>

Shell-ish     open_url(url)            (validated, opens in default browser)
              set_badge(count)         (tray title)
```

### Events (backend → frontend, push)

One channel: **`verify-progress`**, payload `{ done: usize, total: usize }`. Emitted once at the
start of `verify_signatures` and once per inspected app, so the header can show a live percentage
across a ~100-app scan. The frontend subscribes for the duration of the scan and unsubscribes in
a `finally`.

Everything else is request/response. Notably, `run_selected_updates` does **not** stream
per-item progress — it returns the complete `Vec<UpdateResult>` when the batch finishes. This is
a known UX gap for long batches; see [12 — Roadmap](12-roadmap.md).

### Design rules for the boundary

1. **Commands don't reject.** With the exception of the two schedule mutators (which return
   `Result<(), String>` because a launchctl failure is actionable), commands return an empty or
   "failed" value rather than an IPC error. A per-source failure must never blank the window or
   take down the other four sources; it becomes `CategoryStatus { items: [], error: Some(msg) }`.
2. **Every id crossing into the backend is validated there.** The webview is treated as an
   untrusted input source even though we ship its contents. See [06](06-security.md).
3. **The backend holds no session state.** Commands are independent. All in-flight UI state
   (selections, expansion, results) lives in React; all durable state is on disk or in
   `localStorage`.

## Backend layering

`src-tauri/src/main.rs` is one file, but it is three strictly separated layers. This separation
is the project's primary testing strategy, not an aesthetic preference.

```
Layer 3  #[tauri::command] async fn ...        thin; spawn_blocking + unwrap_or_else fallback
            │
Layer 2  fn check_brew() / fetch_url() / ...   I/O: Command::new, fs, network. Not unit-tested.
            │
Layer 1  fn parse_brew_outdated(&str) -> ...   PURE. No I/O. 50 unit tests live here.
```

Layer 1 holds every parser (`parse_brew_outdated`, `parse_mas_outdated`, `parse_appcast_latest`,
`parse_schedule_time`, `parse_signing`, …), every validator (`is_valid_cask_token`,
`is_safe_feed_url`, `is_valid_npm_package`, …), and every decision function
(`build_update_command`, `cert_changed`, `version_is_older`, `select_verify_results`). They take
strings and return values. They are tested against output captured from the real tools.

**Rule for contributors: if a function makes a decision, it belongs in Layer 1 and needs a test.
If it touches the world, it belongs in Layer 2 and should be as close to trivial as possible.**

The mirror of this on the frontend is `src/logic.ts` — pure functions, no React — tested by
`src/logic.test.ts` without rendering anything.

## Concurrency model

Tauri commands run on its async runtime; blocking work is pushed to a blocking pool with
`tauri::async_runtime::spawn_blocking`. Four distinct patterns are in use, each chosen for a
reason:

| Site | Pattern | Why |
|---|---|---|
| `check_outdated` | 4 × `spawn_blocking`, awaited together | Four independent `brew`/`mas`/`npm` processes; wall-time is the slowest one, not the sum. |
| `check_sparkle` | `std::thread::scope`, one thread per feed | Each feed is a network round-trip. The `.collect::<Vec<_>>()` on the handles is load-bearing — without it, spawn and join interleave and the fetches serialise. Hence the `#[allow(clippy::needless_collect)]`. |
| `prefetch_selected` | `thread::scope`, **one batched `brew fetch` per kind** | Not one process per token. Homebrew loads metadata once and parallelises the downloads itself (16-way by default), which measured 1.6× faster than per-token fetches. See [11 — Performance](11-performance.md). |
| `verify_signatures` | one `spawn_blocking` per app + `AtomicUsize` | ~100 apps × (`codesign` + `spctl`). The atomic drives the progress events. |

**The install loop is deliberately serial.** `run_selected_updates_blocking` iterates selections
one at a time. Concurrent `brew upgrade` invocations contend on Homebrew's own locks, and serial
execution keeps each item's output cleanly attributable to that item in the results panel. The
downloads are already overlapped by the prefetch step, so the serial part is mostly local install
work.

One shared mutable resource exists: `~/.freshet/signing.json`. It is guarded by
`signing_store_lock()`, a process-wide `Mutex` held across the whole load → mutate → save
sequence so two concurrent verify runs can't lose each other's updates. Poisoning is recovered
from rather than panicked on (the guarded value is a plain `HashMap`). Cross-*process* races are
not covered and are accepted as last-writer-wins.

## State and where it lives

### Durable, backend — `~/.freshet/` (created `0700`)

| Path | Mode | Contents |
|---|---|---|
| `signing.json` | `0600` | The trust baseline: `{ app name → Team ID }`. Diffed to detect signer drift. |
| `askpass.sh` | `0700` | Generated once per process. `SUDO_ASKPASS` target; shows a native password dialog. |
| `run.sh` | `0700` | Generated when a schedule is installed. What launchd runs. |
| `run.log` | — | Append-only log of scheduled runs. |
| `launchd.out.log`, `launchd.err.log` | — | launchd's own stdout/stderr capture. |

The `0700` directory and `0600`/`0700` file modes are a security control, not tidiness: a
world-writable baseline would let another local user pre-seed an attacker's Team ID and suppress
the hijack warning, and a writable `askpass.sh` is direct code execution under the user's sudo
prompt.

Also durable and owned by the backend:
`~/Library/LaunchAgents/com.freshet.daily.plist` (see [08 — Scheduling](08-scheduling.md)).

### Durable, frontend — `localStorage`

| Key | Type | Meaning |
|---|---|---|
| `freshet.skipped` | `string[]` | Skipped versions, keyed `kind::id::latest` so a skip auto-clears when a newer version ships. |
| `freshet.greedy` | `"0"`/`"1"` | Show self-updating casks. |
| `freshet.notifyOnly` | `"0"`/`"1"` | Detect but never install. |

All three reads are wrapped in `try/catch` — storage being unavailable degrades to "the toggle
works, it just won't survive a relaunch," never to a crash.

### Ephemeral — React state only

Selections (`Record<selKey, Selection>`), category expansion, the results panel, the adopt panel,
verify progress. Selection state is intentionally **not** persisted: what you chose to update
last Tuesday is not a useful default today, and a stale selection silently updating something is
the exact surprise the product is trying to avoid.

## Window and tray behaviour

Configured in `src-tauri/tauri.conf.json` and wired in `main()`:

- **Window** `main`: 420 × 680, `resizable: false`, `decorations: false`, `transparent: true`,
  `skipTaskbar: true`, `visible: false` at launch, `titleBarStyle: Transparent`. `macOSPrivateApi`
  is on, which is what allows the transparent/vibrant popover look.
- **Tray** `main-tray`: template icon (so macOS tints it for light/dark menu bars).
  Left-click up → toggle the window; on show, `move_window(Position::TrayCenter)` re-anchors it
  under the icon. Right-click → a menu with a single **Quit** item. `set_title` carries the badge
  count, cleared to `None` at zero.
- **Dragging** is CSS-driven: `data-tauri-drag-region` on the header and footer strips, explicitly
  disabled (`data-tauri-drag-region="false"`) on the buttons inside them so clicks still land.
  This requires the `core:window:allow-start-dragging` capability.
- **Re-anchoring on every open is intentional.** A drag during a session is not persisted; the
  window returns under the tray icon next time. This matches how other menu-bar utilities behave.

## Frontend architecture

Detailed in [05 — Frontend](05-frontend.md). The essentials:

- Four components in `src/App.tsx`: `App`, `AdoptPanel`, `ResultsPanel`, `CaskDetail`. No router,
  no state library, no component framework. Panels are full-window overlays.
- All derived-state and decision logic lives in `src/logic.ts` as pure functions and is unit
  tested. `App.tsx` holds `useState` and JSX.
- The Sparkle category is fetched by a **separate command** and merged into the display list
  client-side, so a slow or unreachable appcast can never stall the four local checks. A
  placeholder "Other apps" row is always rendered so the user sees it loading rather than seeing
  it appear from nowhere.
- Self-updating casks are always fetched (`--greedy`) and tagged; the greedy toggle filters
  client-side, so flipping it is instantaneous with no backend round-trip.

## Build and distribution

```
pnpm tauri dev     vite dev server on :1420, Rust rebuilt on change
pnpm tauri build   vite build → ../dist → embedded in the Rust binary → .app + .dmg
```

The frontend is fully bundled into the binary; there is no runtime file loading. Fonts are
self-hosted `woff2` under `src/assets/fonts/` specifically so the CSP can forbid external
requests entirely. CSP is set in `tauri.conf.json`: `default-src 'self'`, no remote scripts, no
remote fonts, `connect-src` limited to the IPC origins.

`landing/` is a separate, dependency-free static site (plain HTML/CSS/JS, no build step) for
freshet.app. It is not part of the app build and is excluded from ESLint.

## Known gaps

- **No streaming progress during updates.** A ten-item batch shows "updating…" and nothing else
  until it completes.
- **`plist_path()` and `script_path()` call `.expect("home dir")`** and will panic on a machine
  with no resolvable home directory. Everything else in the codebase degrades gracefully.
- **The header comment in `main.rs` names `run_updates`**, a command that no longer exists; the
  real one is `run_selected_updates`.
- **`is_valid_cask_token` is also used to validate brew *formula* names.** The rule set happens to
  be correct for both, but the name misleads.
