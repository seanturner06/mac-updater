# 05 — Frontend Design (React + TypeScript)

```
src/
  main.tsx        mount
  App.tsx         946 lines — 4 components, all state, all JSX
  logic.ts        110 lines — pure decision logic, zero React imports
  logic.test.ts   173 lines — 24 tests, no rendering
  App.css         713 lines — tokens + every style
```

## What isn't here, and why

No router, no state manager, no component library, no CSS framework, no test-rendering library.

The app is one 420 × 680 window with four views that are mutually exclusive overlays. Adding
Redux or React Router to that would be more machinery than product. The absence is a decision,
not an omission — see [13 — Decisions](13-decisions.md). The point at which it stops being right
is when a *fifth* overlay appears or when prop-drilling through `AdoptPanel` becomes painful;
the answer then is `useReducer` plus one context, not a library.

## The pure-logic layer

`src/logic.ts` is the frontend's Layer 1, mirroring the backend's pure-core discipline. It
contains no React imports and every function is a pure transformation. This exists because a
menu-bar GUI is hard to click-test exhaustively, so the decision logic is extracted to where it
can be asserted directly.

```ts
selKey(kind, id)                        → "kind::id"            selection identity
ignoreKey(kind, id, latest)             → "kind::id::latest"    skip identity — version-scoped
visibleItems(cat, greedy)                                       applies the self-updating filter
displayItems(cat, greedy, ignored)                              + the skipped-version filter
isReadOnly(kind, notifyOnly)                                    sparkle, or notify-only mode
countDisplayed(cats, greedy, ignored)                           the badge / header number
allDisplayedSelections(cats, greedy, ignored)                   what "Update all" actually runs
toggleSelected(selected, kind, id, name)                        one row
toggleCategorySelected(selected, kind, items)                   select-all / clear for a category
prunedSelfUpdating(selected, cats)                              drop now-hidden items
```

Three of these carry real product decisions:

**`ignoreKey` includes the version.** Skipping is per-*version*, not per-app. Skip Slack 4.36 and
it disappears; when 4.37 ships the key no longer matches and it comes back on its own. This is
why the UI calls it "skip this version" rather than "ignore app" — it is a materially different
feature from the ignore/unignore described in the original user stories, and it is the better
one: an ignore list is a thing you forget you have.

**`allDisplayedSelections` is what "Update all" runs.** It is not `brew upgrade`. It enumerates
exactly the rows currently visible — skipped versions excluded, self-updating casks excluded
unless the greedy toggle is on, read-only kinds excluded — and runs those as individual
selections. **The button updates what you can see, and nothing else.** A blanket upgrade would
be faster and would occasionally update something the user had explicitly hidden, which is the
single worst thing a tool with admin rights can do.

**`prunedSelfUpdating`** is called when the greedy toggle is turned *off*. Without it, an item
you selected while greedy was on stays in the selection map after disappearing from the screen,
and then gets updated. Selection state must never outlive visibility.

All nine are tested in `logic.test.ts` (24 tests, including an explicit edge-case block for empty
categories and the all-items-filtered case).

## State model

All state lives in `App`. Grouped by lifetime:

**Server-ish data** — `categories`, `sparkle`, `lastChecked`, `scheduleEnabled`, `hour`, `minute`.

**Loading flags** — `loading`, `sparkleLoading`, `updating`, `verifying`, `verifyProgress`,
`adoptSearching`, `adoptingToken`, `batchAdopting`, `suggestLoading`. Deliberately separate rather
than one status enum: several of them are concurrent (the local check and the Sparkle check
overlap by design), and the header shows a priority-ordered message across them.

**User intent, ephemeral** — `selected`, `expanded`, `openLogs`, `adoptSelected`, `adoptQuery`.

**User intent, persisted** — `greedy`, `notifyOnly`, `ignored`, initialised lazily from
`localStorage` via `useState(() => …)`.

**View routing** — `results` + `resultsTitle`, `showAdopt`, `showSettings`. Whichever is truthy
renders as an overlay; there is no route table.

`AdoptPanel` owns three pieces of local state (`infoOpen`, `infoData`, `infoLoading`) because
expanded cask details are purely its concern and caching them there means reopening a detail is
free.

### Persistence

```ts
freshet.skipped    JSON string[]   the ignoreKey set
freshet.greedy     "0" | "1"
freshet.notifyOnly "0" | "1"
```

Every read and write is wrapped in `try/catch`. Storage being unavailable degrades to "the
setting works this session"; it never throws into a render. `JSON.parse` output is validated as
an array of strings before becoming a `Set` — an unvalidated parse here was one of the real bugs
that the strict TypeScript ruleset surfaced.

Selections are **not** persisted. Fresh decision every session.

## Effects

Only three, all with justified dependencies:

1. **Mount** — `checkNow()` and `get_schedule()`. The schedule is read from the installed plist,
   so the picker shows what is actually scheduled after a relaunch rather than a hardcoded
   default.
2. **`[showAdopt]`** — rescan for adoptable apps whenever the panel opens, clearing any previous
   selection. `/Applications` changes between openings.
3. **`[totalOutdated]`** — push the count to the tray badge. One-way, derived, idempotent.

`react-hooks/set-state-in-effect` is set to **warn** rather than error specifically because the
"reset and fetch on open" shape in effect 2 is legitimate and the rule can't tell. Visible,
not blocking.

## Data flow

### A check

```
checkNow()
  ├─ fire check_sparkle_apps  (not awaited) → setSparkle       ~1–8 s, network
  └─ await check_outdated                   → setCategories    ~1–4 s, local
                                            → setSelected({})   clear stale selections
                                            → setLastChecked(now)

render: displayCategories = [...categories, sparkle ?? placeholder]
        → displayItems() per category → rows
        → countDisplayed() → header text + tray badge
```

The Sparkle promise is deliberately not awaited. The local list must paint as soon as it is
ready. The placeholder category renders from the first frame showing "checking…", so the section
doesn't pop into existence later and shift the layout.

On an IPC failure `checkNow` keeps the previous list rather than blanking the window — an empty
window reads as "everything is current," which would be a lie.

### An update

```
updateSelected / updateAll
  → selections (from `selected`, or from allDisplayedSelections)
  → caskTokens = selections where kind === "cask"
  → run_selected_updates(selections)                 backend: brew update → prefetch → serial loop
  → if caskTokens.length: verify_updated_casks(tokens)   ← the hijack alarm, best-effort
  → if any drift: prepend those rows, title becomes "Update results — ⚠ N signer changes"
  → showResults(): auto-expand every failed row's log
  → checkNow()                                       refresh; updated items drop off the list
```

Two details that matter:

- **The verify call is wrapped in its own try/catch and can only add information.** A failure in
  the signature check must never prevent the user from seeing their update results.
- **Failures auto-expand.** `showResults` opens the log for every `success: false` row. Successes
  stay collapsed. The panel is for finding out what went wrong.

## Components

```
App
├── header        status dot · title · subtitle (priority: verifying → loading → current → count)
│                 refresh + settings icon buttons     [drag region]
├── categories    5 collapsible sections, each: tri-state select-all, count pill, rows
├── actions       Update selected (N) · Update all    [NOT a drag region]
├── footer        last checked HH:MM                  [drag region]
└── overlays (mutually exclusive)
    ├── Settings      display toggles · schedule · tools · skipped-version count
    ├── AdoptPanel    suggestions (batch) + cask search → CaskDetail
    └── ResultsPanel  per-row ✓/✕ with expandable <pre> logs
```

**Row rendering** branches on `isReadOnly(cat.kind, notifyOnly)`. A read-only row is a `<div>`
with no checkbox; an updatable row is a `<label>` wrapping a checkbox, which makes the entire
row a click target (Fitts's law — the checkbox itself is small).

The two row actions (`↗` release notes, `✕` skip this version) call `preventDefault()` and
`stopPropagation()`, because they live inside that `<label>` and would otherwise toggle the
selection.

**Category select-all is tri-state.** `checked` when all displayed items are selected,
`indeterminate` via a ref callback when some are. `indeterminate` is not an HTML attribute and
can only be set imperatively, which is the one ref in the codebase.

**`ResultsPanel` uses the array index as its key.** Acceptable only because the list is
render-once and never reordered or mutated. If results ever stream in, this must change.

## Conventions

- `type X = { … }`, never `interface` — enforced by
  `@typescript-eslint/consistent-type-definitions`.
- Types crossing IPC are declared locally in `App.tsx` (`UpdateResult`, `ScheduleStatus`,
  `CaskSuggestion`, `AdoptableApp`, `CaskInfo`) and must be kept in sync with the Rust structs by
  hand. There is no generated binding. Renaming a field in Rust will not fail the TypeScript
  build — it will produce `undefined` at runtime. **When you change a serialized struct, grep for
  its fields in `src/`.**
- `invoke<T>("cmd", { args })` is always parameterised with the expected return type;
  `type-coverage` is enforced at ≥99% (currently 100%), so an unparameterised `invoke` would show
  up as `any` and fail CI.
- Async handlers in JSX are permitted (`no-misused-promises` with `checksVoidReturn.attributes:
  false`), because the parent doesn't await them anyway. Fire-and-forget calls use `void invoke(…)`
  explicitly so `no-floating-promises` stays on everywhere else.

## Testing

`logic.test.ts` covers all nine exported functions plus an edge-case block, with no rendering.
That is deliberate: the value is in the decision logic, and a rendering harness would add a
dependency and a maintenance surface for assertions like "the div exists."

**What is consequently untested:** every effect, the overlay routing, the `localStorage`
round-trip, and the drift-prepending logic inside `runUpdate`. If a bug is found in one of
those, the fix is to extract the decision into `logic.ts` and test it there — not to add a DOM
harness. See [10 — Testing](10-testing-and-quality.md).

## Known gaps

- **No progress during updates.** `updating` is a boolean. A twelve-item batch shows "updating…"
  for several minutes with no indication of which item is running. The backend would need to emit
  per-item events (the `verify-progress` pattern already exists to copy).
- **No keyboard navigation or focus management** in the overlays; no `Escape` to close, no focus
  trap, no `aria-modal`.
- **Accessibility is thin.** Icon buttons have `title` but mostly lack `aria-label`; the results
  panel is not announced; the status dot carries meaning by colour alone (the text next to it
  does say the same thing, which saves it).
- **`searchCasks` has no error handling** — an unexpected rejection would leave `adoptSearching`
  true. The command doesn't reject today, so this is latent.
- **No light mode.** The palette is dark-only. The token structure supports adding a
  `prefers-color-scheme: light` block; see [09 — UI Design System](09-ui-design-system.md).
