// Outcome of an update (or a signature scan): one row per item with its log, how long it took,
// and a follow-up action where one applies — trust a changed publisher, or roll an app back.
import { useState } from "react";
import { formatDuration, type ResultAction, type UpdateResult } from "./logic";

type ActionState = { status: "busy" | "done" | "error"; message: string };

function actionLabel(a: ResultAction): string {
  if (a.type === "trust_signer") return "Trust new publisher";
  if (a.type === "quit_and_update") return `Quit & update`;
  return "Roll back";
}

function busyLabel(a: ResultAction): string {
  if (a.type === "trust_signer") return "Trusting…";
  if (a.type === "quit_and_update") return "Quitting & updating…";
  return "Restoring…";
}

function actionHint(a: ResultAction): string {
  if (a.type === "trust_signer") return `Only if you know why Team ${a.team} now signs this app.`;
  if (a.type === "quit_and_update") return `${a.app.replace(/\.app$/, "")} will be asked to quit, as with ⌘Q.`;
  return "The previous version was saved before updating.";
}

export function ResultsPanel({
  title,
  results,
  totalMs,
  openLogs,
  onToggleLog,
  onAction,
  onClose,
}: {
  title: string;
  results: UpdateResult[];
  totalMs: number | null;
  openLogs: Record<number, boolean>;
  onToggleLog: (i: number) => void;
  // Resolves to a message to show in place of the button; rejects with an error message.
  onAction: (action: ResultAction) => Promise<string>;
  onClose: () => void;
}) {
  const [actions, setActions] = useState<Record<number, ActionState>>({});
  const skipped = results.filter((r) => r.skipped).length;
  const failed = results.filter((r) => !r.success && !r.skipped).length;
  const succeeded = results.length - failed - skipped;

  async function run(i: number, action: ResultAction) {
    setActions((a) => ({ ...a, [i]: { status: "busy", message: "" } }));
    try {
      const message = await onAction(action);
      setActions((a) => ({ ...a, [i]: { status: "done", message } }));
    } catch (e) {
      setActions((a) => ({ ...a, [i]: { status: "error", message: String(e) } }));
    }
  }

  return (
    <div className="results-overlay">
      <div className="results-header" data-tauri-drag-region>
        <div>
          <div className="results-title">{title}</div>
          <div className="results-summary">
            <span className="ok-text">{succeeded} succeeded</span>
            {failed > 0 && <span className="fail-text">{failed} failed</span>}
            {skipped > 0 && <span className="muted-text">{skipped} skipped</span>}
            {totalMs !== null && <span className="muted-text">in {formatDuration(totalMs)}</span>}
          </div>
        </div>
        <button className="icon-btn" onClick={onClose} title="Done" data-tauri-drag-region="false">
          <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M4 4l8 8M12 4l-8 8" strokeLinecap="round" />
          </svg>
        </button>
      </div>

      <div className="results-body">
        {results.map((r, i) => {
          const open = openLogs[i] ?? false;
          const state = actions[i] as ActionState | undefined;
          const warn = r.action?.type === "trust_signer";
          const tone = r.success ? "ok" : r.skipped ? "skip" : warn ? "warn" : "fail";
          return (
            <div key={i} className={`result-row ${tone}`}>
              <button className="result-head" onClick={() => r.output && onToggleLog(i)} disabled={!r.output}>
                <span className={`result-badge ${tone}`}>
                  {{ ok: "✓", skip: "–", warn: "!", fail: "✕" }[tone]}
                </span>
                <span className="result-name">{r.name}</span>
                {r.elapsed_ms !== undefined && <span className="result-time">{formatDuration(r.elapsed_ms)}</span>}
                {r.output && (
                  <span className="result-chevron" style={{ transform: open ? "rotate(90deg)" : "" }}>
                    ›
                  </span>
                )}
              </button>
              {open && r.output && <pre className="result-log">{r.output}</pre>}
              {r.action && (
                <div className="result-action">
                  {state?.status === "done" ? (
                    <span className="result-action-msg ok-text">{state.message}</span>
                  ) : state?.status === "error" ? (
                    <span className="result-action-msg fail-text">{state.message}</span>
                  ) : (
                    <>
                      <span className="result-action-msg">{actionHint(r.action)}</span>
                      <button
                        className={`btn-ghost btn-compact${warn ? " btn-warn" : ""}`}
                        disabled={state?.status === "busy"}
                        onClick={() => {
                          if (r.action) void run(i, r.action);
                        }}
                      >
                        {state?.status === "busy" ? busyLabel(r.action) : actionLabel(r.action)}
                      </button>
                    </>
                  )}
                </div>
              )}
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
