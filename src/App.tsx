import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  selKey,
  ignoreKey,
  displayItems,
  isReadOnly,
  countDisplayed,
  toggleSelected,
  toggleCategorySelected,
  prunedSelfUpdating,
  allDisplayedSelections,
  toSettings,
  scheduledSelections,
  splitDependencies,
  iconAppName,
  categorySummary,
  type CategoryStatus,
  type OutdatedItem,
  type PackageInfo,
  type Selection,
  type Settings,
  type ScheduledRunSummary,
  type UpdateResult,
  type ResultAction,
  type VulnReport,
  type Snapshot,
  type UpdateProgress,
  type AdoptProgress,
  adoptStatusText,
  type HistoryEntry,
  formatClock,
  type AppUpdate,
  isStale,
  progressText,
  advanceSteps,
  liveView,
  formatElapsed,
  formatDuration,
  needsAttention,
  keyAction,
  routeKey,
  stepCursor,
  parseNavId,
  catNavId,
  rowNavId,
  filterItems,
  matchText,
  SHORTCUTS,
  type LiveStep,
  type TrayState,
} from "./logic";
import { KindIcon, PackageRow, SkeletonRows } from "./PackageRow";
import { SettingsPanel } from "./SettingsPanel";
import { ResultsPanel } from "./ResultsPanel";
import { HistoryPanel } from "./HistoryPanel";
import { loadTheme, saveTheme } from "./theme";
import "./App.css";

const IGNORE_STORE = "freshet.skipped";
const GREEDY_STORE = "freshet.greedy";
const NOTIFY_STORE = "freshet.notifyOnly";

function loadFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}
function saveFlag(key: string, value: boolean) {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // Storage unavailable — the toggle still works, it just won't survive a relaunch.
  }
}
function loadIgnored(): Set<string> {
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

type ScheduleStatus = { installed: boolean; hour: number | null; minute: number | null };
type CaskSuggestion = { token: string; installed: boolean };
type AdoptableApp = { name: string; token: string };
type CaskInfo = {
  token: string;
  name: string;
  desc: string;
  homepage: string;
  version: string;
  auto_updates: boolean;
};

export default function App() {
  const [categories, setCategories] = useState<CategoryStatus[]>([]);
  const [loading, setLoading] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [lastChecked, setLastChecked] = useState<Date | null>(null);
  const [scheduleEnabled, setScheduleEnabled] = useState(false);
  const [scheduleError, setScheduleError] = useState<string | null>(null);
  const [scheduleConfirm, setScheduleConfirm] = useState(false);
  const [lastRun, setLastRun] = useState<ScheduledRunSummary | null>(null);
  // A scheduled (launchd) run is in progress right now.
  const [scheduledRunning, setScheduledRunning] = useState(false);
  // finished_at of the newest scheduled run this window has already refreshed for.
  const lastRunSeen = useRef<number | null>(null);
  const checkNowRef = useRef<() => Promise<void>>(() => Promise.resolve());
  const saveTimer = useRef<number | undefined>(undefined);
  const [hour, setHour] = useState(10);
  const [minute, setMinute] = useState(0);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selected, setSelected] = useState<Record<string, Selection>>({});
  const [results, setResults] = useState<UpdateResult[] | null>(null);
  const [resultsMs, setResultsMs] = useState<number | null>(null);
  // Live progress of a running update (from the backend's "update-progress" events).
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress | null>(null);
  const [stopping, setStopping] = useState(false);
  const [history, setHistory] = useState<HistoryEntry[] | null>(null);
  const [showHistory, setShowHistory] = useState(false);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [provenanceActive, setProvenanceActive] = useState<boolean | null>(null);
  const [appVersion, setAppVersion] = useState("");
  const [appUpdate, setAppUpdate] = useState<AppUpdate | null>(null);
  const [appUpdateState, setAppUpdateState] = useState<{ busy: boolean; error: string | null }>({
    busy: false,
    error: null,
  });
  const [checkVulns, setCheckVulns] = useState(false);
  // npm package name → known vulnerabilities (only fetched when the user opted in).
  const [vulns, setVulns] = useState<Record<string, VulnReport>>({});
  const [snapshots, setSnapshots] = useState<Snapshot[]>([]);
  const [resultsTitle, setResultsTitle] = useState("Update results");
  const [openLogs, setOpenLogs] = useState<Record<number, boolean>>({});
  const [greedy, setGreedy] = useState(() => loadFlag(GREEDY_STORE));
  const [notifyOnly, setNotifyOnly] = useState(() => loadFlag(NOTIFY_STORE));
  const [verifying, setVerifying] = useState(false);
  const [ignored, setIgnored] = useState<Set<string>>(loadIgnored);
  const [verifyProgress, setVerifyProgress] = useState<{ done: number; total: number }>({
    done: 0,
    total: 0,
  });
  const [sparkle, setSparkle] = useState<CategoryStatus | null>(null);
  const [sparkleLoading, setSparkleLoading] = useState(false);
  // Per-package descriptions/links ("kind::id" → info) and app icons (.app name → data URL, or
  // null when the app has no usable icon). Both are fetched in the background after a check.
  const [details, setDetails] = useState<Record<string, PackageInfo>>({});
  const [icons, setIcons] = useState<Record<string, string | null>>({});
  const iconsRequested = useRef(new Set<string>());
  const [openInfo, setOpenInfo] = useState<string | null>(null);
  const [showDeps, setShowDeps] = useState<Record<string, boolean>>({});
  const [showAdopt, setShowAdopt] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [adoptQuery, setAdoptQuery] = useState("");
  const [adoptResults, setAdoptResults] = useState<CaskSuggestion[]>([]);
  const [adoptSearching, setAdoptSearching] = useState(false);
  // Casks being adopted right now, and the live status of the current one.
  const [adopting, setAdopting] = useState<string[]>([]);
  const [adoptProgress, setAdoptProgress] = useState<AdoptProgress | null>(null);
  const [suggestions, setSuggestions] = useState<AdoptableApp[]>([]);
  const [suggestLoading, setSuggestLoading] = useState(false);
  const [adoptSelected, setAdoptSelected] = useState<Record<string, boolean>>({});
  // Live run view: the running update's steps, and a clock that ticks once a second meanwhile.
  const [liveSteps, setLiveSteps] = useState<LiveStep[]>([]);
  const [now, setNow] = useState(() => performance.now());
  const [theme, setTheme] = useState(loadTheme);
  const [filter, setFilter] = useState("");
  const [showFilter, setShowFilter] = useState(false);
  const filterRef = useRef<HTMLInputElement>(null);
  const [showHelp, setShowHelp] = useState(false);
  // The keyboard cursor (a data-nav-id), and whether to draw it: hidden again once the mouse is
  // used, but remembered, so the keyboard picks up from the last clicked row.
  const [cursor, setCursor] = useState<string | null>(null);
  const [kbMode, setKbMode] = useState(false);

  useEffect(() => {
    // Show the last check immediately (stale-while-revalidate); checkNow replaces it.
    void invoke<{ checked_at: number; categories: CategoryStatus[] } | null>("get_cached_check")
      .then((cached) => {
        if (!cached) return;
        setCategories((cur) => (cur.length === 0 ? cached.categories : cur));
        setLastChecked((cur) => cur ?? new Date(cached.checked_at * 1000));
        void loadDetails(cached.categories);
      })
      .catch(() => {
        // No cache yet.
      });
    void refreshSnapshots();
    void invoke<string>("app_version").then(setAppVersion).catch(() => undefined);
    void invoke<boolean>("get_launch_at_login").then(setLaunchAtLogin).catch(() => undefined);
    void invoke<boolean>("homebrew_provenance_active").then(setProvenanceActive).catch(() => undefined);
    void checkAppUpdate().catch(() => undefined);
    void checkNow();
    // Reflect the schedule that's actually installed (launchd plist), not defaults.
    void invoke<ScheduleStatus>("get_schedule")
      .then((s) => {
        setScheduleEnabled(s.installed);
        if (s.hour !== null) setHour(s.hour);
        if (s.minute !== null) setMinute(s.minute);
      })
      .catch(() => {
        setScheduleEnabled(false);
      });
    void invoke<ScheduledRunSummary | null>("get_last_scheduled_run")
      .then(setLastRun)
      .catch(() => setLastRun(null));
    // ~/.freshet/settings.json is the source of truth (the scheduled run reads it). localStorage
    // only seeds the first paint, and is migrated into the file the first time it's missing.
    void invoke<Settings | null>("get_settings")
      .then((s) => {
        if (s) {
          setGreedy(s.greedy);
          setNotifyOnly(s.notify_only);
          setIgnored(new Set(s.skipped));
          setCheckVulns(s.check_vulnerabilities);
        } else {
          saveSettings(loadFlag(GREEDY_STORE), loadFlag(NOTIFY_STORE), loadIgnored(), false);
        }
      })
      .catch(() => {
        // Backend unavailable — keep the localStorage values.
      });
    // Mount-only by design: the initial check and loading persisted state must run exactly once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Scan for adoptable apps each time the adopt panel opens.
  useEffect(() => {
    if (!showAdopt) return;
    // The selection is kept: closing and reopening the panel mid-adoption must not lose track
    // of what's being adopted. It's cleared when an adoption finishes.
    setSuggestLoading(true);
    void invoke<AdoptableApp[]>("suggest_adoptions")
      .then(setSuggestions)
      .catch(() => {
        setSuggestions([]);
      })
      .finally(() => {
        setSuggestLoading(false);
      });
  }, [showAdopt]);

  // Takes every value explicitly: state set in the same handler isn't visible yet.
  function saveSettings(g: boolean, n: boolean, ig: Set<string>, cv: boolean = checkVulns) {
    saveFlag(GREEDY_STORE, g);
    saveFlag(NOTIFY_STORE, n);
    try {
      localStorage.setItem(IGNORE_STORE, JSON.stringify([...ig]));
    } catch {
      // Storage unavailable — the settings file below is what matters.
    }
    void invoke("save_settings", { settings: toSettings(g, n, ig, cv) }).catch(() => {
      // Not fatal for the app itself; the scheduled run falls back to its last-saved settings.
    });
  }

  function persistIgnored(next: Set<string>) {
    setIgnored(next);
    saveSettings(greedy, notifyOnly, next);
  }
  function skipVersion(kind: string, id: string, latest: string) {
    const next = new Set(ignored);
    next.add(ignoreKey(kind, id, latest));
    persistIgnored(next);
  }
  function unskip(key: string) {
    const next = new Set(ignored);
    next.delete(key);
    persistIgnored(next);
  }
  function clearSkipped() {
    persistIgnored(new Set());
  }

  async function checkNow() {
    setLoading(true);
    // Sparkle is network-bound — load it in the background so it never blocks the local list.
    setSparkleLoading(true);
    invoke<CategoryStatus>("check_sparkle_apps")
      .then(setSparkle)
      .catch(() =>
        setSparkle({ category: "Other apps", kind: "sparkle", items: [], error: "check failed" })
      )
      .finally(() => setSparkleLoading(false));
    try {
      const result = await invoke<CategoryStatus[]>("check_outdated");
      setCategories(result);
      setSelected({});
      setLastChecked(new Date());
      void loadDetails(result);
    } catch {
      // IPC failure (should be rare — the command itself never rejects). Keep the
      // previous list rather than blanking the window.
    } finally {
      setLoading(false);
    }
  }

  async function refreshSnapshots() {
    try {
      setSnapshots(await invoke<Snapshot[]>("list_snapshots"));
    } catch {
      setSnapshots([]);
    }
  }

  // Follow-ups offered in the results panel.
  async function runResultAction(action: ResultAction): Promise<string> {
    if (action.type === "trust_signer") {
      await invoke("trust_signer", { bundleId: action.bundle_id, team: action.team });
      return `Trusted Team ${action.team}. Update again to install it.`;
    }
    if (action.type === "allow_npm_scripts") {
      // Record approval for this exact name@version, then install it with its scripts.
      await invoke("approve_npm_scripts", { name: action.name, version: action.version });
      const res = await invoke<UpdateResult[]>("run_selected_updates", {
        selections: [{ kind: "npm", id: action.name, name: action.name }],
      });
      void checkNow();
      const mine = res.find((r) => r.name === action.name);
      if (mine?.success) return `Installed ${action.name} ${action.version} with its install scripts.`;
      throw new Error(mine?.output.split("\n")[0] ?? "The install didn't run.");
    }
    if (action.type === "quit_and_update") {
      const res = await invoke<UpdateResult[]>("quit_and_update", {
        kind: action.kind,
        id: action.id,
        name: action.name,
        appName: action.app,
      });
      void checkNow();
      const mine = res.find((r) => r.name === action.name);
      if (mine?.success) return `Updated ${action.name}.`;
      throw new Error(mine?.output.split("\n")[0] ?? "The update didn't run.");
    }
    const message = await invoke<string>("restore_snapshot", { token: action.token });
    void checkNow();
    return message;
  }

  async function stopUpdates() {
    setStopping(true);
    await invoke("stop_updates");
  }

  async function openHistory() {
    setShowSettings(false);
    setShowHistory(true);
    setHistory(null);
    try {
      setHistory(await invoke<HistoryEntry[]>("get_history", { limit: 300 }));
    } catch {
      setHistory([]);
    }
  }

  async function applyLaunchAtLogin(next: boolean) {
    try {
      setLaunchAtLogin(await invoke<boolean>("set_launch_at_login", { enabled: next }));
    } catch {
      // Leave the toggle showing the real state.
    }
  }

  // Resolves to a status line for the Settings footer.
  async function checkAppUpdate(): Promise<string> {
    try {
      const update = await invoke<AppUpdate | null>("check_app_update");
      setAppUpdate(update);
      return update ? `${update.version} available` : "Freshet is up to date";
    } catch {
      // No release published yet, offline, or a dev build: nothing to offer.
      return "Couldn't check right now";
    }
  }

  async function installAppUpdate() {
    setAppUpdateState({ busy: true, error: null });
    try {
      await invoke("install_app_update"); // restarts the app on success
    } catch (e) {
      setAppUpdateState({ busy: false, error: String(e) });
    }
  }

  function applyCheckVulns(next: boolean) {
    setCheckVulns(next);
    saveSettings(greedy, notifyOnly, ignored, next);
  }

  // Known vulnerabilities for npm updates, re-fetched whenever the list or the opt-in changes
  // (so it doesn't matter whether settings or the first check loads first). The backend
  // enforces the setting too.
  useEffect(() => {
    if (!checkVulns) return;
    const packages = (categories.find((c) => c.kind === "npm")?.items ?? [])
      .filter((i) => i.current)
      .map((i) => ({ name: i.id, current: i.current, latest: i.latest }));
    if (packages.length === 0) return;
    invoke<Record<string, VulnReport>>("check_vulnerabilities", { packages })
      .then(setVulns)
      .catch(() => {
        // Offline or osv.dev unavailable: no badges rather than an error.
      });
  }, [categories, checkVulns]);

  // One bulk `brew info` per kind — descriptions, homepages, release notes, dependency flags.
  async function loadDetails(cats: CategoryStatus[]) {
    const ids = (kind: string) => cats.find((c) => c.kind === kind)?.items.map((i) => i.id) ?? [];
    const formulae = ids("brew");
    const casks = ids("cask");
    if (formulae.length === 0 && casks.length === 0) return;
    // Homebrew's recorded version per cask, so the backend can flag apps whose files say
    // otherwise (rolled back, or changed outside Homebrew).
    const installed = Object.fromEntries(
      (cats.find((c) => c.kind === "cask")?.items ?? []).map((i) => [i.id, i.current])
    );
    try {
      const d = await invoke<Record<string, PackageInfo>>("package_details", { formulae, casks, installed });
      setDetails((prev) => ({ ...prev, ...d }));
    } catch {
      // Details are a nicety; the list works without them.
    }
  }

  // Toggle the self-updating filter (no backend re-fetch). When turning it off, drop any
  // now-hidden self-updating items from the selection so they aren't silently updated.
  function applyGreedy(next: boolean) {
    setGreedy(next);
    saveSettings(next, notifyOnly, ignored);
    if (!next) setSelected((prev) => prunedSelfUpdating(prev, categories));
  }

  function applyNotifyOnly(next: boolean) {
    setNotifyOnly(next);
    saveSettings(greedy, next, ignored);
  }

  function toggleItem(kind: string, id: string, name: string) {
    setSelected((prev) => toggleSelected(prev, kind, id, name));
  }

  function toggleCategory(cat: CategoryStatus) {
    const items = displayItems(cat, greedy, ignored);
    setSelected((prev) => toggleCategorySelected(prev, cat.kind, items));
  }

  // Auto-expand any failures so problems are visible immediately.
  function showResults(title: string, res: UpdateResult[]) {
    setOpenLogs(res.reduce((acc, r, i) => (r.success ? acc : { ...acc, [i]: true }), {}));
    setResultsTitle(title);
    setResults(res);
  }

  // Tokens of casks we just upgraded; the post-update verify resolves these to .app filenames
  // via brew's local metadata and inspects only those bundles (vs. all of /Applications).
  // Takes a thunk (not a promise) so the progress listener is attached before the update starts.
  // caskTokens / appStoreApps: what to run the post-update publisher check on.
  async function runUpdate(
    start: () => Promise<UpdateResult[]>,
    caskTokens: string[],
    appStoreApps: string[] = []
  ) {
    setUpdating(true);
    setStopping(false);
    setUpdateProgress(null);
    setLiveSteps([]);
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await listen<UpdateProgress>("update-progress", (e) => {
        setUpdateProgress(e.payload);
        setLiveSteps((steps) => advanceSteps(steps, e.payload, performance.now()));
      });
    } catch {
      // No live progress; the update still runs.
    }
    const started = performance.now();
    try {
      const updateRes = await start();
      unlisten?.();
      unlisten = null;
      setUpdateProgress(null);
      setResultsMs(performance.now() - started);
      void refreshSnapshots();

      // If a `brew upgrade` pulled in a binary signed by a *different* developer than last time
      // — the hijacked-release signal — surface it now, while the user is looking. Targeted so
      // a 5-cask upgrade verifies 5 apps, not all 150 in /Applications.
      let flagged: UpdateResult[] = [];
      if (caskTokens.length > 0 || appStoreApps.length > 0) {
        try {
          flagged = await invoke<UpdateResult[]>("verify_updated_casks", {
            tokens: caskTokens,
            apps: appStoreApps,
          });
        } catch {
          // Best effort. Never block the update results panel on the signature check.
        }
      }

      if (flagged.length > 0) {
        const n = flagged.length;
        const title = `Update results — ⚠ ${n} signer change${n === 1 ? "" : "s"}`;
        showResults(title, [...flagged, ...updateRes]);
      } else {
        showResults("Update results", updateRes);
      }
      await checkNow();
    } finally {
      unlisten?.();
      setUpdateProgress(null);
      setStopping(false);
      setUpdating(false);
    }
  }

  async function verifySignatures() {
    setVerifying(true);
    setVerifyProgress({ done: 0, total: 0 });
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await listen<{ done: number; total: number }>(
        "verify-progress",
        (e) => setVerifyProgress(e.payload)
      );
    } catch {
      // Progress events unavailable — verify anyway, just without the live ring.
    }
    const started = performance.now();
    try {
      const res = await invoke<UpdateResult[]>("verify_signatures");
      setResultsMs(performance.now() - started);
      showResults("Signature check", res);
    } finally {
      unlisten?.();
      setVerifying(false);
    }
  }

  async function searchCasks() {
    if (!adoptQuery.trim()) {
      setAdoptResults([]);
      return;
    }
    setAdoptSearching(true);
    try {
      setAdoptResults(await invoke<CaskSuggestion[]>("search_casks", { query: adoptQuery }));
    } finally {
      setAdoptSearching(false);
    }
  }

  // Runs at the App level (not in the panel), so leaving the panel doesn't lose the adoption:
  // progress keeps arriving and the header shows it.
  async function runAdoption(tokens: string[]) {
    if (tokens.length === 0 || adopting.length > 0) return;
    setAdopting(tokens);
    setAdoptProgress(null);
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await listen<AdoptProgress>("adopt-progress", (e) => setAdoptProgress(e.payload));
    } catch {
      // No live status; the adoption still runs.
    }
    try {
      const res = await invoke<UpdateResult[]>("adopt_casks", { tokens });
      setShowAdopt(false);
      setAdoptSelected({});
      showResults("Homebrew adoption", res);
      await checkNow();
    } finally {
      unlisten?.();
      setAdopting([]);
      setAdoptProgress(null);
    }
  }

  async function adopt(token: string) {
    await runAdoption([token]);
  }

  async function cancelAdoption() {
    await invoke("cancel_adoption");
  }

  function toggleAdoptSelect(token: string) {
    setAdoptSelected((prev) => {
      const next = { ...prev };
      if (next[token]) delete next[token];
      else next[token] = true;
      return next;
    });
  }

  async function adoptSelectedApps() {
    await runAdoption(Object.keys(adoptSelected));
  }

  // "Update all" = update exactly what's displayed. Runs per-item selections (not a blanket
  // `brew upgrade`) so skipped versions and hidden self-updating casks stay untouched.
  // While filtering, "all" means everything the filter shows: what you see is what updates.
  async function updateAll() {
    const selections = filterItems(allDisplayedSelections(categories, greedy, ignored), filter);
    if (selections.length === 0) return;
    const caskTokens = selections.filter((s) => s.kind === "cask").map((s) => s.id);
    const appStoreApps = selections.filter((s) => s.kind === "mas").map((s) => `${s.name}.app`);
    await runUpdate(
      () => invoke<UpdateResult[]>("run_selected_updates", { selections }),
      caskTokens,
      appStoreApps
    );
  }

  async function updateSelected() {
    const selections = Object.values(selected);
    if (selections.length === 0) return;
    const caskTokens = selections.filter((s) => s.kind === "cask").map((s) => s.id);
    const appStoreApps = selections.filter((s) => s.kind === "mas").map((s) => `${s.name}.app`);
    await runUpdate(
      () => invoke<UpdateResult[]>("run_selected_updates", { selections }),
      caskTokens,
      appStoreApps
    );
  }

  // Enabling is confirmed first when the first run would install something, so nobody is
  // surprised by dozens of upgrades the next morning.
  async function toggleSchedule(enabled: boolean, confirmed = false) {
    if (enabled && !confirmed && !notifyOnly && scheduledCount > 0) {
      setScheduleConfirm(true);
      return;
    }
    setScheduleConfirm(false);
    try {
      if (enabled) {
        await invoke("install_schedule", { hour, minute });
      } else {
        await invoke("uninstall_schedule");
      }
      setScheduleEnabled(enabled);
      setScheduleError(null);
    } catch (e) {
      // launchctl failed — leave the toggle reflecting the real (unchanged) state, and say why.
      setScheduleError(String(e));
    }
  }

  // Takes the new time explicitly: reading `hour`/`minute` state here would see the values
  // from the render the handler was created in (stale closure), installing the old time.
  function changeScheduleTime(h: number, m: number) {
    setHour(h);
    setMinute(m);
    if (scheduleEnabled) {
      // Save once the user stops adjusting. Picking the hour and then the minute shouldn't
      // install the in-between time (e.g. 2:00 PM when 2:45 PM is coming).
      window.clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(() => {
        // Keep the picker values on failure; the next successful toggle rewrites the plist.
        invoke("install_schedule", { hour: h, minute: m })
          .then(() => setScheduleError(null))
          .catch((e: unknown) => setScheduleError(String(e)));
      }, 800);
    }
  }

  // The 4 local categories plus the separately-loaded "Other apps" (Sparkle) section. We always
  // show an "Other apps" entry so the user sees it loading; it's a placeholder until the fetch lands.
  const sparkleCategory: CategoryStatus =
    sparkle ?? { category: "Other apps", kind: "sparkle", items: [], error: null };
  const displayCategories = [...categories, sparkleCategory];

  const totalOutdated = countDisplayed(displayCategories, greedy, ignored);
  // What "Update all" would actually run — excludes sparkle (read-only), so the button is
  // disabled when only self-updating apps have updates.
  const updatableCount = filterItems(allDisplayedSelections(categories, greedy, ignored), filter).length;
  const filtering = filter.trim() !== "";
  const filterMatches = displayCategories.reduce(
    (n, c) => n + filterItems(displayItems(c, greedy, ignored), filter).length,
    0
  );
  const scheduledCount = scheduledSelections(categories, greedy, ignored).length;
  const selectedCount = Object.keys(selected).length;
  const allCurrent = !loading && !sparkleLoading && totalOutdated === 0;
  const verifyPct =
    verifyProgress.total > 0 ? verifyProgress.done / verifyProgress.total : 0;

  // Fetch each visible app's icon once. setState happens in the promise callback, not the effect body.
  useEffect(() => {
    const wanted = [...categories, ...(sparkle ? [sparkle] : [])].flatMap((c) =>
      c.items.map((i) => iconAppName(c.kind, i, details)).filter((a): a is string => a !== null)
    );
    for (const app of wanted) {
      if (iconsRequested.current.has(app)) continue;
      iconsRequested.current.add(app);
      invoke<string | null>("app_icon", { app })
        .then((url) => setIcons((prev) => ({ ...prev, [app]: url })))
        .catch(() => setIcons((prev) => ({ ...prev, [app]: null })));
    }
  }, [categories, sparkle, details]);

  // Keep the list fresh without being asked: every 3 hours while open, and whenever the window
  // comes back into view after 30+ minutes. Never mid-check or mid-update.
  useEffect(() => {
    const refreshIfOlderThan = (maxAgeMs: number) => {
      if (!loading && !updating && isStale(lastChecked, new Date(), maxAgeMs)) void checkNow();
    };
    const timer = window.setInterval(() => refreshIfOlderThan(3 * 3_600_000), 10 * 60_000);
    const onShow = () => {
      if (document.visibilityState === "visible") refreshIfOlderThan(30 * 60_000);
    };
    document.addEventListener("visibilitychange", onShow);
    window.addEventListener("focus", onShow);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onShow);
      window.removeEventListener("focus", onShow);
    };
  });

  // A menu-bar app stays open for days: look for a newer Freshet once a day, not just at launch.
  useEffect(() => {
    const timer = window.setInterval(() => {
      invoke<AppUpdate | null>("check_app_update")
        .then(setAppUpdate)
        .catch(() => undefined);
    }, 24 * 3_600_000);
    return () => window.clearInterval(timer);
  }, []);

  // Notice scheduled runs: poll a tiny status file every 30s and whenever the window is shown.
  // While one runs, say so (and hold off manual updates); when it finishes, refresh the list —
  // so the window never shows updates the schedule already installed.
  useEffect(() => {
    checkNowRef.current = checkNow;
  });
  useEffect(() => {
    const poll = () => {
      invoke<{ running: boolean; last: ScheduledRunSummary | null }>("get_scheduled_state")
        .then(({ running, last }) => {
          setScheduledRunning(running);
          if (!last) return;
          if (lastRunSeen.current !== null && last.finished_at > lastRunSeen.current) {
            void checkNowRef.current();
          }
          lastRunSeen.current = last.finished_at;
          setLastRun(last);
        })
        .catch(() => undefined);
    };
    poll();
    const timer = window.setInterval(poll, 30_000);
    const onShow = () => {
      if (document.visibilityState === "visible") poll();
    };
    document.addEventListener("visibilitychange", onShow);
    window.addEventListener("focus", onShow);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onShow);
      window.removeEventListener("focus", onShow);
    };
  }, []);

  // The menu-bar icon mirrors the window: running (outline drop + "3/12"), needs a look (amber
  // drop), or the plain drop with the update count.
  const attention = needsAttention({ vulns, checkVulns, lastRun, results });
  const trayState: TrayState = {
    count: totalOutdated,
    attention,
    running: updating || scheduledRunning,
    done: updating ? (updateProgress?.done ?? 0) : 0,
    total: updating ? (updateProgress?.total ?? 0) : 0,
  };
  const trayKey = JSON.stringify(trayState);
  useEffect(() => {
    void invoke("set_tray_state", { state: JSON.parse(trayKey) as TrayState }).catch(() => undefined);
  }, [trayKey]);

  // Keep the keyboard cursor's row on screen.
  useEffect(() => {
    if (!kbMode || cursor === null) return;
    const el = [...document.querySelectorAll<HTMLElement>(".categories [data-nav-id]")].find(
      (e) => e.dataset.navId === cursor
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [cursor, kbMode]);

  // Tick the live run view's clock while an update runs.
  useEffect(() => {
    if (!updating) return;
    const timer = window.setInterval(() => setNow(performance.now()), 1000);
    return () => window.clearInterval(timer);
  }, [updating]);

  function applyTheme(id: string) {
    setTheme(id);
    saveTheme(id);
  }

  function openFilter() {
    setShowFilter(true);
    // After the input renders.
    window.setTimeout(() => filterRef.current?.focus(), 0);
  }
  function closeFilter() {
    setFilter("");
    setShowFilter(false);
  }

  // Keyboard control of the main list. Panels (settings, results, history, adopt) only take Esc.
  const keyHandler = useRef<(e: KeyboardEvent) => void>(() => undefined);

  // The rows the keyboard can land on, in screen order.
  function navIds(): string[] {
    return [...document.querySelectorAll<HTMLElement>(".categories [data-nav-id]")].flatMap((el) =>
      el.dataset.navId ? [el.dataset.navId] : []
    );
  }
  function rowByKey(key: string): { cat: CategoryStatus; item: OutdatedItem } | null {
    for (const cat of displayCategories) {
      const item = cat.items.find((i) => selKey(cat.kind, i.id) === key);
      if (item) return { cat, item };
    }
    return null;
  }
  function moveCursor(id: string | null) {
    setCursor(id);
    setKbMode(true);
    // Nothing else keeps focus meanwhile, so Space can't also click a mouse-focused checkbox.
    const active = document.activeElement as HTMLElement | null;
    if (active?.closest(".categories, .filter-bar")) active.blur();
  }
  keyHandler.current = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement | null;
    const inField = target?.tagName === "INPUT" && (target as HTMLInputElement).type !== "checkbox";
    const action = keyAction({ key: e.key, metaKey: e.metaKey, inField: inField || target?.tagName === "TEXTAREA" });
    if (!action) return;
    const { cmd, prevent } = routeKey(action, {
      help: showHelp,
      results: results !== null,
      history: showHistory,
      adopt: showAdopt,
      settings: showSettings,
      filterOpen: showFilter,
      loading,
      verifying,
      updating,
      notifyOnly,
      scheduledRunning,
      selectedCount,
      updatableCount,
    });
    if (prevent) e.preventDefault();
    switch (cmd.do) {
      case "close":
        if (cmd.what === "help") setShowHelp(false);
        else if (cmd.what === "results") {
          setResults(null);
          setResultsMs(null);
          setOpenLogs({});
        } else if (cmd.what === "history") setShowHistory(false);
        else if (cmd.what === "adopt") setShowAdopt(false);
        else if (cmd.what === "settings") setShowSettings(false);
        else closeFilter();
        return;
      case "settings":
        setShowSettings(true);
        return;
      case "refresh":
        void checkNow();
        return;
      case "toggleHelp":
        setShowHelp((h) => !h);
        return;
      case "filter":
        openFilter();
        return;
      case "update":
        void (cmd.which === "selected" ? updateSelected() : updateAll());
        return;
      case "move":
      case "expand":
      case "collapse":
      case "toggle":
        break;
      case "nothing":
        return;
    }
    // A cursor on a row that's gone (its section closed, the filter changed) counts as none.
    const onScreen = cursor !== null && navIds().includes(cursor) ? cursor : null;
    if (cmd.do === "move") {
      // From the last clicked row too, even while the cursor is hidden.
      moveCursor(stepCursor(navIds(), onScreen, cmd.delta));
      return;
    }
    const here = kbMode && onScreen !== null ? parseNavId(onScreen) : null;
    if (!here) return; // Space, → and ← with no keyboard cursor: leave them to the page
    e.preventDefault(); // Space would otherwise also click whatever the mouse last focused
    if (here.type === "cat") {
      const cat = here.cat;
      const open = cmd.do === "toggle" ? !(expanded[cat] ?? false) : cmd.do === "expand";
      setExpanded((x) => ({ ...x, [cat]: open }));
      return;
    }
    const key = here.key;
    const row = rowByKey(key);
    if (!row) return;
    if (cmd.do === "toggle") {
      if (isReadOnly(row.cat.kind, notifyOnly)) setOpenInfo((cur) => (cur === key ? null : key));
      else if (!updating) toggleItem(row.cat.kind, row.item.id, row.item.name);
    } else if (cmd.do === "expand") setOpenInfo(key);
    else if (openInfo === key) setOpenInfo(null);
    // ← on a closed row: back up to its section header.
    else moveCursor(catNavId(row.cat.category));
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => keyHandler.current(e);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function renderRow(cat: CategoryStatus, item: OutdatedItem) {
    const key = selKey(cat.kind, item.id);
    const app = iconAppName(cat.kind, item, details);
    return (
      <PackageRow
        key={key}
        kind={cat.kind}
        item={item}
        info={details[key]}
        vuln={checkVulns && cat.kind === "npm" ? vulns[item.id] : undefined}
        rolledBack={cat.kind === "cask" && snapshots.some((s) => s.token === item.id && s.in_use)}
        icon={app ? icons[app] : null}
        selectable={!isReadOnly(cat.kind, notifyOnly)}
        checked={key in selected}
        locked={updating}
        open={openInfo === key}
        navId={rowNavId(cat.kind, item.id)}
        category={cat.category}
        cursor={kbMode && cursor === rowNavId(cat.kind, item.id)}
        onToggle={() => toggleItem(cat.kind, item.id, item.name)}
        onToggleInfo={() => setOpenInfo((cur) => (cur === key ? null : key))}
        onSkip={() => skipVersion(cat.kind, item.id, item.latest)}
      />
    );
  }

  function renderCategory(cat: CategoryStatus) {
    const items = filterItems(displayItems(cat, greedy, ignored), filter);
    if (filter.trim() && items.length === 0) return null;
    const itemKeys = items.map((i) => selKey(cat.kind, i.id));
    const selectedInCat = itemKeys.filter((k) => k in selected).length;
    const allInCatSelected = itemKeys.length > 0 && selectedInCat === itemKeys.length;
    // A filter opens every section with a match.
    const isOpen = filter.trim() ? true : (expanded[cat.category] ?? false);
    const pending = cat.kind === "sparkle" && sparkleLoading;
    const { primary, deps } = splitDependencies(cat.kind, items, details);
    const depsOpen = showDeps[cat.category] ?? false;
    return (
      <section key={cat.category} className={`cat${isOpen ? " open" : ""}${items.length === 0 ? " empty" : ""}`}>
        <div className="cat-head">
          <button
            onClick={() => setExpanded((e) => ({ ...e, [cat.category]: !isOpen }))}
            aria-expanded={isOpen}
            data-nav-id={catNavId(cat.category)}
            data-cat={cat.category}
            className={`cat-toggle${kbMode && cursor === catNavId(cat.category) ? " kb-cursor" : ""}`}
          >
            <span className={`cat-icon kind-${cat.kind}`}>
              <KindIcon kind={cat.kind} />
            </span>
            <span className="cat-text">
              <span className="cat-name">{cat.category}</span>
              <span className="cat-sub">
                {cat.error
                  ? "check failed"
                  : pending
                  ? "checking…"
                  : items.length === 0
                  ? "all current"
                  : categorySummary(cat.kind, items, details)}
              </span>
            </span>
            {items.length > 0 && <span className="count">{items.length}</span>}
            {items.length === 0 && !pending && !cat.error && <span className="count count-ok">✓</span>}
            <span className="chevron">›</span>
          </button>
        </div>
        {isOpen && (
          <div className="cat-body">
            {items.length > 1 && !isReadOnly(cat.kind, notifyOnly) && (
              <label className="select-all">
                <input
                  type="checkbox"
                  className="check"
                  checked={allInCatSelected}
                  ref={(el) => {
                    if (el) el.indeterminate = selectedInCat > 0 && !allInCatSelected;
                  }}
                  onChange={() => toggleCategory(cat)}
                  disabled={updating}
                />
                <span>{selectedInCat > 0 ? `${selectedInCat} of ${items.length} selected` : `Select all ${items.length}`}</span>
              </label>
            )}
            {cat.error && <div className="error-msg">{cat.error}</div>}
            {pending && <div className="empty-msg">checking…</div>}
            {items.length === 0 && !cat.error && !pending && <div className="empty-msg">Nothing to update.</div>}
            {primary.map((item) => renderRow(cat, item))}
            {deps.length > 0 && (
              <>
                <button
                  className={`deps-toggle${depsOpen ? " open" : ""}`}
                  onClick={() => setShowDeps((d) => ({ ...d, [cat.category]: !depsOpen }))}
                  aria-expanded={depsOpen}
                >
                  <span className="chevron">›</span>
                  <span>
                    {deps.length} {deps.length === 1 ? "dependency" : "dependencies"}
                  </span>
                  <span className="deps-hint">installed by other packages</span>
                </button>
                {depsOpen && deps.map((item) => renderRow(cat, item))}
              </>
            )}
          </div>
        )}
      </section>
    );
  }

  return (
    <div className="app">
      <div className="header" data-tauri-drag-region>
        <div className="header-left">
          <div className={`status-dot ${allCurrent ? "ok" : totalOutdated > 0 ? "warn" : "idle"}`} />
          <div>
            <div className="title">Freshet</div>
            <div className="subtitle">
              {adopting.length > 0 && !showAdopt
                ? `adopting ${adopting.join(", ")}… ${adoptProgress?.stalled ? "(download stalled)" : ""}`.trim()
                : scheduledRunning && !updating
                ? "scheduled update running…"
                : updating
                ? updateProgress && updateProgress.total > 0
                  ? `updating ${updateProgress.done} of ${updateProgress.total}…`
                  : "updating…"
                : verifying
                ? `verifying signatures… ${Math.round(verifyPct * 100)}%`
                : loading
                ? "checking…"
                : allCurrent
                ? "everything is current"
                : `${totalOutdated} update${totalOutdated === 1 ? "" : "s"} available`}
            </div>
          </div>
        </div>
        <div className="header-actions" data-tauri-drag-region="false">
          <button
            className={`icon-btn${showFilter ? " active" : ""}`}
            onClick={() => (showFilter ? closeFilter() : openFilter())}
            title="Filter (⌘F)"
          >
            <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.5">
              <circle cx="7" cy="7" r="4.5" />
              <path d="M10.4 10.4 14 14" strokeLinecap="round" />
            </svg>
          </button>
          <button className="icon-btn" onClick={() => checkNow()} disabled={loading || verifying} title="Refresh">
            <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.5">
              <path d="M2 8a6 6 0 0 1 10.5-4M14 8a6 6 0 0 1-10.5 4M12 2v3h-3M4 14v-3h3" strokeLinecap="round" strokeLinejoin="round"/>
            </svg>
          </button>
          <button className="icon-btn" onClick={() => setShowSettings(true)} disabled={loading} title="Settings">
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.3">
              <circle cx="8" cy="8" r="2.2" />
              <path d="M8 1.5v2M8 12.5v2M14.5 8h-2M3.5 8h-2M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4M12.6 12.6l-1.4-1.4M4.8 4.8L3.4 3.4" strokeLinecap="round" />
            </svg>
          </button>
        </div>
      </div>

      {appUpdate && (
        <div className="app-update-banner" role="status">
          <span className="app-update-text">
            <strong>Freshet {appUpdate.version}</strong> is available
            {appUpdateState.error && <span className="fail-text"> · {appUpdateState.error}</span>}
          </span>
          <button
            className="btn-primary btn-compact"
            onClick={() => void installAppUpdate()}
            disabled={appUpdateState.busy || updating}
          >
            {appUpdateState.busy ? "Installing…" : "Install & restart"}
          </button>
        </div>
      )}

      {showFilter && (
        <div className="filter-bar">
          <input
            ref={filterRef}
            className="filter-input"
            placeholder="Filter updates…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => {
              // ↓ from the filter goes straight into the results.
              if (e.key === "ArrowDown") {
                e.preventDefault();
                moveCursor(stepCursor(navIds(), null, 1));
              }
            }}
            aria-label="Filter updates"
          />
          {filter && <span className="filter-count">{matchText(filterMatches)}</span>}
        </div>
      )}

      <div
        className="categories"
        onMouseDown={(e) => {
          // A click hides the keyboard cursor but moves it here, so ↓ continues from this row.
          const el = (e.target as HTMLElement).closest<HTMLElement>("[data-nav-id]");
          setKbMode(false);
          if (el?.dataset.navId) setCursor(el.dataset.navId);
        }}
      >
        {loading && categories.length === 0 ? (
          <SkeletonRows count={5} />
        ) : allCurrent && !filter.trim() && !displayCategories.some((c) => c.error) ? (
          <div className="all-current">
            <div className="all-current-mark">
              <svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="currentColor" strokeWidth="1.8">
                <path d="M5 12.5l4.5 4.5L19 7.5" strokeLinecap="round" strokeLinejoin="round" />
              </svg>
            </div>
            <div className="all-current-title">Everything is current</div>
            <div className="all-current-sub">Homebrew, apps, App Store and npm are all up to date.</div>
          </div>
        ) : (
          displayCategories.map((cat) => renderCategory(cat))
        )}
      </div>

      <div className="actions">
        {notifyOnly ? (
          <div className="notify-note">Notify-only is on. Freshet shows updates but won't install them.</div>
        ) : (
          <div className="actions-row">
            {updating ? (
              <div className="update-progress" role="status" aria-live="polite">
                <LiveRun steps={liveSteps} now={now} />
                <div className="update-progress-text">
                  <span className="spinner" />
                  <span className="update-progress-label">{progressText(updateProgress)}</span>
                  {updateProgress && updateProgress.total > 0 && (
                    <span className="update-progress-count">
                      {updateProgress.done} of {updateProgress.total}
                    </span>
                  )}
                  <button
                    className="btn-ghost btn-compact update-stop"
                    onClick={() => void stopUpdates()}
                    disabled={stopping}
                    title="Finish the current package, then stop"
                  >
                    {stopping ? "Stopping…" : "Stop"}
                  </button>
                </div>
                {stopping && (
                  <div className="update-progress-note">Finishing the current package, then stopping.</div>
                )}
                <div className="update-progress-track">
                  <div
                    className="update-progress-bar"
                    style={{
                      width: `${updateProgress && updateProgress.total > 0 ? Math.max(3, (updateProgress.done / updateProgress.total) * 100) : 3}%`,
                    }}
                  />
                </div>
              </div>
            ) : (
              <>
                <button
                  className="btn-primary"
                  onClick={selectedCount > 0 ? updateSelected : updateAll}
                  disabled={loading || scheduledRunning || (selectedCount === 0 && updatableCount === 0)}
                  title={scheduledRunning ? "A scheduled update is running; try again when it finishes" : undefined}
                >
                  {selectedCount > 0
                    ? `Update ${selectedCount} selected`
                    : filtering
                    ? `Update ${updatableCount} shown`
                    : `Update all${updatableCount > 0 ? ` (${updatableCount})` : ""}`}
                </button>
                {selectedCount > 0 && (
                  <button className="btn-ghost" onClick={() => setSelected({})}>
                    Clear
                  </button>
                )}
              </>
            )}
          </div>
        )}
      </div>

      <div className="footer" data-tauri-drag-region>
        <span>
          {lastChecked
            ? `Checked ${lastChecked.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`
            : "Not checked yet"}
        </span>
        {scheduleEnabled && (
          <span>
            {scheduledRunning ? "Scheduled update running…" : `Daily at ${formatClock(hour, minute)}`}
          </span>
        )}
        <button className="footer-keys" onClick={() => setShowHelp(true)} title="Keyboard shortcuts">
          ?
        </button>
      </div>

      {showHelp && (
        <div className="help-overlay" onClick={() => setShowHelp(false)}>
          <div className="help-card" onClick={(e) => e.stopPropagation()}>
            <div className="help-title">Keyboard</div>
            {SHORTCUTS.map((s) => (
              <div key={s.keys} className="help-row">
                <kbd>{s.keys}</kbd>
                <span>{s.what}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {showSettings && (
        <SettingsPanel
          onClose={() => setShowSettings(false)}
          greedy={greedy}
          notifyOnly={notifyOnly}
          onGreedy={applyGreedy}
          onNotifyOnly={applyNotifyOnly}
          scheduleEnabled={scheduleEnabled}
          scheduleLocked={loading}
          hour={hour}
          minute={minute}
          onToggleSchedule={(enabled) => void toggleSchedule(enabled)}
          onChangeTime={changeScheduleTime}
          scheduleError={scheduleError}
          scheduleConfirm={scheduleConfirm}
          scheduledCount={scheduledCount}
          onConfirmSchedule={() => void toggleSchedule(true, true)}
          onCancelSchedule={() => setScheduleConfirm(false)}
          lastRun={lastRun}
          ignored={ignored}
          onUnskip={unskip}
          onClearSkipped={clearSkipped}
          verifying={verifying}
          verifyPct={verifyPct}
          onAdopt={() => {
            setShowSettings(false);
            setShowAdopt(true);
          }}
          onVerify={() => void verifySignatures()}
          checkVulns={checkVulns}
          onCheckVulns={applyCheckVulns}
          snapshots={snapshots}
          onRestore={(token) => runResultAction({ type: "rollback", token })}
          launchAtLogin={launchAtLogin}
          onLaunchAtLogin={(v) => void applyLaunchAtLogin(v)}
          onShowHistory={() => void openHistory()}
          appVersion={appVersion}
          appUpdate={appUpdate}
          onCheckAppUpdate={checkAppUpdate}
          provenanceActive={provenanceActive}
          theme={theme}
          onTheme={applyTheme}
        />
      )}

      {showHistory && <HistoryPanel entries={history} onClose={() => setShowHistory(false)} />}

      {showAdopt && (
        <AdoptPanel
          query={adoptQuery}
          setQuery={setAdoptQuery}
          results={adoptResults}
          searching={adoptSearching}
          suggestions={suggestions}
          suggestLoading={suggestLoading}
          adoptSelected={adoptSelected}
          onToggleSelect={toggleAdoptSelect}
          onAdoptSelected={adoptSelectedApps}
          adopting={adopting}
          adoptProgress={adoptProgress}
          onCancelAdoption={() => void cancelAdoption()}
          onSearch={searchCasks}
          onAdopt={adopt}
          onClose={() => setShowAdopt(false)}
        />
      )}

      {results && (
        <ResultsPanel
          title={resultsTitle}
          results={results}
          totalMs={resultsMs}
          openLogs={openLogs}
          onToggleLog={(i) =>
            setOpenLogs((o) => ({ ...o, [i]: !o[i] }))
          }
          onAction={runResultAction}
          onClose={() => {
            setResults(null);
            setResultsMs(null);
            setOpenLogs({});
          }}
        />
      )}
    </div>
  );
}

function AdoptPanel({
  query,
  setQuery,
  results,
  searching,
  suggestions,
  suggestLoading,
  adoptSelected,
  onToggleSelect,
  onAdoptSelected,
  adopting,
  adoptProgress,
  onCancelAdoption,
  onSearch,
  onAdopt,
  onClose,
}: {
  query: string;
  setQuery: (q: string) => void;
  results: CaskSuggestion[];
  searching: boolean;
  suggestions: AdoptableApp[];
  suggestLoading: boolean;
  adoptSelected: Record<string, boolean>;
  onToggleSelect: (token: string) => void;
  onAdoptSelected: () => void;
  adopting: string[];
  adoptProgress: AdoptProgress | null;
  onCancelAdoption: () => void;
  onSearch: () => void;
  onAdopt: (token: string) => void;
  onClose: () => void;
}) {
  const selectedCount = Object.keys(adoptSelected).length;
  const busy = adopting.length > 0;
  const [infoOpen, setInfoOpen] = useState<string | null>(null);
  const [infoData, setInfoData] = useState<Record<string, CaskInfo>>({});
  const [infoLoading, setInfoLoading] = useState<string | null>(null);

  async function toggleInfo(token: string) {
    if (infoOpen === token) {
      setInfoOpen(null);
      return;
    }
    setInfoOpen(token);
    if (!(token in infoData)) {
      setInfoLoading(token);
      try {
        const info = await invoke<CaskInfo | null>("cask_info", { token });
        if (info) setInfoData((d) => ({ ...d, [token]: info }));
      } finally {
        setInfoLoading(null);
      }
    }
  }
  return (
    <div className="results-overlay">
      <div className="results-header" data-tauri-drag-region>
        <div>
          <div className="results-title">Adopt into Homebrew</div>
          <div className="results-summary">
            <span>bring a manually-installed app under brew, so it can update here</span>
          </div>
        </div>
        <button className="icon-btn" onClick={onClose} title="Done" data-tauri-drag-region="false">
          <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
          </svg>
        </button>
      </div>

      <div className="results-body">
        <div className="adopt-section-label">Suggested — installed apps Homebrew can manage</div>
        {suggestLoading && <div className="empty-msg">scanning /Applications…</div>}
        {!suggestLoading && suggestions.length === 0 && (
          <div className="empty-msg">no adoptable apps found</div>
        )}
        {suggestions.map((s) => (
          <label key={s.token} className="adopt-row adopt-row-select">
            <input
              type="checkbox"
              className="check"
              checked={adoptSelected[s.token] ?? false}
              onChange={() => onToggleSelect(s.token)}
              disabled={busy}
            />
            <span className="adopt-token">
              {s.name} <span className="adopt-token-sub">{s.token}</span>
            </span>
            {adopting.includes(s.token) && (
              <span className={`adopt-status${adoptProgress?.token === s.token && adoptProgress.stalled ? " stalled" : ""}`}>
                {adoptProgress?.token === s.token ? adoptStatusText(adoptProgress) : "Waiting…"}
              </span>
            )}
          </label>
        ))}
        {suggestions.length > 0 && (
          busy ? (
            <button className="btn-secondary adopt-selected-btn" onClick={onCancelAdoption}>
              Cancel adoption
            </button>
          ) : (
            <button
              className="btn-primary adopt-selected-btn"
              onClick={onAdoptSelected}
              disabled={selectedCount === 0}
            >
              {selectedCount > 0 ? `Adopt selected (${selectedCount})` : "Adopt selected"}
            </button>
          )
        )}

        <div className="adopt-section-label">Or search all casks</div>
        <form
          className="adopt-search"
          onSubmit={(e) => {
            e.preventDefault();
            onSearch();
          }}
        >
          <input
            className="adopt-input"
            placeholder="Search casks (e.g. chrome, slack, docker)…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <button className="btn-secondary adopt-go" type="submit" disabled={searching}>
            {searching ? "…" : "Search"}
          </button>
        </form>
        {searching && <div className="empty-msg">searching…</div>}
        {!searching && results.length === 0 && query.trim() !== "" && (
          <div className="empty-msg">no matching casks</div>
        )}
        {results.map((r) => (
          <div key={r.token} className="adopt-item">
            <div className="adopt-row">
              <span className="adopt-token">{r.token}</span>
              <div className="adopt-actions">
                <button
                  className={`info-btn ${infoOpen === r.token ? "active" : ""}`}
                  onClick={() => toggleInfo(r.token)}
                  title="Details"
                  aria-label="Details"
                >
                  <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.4">
                    <circle cx="8" cy="8" r="6.5" />
                    <path d="M8 7.2v3.4" strokeLinecap="round" />
                    <circle cx="8" cy="5" r="0.6" fill="currentColor" stroke="none" />
                  </svg>
                </button>
                {r.installed ? (
                  <span className="adopt-badge">managed</span>
                ) : (
                  <button
                    className="btn-secondary adopt-btn"
                    onClick={() => onAdopt(r.token)}
                    disabled={busy}
                  >
                    {adopting.includes(r.token) ? "Adopting…" : "Adopt"}
                  </button>
                )}
              </div>
            </div>
            {adopting.includes(r.token) && (
              <div className={`adopt-status adopt-status-row${adoptProgress?.token === r.token && adoptProgress.stalled ? " stalled" : ""}`}>
                {adoptProgress?.token === r.token ? adoptStatusText(adoptProgress) : "Waiting…"}
                <button className="link-btn" onClick={onCancelAdoption}>
                  Cancel
                </button>
              </div>
            )}
            {infoOpen === r.token && (
              <CaskDetail info={infoData[r.token]} loading={infoLoading === r.token} />
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

// What the running update is doing now: the current step with a ticking clock, and the last few
// finished steps above it with how long each took — so a 2-minute dependency install reads as
// progress, not a hang.
function LiveRun({ steps, now }: { steps: LiveStep[]; now: number }) {
  const { done, running } = liveView(steps, 3);
  if (!running && done.length === 0) return null;
  return (
    <div className="live-run">
      {done.map((s, i) => (
        <div key={`${s.key}-${s.startedAt}`} className="live-step done" style={{ opacity: 0.45 + 0.18 * i }}>
          <span className="live-mark">✓</span>
          <span className="live-label">{s.label}</span>
          <span className="live-time">{formatDuration(s.ms ?? 0)}</span>
        </div>
      ))}
      {running && (
        <div className="live-step running">
          <span className="live-mark live-pulse" />
          <span className="live-label">{running.label}</span>
          <span className="live-time">{formatElapsed(now - running.startedAt)}</span>
        </div>
      )}
    </div>
  );
}

function CaskDetail({ info, loading }: { info?: CaskInfo; loading: boolean }) {
  if (loading) return <div className="cask-detail">loading…</div>;
  if (!info) return <div className="cask-detail">no details available</div>;
  return (
    <div className="cask-detail">
      {info.desc && <div className="cask-desc">{info.desc}</div>}
      <div className="cask-meta">
        <span>v{info.version}</span>
        {info.auto_updates && <span>· self-updates</span>}
        {/^https?:\/\//.test(info.homepage) && (
          <button className="cask-link" onClick={() => invoke("open_url", { url: info.homepage })}>
            homepage ↗
          </button>
        )}
      </div>
    </div>
  );
}

