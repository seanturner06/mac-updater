import { describe, it, expect } from "vitest";
import {
  selKey,
  ignoreKey,
  visibleItems,
  displayItems,
  isReadOnly,
  countDisplayed,
  toggleSelected,
  toggleCategorySelected,
  prunedSelfUpdating,
  allDisplayedSelections,
  toSettings,
  scheduledSelections,
  lastRunText,
  versionBump,
  splitDependencies,
  iconAppName,
  categorySummary,
  nextRunText,
  parseSkipped,
  formatClock,
  hourLabel,
  formatDuration,
  isStale,
  groupHistoryByDay,
  type HistoryEntry,
  progressText,
  type CategoryStatus,
  type PackageInfo,
  type OutdatedItem,
  type ScheduledRunSummary,
  type Selection,
} from "./logic";

const item = (id: string, self_updating = false, latest = "2.0"): OutdatedItem => ({
  id,
  name: id,
  current: "1.0",
  latest,
  self_updating,
  notes_url: "",
});
const cat = (kind: string, items: OutdatedItem[]): CategoryStatus => ({
  category: kind,
  kind,
  items,
  error: null,
});

describe("selKey", () => {
  it("joins kind and id", () => {
    expect(selKey("cask", "firefox")).toBe("cask::firefox");
  });
});

describe("visibleItems", () => {
  it("hides self-updating casks when not greedy", () => {
    const c = cat("cask", [item("a"), item("b", true)]);
    expect(visibleItems(c, false).map((i) => i.id)).toEqual(["a"]);
  });
  it("shows everything when greedy", () => {
    const c = cat("cask", [item("a"), item("b", true)]);
    expect(visibleItems(c, true)).toHaveLength(2);
  });
});

describe("isReadOnly", () => {
  it("sparkle is always read-only", () => {
    expect(isReadOnly("sparkle", false)).toBe(true);
  });
  it("notify-only makes every category read-only", () => {
    expect(isReadOnly("cask", true)).toBe(true);
  });
  it("normal categories are editable", () => {
    expect(isReadOnly("cask", false)).toBe(false);
  });
});

describe("ignore / skip-this-version", () => {
  it("ignoreKey includes the version so a new version reappears", () => {
    expect(ignoreKey("cask", "firefox", "2.0")).toBe("cask::firefox::2.0");
    expect(ignoreKey("cask", "firefox", "2.0")).not.toBe(ignoreKey("cask", "firefox", "3.0"));
  });
  it("displayItems hides a skipped version but not other versions/items", () => {
    const c = cat("cask", [item("a", false, "2.0"), item("b", false, "2.0")]);
    const ignored = new Set([ignoreKey("cask", "a", "2.0")]);
    expect(displayItems(c, false, ignored).map((i) => i.id)).toEqual(["b"]);
    // a newer version of "a" is no longer matched by the old skip key
    const c2 = cat("cask", [item("a", false, "3.0")]);
    expect(displayItems(c2, false, ignored).map((i) => i.id)).toEqual(["a"]);
  });
});

describe("countDisplayed", () => {
  it("sums shown items across categories, respecting greedy + ignored", () => {
    const cats = [cat("brew", [item("a")]), cat("cask", [item("b"), item("c", true)])];
    const none = new Set<string>();
    expect(countDisplayed(cats, false, none)).toBe(2); // c (self-updating) hidden
    expect(countDisplayed(cats, true, none)).toBe(3);
    const skipB = new Set([ignoreKey("cask", "b", "2.0")]);
    expect(countDisplayed(cats, false, skipB)).toBe(1);
  });
});

describe("toggleSelected", () => {
  const sel: Record<string, Selection> = {};
  it("adds when absent, removes when present", () => {
    const added = toggleSelected(sel, "cask", "firefox", "Firefox");
    expect(added["cask::firefox"]).toEqual({ kind: "cask", id: "firefox", name: "Firefox" });
    const removed = toggleSelected(added, "cask", "firefox", "Firefox");
    expect("cask::firefox" in removed).toBe(false);
  });
  it("does not mutate the input", () => {
    const before = { ...sel };
    toggleSelected(sel, "npm", "x", "x");
    expect(sel).toEqual(before);
  });
});

describe("toggleCategorySelected", () => {
  const items = [item("a"), item("b")];
  it("selects all when none selected", () => {
    const r = toggleCategorySelected({}, "cask", items);
    expect(Object.keys(r).sort()).toEqual(["cask::a", "cask::b"]);
  });
  it("clears all when all already selected", () => {
    const all = toggleCategorySelected({}, "cask", items);
    expect(toggleCategorySelected(all, "cask", items)).toEqual({});
  });
  it("fills in the rest when only some selected", () => {
    const some = toggleSelected({}, "cask", "a", "a");
    const r = toggleCategorySelected(some, "cask", items);
    expect(Object.keys(r).sort()).toEqual(["cask::a", "cask::b"]);
  });
  it("treats an empty category as a no-op", () => {
    expect(toggleCategorySelected({}, "cask", [])).toEqual({});
  });
});

describe("prunedSelfUpdating", () => {
  it("drops self-updating items, keeps the rest", () => {
    const cats = [cat("cask", [item("a"), item("b", true)])];
    const selected = toggleCategorySelected({}, "cask", [item("a"), item("b", true)]);
    const pruned = prunedSelfUpdating(selected, cats);
    expect(Object.keys(pruned)).toEqual(["cask::a"]); // b (self-updating) removed
  });
});

describe("allDisplayedSelections", () => {
  it("collects every visible item across categories as selections", () => {
    const cats = [cat("brew", [item("wget")]), cat("cask", [item("firefox")])];
    expect(allDisplayedSelections(cats, false, new Set())).toEqual([
      { kind: "brew", id: "wget", name: "wget" },
      { kind: "cask", id: "firefox", name: "firefox" },
    ]);
  });
  it("respects skipped versions — Update all must not upgrade what the user hid", () => {
    const cats = [cat("cask", [item("a", false, "2.0"), item("b", false, "2.0")])];
    const ignored = new Set([ignoreKey("cask", "a", "2.0")]);
    expect(allDisplayedSelections(cats, false, ignored).map((s) => s.id)).toEqual(["b"]);
  });
  it("excludes self-updating casks unless greedy, and sparkle always", () => {
    const cats = [cat("cask", [item("a"), item("b", true)]), cat("sparkle", [item("s")])];
    expect(allDisplayedSelections(cats, false, new Set()).map((s) => s.id)).toEqual(["a"]);
    expect(allDisplayedSelections(cats, true, new Set()).map((s) => s.id)).toEqual(["a", "b"]);
  });
  it("is empty when nothing is displayed", () => {
    expect(allDisplayedSelections([], false, new Set())).toEqual([]);
    expect(allDisplayedSelections([cat("sparkle", [item("s")])], false, new Set())).toEqual([]);
  });
});

describe("edge cases", () => {
  it("visibleItems on an empty category is empty", () => {
    expect(visibleItems(cat("cask", []), false)).toEqual([]);
  });
  it("displayItems removes multiple ignored versions", () => {
    const c = cat("cask", [item("a"), item("b"), item("c")]);
    const ignored = new Set([ignoreKey("cask", "a", "2.0"), ignoreKey("cask", "c", "2.0")]);
    expect(displayItems(c, false, ignored).map((i) => i.id)).toEqual(["b"]);
  });
  it("countDisplayed of no categories is 0", () => {
    expect(countDisplayed([], false, new Set())).toBe(0);
  });
  it("isReadOnly is false for every normal kind, true for sparkle/notify", () => {
    for (const k of ["brew", "cask", "mas", "npm"]) expect(isReadOnly(k, false)).toBe(false);
    expect(isReadOnly("sparkle", false)).toBe(true);
    expect(isReadOnly("brew", true)).toBe(true);
  });
});

describe("scheduled run", () => {
  it("toSettings matches the Rust Settings shape", () => {
    expect(toSettings(true, false, new Set(["b::x::2", "a::y::1"]), true)).toEqual({
      greedy: true,
      notify_only: false,
      skipped: ["a::y::1", "b::x::2"],
      check_vulnerabilities: true,
    });
  });

  it("scheduledSelections is the displayed brew + cask items only", () => {
    const cats = [
      cat("brew", [item("git"), item("node", false, "24")]),
      cat("cask", [item("iterm2"), item("chrome", true)]),
      cat("npm", [item("typescript")]),
    ];
    const ignored = new Set([ignoreKey("brew", "node", "24")]);
    expect(scheduledSelections(cats, false, ignored).map((s) => `${s.kind}:${s.id}`)).toEqual([
      "brew:git",
      "cask:iterm2",
    ]);
    expect(scheduledSelections(cats, true, ignored)).toHaveLength(3);
  });

  const summary = (over: Partial<ScheduledRunSummary>): ScheduledRunSummary => ({
    finished_at: 0,
    notify_only: false,
    available: 0,
    updated: [],
    needs_admin: [],
    failed: [],
    signer_changed: [],
    ...over,
  });

  it("lastRunText", () => {
    expect(lastRunText(summary({}))).toBe("up to date");
    expect(lastRunText(summary({ notify_only: true, available: 2 }))).toBe(
      "2 updates found, none installed (notify-only)"
    );
    expect(
      lastRunText(summary({ updated: ["a", "b"], needs_admin: ["zoom"], failed: ["x"] }))
    ).toBe("2 updated · 1 needs your password: zoom · 1 failed");
  });
});

describe("package presentation", () => {
  it("versionBump classifies semver-ish jumps", () => {
    expect(versionBump("0.10.4", "1.0.0")).toBe("major");
    expect(versionBump("3.14.1", "3.15.1")).toBe("minor");
    expect(versionBump("2.49.0", "2.49.1")).toBe("patch");
    expect(versionBump("1.2", "1.2.1")).toBe("patch");
    expect(versionBump("3.14.1", "3.14.1_1")).toBe("patch");
    expect(versionBump("v1.9", "v2.0")).toBe("major");
    // Cask ",build" suffixes are ignored.
    expect(versionBump("4.45.1,abc", "4.46.101,def")).toBe("minor");
  });

  it("versionBump refuses to guess", () => {
    expect(versionBump("20260526.0", "20260817.0")).toBeNull(); // calendar version
    expect(versionBump("", "1.0")).toBeNull();
    expect(versionBump("latest", "latest")).toBeNull();
    expect(versionBump("1.0", "1.0")).toBeNull();
    expect(versionBump("2.0", "1.9")).toBeNull();
  });

  const info = (over: Partial<PackageInfo> = {}): PackageInfo => ({
    desc: "",
    homepage: "",
    license: "",
    dependency: false,
    auto_updates: false,
    release_notes: "",
    app: "",
    ...over,
  });

  it("splitDependencies only splits formulae, and only once details say so", () => {
    const items = [item("git"), item("abseil")];
    const details = { "brew::abseil": info({ dependency: true }) };
    const split = splitDependencies("brew", items, details);
    expect(split.primary.map((i) => i.id)).toEqual(["git"]);
    expect(split.deps.map((i) => i.id)).toEqual(["abseil"]);
    expect(splitDependencies("brew", items, {}).deps).toEqual([]);
    expect(splitDependencies("cask", items, { "cask::abseil": info({ dependency: true }) }).deps).toEqual([]);
  });

  it("iconAppName", () => {
    const details = { "cask::slack": info({ app: "Slack.app" }) };
    expect(iconAppName("cask", item("slack"), details)).toBe("Slack.app");
    expect(iconAppName("cask", item("zoom"), details)).toBeNull();
    expect(iconAppName("mas", { ...item("1"), name: "Xcode" }, {})).toBe("Xcode.app");
    expect(iconAppName("brew", item("git"), {})).toBeNull();
  });

  it("categorySummary", () => {
    const items = [item("git", false, "3.0"), { ...item("abseil"), current: "1.0", latest: "1.1" }];
    expect(categorySummary("brew", items, { "brew::abseil": info({ dependency: true }) })).toBe(
      "1 yours · 1 dependency · 1 major"
    );
    expect(categorySummary("npm", [], {})).toBe("global packages");
  });
});

describe("settings helpers", () => {
  const at = (h: number, m: number) => new Date(2026, 9, 3, h, m);
  const time = (h: number, m: number) => at(h, m).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

  // Reported: scheduling 2:45 PM at ~2:30 PM said "tomorrow". Same hour, later minute.
  it("nextRunText: later today in the same hour is today", () => {
    expect(nextRunText(14, 45, at(14, 30))).toBe(`today at ${time(14, 45)}`);
    expect(nextRunText(14, 45, at(14, 44))).toBe(`today at ${time(14, 45)}`);
    expect(nextRunText(14, 45, at(9, 0))).toBe(`today at ${time(14, 45)}`);
    expect(nextRunText(14, 45, at(14, 46))).toBe(`tomorrow at ${time(14, 45)}`);
  });

  it("nextRunText is today when the time is still ahead, else tomorrow", () => {
    expect(nextRunText(3, 30, at(1, 0))).toBe(`today at ${time(3, 30)}`);
    expect(nextRunText(3, 30, at(20, 0))).toBe(`tomorrow at ${time(3, 30)}`);
    expect(nextRunText(3, 30, at(3, 30))).toBe(`tomorrow at ${time(3, 30)}`);
  });

  it("parseSkipped splits keys, labels kinds, drops junk, sorts by name", () => {
    expect(parseSkipped(["cask::zoom::6.1", "brew::node::24", "garbage"])).toEqual([
      { key: "brew::node::24", kind: "brew", label: "Homebrew", id: "node", version: "24" },
      { key: "cask::zoom::6.1", kind: "cask", label: "App", id: "zoom", version: "6.1" },
    ]);
  });
});

describe("timing helpers", () => {
  it("formatDuration", () => {
    expect(formatDuration(800)).toBe("0.8s");
    expect(formatDuration(9_949)).toBe("9.9s");
    expect(formatDuration(12_400)).toBe("12s");
    expect(formatDuration(125_000)).toBe("2m 05s");
  });

  it("isStale", () => {
    const t = new Date(2026, 9, 3, 12, 0);
    const later = (min: number) => new Date(t.getTime() + min * 60_000);
    expect(isStale(null, t, 1000)).toBe(true);
    expect(isStale(t, later(29), 30 * 60_000)).toBe(false);
    expect(isStale(t, later(30), 30 * 60_000)).toBe(true);
  });
});

describe("update progress", () => {
  it("progressText", () => {
    expect(progressText(null)).toBe("Starting…");
    expect(progressText({ phase: "Refreshing Homebrew", current: null, done: 0, total: 65 })).toBe("Refreshing Homebrew");
    expect(progressText({ phase: "Upgrading", current: "ffmpeg", done: 38, total: 65 })).toBe("Upgrading ffmpeg");
  });
});

describe("history", () => {
  const entry = (at: Date, id: string): HistoryEntry => ({
    at: at.getTime() / 1000,
    source: "manual",
    kind: "brew",
    id,
    name: id,
    from: "1",
    to: "2",
    outcome: "updated",
  });

  it("groups newest-first entries into Today / Yesterday / dated days", () => {
    const now = new Date(2026, 9, 10, 15, 0);
    const days = groupHistoryByDay(
      [
        entry(new Date(2026, 9, 10, 9, 0), "a"),
        entry(new Date(2026, 9, 10, 8, 0), "b"),
        entry(new Date(2026, 9, 9, 22, 0), "c"),
        entry(new Date(2026, 9, 3, 21, 0), "d"),
      ],
      now
    );
    expect(days.map((d) => [d.label, d.entries.map((e) => e.id).join("")])).toEqual([
      ["Today", "ab"],
      ["Yesterday", "c"],
      [new Date(2026, 9, 3).toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" }), "d"],
    ]);
  });
});

describe("clock formatting", () => {
  // Locale-independent: must match exactly what the "Checked …" time uses.
  const checkedStyle = (h: number, m: number) =>
    new Date(2026, 9, 10, h, m).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

  it("formatClock matches the Checked-time format", () => {
    expect(formatClock(14, 45)).toBe(checkedStyle(14, 45));
    expect(formatClock(0, 5)).toBe(checkedStyle(0, 5));
  });

  it("hourLabel shows just the hour in the same style", () => {
    expect(formatClock(14, 0).startsWith(hourLabel(14).replace(/\s?[AP]M$/i, ""))).toBe(true);
    expect(hourLabel(9)).not.toContain(":");
  });
});
