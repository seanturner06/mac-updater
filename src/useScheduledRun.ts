// Notices scheduled (launchd) runs: polls a tiny status file every 30s and whenever the window is
// shown. While one runs, the window says so (and holds off manual updates); when one finishes,
// `onRunFinished` is called so the list can be refreshed — the window never shows updates the
// schedule already installed.
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ScheduledRunSummary } from "./logic";

export function useScheduledRun(onRunFinished: () => void): {
  lastRun: ScheduledRunSummary | null;
  // A scheduled run is in progress right now.
  scheduledRunning: boolean;
} {
  const [lastRun, setLastRun] = useState<ScheduledRunSummary | null>(null);
  const [scheduledRunning, setScheduledRunning] = useState(false);
  // finished_at of the newest scheduled run this window has already refreshed for.
  const lastRunSeen = useRef<number | null>(null);
  // The poll is set up once; it reaches the latest callback through this.
  const onRunFinishedRef = useRef(onRunFinished);
  useEffect(() => {
    onRunFinishedRef.current = onRunFinished;
  });

  useEffect(() => {
    void invoke<ScheduledRunSummary | null>("get_last_scheduled_run")
      .then(setLastRun)
      .catch(() => setLastRun(null));
    const poll = () => {
      invoke<{ running: boolean; last: ScheduledRunSummary | null }>("get_scheduled_state")
        .then(({ running, last }) => {
          setScheduledRunning(running);
          if (!last) return;
          if (lastRunSeen.current !== null && last.finished_at > lastRunSeen.current) {
            onRunFinishedRef.current();
          }
          lastRunSeen.current = last.finished_at;
          // Same run as before: keep the object we have, so a poll doesn't re-render the window.
          setLastRun((cur) => (cur?.finished_at === last.finished_at ? cur : last));
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

  return { lastRun, scheduledRunning };
}
