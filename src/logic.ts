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
  | { type: "quit_and_update"; kind: string; id: string; name: string; app: string };

export type UpdateResult = {
  kind: string;
  name: string;
  success: boolean;
  output: string;
  action?: ResultAction;
  elapsed_ms?: number;
  // Deliberately not attempted (app was open, or the run was stopped).
  skipped?: boolean;
};

// Rust `VulnReport`: OSV ids affecting the installed version, and those the update fixes.
export type VulnReport = { current: string[]; fixed: string[] };

// Rust `rollback::Snapshot`.
export type Snapshot = { token: string; version: string; created_at: number; apps: string[] };

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
export type UpdateProgress = { phase: string; current: string | null; done: number; total: number };

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
