// Freshet updating itself: its version, whether a newer one is out, and installing it.
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppUpdate } from "./logic";

export function useAppUpdate() {
  const [appVersion, setAppVersion] = useState("");
  const [appUpdate, setAppUpdate] = useState<AppUpdate | null>(null);
  const [appUpdateState, setAppUpdateState] = useState<{ busy: boolean; error: string | null }>({
    busy: false,
    error: null,
  });

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

  // At launch, and — a menu-bar app stays open for days — once a day after that.
  useEffect(() => {
    void invoke<string>("app_version").then(setAppVersion).catch(() => undefined);
    const check = () => {
      invoke<AppUpdate | null>("check_app_update")
        .then(setAppUpdate)
        .catch(() => undefined);
    };
    check();
    const timer = window.setInterval(check, 24 * 3_600_000);
    return () => window.clearInterval(timer);
  }, []);

  return { appVersion, appUpdate, appUpdateState, checkAppUpdate, installAppUpdate };
}
