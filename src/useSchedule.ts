// The daily schedule (a launchd job): whether it's installed, at what time, and changing either.
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type ScheduleStatus = { installed: boolean; hour: number | null; minute: number | null };

// `firstRunInstalls`: turning the schedule on now would install something at its first run
// (updates are waiting and notify-only is off), so enabling is confirmed first.
export function useSchedule(firstRunInstalls: boolean) {
  const [scheduleEnabled, setScheduleEnabled] = useState(false);
  const [scheduleError, setScheduleError] = useState<string | null>(null);
  const [scheduleConfirm, setScheduleConfirm] = useState(false);
  const [hour, setHour] = useState(10);
  const [minute, setMinute] = useState(0);
  const saveTimer = useRef<number | undefined>(undefined);

  // Reflect the schedule that's actually installed (launchd plist), not defaults.
  useEffect(() => {
    void invoke<ScheduleStatus>("get_schedule")
      .then((s) => {
        setScheduleEnabled(s.installed);
        if (s.hour !== null) setHour(s.hour);
        if (s.minute !== null) setMinute(s.minute);
      })
      .catch(() => {
        setScheduleEnabled(false);
      });
  }, []);

  // Enabling is confirmed first when the first run would install something, so nobody is
  // surprised by dozens of upgrades the next morning.
  async function toggleSchedule(enabled: boolean, confirmed = false) {
    if (enabled && !confirmed && firstRunInstalls) {
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

  return {
    scheduleEnabled,
    scheduleError,
    scheduleConfirm,
    hour,
    minute,
    toggleSchedule,
    changeScheduleTime,
    cancelScheduleConfirm: () => setScheduleConfirm(false),
  };
}
