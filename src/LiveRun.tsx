import { formatDuration, formatElapsed, liveView, type LiveStep } from "./logic";

// What the running update is doing now: the current step with a ticking clock, and the last few
// finished steps above it with how long each took — so a 2-minute dependency install reads as
// progress, not a hang.
export function LiveRun({ steps, now }: { steps: LiveStep[]; now: number }) {
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
          <span className="live-mark">›</span>
          <span className="live-label">{running.label}</span>
          <span className="live-time">{formatElapsed(now - running.startedAt)}</span>
        </div>
      )}
    </div>
  );
}
