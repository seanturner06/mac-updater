// What changed on this Mac, and when: every package Freshet updated (or tried to), newest first.
import { formatDuration, groupHistoryByDay, OUTCOME_LABEL, type HistoryEntry } from "./logic";

export function HistoryPanel({ entries, onClose }: { entries: HistoryEntry[] | null; onClose: () => void }) {
  const days = entries ? groupHistoryByDay(entries, new Date()) : [];
  return (
    <div className="results-overlay settings">
      <div className="settings-header" data-tauri-drag-region>
        <button className="icon-btn" onClick={onClose} title="Back" aria-label="Back" data-tauri-drag-region="false">
          <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M10 3.5 5.5 8l4.5 4.5" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
        <div className="results-title">History</div>
      </div>
      <div className="settings-body">
        {entries === null ? (
          <div className="empty-msg">Loading…</div>
        ) : days.length === 0 ? (
          <div className="history-empty">
            <div className="srow-title">Nothing yet</div>
            <div className="srow-sub">Every update Freshet runs, by hand or on schedule, is recorded here.</div>
          </div>
        ) : (
          days.map((day) => (
            <section key={day.label}>
              <div className="settings-label">{day.label}</div>
              <div className="sgroup">
                {day.entries.map((e, i) => (
                  <div key={`${e.at}-${e.kind}-${e.id}-${i}`} className={`srow history-row ${e.outcome}`}>
                    <span className={`history-dot ${e.outcome}`} aria-hidden />
                    <span className="srow-text">
                      <span className="srow-title">
                        {e.name}
                        {e.source === "scheduled" && <span className="chip history-chip">scheduled</span>}
                      </span>
                      <span className="srow-sub">
                        {e.from && e.to ? (
                          <span className="mono">
                            {e.from} → {e.to}
                          </span>
                        ) : null}
                        {e.outcome !== "updated" && (
                          <span className={`history-outcome ${e.outcome}`}>
                            {e.from && e.to ? " · " : ""}
                            {OUTCOME_LABEL[e.outcome]}
                          </span>
                        )}
                      </span>
                    </span>
                    <span className="history-time">
                      {new Date(e.at * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}
                      {typeof e.elapsed_ms === "number" && (
                        <span className="history-took">{formatDuration(e.elapsed_ms)}</span>
                      )}
                    </span>
                  </div>
                ))}
              </div>
            </section>
          ))
        )}
      </div>
    </div>
  );
}
