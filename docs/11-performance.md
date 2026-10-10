# 11 — Performance

## Where the time actually goes

Essentially all user-visible latency in Freshet is spent inside external processes:

```
brew outdated --json=v2              ~0.5 – 2 s     Ruby startup + metadata load
brew outdated --cask --greedy        ~0.5 – 2 s     (and we run the non-greedy one too)
mas outdated                         ~1 – 3 s       talks to Apple's servers
npm outdated -g --json               ~1 – 4 s       hits the registry
curl <appcast> × N                   ~0.2 – 8 s     N network round-trips, parallel
codesign + spctl × ~100 apps         ~3 – 10 s      two process spawns per app
brew upgrade <anything>              seconds → minutes   download + install
```

The Rust orchestration around all of this runs in microseconds. Parsing a few hundred kilobytes
of JSON is not where four seconds went.

**So the operative question is never "is this function fast?" It is "which subprocess did we
wait on, and did we need to?"** That is an instrumentation question, not a benchmarking one.

## The principles

1. **Optimise user-perceived latency.** Nobody can tell whether `parse_brew_outdated` takes 50µs
   or 500µs. Everybody can tell whether a check takes 2s or 8s.
2. **Prefer doing less work over doing the same work faster.** Scope reduction beats parallelism,
   which beats micro-optimisation. The biggest win in the project came from inspecting 3 apps
   instead of 150.
3. **Never let a slow thing block a fast thing.** Network-bound work is separated from local work
   at the command level so it can't stall it.
4. **Measure before optimising, on a real machine.** Every optimisation below has a number
   attached.
5. **No speculative benchmarks.** Criterion microbenchmarks for the pure parsers would be
   ceremony. Add them when a hot loop actually emerges; it will not be a parser.

## What has been optimised, and by how much

### 1. Sparkle is a separate command

`check_sparkle_apps` is its own Tauri command rather than a fifth entry in `check_outdated`.

A machine with 20 Sparkle apps makes 20 HTTPS requests. If one server is unreachable, that thread
sits until curl's 8-second `--max-time`. Folding Sparkle into `check_outdated` would make every
refresh wait on the slowest appcast on the internet.

The frontend fires both, awaits only the local one, and renders a placeholder "Other apps" row
showing `checking…` so the section doesn't appear later and shift the layout. **Local results
paint at local speed, always.**

### 2. The greedy toggle filters client-side

Self-updating casks are always fetched with `--greedy` and tagged `self_updating`. Flipping the
toggle re-filters an array in React — **zero backend round-trips, instant**.

The alternative (re-running `brew outdated` with and without `--greedy` on each toggle) would
cost 1–2 seconds every time someone flipped a switch. The price paid is one extra `brew
outdated --cask` per check to compute the non-greedy name set. Worth it.

### 3. Batched concurrent prefetch — measured 1.60×

The install loop is serial. Without prefetching, each item downloads and then installs before the
next one starts, so the network sits idle during every install.

`prefetch_selected` warms Homebrew's download cache before the loop, and the *shape* of the
batching turned out to matter more than the concurrency:

**One `brew fetch` per kind — not one per token.** Homebrew loads formula metadata once and
parallelises the downloads internally (`HOMEBREW_DOWNLOAD_CONCURRENCY`, 16 by default on brew
4.4+). Spawning a process per token pays brew's ~1s Ruby/metadata startup 24 times over. The two
kinds run on their own threads so formula and cask downloads overlap.

Measured on a real machine, identical 463.22 MB across 24 formulae + 2 casks, cache cleared
before each run:

| Approach | Wall clock | Aggregate throughput |
|---|---|---|
| Per-item `brew fetch` (one process per token) | 17.80 s | 26.03 MB/s |
| **Batched per kind (shipped)** | **11.11 s** | **41.69 MB/s** |
| | **1.60× faster** | **1.60× higher** |

Breakdown: the 24 formulae (164 MB) fetched in one process in 8.82 s; the two casks (299 MB —
65% of the bytes) in 11.10 s on the other thread. The cask batch *is* the critical path, and
299 MB / 11.1 s ≈ 27 MB/s is the CDN/link ceiling for that machine, not a concurrency defect.

**The remaining time is genuine transfer, not overhead.** There is nothing left to squeeze here
without more streams on the two big casks, which is upstream's decision.

Prefetch is best-effort: a miss just means the upgrade downloads inline.

### 4. Targeted post-update verification — O(casks) not O(apps)

The original post-update signature check scanned all of `/Applications`: ~150 apps × two process
spawns, several seconds, after *every* update.

`verify_updated_casks(tokens)` resolves just-upgraded cask tokens to their `.app` filenames via
brew's **local** JWS metadata (no network), filters `/Applications` to those bundles, and
inspects only them. Upgrading three casks inspects three apps.

This is the canonical example of principle 2. The scan itself did not get faster; there is
simply almost none of it left. Full detail in [07 — Provenance](07-provenance.md).

Note that the targeted path inspects **serially** — one to five apps is already fast, and
threading it would be premature.

### 5. Memoised `brew --cache`

`brew --cache` shells out and is stable for the process lifetime, so it is behind a `OnceLock`.
Small, but it was being called repeatedly in the adoption path. The performance harness
originally included a redundant `brew --cache` probe per item, which inflated its own
measurements — a reminder to check the instrument before trusting the reading.

### 6. Parallel local checks

`check_outdated` runs brew, cask, mas, and npm on four blocking threads. Wall time is the slowest
one (usually npm or mas) rather than the sum — roughly 2–4 s instead of 4–9 s.

## What is deliberately *not* optimised

**The install loop is serial.** Concurrent `brew upgrade` invocations contend on Homebrew's own
locks, and serial execution keeps each item's output cleanly attributable in the results panel.
The downloads — the actually slow part — are already overlapped by the prefetch.

**`verify_signatures` (full inventory) is ~100 spawn_blocking tasks.** It is a user-initiated,
explicitly-slow operation with a progress ring. It is allowed to take ten seconds.

**Appcast XML is hand-parsed rather than streamed.** The documents are kilobytes.

**No caching of check results between launches.** Stale update information is worse than a
two-second wait — showing a user an update that no longer exists, or hiding one that does, costs
more trust than it saves time.

## Measuring

### First: instrumentation, not benchmarks

When something feels slow, the question is *which subprocess*. The intended tool is `tracing`
with `#[instrument]` on the command entry points and the per-source `check_*` helpers, run as
`RUST_LOG=info pnpm tauri dev`. That distinguishes "`brew update` took 4 seconds" (nothing we can
do) from "our code took 4 seconds" (something we can).

**This is not yet wired up.** It is the highest-value performance work outstanding.

### Then: a profiler, if a specific thing is slow

```sh
samply record cargo tauri dev     # opens in the Firefox Profiler
```

Don't run it speculatively.

### The live harness

```sh
python3 tests/performance-harness.py [--no-install] [--batched] [formula:a,b cask:x,y]
```

`tests/performance-harness.py` exercises the operations the app performs — concurrent `brew
fetch` to warm the cache, then the installs — **against whatever is genuinely outdated on the
machine right now. Nothing is mocked.** It measures per-item bytes and duration plus aggregate
wall clock, and writes a timestamped report to `tests/performance-results/` (never overwriting).

Flags worth knowing:
- `--no-install` — download phase only.
- `--batched` — mirrors the shipped `prefetch_selected` (one fetch per kind). Without it, the
  harness spawns one fetch per item, which is how the 1.60× head-to-head above was produced.
- `formula:a,b,c` / `cask:x,y` — explicit targets, so a run can be repeated against a known set
  after everything has already been installed.

It sets `SUDO_ASKPASS` the same way the app does, so an elevation prompt behaves identically.

**It is not in CI and should not be.** The result depends entirely on what is outdated and how
fast the network is that afternoon. It is a measurement tool, not a regression gate.

### Frontend

Tauri DevTools (`Cmd+Opt+I` in dev) → Performance, and the React DevTools Profiler if a render
feels janky. Neither has been needed: the component tree is four components and the data is a
list of a few dozen rows.

## Adding a performance claim

If you assert something is faster, attach a number from a real run, and say what the new
bottleneck is. The prefetch analysis above is the template: the measurement, the breakdown, *why*
it is faster, and where the remaining floor sits. "Optimised X" with no number is not reviewable.
