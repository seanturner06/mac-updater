// The colour theme: chosen in Settings › Appearance, applied as <html data-theme="…"> (the blocks
// in App.css). Kept in localStorage, not settings.json: it's how this window looks, and the
// scheduled run has no use for it.
import { resolveTheme } from "./logic";

const THEME_STORE = "freshet.theme";

export function loadTheme(): string {
  try {
    return resolveTheme(localStorage.getItem(THEME_STORE));
  } catch {
    return resolveTheme(null);
  }
}

export function applyThemeAttr(id: string) {
  document.documentElement.dataset.theme = id;
}

export function saveTheme(id: string) {
  applyThemeAttr(id);
  try {
    localStorage.setItem(THEME_STORE, id);
  } catch {
    // Not remembered across launches; still applied now.
  }
}
