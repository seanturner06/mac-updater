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
