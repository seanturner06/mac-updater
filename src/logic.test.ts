import { describe, it, expect } from "vitest";
import css from "./App.css?raw";
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
  advanceSteps,
  liveView,
  formatElapsed,
  needsAttention,
  keyAction,
  routeKey,
  type KeyContext,
  moveFocus,
  stepCursor,
  parseNavId,
  catNavId,
  rowNavId,
  filterItems,
  matchText,
  resolveTheme,
  THEMES,
  SHORTCUTS,
  stepBreakdown,
  slowestStep,
  dependencyNames,
  slowSummary,
  isStale,
  adoptStatusText,
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
    on_disk_version: "",
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

  // Labels as brew_steps produces them from a real awscli upgrade log.
  const awscliSteps = [
    { label: "Upgrading awscli", ms: 900 },
    { label: "Installing dependencies for awscli: ca-certificates and python@3.15", ms: 100 },
    { label: "Upgrading awscli dependency: ca-certificates (2026-09-25 -> 2026-10-20)", ms: 500 },
    { label: "Pouring ca-certificates", ms: 1500 },
    { label: "Installing awscli dependency: python@3.15 (3.15.1)", ms: 1000 },
    { label: "Pouring python@3.15", ms: 41_000 },
    { label: "Installing awscli", ms: 500 },
    { label: "Pouring awscli", ms: 54_500 },
    { label: "Cleanup", ms: 18_000 },
    { label: "Caveats", ms: 100 },
  ];

  it("stepBreakdown keeps the steps that matter and sums the rest", () => {
    const { shown, otherMs } = stepBreakdown(awscliSteps);
    expect(shown.map((s) => s.label)).toEqual([
      "Pouring ca-certificates",
      "Installing awscli dependency: python@3.15 (3.15.1)",
      "Pouring python@3.15",
      "Pouring awscli",
      "Cleanup",
    ]);
    expect(otherMs).toBe(900 + 100 + 500 + 500 + 100);
    // On a short run a sub-second step can still be a big share.
    expect(stepBreakdown([{ label: "a", ms: 300 }, { label: "b", ms: 5 }]).shown.map((s) => s.label)).toEqual(["a"]);
  });

  it("slowestStep / dependencyNames", () => {
    expect(slowestStep(awscliSteps)?.label).toBe("Pouring awscli");
    expect(slowestStep([])).toBeNull();
    expect(dependencyNames(awscliSteps)).toEqual(["ca-certificates", "python@3.15"]);
    expect(dependencyNames([{ label: "Installing dependencies for awscli: a, b", ms: 1 }])).toEqual([]);
  });

  it("slowSummary explains only slow items", () => {
    expect(slowSummary({ elapsed_ms: 119_000, steps: awscliSteps })).toBe(
      "+2 dependencies · slowest: Pouring awscli 55s",
    );
    expect(slowSummary({ elapsed_ms: 4_000, steps: awscliSteps })).toBeNull();
    expect(slowSummary({ elapsed_ms: 30_000 })).toBeNull();
    expect(slowSummary({ elapsed_ms: 12_000, steps: [{ label: "Pouring git", ms: 12_000 }] })).toBe(
      "slowest: Pouring git 12s",
    );
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

describe("adoption status", () => {
  const p = (phase: string, mb = 0, stalled = false) => ({ token: "whatsapp", phase, downloaded_mb: mb, stalled });
  it("adoptStatusText", () => {
    expect(adoptStatusText(null)).toBe("Starting…");
    expect(adoptStatusText(p("Downloading"))).toBe("Downloading…");
    expect(adoptStatusText(p("Downloading", 148))).toBe("Downloading · 148 MB");
    expect(adoptStatusText(p("Downloading", 148, true))).toContain("stalled at 148 MB");
    expect(adoptStatusText(p("Adopting", 160))).toBe("Adopting…");
  });
});

describe("blocked builds in the last run", () => {
  it("lastRunText reports provenance blocks", () => {
    expect(
      lastRunText({ finished_at: 0, notify_only: false, available: 2, updated: ["git"], needs_admin: [], failed: [], signer_changed: [], blocked: ["xz"] })
    ).toBe("blocked (failed build verification): xz · 1 updated");
  });
});

describe("run view, menu bar, keyboard and themes", () => {
  describe("live run view", () => {
    const ev = (step: string | null, current: string | null = "awscli", done = 0) => ({
      phase: "Upgrading",
      current,
      done,
      total: 2,
      step,
    });

    it("a new step closes the previous one with its duration", () => {
      let steps = advanceSteps([], ev("Upgrading awscli"), 1_000);
      steps = advanceSteps(steps, ev("Pouring python@3.15"), 2_500);
      steps = advanceSteps(steps, ev("Pouring awscli"), 43_500);
      expect(steps.map((s) => [s.label, s.ms])).toEqual([
        ["Upgrading awscli", 1_500],
        ["Pouring python@3.15", 41_000],
        ["Pouring awscli", undefined],
      ]);
    });

    it("a repeat of the running step changes nothing", () => {
      const steps = advanceSteps([], ev("Pouring awscli"), 0);
      expect(advanceSteps(steps, ev("Pouring awscli", "awscli", 1), 900)).toBe(steps);
    });

    it("the same label for another package is a new step", () => {
      let steps = advanceSteps([], ev("Cleanup", "git"), 0);
      steps = advanceSteps(steps, ev("Cleanup", "wget"), 300);
      expect(steps).toHaveLength(2);
      expect(steps[0].ms).toBe(300);
    });

    it("phases stand in when brew gives no step (mas, npm, preverify)", () => {
      const steps = advanceSteps([], { phase: "Checking publisher", current: "zoom", done: 0, total: 1 }, 0);
      expect(steps[0].label).toBe("Checking publisher zoom");
    });

    it("keeps a bounded history", () => {
      let steps: ReturnType<typeof advanceSteps> = [];
      for (let i = 0; i < 100; i++) steps = advanceSteps(steps, ev(`step ${i}`), i);
      expect(steps).toHaveLength(60);
      expect(steps[steps.length - 1].label).toBe("step 99");
    });

    it("liveView splits the running step from the recent finished ones", () => {
      let steps = advanceSteps([], ev("a"), 0);
      steps = advanceSteps(steps, ev("b"), 1);
      steps = advanceSteps(steps, ev("c"), 2);
      const view = liveView(steps, 1);
      expect(view.running?.label).toBe("c");
      expect(view.done.map((s) => s.label)).toEqual(["b"]);
      expect(liveView([], 3)).toEqual({ done: [], running: null });
    });

    it("formatElapsed", () => {
      expect(formatElapsed(0)).toBe("0:00");
      expect(formatElapsed(7_900)).toBe("0:07");
      expect(formatElapsed(102_000)).toBe("1:42");
      expect(formatElapsed(-5)).toBe("0:00");
    });
  });

  describe("needsAttention", () => {
    const quiet = { vulns: {}, checkVulns: true, lastRun: null, results: null };
    const run = {
      finished_at: 1,
      notify_only: false,
      available: 3,
      updated: ["git"],
      needs_admin: [],
      failed: [],
      signer_changed: [],
    };
    it("is off when there's nothing to look at", () => {
      expect(needsAttention(quiet)).toBe(false);
      expect(needsAttention({ ...quiet, lastRun: run })).toBe(false);
      expect(needsAttention({ ...quiet, lastRun: { ...run, needs_admin: ["zoom"] } })).toBe(false);
    });
    it("flags vulnerabilities only when the user opted in", () => {
      const vulns = { lodash: { current: ["GHSA-1"], fixed: [] } };
      expect(needsAttention({ ...quiet, vulns })).toBe(true);
      expect(needsAttention({ ...quiet, vulns, checkVulns: false })).toBe(false);
      expect(needsAttention({ ...quiet, vulns: { x: { current: [], fixed: [] } } })).toBe(false);
    });
    it("flags publisher changes and scheduled-run problems", () => {
      const flagged = [
        { kind: "cask", name: "zoom", success: false, output: "", action: { type: "trust_signer" as const, bundle_id: "b", team: "T" } },
      ];
      expect(needsAttention({ ...quiet, results: flagged })).toBe(true);
      expect(needsAttention({ ...quiet, lastRun: { ...run, failed: ["x"] } })).toBe(true);
      expect(needsAttention({ ...quiet, lastRun: { ...run, signer_changed: ["x"] } })).toBe(true);
      expect(needsAttention({ ...quiet, lastRun: { ...run, blocked: ["x"] } })).toBe(true);
    });
  });

  describe("keyboard", () => {
    const k = (key: string, metaKey = false, inField = false) => keyAction({ key, metaKey, inField });
    it("maps plain keys outside text fields", () => {
      expect([k("ArrowDown"), k("j"), k("ArrowUp"), k("k"), k(" "), k("ArrowRight"), k("ArrowLeft")]).toEqual([
        "down",
        "down",
        "up",
        "up",
        "toggle",
        "expand",
        "collapse",
      ]);
      expect([k("/"), k("?"), k("x")]).toEqual(["filter", "help", null]);
    });
    it("lets text fields keep their keys, except Esc and ⌘ shortcuts", () => {
      expect(k("j", false, true)).toBeNull();
      expect(k(" ", false, true)).toBeNull();
      expect(k("Escape", false, true)).toBe("escape");
      expect(k("Enter", true, true)).toBe("update");
    });
    it("maps ⌘ shortcuts", () => {
      expect([k("Enter", true), k("f", true), k("r", true), k(",", true), k("q", true)]).toEqual([
        "update",
        "filter",
        "refresh",
        "settings",
        null,
      ]);
      expect(k("Enter"), "plain Return never starts an update").toBeNull();
    });
    it("lists a shortcut for every action", () => {
      expect(SHORTCUTS.length).toBeGreaterThanOrEqual(9);
    });
    it("moveFocus stops at the ends", () => {
      expect(moveFocus(-1, 1, 5)).toBe(0);
      expect(moveFocus(-1, -1, 5)).toBe(4);
      expect(moveFocus(4, 1, 5)).toBe(4);
      expect(moveFocus(0, -1, 5)).toBe(0);
      expect(moveFocus(2, 1, 5)).toBe(3);
      expect(moveFocus(3, 1, 0)).toBe(-1);
    });
  });

  it("filterItems matches every word against name and id", () => {
    const items = [
      { id: "awscli", name: "awscli" },
      { id: "visual-studio-code", name: "Visual Studio Code" },
      { id: "python@3.15", name: "python@3.15" },
    ];
    expect(filterItems(items, "")).toBe(items);
    expect(filterItems(items, "  ").map((i) => i.id)).toEqual(["awscli", "visual-studio-code", "python@3.15"]);
    expect(filterItems(items, "CODE visual").map((i) => i.id)).toEqual(["visual-studio-code"]);
    expect(filterItems(items, "studio-code").map((i) => i.id)).toEqual(["visual-studio-code"]);
    expect(filterItems(items, "py 3.15").map((i) => i.id)).toEqual(["python@3.15"]);
    expect(filterItems(items, "zzz")).toEqual([]);
  });

  it("matchText", () => {
    expect([matchText(0), matchText(1), matchText(4)]).toEqual(["no matches", "1 match", "4 matches"]);
  });

  it("resolveTheme falls back to the default", () => {
    expect(resolveTheme("cobalt")).toBe("cobalt");
    expect(resolveTheme(null)).toBe("noir");
    expect(resolveTheme("removed-theme")).toBe("noir");
    expect(new Set(THEMES.map((t) => t.id)).size).toBe(THEMES.length);
  });

  it("every theme has a [data-theme] block defining every colour token", () => {
    expect(css.length, "App.css loaded").toBeGreaterThan(1000);
    const root = /:root \{([^}]*)\}/.exec(css)?.[1] ?? "";
    const colourTokens = [...root.matchAll(/(--[a-z-]+):\s*(?:rgba?\(|#)/g)].map((m) => m[1]);
    expect(colourTokens.length).toBeGreaterThan(15);
    for (const t of THEMES.filter((t) => t.id !== "noir")) {
      const block = new RegExp(`:root\\[data-theme="${t.id}"\\] \\{([^}]*)\\}`).exec(css)?.[1];
      expect(block, t.id).toBeDefined();
      for (const token of colourTokens) expect(block, `${t.id} ${token}`).toContain(`${token}:`);
    }
  });
});

describe("routeKey", () => {
  const idle: KeyContext = {
    help: false,
    results: false,
    history: false,
    adopt: false,
    settings: false,
    filterOpen: false,
    loading: false,
    verifying: false,
    updating: false,
    notifyOnly: false,
    scheduledRunning: false,
    selectedCount: 0,
    updatableCount: 5,
  };
  const panels = ["help", "results", "history", "adopt", "settings"] as const;

  it("Esc closes the topmost layer first", () => {
    const all = { ...idle, help: true, results: true, history: true, adopt: true, settings: true, filterOpen: true };
    const order = ["help", "results", "history", "adopt", "settings", "filter"];
    let c: KeyContext = all;
    for (const what of order) {
      expect(routeKey("escape", c)).toEqual({ cmd: { do: "close", what }, prevent: true });
      c = { ...c, [what === "filter" ? "filterOpen" : what]: false };
    }
    expect(routeKey("escape", c)).toEqual({ cmd: { do: "nothing" }, prevent: true });
  });

  it("arrows move, → ← open and close, Space is left to the control", () => {
    expect(routeKey("down", idle)).toEqual({ cmd: { do: "move", delta: 1 }, prevent: true });
    expect(routeKey("up", idle)).toEqual({ cmd: { do: "move", delta: -1 }, prevent: true });
    expect(routeKey("expand", idle).cmd).toEqual({ do: "expand" });
    expect(routeKey("collapse", idle).cmd).toEqual({ do: "collapse" });
    expect(routeKey("toggle", idle)).toEqual({ cmd: { do: "toggle" }, prevent: false });
  });

  it("the list keys do nothing, and don't swallow the key, while a panel is on top", () => {
    for (const panel of panels) {
      const c = { ...idle, [panel]: true };
      for (const action of ["down", "up", "expand", "collapse", "toggle", "filter", "update"] as const) {
        expect(routeKey(action, c), `${action} under ${panel}`).toEqual({ cmd: { do: "nothing" }, prevent: false });
      }
    }
  });

  it("? opens the shortcut list, and closes it again from on top", () => {
    expect(routeKey("help", idle).cmd).toEqual({ do: "toggleHelp" });
    expect(routeKey("help", { ...idle, help: true }).cmd).toEqual({ do: "toggleHelp" });
    expect(routeKey("help", { ...idle, settings: true }).cmd).toEqual({ do: "nothing" });
  });

  it("⌘, and ⌘R work over panels, but not mid-check", () => {
    for (const panel of panels) {
      const c = { ...idle, [panel]: true };
      expect(routeKey("settings", c).cmd).toEqual({ do: "settings" });
      expect(routeKey("refresh", c).cmd).toEqual({ do: "refresh" });
    }
    expect(routeKey("settings", { ...idle, loading: true })).toEqual({ cmd: { do: "nothing" }, prevent: true });
    for (const busy of ["loading", "verifying", "updating"] as const) {
      // Still prevented: ⌘R must never reload the web view.
      expect(routeKey("refresh", { ...idle, [busy]: true }), busy).toEqual({ cmd: { do: "nothing" }, prevent: true });
    }
  });

  it("⌘↩ updates the selection, else everything shown, else nothing", () => {
    expect(routeKey("update", { ...idle, selectedCount: 2 }).cmd).toEqual({ do: "update", which: "selected" });
    expect(routeKey("update", idle).cmd).toEqual({ do: "update", which: "all" });
    expect(routeKey("update", { ...idle, updatableCount: 0 }).cmd).toEqual({ do: "nothing" });
  });

  it("⌘↩ never starts an update when it shouldn't", () => {
    for (const block of ["notifyOnly", "updating", "loading", "scheduledRunning"] as const) {
      expect(routeKey("update", { ...idle, selectedCount: 3, [block]: true }).cmd, block).toEqual({ do: "nothing" });
    }
  });

  it("the filter opens from the list, including while it's already open", () => {
    expect(routeKey("filter", idle).cmd).toEqual({ do: "filter" });
    expect(routeKey("filter", { ...idle, filterOpen: true }).cmd).toEqual({ do: "filter" });
    expect(routeKey("down", { ...idle, filterOpen: true }).cmd).toEqual({ do: "move", delta: 1 });
  });
});

describe("keyboard cursor", () => {
  const ids = [catNavId("Homebrew"), rowNavId("brew", "git"), rowNavId("brew", "wget"), catNavId("Apps")];

  it("steps through every row in order and stops at the ends", () => {
    const seen: (string | null)[] = [];
    let at: string | null = null;
    for (let i = 0; i < 6; i++) {
      at = stepCursor(ids, at, 1);
      seen.push(at);
    }
    expect(seen).toEqual([...ids, ids[3], ids[3]]);
    for (let i = 0; i < 6; i++) at = stepCursor(ids, at, -1);
    expect(at).toBe(ids[0]);
  });

  it("starts at the top on ↓ and the bottom on ↑ from nowhere", () => {
    expect(stepCursor(ids, null, 1)).toBe(ids[0]);
    expect(stepCursor(ids, null, -1)).toBe(ids[3]);
  });

  it("restarts when its row is gone (section closed, filter changed)", () => {
    expect(stepCursor(ids, rowNavId("brew", "hidden"), 1)).toBe(ids[0]);
    expect(stepCursor(ids, rowNavId("brew", "hidden"), -1)).toBe(ids[3]);
  });

  it("has nowhere to go in an empty list", () => {
    expect(stepCursor([], null, 1)).toBeNull();
    expect(stepCursor([], "cat:x", -1)).toBeNull();
  });

  it("ids round-trip, including names with colons and spaces", () => {
    expect(parseNavId(catNavId("Other apps"))).toEqual({ type: "cat", cat: "Other apps" });
    expect(parseNavId(rowNavId("cask", "visual-studio-code"))).toEqual({ type: "row", key: "cask::visual-studio-code" });
    expect(parseNavId(rowNavId("npm", "@scope/pkg"))).toEqual({ type: "row", key: "npm::@scope/pkg" });
    expect(parseNavId(catNavId("a:b"))).toEqual({ type: "cat", cat: "a:b" });
    expect(parseNavId("bogus")).toBeNull();
  });
});
