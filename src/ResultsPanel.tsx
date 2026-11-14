// Outcome of an update (or a signature scan): one row per item with its log, how long it took,
// and a follow-up action where one applies — trust a changed publisher, or roll an app back.
import { useState } from "react";
import {
  formatDuration,
  slowestStep,
  slowSummary,
  stepBreakdown,
  type ResultAction,
  type Step,
  type UpdateResult,
} from "./logic";

type ActionState = { status: "busy" | "done" | "error"; message: string };

function actionLabel(a: ResultAction): string {
  if (a.type === "trust_signer") return "Trust new publisher";
  if (a.type === "quit_and_update") return `Quit & update`;
  if (a.type === "allow_npm_scripts") return "Allow install scripts";
  return "Roll back";
}

function busyLabel(a: ResultAction): string {
  if (a.type === "trust_signer") return "Trusting…";
  if (a.type === "quit_and_update") return "Quitting & updating…";
  if (a.type === "allow_npm_scripts") return "Installing…";
  return "Restoring…";
}

function actionHint(a: ResultAction): string {
  if (a.type === "trust_signer") return `Only if you know why Team ${a.team} now signs this app.`;
  if (a.type === "quit_and_update") return `${a.app.replace(/\.app$/, "")} will be asked to quit, as with ⌘Q.`;
  if (a.type === "allow_npm_scripts")
    return `Only if you trust ${a.name} ${a.version}: its scripts run with your account's full access.`;
  return "The previous version was saved before updating.";
}

// Where an item's time went: one bar per step that mattered, the slowest highlighted.
function StepTimes({ steps }: { steps: Step[] }) {
  const { shown, otherMs } = stepBreakdown(steps);
  const max = Math.max(...steps.map((s) => s.ms), 1);
  const slowest = slowestStep(steps);
  return (
    <div className="step-times">
      {shown.map((s, i) => (
        <div key={i} className={`step-row${s === slowest ? " slowest" : ""}`} title={s.label}>
          <span className="step-label">
            {s.label}
            {s.shared && <span className="step-shared">whole batch</span>}
          </span>
          <span className="step-bar">
            <span style={{ width: `${Math.max(2, (s.ms / max) * 100)}%` }} />
          </span>
          <span className="step-ms">{formatDuration(s.ms)}</span>
        </div>
      ))}
      {otherMs >= 100 && (
        <div className="step-row other">
          <span className="step-label">Everything else</span>
          <span className="step-bar" />
          <span className="step-ms">{formatDuration(otherMs)}</span>
        </div>
      )}
    </div>
  );
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
              {!open && slowSummary(r) && <div className="result-sub">{slowSummary(r)}</div>}
              {open && r.steps && r.steps.length > 0 && <StepTimes steps={r.steps} />}
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
