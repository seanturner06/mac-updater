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
  type CategoryStatus,
  type OutdatedItem,
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
