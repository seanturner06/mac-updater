import { useEffect, useState } from "react";
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
  type CategoryStatus,
  type Selection,
} from "./logic";
import "./App.css";

const IGNORE_STORE = "freshet.skipped";
function loadIgnored(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(IGNORE_STORE) ?? "[]"));
  } catch {
    return new Set();
  }
}

type UpdateResult = { kind: string; name: string; success: boolean; output: string };
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
  const [hour, setHour] = useState(10);
  const [minute, setMinute] = useState(0);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selected, setSelected] = useState<Record<string, Selection>>({});
  const [results, setResults] = useState<UpdateResult[] | null>(null);
  const [resultsTitle, setResultsTitle] = useState("Update results");
  const [openLogs, setOpenLogs] = useState<Record<number, boolean>>({});
  const [greedy, setGreedy] = useState(false);
  const [notifyOnly, setNotifyOnly] = useState(false);
  const [verifying, setVerifying] = useState(false);
  const [ignored, setIgnored] = useState<Set<string>>(loadIgnored);
  const [verifyProgress, setVerifyProgress] = useState<{ done: number; total: number }>({
    done: 0,
    total: 0,
  });
  const [sparkle, setSparkle] = useState<CategoryStatus | null>(null);
  const [sparkleLoading, setSparkleLoading] = useState(false);
  const [showAdopt, setShowAdopt] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [adoptQuery, setAdoptQuery] = useState("");
  const [adoptResults, setAdoptResults] = useState<CaskSuggestion[]>([]);
  const [adoptSearching, setAdoptSearching] = useState(false);
  const [adoptingToken, setAdoptingToken] = useState<string | null>(null);
  const [suggestions, setSuggestions] = useState<AdoptableApp[]>([]);
  const [suggestLoading, setSuggestLoading] = useState(false);
  const [adoptSelected, setAdoptSelected] = useState<Record<string, boolean>>({});
  const [batchAdopting, setBatchAdopting] = useState(false);

  useEffect(() => {
    checkNow();
    invoke<boolean>("is_schedule_installed").then(setScheduleEnabled);
  }, []);

  // Scan for adoptable apps each time the adopt panel opens.
  useEffect(() => {
    if (!showAdopt) return;
    setAdoptSelected({});
    setSuggestLoading(true);
    invoke<AdoptableApp[]>("suggest_adoptions")
      .then(setSuggestions)
      .catch(() => setSuggestions([]))
      .finally(() => setSuggestLoading(false));
  }, [showAdopt]);

  function persistIgnored(next: Set<string>) {
    setIgnored(next);
    localStorage.setItem(IGNORE_STORE, JSON.stringify([...next]));
  }
  function skipVersion(kind: string, id: string, latest: string) {
    const next = new Set(ignored);
    next.add(ignoreKey(kind, id, latest));
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
    } finally {
      setLoading(false);
    }
  }

  // Toggle the self-updating filter (no backend re-fetch). When turning it off, drop any
  // now-hidden self-updating items from the selection so they aren't silently updated.
  function applyGreedy(next: boolean) {
    setGreedy(next);
    if (!next) setSelected((prev) => prunedSelfUpdating(prev, categories));
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

  async function runUpdate(promise: Promise<UpdateResult[]>) {
    setUpdating(true);
    try {
      const res = await promise;
      showResults("Update results", res);
      await checkNow();
    } finally {
      setUpdating(false);
    }
  }

  async function verifySignatures() {
    setVerifying(true);
    setVerifyProgress({ done: 0, total: 0 });
    let unlisten = () => {};
    try {
      unlisten = await listen<{ done: number; total: number }>(
        "verify-progress",
        (e) => setVerifyProgress(e.payload)
      );
    } catch {
      // Progress events unavailable — verify anyway, just without the live ring.
    }
    try {
      const res = await invoke<UpdateResult[]>("verify_signatures");
      showResults("Signature check", res);
    } finally {
      unlisten();
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

  async function adopt(token: string) {
    setAdoptingToken(token);
    try {
      const res = await invoke<UpdateResult>("adopt_cask", { token });
      setShowAdopt(false);
      showResults("Homebrew adoption", [res]);
      await checkNow();
    } finally {
      setAdoptingToken(null);
    }
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
    const tokens = Object.keys(adoptSelected);
    if (tokens.length === 0) return;
    setBatchAdopting(true);
    try {
      const res = await invoke<UpdateResult[]>("adopt_casks", { tokens });
      setShowAdopt(false);
      showResults("Homebrew adoption", res);
      await checkNow();
    } finally {
      setBatchAdopting(false);
    }
  }

  async function updateAll() {
    await runUpdate(invoke<UpdateResult[]>("run_updates"));
  }

  async function updateSelected() {
    const selections = Object.values(selected);
    if (selections.length === 0) return;
    await runUpdate(invoke<UpdateResult[]>("run_selected_updates", { selections }));
  }

  async function toggleSchedule(enabled: boolean) {
    if (enabled) {
      await invoke("install_schedule", { hour, minute });
    } else {
      await invoke("uninstall_schedule");
    }
    setScheduleEnabled(enabled);
  }

  async function updateScheduleTime() {
    if (scheduleEnabled) {
      await invoke("install_schedule", { hour, minute });
    }
  }

  // The 4 local categories plus the separately-loaded "Other apps" (Sparkle) section. We always
  // show an "Other apps" entry so the user sees it loading; it's a placeholder until the fetch lands.
  const sparkleCategory: CategoryStatus =
    sparkle ?? { category: "Other apps", kind: "sparkle", items: [], error: null };
  const displayCategories = [...categories, sparkleCategory];

  const totalOutdated = countDisplayed(displayCategories, greedy, ignored);
  const selectedCount = Object.keys(selected).length;
  const allCurrent = !loading && !sparkleLoading && totalOutdated === 0;
  const verifyPct =
    verifyProgress.total > 0 ? verifyProgress.done / verifyProgress.total : 0;

  // Keep the menu-bar badge in sync with the visible update count.
  useEffect(() => {
    invoke("set_badge", { count: totalOutdated });
  }, [totalOutdated]);

  return (
    <div className="app">
      <div className="header">
        <div className="header-left">
          <div className={`status-dot ${allCurrent ? "ok" : totalOutdated > 0 ? "warn" : "idle"}`} />
          <div>
            <div className="title">Freshet</div>
            <div className="subtitle">
              {verifying
                ? `verifying signatures… ${Math.round(verifyPct * 100)}%`
                : loading
                ? "checking…"
                : allCurrent
                ? "everything is current"
                : `${totalOutdated} update${totalOutdated === 1 ? "" : "s"} available`}
            </div>
          </div>
        </div>
        <div className="header-actions">
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

      <div className="categories">
        {displayCategories.map((cat) => {
          const items = displayItems(cat, greedy, ignored);
          const itemKeys = items.map((i) => selKey(cat.kind, i.id));
          const selectedInCat = itemKeys.filter((k) => k in selected).length;
          const allInCatSelected = itemKeys.length > 0 && selectedInCat === itemKeys.length;
          return (
            <div key={cat.category} className="category">
              <div className="category-header">
                {items.length > 0 && !isReadOnly(cat.kind, notifyOnly) && (
                  <input
                    type="checkbox"
                    className="check cat-check"
                    checked={allInCatSelected}
                    ref={(el) => {
                      if (el) el.indeterminate = selectedInCat > 0 && !allInCatSelected;
                    }}
                    onChange={() => toggleCategory(cat)}
                    title="Select all in category"
                  />
                )}
                <button
                  className="category-toggle"
                  onClick={() =>
                    setExpanded((e) => ({ ...e, [cat.category]: !e[cat.category] }))
                  }
                >
                  <div className="category-name">
                    <span className={`pill ${items.length > 0 ? "pill-warn" : "pill-ok"}`}>
                      {items.length}
                    </span>
                    <span>{cat.category}</span>
                  </div>
                  <span className="chevron" style={{ transform: expanded[cat.category] ? "rotate(90deg)" : "" }}>
                    ›
                  </span>
                </button>
              </div>
              {expanded[cat.category] && (
                <div className="category-body">
                  {cat.error && <div className="error-msg">{cat.error}</div>}
                  {cat.kind === "sparkle" && sparkleLoading && (
                    <div className="empty-msg">checking…</div>
                  )}
                  {items.length === 0 && !cat.error && !(cat.kind === "sparkle" && sparkleLoading) && (
                    <div className="empty-msg">all current</div>
                  )}
                  {cat.kind === "sparkle" && items.length > 0 && (
                    <div className="empty-msg">these apps update themselves — open them to update</div>
                  )}
                  {items.map((item) => {
                    const versions = (
                      <span className="item-versions">
                        {item.current && <span className="ver-old">{item.current}</span>}
                        {item.current && <span className="arrow">→</span>}
                        <span className="ver-new">{item.latest}</span>
                      </span>
                    );
                    const rowActions = (
                      <span className="item-actions">
                        {item.notes_url && (
                          <button
                            className="item-link"
                            title="What's new"
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              invoke("open_url", { url: item.notes_url });
                            }}
                          >
                            ↗
                          </button>
                        )}
                        <button
                          className="item-skip"
                          title="Skip this version"
                          onClick={(e) => {
                            e.preventDefault();
                            e.stopPropagation();
                            skipVersion(cat.kind, item.id, item.latest);
                          }}
                        >
                          ✕
                        </button>
                      </span>
                    );
                    if (isReadOnly(cat.kind, notifyOnly)) {
                      return (
                        <div key={item.id} className="item item-readonly">
                          <span className="item-name">{item.name}</span>
                          {versions}
                          {rowActions}
                        </div>
                      );
                    }
                    return (
                      <label key={item.id} className="item">
                        <input
                          type="checkbox"
                          className="check item-check"
                          checked={selKey(cat.kind, item.id) in selected}
                          onChange={() => toggleItem(cat.kind, item.id, item.name)}
                        />
                        <span className="item-name">{item.name}</span>
                        {versions}
                        {rowActions}
                      </label>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </div>

      <div className="actions">
        {notifyOnly ? (
          <div className="notify-note">
            Notify-only — Freshet won't install anything. Update apps yourself.
          </div>
        ) : (
          <>
            <button
              className="btn-primary"
              onClick={updateSelected}
              disabled={updating || loading || selectedCount === 0}
            >
              {updating
                ? "updating…"
                : selectedCount > 0
                ? `Update selected (${selectedCount})`
                : "Update selected"}
            </button>
            <button
              className="btn-secondary"
              onClick={updateAll}
              disabled={updating || loading || totalOutdated === 0}
            >
              Update all
            </button>
          </>
        )}
      </div>


      <div className="footer">
        {lastChecked && (
          <span>last checked {lastChecked.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
        )}
      </div>

      {showSettings && (
        <div className="results-overlay">
          <div className="results-header">
            <div>
              <div className="results-title">Settings</div>
            </div>
            <button className="icon-btn" onClick={() => setShowSettings(false)} title="Done">
              <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.6">
                <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
              </svg>
            </button>
          </div>
          <div className="results-body">
            <div className="settings-label">Display</div>
            <div className="settings-row">
              <div className="settings-text">
                <div className="settings-title">Include self-updating apps</div>
                <div className="settings-sub">{greedy ? "showing apps that auto-update" : "hidden by default"}</div>
              </div>
              <label className="toggle">
                <input type="checkbox" checked={greedy} onChange={(e) => applyGreedy(e.target.checked)} />
                <span className="toggle-slider" />
              </label>
            </div>
            <div className="settings-row">
              <div className="settings-text">
                <div className="settings-title">Notify only</div>
                <div className="settings-sub">{notifyOnly ? "showing updates; won't install" : "updates can be installed"}</div>
              </div>
              <label className="toggle">
                <input type="checkbox" checked={notifyOnly} onChange={(e) => setNotifyOnly(e.target.checked)} />
                <span className="toggle-slider" />
              </label>
            </div>

            <div className="settings-label">Automatic updates</div>
            <div className="settings-row">
              <div className="settings-text">
                <div className="settings-title">Auto-update daily</div>
                <div className="settings-sub">
                  {scheduleEnabled
                    ? `runs at ${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`
                    : "off"}
                </div>
              </div>
              <label className="toggle">
                <input type="checkbox" checked={scheduleEnabled} onChange={(e) => toggleSchedule(e.target.checked)} />
                <span className="toggle-slider" />
              </label>
            </div>
            {scheduleEnabled && (
              <div className="time-picker">
                <select
                  value={hour}
                  onChange={(e) => {
                    setHour(parseInt(e.target.value));
                    setTimeout(updateScheduleTime, 0);
                  }}
                >
                  {Array.from({ length: 24 }, (_, i) => (
                    <option key={i} value={i}>{String(i).padStart(2, "0")}</option>
                  ))}
                </select>
                <span className="time-colon">:</span>
                <select
                  value={minute}
                  onChange={(e) => {
                    setMinute(parseInt(e.target.value));
                    setTimeout(updateScheduleTime, 0);
                  }}
                >
                  {[0, 15, 30, 45].map((m) => (
                    <option key={m} value={m}>{String(m).padStart(2, "0")}</option>
                  ))}
                </select>
              </div>
            )}

            <div className="settings-label">Tools</div>
            <button
              className="settings-link-row"
              onClick={() => {
                setShowSettings(false);
                setShowAdopt(true);
              }}
            >
              <span>Adopt an app into Homebrew</span>
              <span className="chevron">›</span>
            </button>
            <button
              className="settings-link-row"
              disabled={verifying}
              onClick={() => {
                setShowSettings(false);
                verifySignatures();
              }}
            >
              <span>{verifying ? "Verifying signatures…" : "Verify app signatures"}</span>
              <span className="chevron">›</span>
            </button>

            {ignored.size > 0 && (
              <>
                <div className="settings-label">Skipped versions</div>
                <div className="settings-row">
                  <div className="settings-text">
                    <div className="settings-title">{ignored.size} skipped</div>
                    <div className="settings-sub">hidden until a newer version ships</div>
                  </div>
                  <button className="btn-secondary adopt-btn" onClick={clearSkipped}>
                    Clear
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}

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
          batchAdopting={batchAdopting}
          adoptingToken={adoptingToken}
          onSearch={searchCasks}
          onAdopt={adopt}
          onClose={() => setShowAdopt(false)}
        />
      )}

      {results && (
        <ResultsPanel
          title={resultsTitle}
          results={results}
          openLogs={openLogs}
          onToggleLog={(i) =>
            setOpenLogs((o) => ({ ...o, [i]: !o[i] }))
          }
          onClose={() => {
            setResults(null);
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
  batchAdopting,
  adoptingToken,
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
  batchAdopting: boolean;
  adoptingToken: string | null;
  onSearch: () => void;
  onAdopt: (token: string) => void;
  onClose: () => void;
}) {
  const selectedCount = Object.keys(adoptSelected).length;
  const [infoOpen, setInfoOpen] = useState<string | null>(null);
  const [infoData, setInfoData] = useState<Record<string, CaskInfo>>({});
  const [infoLoading, setInfoLoading] = useState<string | null>(null);

  async function toggleInfo(token: string) {
    if (infoOpen === token) {
      setInfoOpen(null);
      return;
    }
    setInfoOpen(token);
    if (!infoData[token]) {
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
      <div className="results-header">
        <div>
          <div className="results-title">Adopt into Homebrew</div>
          <div className="results-summary">
            <span>bring a manually-installed app under brew, so it can update here</span>
          </div>
        </div>
        <button className="icon-btn" onClick={onClose} title="Done">
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
              checked={!!adoptSelected[s.token]}
              onChange={() => onToggleSelect(s.token)}
              disabled={batchAdopting}
            />
            <span className="adopt-token">
              {s.name} <span className="adopt-token-sub">{s.token}</span>
            </span>
          </label>
        ))}
        {suggestions.length > 0 && (
          <button
            className="btn-primary adopt-selected-btn"
            onClick={onAdoptSelected}
            disabled={batchAdopting || selectedCount === 0}
          >
            {batchAdopting
              ? "adopting…"
              : selectedCount > 0
              ? `Adopt selected (${selectedCount})`
              : "Adopt selected"}
          </button>
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
                    disabled={adoptingToken !== null}
                  >
                    {adoptingToken === r.token ? "adopting…" : "Adopt"}
                  </button>
                )}
              </div>
            </div>
            {infoOpen === r.token && (
              <CaskDetail info={infoData[r.token]} loading={infoLoading === r.token} />
            )}
          </div>
        ))}
      </div>
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

function ResultsPanel({
  title,
  results,
  openLogs,
  onToggleLog,
  onClose,
}: {
  title: string;
  results: UpdateResult[];
  openLogs: Record<number, boolean>;
  onToggleLog: (i: number) => void;
  onClose: () => void;
}) {
  const failed = results.filter((r) => !r.success).length;
  const succeeded = results.length - failed;

  return (
    <div className="results-overlay">
      <div className="results-header">
        <div>
          <div className="results-title">{title}</div>
          <div className="results-summary">
            <span className="ok-text">{succeeded} succeeded</span>
            {failed > 0 && <span className="fail-text">{failed} failed</span>}
          </div>
        </div>
        <button className="icon-btn" onClick={onClose} title="Done">
          <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
          </svg>
        </button>
      </div>

      <div className="results-body">
        {results.map((r, i) => {
          const open = !!openLogs[i];
          return (
            <div key={i} className={`result-row ${r.success ? "ok" : "fail"}`}>
              <button
                className="result-head"
                onClick={() => r.output && onToggleLog(i)}
                disabled={!r.output}
              >
                <span className={`result-badge ${r.success ? "ok" : "fail"}`}>
                  {r.success ? "✓" : "✕"}
                </span>
                <span className="result-name">{r.name}</span>
                {r.output && (
                  <span className="result-chevron" style={{ transform: open ? "rotate(90deg)" : "" }}>
                    ›
                  </span>
                )}
              </button>
              {open && r.output && <pre className="result-log">{r.output}</pre>}
            </div>
          );
        })}
      </div>

      <div className="results-footer">
        <button className="btn-primary" onClick={onClose}>
          Done
        </button>
      </div>
    </div>
  );
}

