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
  keepSelection,
  allDisplayedSelections,
  toSettings,
  scheduledSelections,
  splitDependencies,
  iconAppName,
  categorySummary,
  type CategoryStatus,
  type Kind,
  type OutdatedItem,
  type PackageInfo,
  type Selection,
  type Settings,
  type UpdateResult,
  type ResultAction,
  type VulnReport,
  type Snapshot,
  type UpdateProgress,
  type HistoryEntry,
  formatClock,
  isStale,
  progressText,
  advanceSteps,
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
import { AdoptPanel } from "./AdoptPanel";
import { StatusMark } from "./StatusMark";
import { LiveRun } from "./LiveRun";
import { loadTheme, saveTheme } from "./theme";
import { GREEDY_STORE, NOTIFY_STORE, loadFlag, loadIgnored, saveFlag, saveIgnored } from "./localSettings";
import { useAdoption } from "./useAdoption";
import { useAppUpdate } from "./useAppUpdate";
import { useSchedule } from "./useSchedule";
import { useScheduledRun } from "./useScheduledRun";
import "./App.css";

export default function App() {
  const [categories, setCategories] = useState<CategoryStatus[]>([]);
  const [loading, setLoading] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [lastChecked, setLastChecked] = useState<Date | null>(null);
  const refreshIfOlderThanRef = useRef<(maxAgeMs: number) => void>(() => undefined);
  // Counts checks, so only the newest one's answer is shown (see checkNow).
  const checkSeq = useRef(0);
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

  // What the daily scheduled run would install right now.
  const scheduledCount = scheduledSelections(categories, greedy, ignored).length;
  const {
    scheduleEnabled,
    scheduleError,
    scheduleConfirm,
    hour,
    minute,
    toggleSchedule,
    changeScheduleTime,
    cancelScheduleConfirm,
  } = useSchedule(!notifyOnly && scheduledCount > 0);
  // When a scheduled run finishes, refresh, so the window never lists what it already installed.
  const { lastRun, scheduledRunning } = useScheduledRun(() => void checkNow());
  const { appVersion, appUpdate, appUpdateState, checkAppUpdate, installAppUpdate } = useAppUpdate();
  const {
    adoptQuery,
    setAdoptQuery,
    adoptResults,
    adoptSearching,
    adopting,
    adoptProgress,
    suggestions,
    suggestLoading,
    adoptSelected,
    searchCasks,
    adopt,
    adoptSelectedApps,
    cancelAdoption,
    toggleAdoptSelect,
  } = useAdoption(showAdopt, async (res) => {
    setShowAdopt(false);
    showResults("Homebrew adoption", res);
    await checkNow();
  });

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
    void invoke<boolean>("get_launch_at_login").then(setLaunchAtLogin).catch(() => undefined);
    void invoke<boolean>("homebrew_provenance_active").then(setProvenanceActive).catch(() => undefined);
    void checkNow();
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

  // Takes every value explicitly: state set in the same handler isn't visible yet.
  function saveSettings(g: boolean, n: boolean, ig: Set<string>, cv: boolean = checkVulns) {
    saveFlag(GREEDY_STORE, g);
    saveFlag(NOTIFY_STORE, n);
    saveIgnored(ig);
    void invoke("save_settings", { settings: toSettings(g, n, ig, cv) }).catch(() => {
      // Not fatal for the app itself; the scheduled run falls back to its last-saved settings.
    });
  }

  function persistIgnored(next: Set<string>) {
    setIgnored(next);
    saveSettings(greedy, notifyOnly, next);
  }
  function skipVersion(kind: Kind, id: string, latest: string) {
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
    // Checks can overlap: startup, a scheduled run ending, the window regaining focus, the end of
    // an update. Only the newest one's answer is shown, so an older, slower check can't land
    // last and put a stale list (say, from before an update) back on screen.
    const seq = ++checkSeq.current;
    const newest = () => seq === checkSeq.current;
    setLoading(true);
    // Sparkle is network-bound — load it in the background so it never blocks the local list.
    setSparkleLoading(true);
    invoke<CategoryStatus>("check_sparkle_apps")
      .then((s) => {
        if (newest()) setSparkle(s);
      })
      .catch(() => {
        if (newest()) setSparkle({ category: "Other apps", kind: "sparkle", items: [], error: "check failed" });
      })
      .finally(() => {
        if (newest()) setSparkleLoading(false);
      });
    try {
      const result = await invoke<CategoryStatus[]>("check_outdated");
      if (!newest()) return;
      setCategories(result);
      setSelected((prev) => keepSelection(prev, result));
      setLastChecked(new Date());
      void loadDetails(result);
    } catch {
      // IPC failure (should be rare — the command itself never rejects). Keep the
      // previous list rather than blanking the window.
    } finally {
      if (newest()) setLoading(false);
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
    if (action.type === "allow_app_management") {
      await invoke("open_app_management_settings");
      return "Opened System Settings. Turn Freshet on, then try again.";
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
    const ids = (kind: Kind) => cats.find((c) => c.kind === kind)?.items.map((i) => i.id) ?? [];
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

  function toggleItem(kind: Kind, id: string, name: string) {
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
  // The timer is set up once and reads the latest state through a ref. Set up in an effect that
  // re-ran on every render, it was torn down and restarted each time the window re-rendered —
  // at least every 30s, by the scheduled-run poll below — so its 10 minutes never elapsed.
  useEffect(() => {
    refreshIfOlderThanRef.current = (maxAgeMs: number) => {
      if (!loading && !updating && isStale(lastChecked, new Date(), maxAgeMs)) void checkNow();
    };
  });
  useEffect(() => {
    const timer = window.setInterval(() => refreshIfOlderThanRef.current(3 * 3_600_000), 10 * 60_000);
    const onShow = () => {
      if (document.visibilityState === "visible") refreshIfOlderThanRef.current(30 * 60_000);
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

  // A panel covers the whole window, not just the list: with every section collapsed the list is
  // short, and Settings was squashed into the top half of the window.
  const panelOpen = showSettings || showHistory || showAdopt || results !== null;

  return (
    <div className={`app${panelOpen ? " panel-open" : ""}`}>
      <div className="header" data-tauri-drag-region>
        <div className="header-left">
          <StatusMark state={allCurrent ? "ok" : totalOutdated > 0 ? "warn" : "idle"} />
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
          onCancelSchedule={cancelScheduleConfirm}
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


