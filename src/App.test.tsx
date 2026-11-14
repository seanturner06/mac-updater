// @vitest-environment jsdom
// The whole window, driven the way a person uses it: real key presses and clicks against the real
// App, with the Rust side replaced by a fake IPC (Tauri's own mocks). Covers keyboard control, the
// filter, the live run view, the menu-bar state and the theme picker.
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { SHORTCUTS, THEMES, type CategoryStatus, type TrayState, type UpdateResult } from "./logic";

const item = (id: string, current: string, latest: string, name = id) => ({
  id,
  name,
  current,
  latest,
  self_updating: false,
  notes_url: "",
});

const CATEGORIES: CategoryStatus[] = [
  {
    category: "Homebrew",
    kind: "brew",
    error: null,
    items: [item("awscli", "2.37.12_2", "2.38.5"), item("git", "2.55.0", "2.56.0"), item("ffmpeg", "7.1", "8.0")],
  },
  {
    category: "Apps",
    kind: "cask",
    error: null,
    items: [item("visual-studio-code", "1.105.0", "1.106.1", "Visual Studio Code"), item("zoom", "6.6.0", "6.6.2")],
  },
  { category: "npm", kind: "npm", error: null, items: [item("pnpm", "12.14.0", "12.15.0")] },
];

type Call = { cmd: string; args: Record<string, unknown> };

// The fake backend. `options` changes what it reports; `calls` records every command.
let calls: Call[];
let options: { notifyOnly: boolean; checkVulns: boolean; holdUpdates: boolean };
let releaseUpdate: ((r: UpdateResult[]) => void) | null;

function installBackend() {
  calls = [];
  releaseUpdate = null;
  mockIPC(
    (cmd, payload) => {
      const args = (payload ?? {}) as Record<string, unknown>;
      calls.push({ cmd, args });
      switch (cmd) {
        case "check_outdated":
          return CATEGORIES;
        case "check_sparkle_apps":
          return { category: "Other apps", kind: "sparkle", items: [], error: null };
        case "get_settings":
          return {
            greedy: false,
            notify_only: options.notifyOnly,
            skipped: [],
            check_vulnerabilities: options.checkVulns,
          };
        case "check_vulnerabilities":
          return { pnpm: { current: ["GHSA-test-0001"], fixed: ["GHSA-test-0001"] } };
        case "get_schedule":
          return { installed: false, hour: 10, minute: 0 };
        case "get_scheduled_state":
          return { running: false, last: null };
        case "run_selected_updates": {
          const selections = args.selections as { kind: string; id: string; name: string }[];
          const results: UpdateResult[] = selections.map((s) => ({
            kind: s.kind,
            name: s.name,
            success: true,
            output: `==> Upgrading ${s.id}`,
            elapsed_ms: 1000,
          }));
          if (!options.holdUpdates) return results;
          return new Promise<UpdateResult[]>((resolve) => {
            releaseUpdate = () => resolve(results);
          });
        }
        case "list_snapshots":
        case "get_history":
        case "verify_updated_casks":
          return [];
        case "package_details":
          return {};
        case "app_version":
          return "0.1.0";
        case "get_launch_at_login":
        case "homebrew_provenance_active":
          return false;
        default:
          // get_cached_check, check_app_update, app_icon, set_tray_state, …
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

// Let a held update (options.holdUpdates) return, and React settle.
async function finishUpdate() {
  await act(async () => {
    releaseUpdate?.([]);
    await Promise.resolve();
  });
}

const callsOf = (cmd: string) => calls.filter((c) => c.cmd === cmd);
const lastTray = () => {
  const sent = callsOf("set_tray_state");
  return sent.length > 0 ? (sent[sent.length - 1].args.state as TrayState) : undefined;
};
const focused = () => document.activeElement as HTMLElement;
// What the keyboard cursor is drawn on: a section's name, or a row's package name. This is what
// the user sees, so it must be exactly one element (the bug this guards: the highlight vanishing
// while the cursor moved through the middle rows).
const highlighted = () => {
  const lit = [...document.querySelectorAll<HTMLElement>(".kb-cursor")];
  expect(lit, "exactly one highlighted row").toHaveLength(1);
  const el = lit[0];
  return el.dataset.navId?.startsWith("cat:") ? el.dataset.cat : el.querySelector(".pkg-name-text")?.textContent;
};
const nothingHighlighted = () => document.querySelectorAll(".kb-cursor").length === 0;

async function setup() {
  const user = userEvent.setup();
  render(<App />);
  // The first check has landed once the section headers are drawn.
  await screen.findByText("Homebrew");
  await screen.findByText("Other apps");
  return user;
}

beforeAll(() => {
  // jsdom has no layout, so no scrollIntoView.
  Element.prototype.scrollIntoView = () => undefined;
});

beforeEach(() => {
  options = { notifyOnly: false, checkVulns: false, holdUpdates: false };
  localStorage.clear();
  delete document.documentElement.dataset.theme;
  installBackend();
});

afterEach(() => {
  cleanup();
  clearMocks();
  vi.restoreAllMocks();
});

describe("keyboard: moving around the list", () => {
  it("↓ and j walk the section headers and stop at the last one", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("Homebrew");
    await user.keyboard("j");
    expect(highlighted()).toBe("Apps");
    await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}");
    expect(highlighted()).toBe("Other apps");
  });

  it("↑ and k walk back and stop at the first one", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}");
    expect(highlighted()).toBe("npm");
    await user.keyboard("k");
    expect(highlighted()).toBe("Apps");
    await user.keyboard("{ArrowUp}{ArrowUp}{ArrowUp}");
    expect(highlighted()).toBe("Homebrew");
  });

  it("↑ with nothing focused starts from the bottom", async () => {
    const user = await setup();
    await user.keyboard("{ArrowUp}");
    expect(highlighted()).toBe("Other apps");
  });

  it("→ opens a section and ↓ continues into its rows", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}{ArrowRight}");
    expect(document.querySelector('[data-cat="Homebrew"]')?.getAttribute("aria-expanded")).toBe("true");
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("awscli");
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(highlighted()).toBe("ffmpeg");
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("Apps");
  });

  it("← on a row goes back to its header, and ← on the header closes the section", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}{ArrowRight}{ArrowDown}{ArrowDown}");
    expect(highlighted()).toBe("git");
    await user.keyboard("{ArrowLeft}");
    expect(highlighted()).toBe("Homebrew");
    await user.keyboard("{ArrowLeft}");
    expect(screen.queryByLabelText("Select git")).toBeNull();
    // ↓ now skips the closed section's rows.
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("Apps");
  });

  it("→ on a row opens its details and ← closes them", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}{ArrowRight}{ArrowDown}");
    const info = screen.getByLabelText("About awscli");
    expect(info.getAttribute("aria-expanded")).toBe("false");
    await user.keyboard("{ArrowRight}");
    expect(info.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("Skip 2.38.5")).toBeTruthy();
    await user.keyboard("{ArrowLeft}");
    expect(info.getAttribute("aria-expanded")).toBe("false");
    expect(highlighted()).toBe("awscli");
  });

  it("Space selects and deselects the focused row", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}{ArrowRight}{ArrowDown}{ArrowDown}");
    await user.keyboard(" ");
    expect(screen.getByRole("button", { name: "Update 1 selected" })).toBeTruthy();
    expect(screen.getByLabelText<HTMLInputElement>("Select git").checked).toBe(true);
    await user.keyboard(" ");
    expect(screen.getByLabelText<HTMLInputElement>("Select git").checked).toBe(false);
    expect(screen.getByRole("button", { name: /^Update all/ })).toBeTruthy();
  });

  it("Space on a header opens and closes it", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown} ");
    expect(screen.getByLabelText("Select awscli")).toBeTruthy();
    await user.keyboard(" ");
    expect(screen.queryByLabelText("Select awscli")).toBeNull();
  });

  it("arrow keys are kept from scrolling the window", async () => {
    await setup();
    const event = new KeyboardEvent("keydown", { key: "ArrowDown", cancelable: true, bubbles: true });
    act(() => {
      window.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(true);
  });
});

describe("keyboard: the highlight", () => {
  async function openSections(user: ReturnType<typeof userEvent.setup>, ...names: string[]) {
    for (const n of names) {
      const header = document.querySelector<HTMLElement>(`[data-cat="${n}"].cat-toggle`);
      if (header) await user.click(header);
    }
  }

  it("every ↓ lights exactly one row, through open sections and their headers", async () => {
    const user = await setup();
    // Opened by mouse, last click on Homebrew: the cursor waits there, hidden, until a key.
    await openSections(user, "Apps", "Homebrew");
    expect(nothingHighlighted()).toBe(true);
    const seen = [];
    for (let i = 0; i < 9; i++) {
      await user.keyboard("{ArrowDown}");
      seen.push(highlighted());
    }
    expect(seen).toEqual([
      "awscli",
      "git",
      "ffmpeg",
      "Apps",
      "Visual Studio Code",
      "zoom",
      "npm",
      "Other apps",
      "Other apps",
    ]);
  });

  it("every ↑ lights exactly one row on the way back", async () => {
    const user = await setup();
    await openSections(user, "Homebrew");
    await user.keyboard("{ArrowDown>9/}");
    expect(highlighted()).toBe("Other apps");
    const seen = [];
    for (let i = 0; i < 8; i++) {
      await user.keyboard("{ArrowUp}");
      seen.push(highlighted());
    }
    expect(seen).toEqual(["npm", "Apps", "ffmpeg", "git", "awscli", "Homebrew", "Homebrew", "Homebrew"]);
  });

  it("doesn't rely on browser focus: nothing in the list holds focus while navigating", async () => {
    const user = await setup();
    await openSections(user, "Homebrew");
    await user.click(screen.getByLabelText("Select git")); // the mouse focuses a checkbox
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(highlighted()).toBe("Apps");
    expect(document.activeElement?.closest(".categories")).toBeNull();
  });

  it("a click hides the highlight, and ↓ carries on from the clicked row", async () => {
    const user = await setup();
    await openSections(user, "Homebrew");
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("awscli");
    await user.click(screen.getByText("git"));
    expect(nothingHighlighted()).toBe(true);
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("ffmpeg");
  });

  it("clicking a header and pressing ↓ goes on to the next section", async () => {
    const user = await setup();
    await user.click(screen.getByText("Apps"));
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("Visual Studio Code");
  });

  it("when the highlighted row disappears, ↓ starts again from the top", async () => {
    const user = await setup();
    await openSections(user, "Homebrew");
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(highlighted()).toBe("git");
    // Closing the section from the keyboard moves the highlight to its header first…
    await user.keyboard("{ArrowLeft}{ArrowLeft}");
    expect(highlighted()).toBe("Homebrew");
    // …but a filter that hides it leaves nothing to stand on.
    await user.keyboard("/");
    await user.keyboard("zoom{Escape}");
    await user.keyboard("/");
    await user.keyboard("zoom");
    expect(nothingHighlighted()).toBe(true);
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("Apps");
  });

  it("Space selects only the highlighted row, even when a checkbox has mouse focus", async () => {
    const user = await setup();
    await openSections(user, "Homebrew");
    await user.click(screen.getByLabelText("Select awscli"));
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("git");
    await user.keyboard(" ");
    expect(screen.getByLabelText<HTMLInputElement>("Select awscli").checked).toBe(true);
    expect(screen.getByLabelText<HTMLInputElement>("Select git").checked).toBe(true);
    expect(screen.getByRole("button", { name: "Update 2 selected" })).toBeTruthy();
  });

  it("Space with no highlight leaves the page alone", async () => {
    const user = await setup();
    await user.keyboard(" ");
    expect(screen.queryByLabelText("Select awscli")).toBeNull();
    expect(callsOf("run_selected_updates")).toHaveLength(0);
  });

  it("Space opens a read-only row's details instead (notify-only)", async () => {
    options.notifyOnly = true;
    const user = await setup();
    await screen.findByText(/Notify-only is on/);
    await openSections(user, "Homebrew");
    await user.keyboard("{ArrowDown} ");
    expect(screen.getByLabelText("About awscli").getAttribute("aria-expanded")).toBe("true");
    await user.keyboard(" ");
    expect(screen.getByLabelText("About awscli").getAttribute("aria-expanded")).toBe("false");
  });

  it("Space doesn't change the selection while an update runs", async () => {
    options.holdUpdates = true;
    const user = await setup();
    await openSections(user, "Homebrew");
    await user.keyboard("{ArrowDown}{ArrowDown}{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    expect(highlighted()).toBe("git");
    await user.keyboard(" ");
    expect(screen.getByLabelText<HTMLInputElement>("Select git").checked).toBe(false);
    await finishUpdate();
  });

  it("↓ from the filter lands in the list, and later keys act on the list", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("git{ArrowDown}");
    expect(highlighted()).toBe("Homebrew");
    expect(document.activeElement).not.toBe(screen.getByLabelText("Filter updates"));
    await user.keyboard("j ");
    expect(screen.getByLabelText<HTMLInputElement>("Filter updates").value).toBe("git");
    expect(screen.getByLabelText<HTMLInputElement>("Select git").checked).toBe(true);
  });
});

describe("keyboard: actions", () => {
  it("⌘↩ updates the selection", async () => {
    const user = await setup();
    await user.keyboard("{ArrowDown}{ArrowRight}{ArrowDown}{ArrowDown} ");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    expect(callsOf("run_selected_updates")[0].args.selections).toEqual([{ kind: "brew", id: "git", name: "git" }]);
  });

  it("⌘↩ with nothing selected updates everything shown", async () => {
    const user = await setup();
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    const ids = (callsOf("run_selected_updates")[0].args.selections as { id: string }[]).map((s) => s.id);
    expect(ids).toEqual(["awscli", "git", "ffmpeg", "visual-studio-code", "zoom", "pnpm"]);
  });

  it("plain ↩ never starts an update", async () => {
    const user = await setup();
    await user.keyboard("{Enter}");
    await user.keyboard("{ArrowDown}{Enter}");
    expect(callsOf("run_selected_updates")).toHaveLength(0);
  });

  it("⌘↩ does nothing in notify-only mode", async () => {
    options.notifyOnly = true;
    const user = await setup();
    await screen.findByText(/Notify-only is on/);
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(callsOf("run_selected_updates")).toHaveLength(0);
  });

  it("⌘↩ doesn't start a second update while one runs", async () => {
    options.holdUpdates = true;
    const user = await setup();
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(callsOf("run_selected_updates")).toHaveLength(1);
    await finishUpdate();
  });

  it("⌘R checks again instead of reloading the window", async () => {
    await setup();
    await waitFor(() => expect(callsOf("check_outdated")).toHaveLength(1));
    const event = new KeyboardEvent("keydown", { key: "r", metaKey: true, cancelable: true, bubbles: true });
    act(() => {
      window.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(true);
    await waitFor(() => expect(callsOf("check_outdated")).toHaveLength(2));
  });

  it("⌘, opens Settings, the list keys are ignored there, and Esc closes it", async () => {
    const user = await setup();
    await user.keyboard("{Meta>},{/Meta}");
    expect(await screen.findByText("Appearance")).toBeTruthy();
    await user.keyboard("{ArrowDown}j");
    expect(nothingHighlighted()).toBe(true);
    await user.keyboard("{Escape}");
    expect(screen.queryByText("Appearance")).toBeNull();
  });

  it("? lists every shortcut; the list underneath doesn't move; Esc closes it", async () => {
    const user = await setup();
    await user.keyboard("?");
    for (const s of SHORTCUTS) expect(screen.getByText(s.what)).toBeTruthy();
    await user.keyboard("{ArrowDown}");
    expect(nothingHighlighted()).toBe(true);
    await user.keyboard("{Escape}");
    expect(screen.queryByText("These shortcuts")).toBeNull();
  });

  it("the footer ? button opens the shortcut list too", async () => {
    const user = await setup();
    await user.click(screen.getByTitle("Keyboard shortcuts"));
    expect(screen.getByText("Keyboard")).toBeTruthy();
  });
});

describe("filter", () => {
  it("⌘F opens it focused, and letters type instead of moving", async () => {
    const user = await setup();
    await user.keyboard("{Meta>}f{/Meta}");
    const input = screen.getByLabelText("Filter updates");
    expect(focused()).toBe(input);
    await user.keyboard("jk");
    expect((input as HTMLInputElement).value).toBe("jk");
    expect(focused()).toBe(input);
  });

  it("/ opens it too", async () => {
    const user = await setup();
    await user.keyboard("/");
    expect(focused()).toBe(screen.getByLabelText("Filter updates"));
  });

  it("narrows every section to matches and opens them", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("code");
    expect(screen.getByText("1 match")).toBeTruthy();
    expect(screen.getByLabelText("Select Visual Studio Code")).toBeTruthy();
    expect(screen.queryByText("Homebrew")).toBeNull();
    expect(screen.queryByLabelText("Select zoom")).toBeNull();
  });

  it("matches names and ids, every word, any case", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("STUDIO visual");
    expect(screen.getByText("1 match")).toBeTruthy();
    await user.clear(screen.getByLabelText("Filter updates"));
    await user.keyboard("a");
    // "a" is in awscli and Visual Studio Code, not in git.
    expect(screen.getByLabelText("Select awscli")).toBeTruthy();
    expect(screen.getByLabelText("Select Visual Studio Code")).toBeTruthy();
    expect(screen.queryByLabelText("Select git")).toBeNull();
  });

  it("the main button says and does 'Update N shown'", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("git");
    const button = screen.getByRole("button", { name: "Update 1 shown" });
    await user.click(button);
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    expect(callsOf("run_selected_updates")[0].args.selections).toEqual([{ kind: "brew", id: "git", name: "git" }]);
  });

  it("⌘↩ from the filter updates only what it shows", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("zoom{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    expect(callsOf("run_selected_updates")[0].args.selections).toEqual([{ kind: "cask", id: "zoom", name: "zoom" }]);
  });

  it("↓ from the filter goes to the first match", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("pnpm{ArrowDown}");
    expect(highlighted()).toBe("npm");
    await user.keyboard("{ArrowDown}");
    expect(highlighted()).toBe("pnpm");
  });

  it("no matches shows an empty list, not 'Everything is current'", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("zzz");
    expect(screen.getByText("no matches")).toBeTruthy();
    expect(screen.queryByText("Everything is current")).toBeNull();
    expect(screen.getByRole("button", { name: "Update 0 shown" })).toHaveProperty("disabled", true);
  });

  it("Esc clears and closes it, and the full list comes back", async () => {
    const user = await setup();
    await user.keyboard("/");
    await user.keyboard("zoom{Escape}");
    expect(screen.queryByLabelText("Filter updates")).toBeNull();
    expect(screen.getByText("Homebrew")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Update all (6)" })).toBeTruthy();
  });

  it("the header button toggles it", async () => {
    const user = await setup();
    await user.click(screen.getByTitle("Filter (⌘F)"));
    expect(screen.getByLabelText("Filter updates")).toBeTruthy();
    await user.click(screen.getByTitle("Filter (⌘F)"));
    expect(screen.queryByLabelText("Filter updates")).toBeNull();
  });
});

describe("live run view", () => {
  const progress = (step: string | null, current = "awscli", done = 0) =>
    act(async () => {
      await emit("update-progress", { phase: "Upgrading", current, done, total: 2, step });
    });

  async function startUpdate() {
    options.holdUpdates = true;
    const user = await setup();
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(1));
    return user;
  }

  it("shows the running step, then moves it into the finished list with its time", async () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    await startUpdate();
    await progress("Pouring python@3.15");
    const live = () => document.querySelector(".live-run") as HTMLElement;
    expect(within(live()).getByText("Pouring python@3.15")).toBeTruthy();
    expect(live().querySelector(".live-step.running")?.textContent).toContain("Pouring python@3.15");

    now += 41_000;
    await progress("Pouring awscli");
    const done = live().querySelectorAll(".live-step.done");
    expect(done).toHaveLength(1);
    expect(done[0].textContent).toContain("Pouring python@3.15");
    expect(done[0].textContent).toContain("41s");
    expect(live().querySelector(".live-step.running")?.textContent).toContain("Pouring awscli");
    await finishUpdate();
  });

  it("keeps only the last three finished steps", async () => {
    let now = 0;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    await startUpdate();
    for (const step of ["a", "b", "c", "d", "e"]) {
      now += 1000;
      await progress(step);
    }
    const labels = [...document.querySelectorAll(".live-step.done .live-label")].map((e) => e.textContent);
    expect(labels).toEqual(["b", "c", "d"]);
    await finishUpdate();
  });

  it("the running step's clock ticks", async () => {
    let now = 0;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const setInterval = vi.spyOn(window, "setInterval");
    await startUpdate();
    await progress("Pouring awscli");
    const time = () => document.querySelector(".live-step.running .live-time")?.textContent;
    expect(time()).toBe("0:00");
    // The 1-second ticker the view installs while updating.
    const tick = setInterval.mock.calls.find(([, ms]) => ms === 1000)?.[0] as (() => void) | undefined;
    expect(tick).toBeDefined();
    now = 102_000;
    act(() => tick?.());
    expect(time()).toBe("1:42");
    await finishUpdate();
  });

  it("disappears when the update finishes", async () => {
    await startUpdate();
    await progress("Pouring awscli");
    expect(document.querySelector(".live-run")).not.toBeNull();
    await finishUpdate();
    await waitFor(() => expect(document.querySelector(".live-run")).toBeNull());
  });

  it("starts empty on the next update", async () => {
    const user = await startUpdate();
    await progress("old step");
    await finishUpdate();
    await screen.findByText("Update results");
    await user.keyboard("{Escape}{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(callsOf("run_selected_updates")).toHaveLength(2));
    expect(screen.queryByText("old step")).toBeNull();
    await finishUpdate();
  });
});

describe("menu-bar icon", () => {
  it("shows the update count when idle", async () => {
    await setup();
    await waitFor(() => expect(lastTray()).toEqual({ count: 6, attention: false, running: false, done: 0, total: 0 }));
  });

  it("shows running with progress during an update, then goes back", async () => {
    options.holdUpdates = true;
    const user = await setup();
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(lastTray()?.running).toBe(true));
    await act(async () => {
      await emit("update-progress", { phase: "Upgrading", current: "git", done: 1, total: 6, step: "Pouring git" });
    });
    await waitFor(() => expect(lastTray()).toMatchObject({ running: true, done: 1, total: 6 }));
    await finishUpdate();
    await waitFor(() => expect(lastTray()).toMatchObject({ running: false, done: 0, total: 0 }));
  });

  it("needs a look when an opted-in vulnerability check finds something", async () => {
    options.checkVulns = true;
    await setup();
    await waitFor(() => expect(lastTray()?.attention).toBe(true));
  });

  it("only re-sends the state when it changes", async () => {
    await setup();
    await waitFor(() => expect(lastTray()?.count).toBe(6));
    const sent = callsOf("set_tray_state").map((c) => JSON.stringify(c.args.state));
    for (let i = 1; i < sent.length; i++) expect(sent[i]).not.toBe(sent[i - 1]);
  });
});

describe("theme picker", () => {
  async function openAppearance() {
    const user = await setup();
    await user.click(screen.getByTitle("Settings"));
    await screen.findByText("Appearance");
    return user;
  }

  it("offers every theme, with the default checked", async () => {
    await openAppearance();
    const radios = screen.getAllByRole("radio");
    expect(radios.map((r) => r.textContent)).toEqual(THEMES.map((t) => t.name));
    expect(screen.getByRole("radio", { name: "Gothic Noir" }).getAttribute("aria-checked")).toBe("true");
  });

  it("picking one applies it to the page and remembers it", async () => {
    const user = await openAppearance();
    await user.click(screen.getByRole("radio", { name: "Cobalt Sky" }));
    expect(document.documentElement.dataset.theme).toBe("cobalt");
    expect(localStorage.getItem("freshet.theme")).toBe("cobalt");
    expect(screen.getByRole("radio", { name: "Cobalt Sky" }).getAttribute("aria-checked")).toBe("true");
    expect(screen.getByRole("radio", { name: "Gothic Noir" }).getAttribute("aria-checked")).toBe("false");
  });

  it("a remembered theme is checked on the next launch", async () => {
    localStorage.setItem("freshet.theme", "ink");
    await openAppearance();
    expect(screen.getByRole("radio", { name: "Ink & Gold" }).getAttribute("aria-checked")).toBe("true");
  });
});
