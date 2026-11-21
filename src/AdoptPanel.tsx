// The Adopt panel: apps Homebrew could manage (suggested from /Applications, or found by search),
// and bringing them under it. The state and the adoption run live in useAdoption.
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { adoptStatusText, type AdoptProgress } from "./logic";

export type CaskSuggestion = { token: string; installed: boolean };
export type AdoptableApp = { name: string; token: string };
type CaskInfo = {
  token: string;
  name: string;
  desc: string;
  homepage: string;
  version: string;
  auto_updates: boolean;
};

export function AdoptPanel({
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
