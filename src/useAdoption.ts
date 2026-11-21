// Homebrew adoption: the suggestions and search behind the Adopt panel, and the adoption run
// itself. Lives above the panel, so leaving the panel doesn't lose a running adoption: progress
// keeps arriving and the header shows it.
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AdoptProgress, UpdateResult } from "./logic";
import type { AdoptableApp, CaskSuggestion } from "./AdoptPanel";

// `open`: the Adopt panel is showing. `onFinished`: an adoption ended, with its results.
export function useAdoption(open: boolean, onFinished: (results: UpdateResult[]) => Promise<void>) {
  const [adoptQuery, setAdoptQuery] = useState("");
  const [adoptResults, setAdoptResults] = useState<CaskSuggestion[]>([]);
  const [adoptSearching, setAdoptSearching] = useState(false);
  // Casks being adopted right now, and the live status of the current one.
  const [adopting, setAdopting] = useState<string[]>([]);
  const [adoptProgress, setAdoptProgress] = useState<AdoptProgress | null>(null);
  const [suggestions, setSuggestions] = useState<AdoptableApp[]>([]);
  const [suggestLoading, setSuggestLoading] = useState(false);
  const [adoptSelected, setAdoptSelected] = useState<Record<string, boolean>>({});

  // Scan for adoptable apps each time the adopt panel opens.
  useEffect(() => {
    if (!open) return;
    // The selection is kept: closing and reopening the panel mid-adoption must not lose track
    // of what's being adopted. It's cleared when an adoption finishes.
    setSuggestLoading(true);
    void invoke<AdoptableApp[]>("suggest_adoptions")
      .then(setSuggestions)
      .catch(() => {
        setSuggestions([]);
      })
      .finally(() => {
        setSuggestLoading(false);
      });
  }, [open]);

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

  async function runAdoption(tokens: string[]) {
    if (tokens.length === 0 || adopting.length > 0) return;
    setAdopting(tokens);
    setAdoptProgress(null);
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await listen<AdoptProgress>("adopt-progress", (e) => setAdoptProgress(e.payload));
    } catch {
      // No live status; the adoption still runs.
    }
    try {
      const res = await invoke<UpdateResult[]>("adopt_casks", { tokens });
      setAdoptSelected({});
      await onFinished(res);
    } finally {
      unlisten?.();
      setAdopting([]);
      setAdoptProgress(null);
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

  return {
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
    adopt: (token: string) => runAdoption([token]),
    adoptSelectedApps: () => runAdoption(Object.keys(adoptSelected)),
    cancelAdoption: () => invoke("cancel_adoption"),
    toggleAdoptSelect,
  };
}
