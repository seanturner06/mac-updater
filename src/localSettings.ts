// The window's own copy of three settings, in localStorage. ~/.freshet/settings.json is the source
// of truth (the scheduled run reads it); these only seed the first paint before it loads, and
// are migrated into the file the first time it's missing.
export const IGNORE_STORE = "freshet.skipped";
export const GREEDY_STORE = "freshet.greedy";
export const NOTIFY_STORE = "freshet.notifyOnly";

export function loadFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

export function saveFlag(key: string, value: boolean) {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // Storage unavailable — the toggle still works, it just won't survive a relaunch.
  }
}

export function loadIgnored(): Set<string> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(IGNORE_STORE) ?? "[]");
    if (Array.isArray(parsed)) {
      return new Set((parsed as unknown[]).filter((v): v is string => typeof v === "string"));
    }
    return new Set();
  } catch {
    return new Set();
  }
}

export function saveIgnored(ignored: Set<string>) {
  try {
    localStorage.setItem(IGNORE_STORE, JSON.stringify([...ignored]));
  } catch {
    // Storage unavailable — the settings file is what matters.
  }
}
