// A stand-in for Freshet's Rust side, so the real app (src/main.tsx) can run inside the demo film
// with believable data. Each film scene loads app.html?p=<preset>; the film drives it through
// window.__demo (events, key presses, clicks). Nothing here talks to a real system.
(() => {
  const preset = new URLSearchParams(location.search).get("p") ?? "hero";

  // The film's theme: set where the app reads it (src/theme.ts) before the app loads.
  try {
    localStorage.setItem("freshet.theme", (window.parent && window.parent.FILM_THEME) || "graphite");
  } catch {
    // Default theme instead.
  }

  // Film time drives the app's clocks (the live run view), so a frame-by-frame render shows the
  // same seconds a viewer would see.
  performance.now = () => ((window.parent && window.parent.__filmT) || 0) * 1000;

  const item = (id, current, latest, name = id) => ({ id, name, current, latest, self_updating: false, notes_url: "" });
  const CATEGORIES = [
    {
      category: "Homebrew",
      kind: "brew",
      error: null,
      items: [
        item("awscli", "2.37.12_2", "2.38.5"),
        item("git", "2.55.0", "2.56.0"),
        item("node", "24.9.0", "24.11.1"),
        item("ffmpeg", "7.1.2", "8.0"),
      ],
    },
    {
      category: "Apps",
      kind: "cask",
      error: null,
      items: [
        item("visual-studio-code", "1.105.1", "1.106.2", "Visual Studio Code"),
        item("docker-desktop", "4.49.0", "4.51.0", "Docker"),
        item("figma", "125.9.10", "125.10.4", "Figma"),
        item("postman", "11.70.2", "11.72.0", "Postman"),
        item("zoom", "6.6.0", "6.6.2", "Zoom"),
      ],
    },
    { category: "App Store", kind: "mas", error: null, items: [item("497799835", "26.1", "26.2", "Xcode")] },
    {
      category: "npm",
      kind: "npm",
      error: null,
      items: [item("pnpm", "10.18.0", "10.20.0"), item("typescript", "5.9.2", "5.9.3")],
    },
  ];
  const info = (desc, extra = {}) => ({
    desc,
    homepage: "https://example.com",
    license: "",
    dependency: false,
    auto_updates: false,
    release_notes: "https://example.com/releases",
    app: "",
    on_disk_version: "",
    ...extra,
  });
  const DETAILS = {
    "brew::awscli": info("Official Amazon AWS command-line interface", { license: "Apache-2.0" }),
    "brew::git": info("Distributed revision control system", { license: "GPL-2.0-only" }),
    "brew::node": info("Open-source, cross-platform JavaScript runtime environment", { license: "MIT" }),
    "brew::ffmpeg": info("Play, record, convert, and stream audio and video", { license: "GPL-2.0-or-later" }),
    "cask::visual-studio-code": info("Open-source code editor", { app: "Visual Studio Code.app" }),
    "cask::docker-desktop": info("App to build and share containerised applications", { app: "Docker.app" }),
    "cask::figma": info("Collaborative team software", { app: "Figma.app" }),
    "cask::postman": info("Collaboration platform for API development", { app: "Postman.app" }),
    "cask::zoom": info("Video communication and virtual meeting platform", { app: "zoom.us.app" }),
  };
  const ICONS = {
    "Visual Studio Code.app": "Visual Studio Code",
    "Docker.app": "Docker",
    "Figma.app": "Figma",
    "Postman.app": "Postman",
    "zoom.us.app": "zoom.us",
    "Xcode.app": "Xcode",
  };
  async function icon(app) {
    const file = ICONS[app];
    if (!file) return null;
    const blob = await (await fetch(`/demo/assets/icons/${encodeURIComponent(file)}.png`)).blob();
    return new Promise((resolve) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.readAsDataURL(blob);
    });
  }

  // The awscli upgrade from the real log, as the results panel shows it.
  const AWSCLI_STEPS = [
    { label: "Upgrading awscli", ms: 900 },
    { label: "Upgrading awscli dependency: ca-certificates (2026-09-25 -> 2026-10-20)", ms: 500 },
    { label: "Pouring ca-certificates", ms: 1500 },
    { label: "Installing awscli dependency: python@3.15 (3.15.1)", ms: 1000 },
    { label: "Pouring python@3.15", ms: 41000 },
    { label: "Installing awscli", ms: 500 },
    { label: "Pouring awscli", ms: 54500 },
    { label: "Cleanup", ms: 18000 },
  ];

  const listeners = {};
  const callbacks = {};
  let nextId = 1;
  let held = null;

  function results(selections) {
    return selections.map((s) => ({
      kind: s.kind,
      name: s.name,
      success: true,
      output: `==> Upgrading ${s.id}\n🍺  installed ${s.id}`,
      elapsed_ms: s.id === "awscli" ? 118100 : 2400 + (s.id.length % 5) * 900,
      ...(s.id === "awscli" ? { steps: AWSCLI_STEPS } : {}),
    }));
  }

  window.__TAURI_INTERNALS__ = {
    transformCallback(cb) {
      const id = nextId++;
      callbacks[id] = cb;
      return id;
    },
    unregisterCallback(id) {
      delete callbacks[id];
    },
    convertFileSrc: (s) => s,
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
    async invoke(cmd, args) {
      switch (cmd) {
        case "plugin:event|listen":
          (listeners[args.event] ||= []).push(args.handler);
          return args.handler;
        case "plugin:event|unlisten":
          return null;
        case "check_outdated":
          return CATEGORIES;
        case "check_sparkle_apps":
          return { category: "Other apps", kind: "sparkle", items: [], error: null };
        case "package_details":
          return DETAILS;
        case "app_icon":
          return icon(args.app);
        case "get_settings":
          return { greedy: false, notify_only: false, skipped: [], check_vulnerabilities: true };
        case "check_vulnerabilities":
          return {};
        case "get_schedule":
          return { installed: true, hour: 10, minute: 0 };
        case "get_scheduled_state":
          return { running: false, last: LAST_RUN };
        case "get_last_scheduled_run":
          return LAST_RUN;
        case "list_snapshots":
          return [];
        case "app_version":
          return "1.0.0";
        case "get_launch_at_login":
          return true;
        case "homebrew_provenance_active":
          return true;
        case "run_selected_updates":
          if (preset === "live") {
            return new Promise((resolve) => {
              held = () => resolve(results(args.selections));
            });
          }
          return new Promise((resolve) => setTimeout(() => resolve(results(args.selections)), 250));
        case "verify_updated_casks":
          if (preset !== "security") return [];
          return [
            {
              kind: "cask",
              name: "Zoom",
              success: false,
              output:
                "Not installed: zoom.us.app is signed by a different publisher.\nExpected Team BJ4HAAB9B3 (Zoom Communications), got EVILEVIL99.",
              action: { type: "trust_signer", bundle_id: "us.zoom.xos", team: "EVILEVIL99" },
            },
          ];
        default:
          return null;
      }
    },
  };

  const LAST_RUN = {
    finished_at: Math.floor(new Date(2026, 10, 14, 10, 2).getTime() / 1000),
    notify_only: false,
    available: 9,
    updated: ["git", "node", "awscli", "visual-studio-code", "docker-desktop", "figma", "postman", "pnpm", "typescript"],
    needs_admin: [],
    failed: [],
    signer_changed: [],
    blocked: [],
  };

  const byText = (text) =>
    [...document.querySelectorAll("button, label, span, div")].find((el) => el.textContent.trim() === text);

  // The film's remote control for this frame.
  window.__demo = {
    emit(event, payload) {
      for (const id of listeners[event] || []) callbacks[id]?.({ event, payload, id: 0 });
    },
    release() {
      held?.();
      held = null;
    },
    key(key, meta = false) {
      window.dispatchEvent(new KeyboardEvent("keydown", { key, metaKey: meta, bubbles: true, cancelable: true }));
    },
    click(selectorOrText) {
      const el = document.querySelector(selectorOrText) || byText(selectorOrText);
      el?.click();
    },
    theme(id) {
      document.documentElement.dataset.theme = id;
    },
  };
})();
