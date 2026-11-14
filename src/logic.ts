// Pure UI logic, extracted so it can be unit-tested without rendering React.

export type OutdatedItem = {
  id: string;
  name: string;
  current: string;
  latest: string;
  self_updating: boolean;
  notes_url: string;
};

export type CategoryStatus = {
  category: string;
  kind: string;
  items: OutdatedItem[];
  error: string | null;
};

export const selKey = (kind: string, id: string) => `${kind}::${id}`;

// Keyed by version too, so skipping a version auto-clears when a newer one ships.
export const ignoreKey = (kind: string, id: string, latest: string) => `${kind}::${id}::${latest}`;

// Self-updating casks are hidden unless the "include self-updating apps" (greedy) toggle is on.
export function visibleItems(cat: CategoryStatus, greedy: boolean): OutdatedItem[] {
  return greedy ? cat.items : cat.items.filter((i) => !i.self_updating);
}

// Items shown after applying both the greedy filter and the user's skipped-version set.
export function displayItems(
  cat: CategoryStatus,
  greedy: boolean,
  ignored: Set<string>
): OutdatedItem[] {
  return visibleItems(cat, greedy).filter(
    (i) => !ignored.has(ignoreKey(cat.kind, i.id, i.latest))
  );
}

// A row is read-only (no checkbox, not updatable from here) for Sparkle apps, or in notify-only mode.
export function isReadOnly(kind: string, notifyOnly: boolean): boolean {
  return kind === "sparkle" || notifyOnly;
}

export function countDisplayed(
  categories: CategoryStatus[],
  greedy: boolean,
  ignored: Set<string>
): number {
  return categories.reduce((sum, c) => sum + displayItems(c, greedy, ignored).length, 0);
}

export type Selection = { kind: string; id: string; name: string };

// Every displayed, updatable item as a Selection — what "Update all" actually runs. Read-only
// kinds (sparkle) are excluded, as are skipped versions and (unless greedy) self-updating casks,
// so the button matches the visible count instead of blanket-upgrading things the user hid.
export function allDisplayedSelections(
  categories: CategoryStatus[],
  greedy: boolean,
  ignored: Set<string>
): Selection[] {
  return categories
    .filter((c) => !isReadOnly(c.kind, false))
    .flatMap((c) =>
      displayItems(c, greedy, ignored).map((i) => ({ kind: c.kind, id: i.id, name: i.name }))
    );
}

// Toggle one item in the selection map (pure — returns a new map).
export function toggleSelected(
  selected: Record<string, Selection>,
  kind: string,
  id: string,
  name: string
): Record<string, Selection> {
  const key = selKey(kind, id);
  const next = { ...selected };
  if (key in next) delete next[key];
  else next[key] = { kind, id, name };
  return next;
}

// Select all of `items` for a category, or clear them if they're already all selected.
export function toggleCategorySelected(
  selected: Record<string, Selection>,
  kind: string,
  items: OutdatedItem[]
): Record<string, Selection> {
  const keys = items.map((i) => selKey(kind, i.id));
  const allSelected = keys.length > 0 && keys.every((k) => k in selected);
  const next = { ...selected };
  if (allSelected) keys.forEach((k) => delete next[k]);
  else items.forEach((i) => (next[selKey(kind, i.id)] = { kind, id: i.id, name: i.name }));
  return next;
}

// Drop any self-updating items from the selection (used when turning the greedy filter off).
export function prunedSelfUpdating(
  selected: Record<string, Selection>,
  categories: CategoryStatus[]
): Record<string, Selection> {
  const next = { ...selected };
  categories.forEach((c) =>
    c.items.forEach((i) => {
      if (i.self_updating) delete next[selKey(c.kind, i.id)];
    })
  );
  return next;
}

// Settings shared with the backend (~/.freshet/settings.json) so the scheduled run honours them.
// Field names match the Rust `Settings` struct.
export type Settings = {
  greedy: boolean;
  notify_only: boolean;
  skipped: string[];
  check_vulnerabilities: boolean;
};

export function toSettings(
  greedy: boolean,
  notifyOnly: boolean,
  ignored: Set<string>,
  checkVulns: boolean
): Settings {
  return { greedy, notify_only: notifyOnly, skipped: [...ignored].sort(), check_vulnerabilities: checkVulns };
}

// What the daily scheduled run would install right now: the displayed Homebrew formulae and
// casks. Mirrors `plan_scheduled_run` in main.rs.
export function scheduledSelections(
  categories: CategoryStatus[],
  greedy: boolean,
  ignored: Set<string>
): Selection[] {
  return allDisplayedSelections(categories, greedy, ignored).filter(
    (s) => s.kind === "brew" || s.kind === "cask"
  );
}

// Mirrors the Rust `ScheduledRunSummary` (~/.freshet/last-run.json).
export type ScheduledRunSummary = {
  finished_at: number;
  notify_only: boolean;
  available: number;
  updated: string[];
  needs_admin: string[];
  failed: string[];
  signer_changed: string[];
  // Formula bottles Homebrew refused because their build provenance didn't verify.
  blocked?: string[];
};

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

// One-line outcome of the last scheduled run for the settings panel.
export function lastRunText(s: ScheduledRunSummary): string {
  if (s.notify_only) {
    return s.available === 0
      ? "up to date"
      : `${plural(s.available, "update", "updates")} found, none installed (notify-only)`;
  }
  const parts: string[] = [];
  if (s.signer_changed.length > 0) parts.push(`signer changed: ${s.signer_changed.join(", ")}`);
  if (s.blocked && s.blocked.length > 0) parts.push(`blocked (failed build verification): ${s.blocked.join(", ")}`);
  if (s.updated.length > 0) parts.push(`${s.updated.length} updated`);
  if (s.needs_admin.length > 0) {
    parts.push(`${plural(s.needs_admin.length, "needs", "need")} your password: ${s.needs_admin.join(", ")}`);
  }
  if (s.failed.length > 0) parts.push(`${s.failed.length} failed`);
  return parts.length > 0 ? parts.join(" · ") : "up to date";
}

// What `package_details` returns per "kind::id" (Rust `PackageInfo`).
export type PackageInfo = {
  desc: string;
  homepage: string;
  license: string;
  dependency: boolean;
  auto_updates: boolean;
  release_notes: string;
  app: string;
  // The installed app's own version, set only when it disagrees with Homebrew's record
  // (e.g. after a rollback). Shown as the real "from" version.
  on_disk_version: string;
};

export type Bump = "major" | "minor" | "patch";

// How big a version jump is, from the first numeric component that grows. Null when it can't be
// told honestly: non-numeric versions, or calendar versions (2026… → 2026…), where a bigger first
// number is just a later date, not a breaking change. Cask versions carry a ",build" suffix that
// isn't part of the version proper.
export function versionBump(current: string, latest: string): Bump | null {
  const parts = (v: string) =>
    v
      .split(",")[0]
      .replace(/^v/i, "")
      .split(/[._-]/)
      .map((s) => (/^\d+$/.test(s) ? Number(s) : NaN));
  const a = parts(current);
  const b = parts(latest);
  if (!current || Number.isNaN(a[0]) || Number.isNaN(b[0])) return null;
  if (a[0] >= 1900 || b[0] >= 1900) return null;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const x = a[i] ?? 0;
    const y = b[i] ?? 0;
    if (Number.isNaN(x) || Number.isNaN(y)) return null;
    if (y > x) return i === 0 ? "major" : i === 1 ? "minor" : "patch";
    if (y < x) return null;
  }
  return null;
}

// Formulae the user asked for vs. ones only installed to satisfy them. Until details load,
// everything counts as primary so nothing is hidden.
export function splitDependencies(
  kind: string,
  items: OutdatedItem[],
  details: Partial<Record<string, PackageInfo>>
): { primary: OutdatedItem[]; deps: OutdatedItem[] } {
  const isDep = (i: OutdatedItem) => kind === "brew" && (details[selKey(kind, i.id)]?.dependency ?? false);
  return { primary: items.filter((i) => !isDep(i)), deps: items.filter(isDep) };
}

// The .app bundle whose icon represents a row: a cask's declared app, or for App Store and
// Sparkle apps the app's own name. Formulae and npm packages have none.
export function iconAppName(
  kind: string,
  item: OutdatedItem,
  details: Partial<Record<string, PackageInfo>>
): string | null {
  if (kind === "cask") {
    const app = details[selKey(kind, item.id)]?.app ?? "";
    return app === "" ? null : app;
  }
  if (kind === "mas" || kind === "sparkle") return `${item.name}.app`;
  return null;
}

const KIND_BLURB: Record<string, string> = {
  brew: "command-line tools & libraries",
  cask: "apps installed with Homebrew",
  mas: "Mac App Store",
  npm: "global packages",
  sparkle: "update themselves; open them to update",
};

// The line under a category name: what it is, then what's notable about its updates.
export function categorySummary(
  kind: string,
  items: OutdatedItem[],
  details: Partial<Record<string, PackageInfo>>
): string {
  const parts: string[] = [];
  const { primary, deps } = splitDependencies(kind, items, details);
  if (deps.length > 0) parts.push(`${primary.length} yours · ${deps.length} ${deps.length === 1 ? "dependency" : "dependencies"}`);
  else parts.push(KIND_BLURB[kind] ?? "");
  const majors = items.filter((i) => versionBump(i.current, i.latest) === "major").length;
  if (majors > 0) parts.push(`${majors} major`);
  return parts.filter(Boolean).join(" · ");
}

// When the daily job fires next, relative to `now`: "today at 3:30 AM" / "tomorrow at 3:30 AM".
export function nextRunText(hour: number, minute: number, now: Date): string {
  const next = new Date(now);
  next.setHours(hour, minute, 0, 0);
  const today = next > now;
  if (!today) next.setDate(next.getDate() + 1);
  const time = next.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return `${today ? "today" : "tomorrow"} at ${time}`;
}

const KIND_LABEL: Record<string, string> = {
  brew: "Homebrew",
  cask: "App",
  mas: "App Store",
  npm: "npm",
  sparkle: "App",
};

export type SkippedVersion = { key: string; kind: string; label: string; id: string; version: string };

// Split an ignoreKey back into its parts for display. Malformed keys are dropped.
export function parseSkipped(keys: Iterable<string>): SkippedVersion[] {
  return [...keys]
    .map((key) => {
      const [kind, id, version] = key.split("::");
      return kind && id && version ? { key, kind, label: KIND_LABEL[kind] ?? kind, id, version } : null;
    })
    .filter((s): s is SkippedVersion => s !== null)
    .sort((a, b) => a.id.localeCompare(b.id));
}

// Mirrors the Rust `UpdateResult` / `ResultAction`.
export type ResultAction =
  | { type: "trust_signer"; bundle_id: string; team: string }
  | { type: "rollback"; token: string }
  | { type: "quit_and_update"; kind: string; id: string; name: string; app: string }
  | { type: "allow_npm_scripts"; name: string; version: string; scripts: string };

export type UpdateResult = {
  kind: string;
  name: string;
  success: boolean;
  output: string;
  action?: ResultAction;
  elapsed_ms?: number;
  // Deliberately not attempted (app was open, or the run was stopped).
  skipped?: boolean;
  steps?: Step[];
};

// Rust `Step`: one `==> …` stage of brew's output and how long it took. `shared` = the batch's
// up-front downloads, counted toward every item in the batch.
export type Step = { label: string; ms: number; shared?: boolean };

// The steps worth a line in the breakdown (≥ 1s, or ≥ 5% of the total), in order; the rest are
// summed into `otherMs` so the bars still add up.
export function stepBreakdown(steps: Step[]): { shown: Step[]; otherMs: number } {
  const total = steps.reduce((t, s) => t + s.ms, 0);
  const shown = steps.filter((s) => s.ms >= 1000 || s.ms >= total * 0.05);
  return { shown, otherMs: total - shown.reduce((t, s) => t + s.ms, 0) };
}

export function slowestStep(steps: Step[]): Step | null {
  return steps.reduce<Step | null>((best, s) => (best === null || s.ms > best.ms ? s : best), null);
}

// Packages brew pulled in for this one ("Installing awscli dependency: python@3.15 (3.15.1)").
export function dependencyNames(steps: Step[]): string[] {
  return steps.flatMap((s) => {
    const m = /^(?:Upgrading|Installing) \S+ dependency: (\S+)/.exec(s.label);
    return m ? [m[1]] : [];
  });
}

// The one-line "why so slow" under a result: only for items that took long enough to wonder.
export function slowSummary(r: { elapsed_ms?: number; steps?: Step[] }): string | null {
  const steps = r.steps ?? [];
  if ((r.elapsed_ms ?? 0) < 10_000 || steps.length === 0) return null;
  const deps = dependencyNames(steps).length;
  const slow = slowestStep(steps);
  const parts = [];
  if (deps > 0) parts.push(`+${deps} ${deps === 1 ? "dependency" : "dependencies"}`);
  if (slow) parts.push(`slowest: ${slow.label} ${formatDuration(slow.ms)}`);
  return parts.join(" · ");
}

// Rust `VulnReport`: OSV ids affecting the installed version, and those the update fixes.
export type VulnReport = { current: string[]; fixed: string[] };

// Rust `rollback::Snapshot`.
export type Snapshot = {
  token: string;
  version: string;
  created_at: number;
  apps: string[];
  // This snapshot's version is the one installed (e.g. it was just restored).
  in_use: boolean;
};

// "0.8s", "12s", "2m 05s".
export function formatDuration(ms: number): string {
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

// Background refresh: when the list is older than `maxAgeMs` (or was never loaded).
export function isStale(lastChecked: Date | null, now: Date, maxAgeMs: number): boolean {
  return lastChecked === null || now.getTime() - lastChecked.getTime() >= maxAgeMs;
}

// Rust `UpdateProgress` ("update-progress" events).
export type UpdateProgress = {
  phase: string;
  current: string | null;
  done: number;
  total: number;
  // The `==> …` step brew is on, when the backend knows it.
  step?: string | null;
};

// --- Live run view: the steps of the running update, as they happen ---

// One step of a running update. `ms` is set once the next step starts.
export type LiveStep = { key: string; label: string; current: string | null; startedAt: number; ms?: number };

// brew's own step when there is one ("Pouring python@3.15"); otherwise the phase ("Checking
// publisher zoom", "Installing Excel").
export function liveLabel(p: UpdateProgress): string {
  return p.step ?? progressText(p);
}

const LIVE_KEEP = 60;

const lastOf = <T,>(xs: T[]): T | undefined => (xs.length > 0 ? xs[xs.length - 1] : undefined);

// Fold one progress event into the step list: a new step closes the previous one. Repeats of the
// current step (the count moved, the step didn't) change nothing. Pure.
export function advanceSteps(steps: LiveStep[], p: UpdateProgress, now: number): LiveStep[] {
  const label = liveLabel(p);
  const key = `${p.current ?? ""}|${label}`;
  const last = lastOf(steps);
  if (last?.key === key && last.ms === undefined) return steps;
  const closed = last && last.ms === undefined ? [...steps.slice(0, -1), { ...last, ms: now - last.startedAt }] : steps;
  return [...closed, { key, label, current: p.current, startedAt: now }].slice(-LIVE_KEEP);
}

// What the live view shows: the running step, and the most recent finished ones (newest last).
export function liveView(steps: LiveStep[], keep: number): { done: LiveStep[]; running: LiveStep | null } {
  const last = lastOf(steps);
  const running = last && last.ms === undefined ? last : null;
  const finished = running ? steps.slice(0, -1) : steps;
  return { done: finished.slice(-keep), running };
}

// "0:07", "1:42": a ticking clock reads better than "7.0s" changing every second.
export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

// --- Menu-bar icon ---

// Mirrors the Rust `TrayState`.
export type TrayState = { count: number; attention: boolean; running: boolean; done: number; total: number };

// Something the user should look at, beyond "updates are available": a known vulnerability in an
// installed npm package, a publisher change waiting in the results, or a scheduled run that
// failed, was blocked or found a publisher change. Pure.
export function needsAttention(o: {
  vulns: Record<string, VulnReport>;
  checkVulns: boolean;
  lastRun: ScheduledRunSummary | null;
  results: UpdateResult[] | null;
}): boolean {
  const vulnerable = o.checkVulns && Object.values(o.vulns).some((v) => v.current.length > 0);
  const signer = (o.results ?? []).some((r) => r.action?.type === "trust_signer");
  const run = o.lastRun;
  const runProblem =
    run !== null && (run.failed.length > 0 || run.signer_changed.length > 0 || (run.blocked ?? []).length > 0);
  return vulnerable || signer || runProblem;
}

// --- Keyboard ---

export type KeyAction =
  | "down"
  | "up"
  | "toggle"
  | "expand"
  | "collapse"
  | "update"
  | "refresh"
  | "settings"
  | "filter"
  | "escape"
  | "help";

// The shortcuts, for the ? overlay. Kept next to keyAction so they can't drift apart.
export const SHORTCUTS: { keys: string; what: string }[] = [
  { keys: "↑ ↓  or  k j", what: "Move between rows" },
  { keys: "Space", what: "Select / deselect" },
  { keys: "→ ←", what: "Open / close a section or details" },
  { keys: "⌘↩", what: "Update selected (or all)" },
  { keys: "⌘F  or  /", what: "Filter" },
  { keys: "⌘R", what: "Check again" },
  { keys: "⌘,", what: "Settings" },
  { keys: "Esc", what: "Close / clear filter" },
  { keys: "?", what: "These shortcuts" },
];

// Which action a key press is. In a text field only Esc and the ⌘ shortcuts apply, so typing
// "j" in the filter types a j. Pure.
export function keyAction(e: { key: string; metaKey: boolean; inField: boolean }): KeyAction | null {
  if (e.key === "Escape") return "escape";
  if (e.metaKey) {
    const meta: Record<string, KeyAction> = { Enter: "update", f: "filter", r: "refresh", ",": "settings" };
    return meta[e.key] ?? null;
  }
  if (e.inField) return null;
  const plain: Record<string, KeyAction> = {
    ArrowDown: "down",
    j: "down",
    ArrowUp: "up",
    k: "up",
    " ": "toggle",
    ArrowRight: "expand",
    ArrowLeft: "collapse",
    "/": "filter",
    "?": "help",
  };
  return plain[e.key] ?? null;
}

// What's on screen when a key arrives, for routeKey.
export type KeyContext = {
  help: boolean;
  results: boolean;
  history: boolean;
  adopt: boolean;
  settings: boolean;
  filterOpen: boolean;
  loading: boolean;
  verifying: boolean;
  updating: boolean;
  notifyOnly: boolean;
  scheduledRunning: boolean;
  selectedCount: number;
  updatableCount: number;
};

export type KeyCommand =
  | { do: "close"; what: "help" | "results" | "history" | "adopt" | "settings" | "filter" }
  | { do: "settings" }
  | { do: "refresh" }
  | { do: "toggleHelp" }
  | { do: "filter" }
  | { do: "update"; which: "selected" | "all" }
  | { do: "move"; delta: 1 | -1 }
  | { do: "expand" }
  | { do: "collapse" }
  // Space: select the row under the keyboard cursor, or open / close its section.
  | { do: "toggle" }
  | { do: "nothing" };

// What a key action does in the current state, and whether to stop the browser's default (⌘R
// reloading the web view, arrows scrolling). Panels on top (settings, results, history, adopt,
// the shortcut list) take only Esc, ⌘, and ⌘R, so keys never act on a list you can't see. Pure.
export function routeKey(action: KeyAction, c: KeyContext): { cmd: KeyCommand; prevent: boolean } {
  const run = (cmd: KeyCommand) => ({ cmd, prevent: true });
  if (action === "escape") {
    // The topmost thing closes first.
    const order = [
      ["help", c.help],
      ["results", c.results],
      ["history", c.history],
      ["adopt", c.adopt],
      ["settings", c.settings],
      ["filter", c.filterOpen],
    ] as const;
    const top = order.find(([, open]) => open);
    return run(top ? { do: "close", what: top[0] } : { do: "nothing" });
  }
  if (action === "settings") return run(c.loading ? { do: "nothing" } : { do: "settings" });
  if (action === "refresh") {
    return run(c.loading || c.verifying || c.updating ? { do: "nothing" } : { do: "refresh" });
  }
  if (action === "help" && c.help) return run({ do: "toggleHelp" });
  if (c.help || c.results || c.history || c.adopt || c.settings) return { cmd: { do: "nothing" }, prevent: false };
  switch (action) {
    case "help":
      return run({ do: "toggleHelp" });
    case "filter":
      return run({ do: "filter" });
    case "update":
      if (c.notifyOnly || c.updating || c.loading || c.scheduledRunning) return run({ do: "nothing" });
      if (c.selectedCount > 0) return run({ do: "update", which: "selected" });
      return run(c.updatableCount > 0 ? { do: "update", which: "all" } : { do: "nothing" });
    case "down":
      return run({ do: "move", delta: 1 });
    case "up":
      return run({ do: "move", delta: -1 });
    case "expand":
      return run({ do: "expand" });
    case "collapse":
      return run({ do: "collapse" });
    case "toggle":
      // Not prevented here: with no keyboard cursor, Space belongs to whatever has focus.
      return { cmd: { do: "toggle" }, prevent: false };
  }
}

// --- The keyboard cursor ---
// The row the keyboard is on is the app's own state, drawn with a class, not the browser's focus:
// in the app's WebKit view, highlighting by :focus-visible / :has() didn't reliably show on every
// row. Each navigable element carries data-nav-id: "cat:<section>" or "row:<kind>::<id>".

export const catNavId = (category: string) => `cat:${category}`;
export const rowNavId = (kind: string, id: string) => `row:${selKey(kind, id)}`;

export type NavTarget = { type: "cat"; cat: string } | { type: "row"; key: string };

export function parseNavId(id: string): NavTarget | null {
  if (id.startsWith("cat:")) return { type: "cat", cat: id.slice(4) };
  if (id.startsWith("row:")) return { type: "row", key: id.slice(4) };
  return null;
}

// The next cursor position among the rows on screen (in order), stopping at the ends. From
// nowhere — or from a row that's no longer shown — ↓ starts at the top and ↑ at the bottom. Pure.
export function stepCursor(ids: string[], current: string | null, delta: 1 | -1): string | null {
  const next = moveFocus(current === null ? -1 : ids.indexOf(current), delta, ids.length);
  return next < 0 ? null : ids[next];
}

// Move through `length` rows, stopping at the ends; from nothing, ↓ starts at the top and ↑ at
// the bottom. Pure.
export function moveFocus(current: number, delta: 1 | -1, length: number): number {
  if (length === 0) return -1;
  if (current < 0) return delta === 1 ? 0 : length - 1;
  return Math.min(length - 1, Math.max(0, current + delta));
}

// Items whose name or id contains every word of the query (any order, any case). Pure.
export function filterItems<T extends { id: string; name: string }>(items: T[], query: string): T[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return items;
  return items.filter((i) => {
    const hay = `${i.name} ${i.id}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

// "no matches", "1 match", "4 matches". Pure.
export function matchText(n: number): string {
  return n === 0 ? "no matches" : `${n} ${n === 1 ? "match" : "matches"}`;
}

// --- Colour themes (each one is a [data-theme] block in App.css) ---

export type Theme = { id: string; name: string; note: string; swatch: [string, string, string] };

export const THEMES: Theme[] = [
  { id: "noir", name: "Gothic Noir", note: "Black, silver, mauve", swatch: ["#040303", "#5e5050", "#b6a0a0"] },
  { id: "cobalt", name: "Cobalt Sky", note: "Navy, cobalt, sky blue", swatch: ["#090c1e", "#0047ab", "#82c8e5"] },
  { id: "obsidian", name: "Obsidian", note: "Near-black, electric blue", swatch: ["#0f1116", "#1d2a44", "#5b9eff"] },
  { id: "sage", name: "Sage & Stone", note: "Warm charcoal, sage", swatch: ["#1a1a18", "#3a4033", "#9caf88"] },
  { id: "pine", name: "Pine & Mist", note: "Cool charcoal, teal", swatch: ["#151818", "#24403a", "#6cae9d"] },
  { id: "ink", name: "Ink & Gold", note: "Blue-black, brass", swatch: ["#0d0f14", "#3a3322", "#c2a86a"] },
  { id: "graphite", name: "Graphite", note: "Monochrome, one green", swatch: ["#111111", "#2c2c2c", "#7fd49a"] },
];

export const DEFAULT_THEME = "noir";

// A stored theme id, or the default when it's missing or no longer exists. Pure.
export function resolveTheme(stored: string | null): string {
  return THEMES.find((t) => t.id === stored)?.id ?? DEFAULT_THEME;
}

// "Upgrading ffmpeg", "Refreshing Homebrew", "Starting…".
export function progressText(p: UpdateProgress | null): string {
  if (!p) return "Starting…";
  return p.current ? `${p.phase} ${p.current}` : p.phase;
}

// Rust `HistoryEntry` (~/.freshet/history.jsonl).
export type HistoryEntry = {
  at: number;
  source: "manual" | "scheduled";
  kind: string;
  id: string;
  name: string;
  from: string;
  to: string;
  outcome: "updated" | "failed" | "needs_admin" | "blocked" | "skipped_open";
  elapsed_ms?: number | null;
};

export type HistoryDay = { label: string; entries: HistoryEntry[] };

// Group newest-first entries by local calendar day: "Today", "Yesterday", then "Mon, Oct 6".
export function groupHistoryByDay(entries: HistoryEntry[], now: Date): HistoryDay[] {
  const dayKey = (d: Date) => `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
  const today = dayKey(now);
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  const yesterday = dayKey(y);
  const days: HistoryDay[] = [];
  for (const e of entries) {
    const d = new Date(e.at * 1000);
    const key = dayKey(d);
    const label =
      key === today
        ? "Today"
        : key === yesterday
          ? "Yesterday"
          : d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" });
    const last = days[days.length - 1] as HistoryDay | undefined;
    if (last?.label === label) last.entries.push(e);
    else days.push({ label, entries: [e] });
  }
  return days;
}

export const OUTCOME_LABEL: Record<HistoryEntry["outcome"], string> = {
  updated: "updated",
  failed: "failed",
  needs_admin: "needed password",
  blocked: "blocked: publisher changed",
  skipped_open: "skipped: app was open",
};

// Rust `AppUpdate`: a newer Freshet is available.
export type AppUpdate = { version: string; notes: string | null };

// A wall-clock time in the user's own format: "2:45 PM" (12-hour locales) or "14:45" (24-hour).
// Everything that shows a time uses this, so the schedule reads like the "Checked" time beside it.
export function formatClock(hour: number, minute: number): string {
  return new Date(2000, 0, 1, hour, minute).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

// The hour alone in the same format, for the picker: "2 PM" or "14".
export function hourLabel(hour: number): string {
  return new Date(2000, 0, 1, hour).toLocaleTimeString([], { hour: "numeric" });
}

// Rust `AdoptProgress` ("adopt-progress" events).
export type AdoptProgress = { token: string; phase: string; downloaded_mb: number; stalled: boolean };

// The status line for an adoption in progress.
export function adoptStatusText(p: AdoptProgress | null): string {
  if (!p || p.phase === "Starting") return "Starting…";
  if (p.phase === "Adopting") return "Adopting…";
  if (p.stalled) return `Download stalled at ${p.downloaded_mb} MB. The server stopped sending; cancel and try later.`;
  return p.downloaded_mb > 0 ? `Downloading · ${p.downloaded_mb} MB` : "Downloading…";
}
