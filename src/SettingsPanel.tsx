// The settings sheet: the daily schedule first (it's the one setting that acts on its own), then
// how updates behave, skipped versions, and tools.
import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import {
  formatClock,
  hourLabel,
  lastRunText,
  nextRunText,
  parseSkipped,
  type AppUpdate,
  type ScheduledRunSummary,
  type Snapshot,
} from "./logic";

const pad = (n: number) => String(n).padStart(2, "0");

function Glyph({ children }: { children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width="15"
      height="15"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {children}
    </svg>
  );
}

const icons = {
  clock: (
    <Glyph>
      <circle cx="8" cy="8" r="6.2" />
      <path d="M8 4.6V8l2.3 1.5" />
    </Glyph>
  ),
  bell: (
    <Glyph>
      <path d="M4 11.5V7.2a4 4 0 0 1 8 0v4.3l1 1H3z" />
      <path d="M6.6 14h2.8" />
    </Glyph>
  ),
  refresh: (
    <Glyph>
      <path d="M13.2 6.2A5.3 5.3 0 0 0 3.4 5M2.8 9.8a5.3 5.3 0 0 0 9.8 1.2" />
      <path d="M3.2 2.6V5.2h2.6M12.8 13.4v-2.6h-2.6" />
    </Glyph>
  ),
  skip: (
    <Glyph>
      <path d="M3 3.5l5 4.5-5 4.5zM9 3.5l5 4.5-5 4.5z" />
    </Glyph>
  ),
  adopt: (
    <Glyph>
      <path d="M8 2v7.5M5 6.8 8 9.8l3-3" />
      <path d="M2.5 10.5v1.8A1.7 1.7 0 0 0 4.2 14h7.6a1.7 1.7 0 0 0 1.7-1.7v-1.8" />
    </Glyph>
  ),
  lock: (
    <Glyph>
      <rect x="3" y="7" width="10" height="7" rx="1.6" />
      <path d="M5.3 7V5.2a2.7 2.7 0 0 1 5.4 0V7" />
    </Glyph>
  ),
  bug: (
    <Glyph>
      <ellipse cx="8" cy="9.2" rx="3.4" ry="4.3" />
      <path d="M8 5v8.5M4.6 7.5 2.5 6.6M11.4 7.5l2.1-.9M4.6 10.8l-2.1.9M11.4 10.8l2.1.9M6.2 4.9 5.2 3.4M9.8 4.9l1-1.5" />
    </Glyph>
  ),
  undo: (
    <Glyph>
      <path d="M5 4.5 2.5 7 5 9.5" />
      <path d="M2.8 7h6.7a3.8 3.8 0 0 1 0 7.5H7" />
    </Glyph>
  ),
  power: (
    <Glyph>
      <path d="M8 2v5.5" />
      <path d="M4.6 4.2a5.2 5.2 0 1 0 6.8 0" />
    </Glyph>
  ),
  history: (
    <Glyph>
      <path d="M2.6 8a5.4 5.4 0 1 0 1.6-3.8" />
      <path d="M2.4 2.6v2.6H5" />
      <path d="M8 5.2V8l2 1.3" />
    </Glyph>
  ),
  shield: (
    <Glyph>
      <path d="M8 1.8 13 3.8v4c0 3.1-2.1 5.4-5 6.4-2.9-1-5-3.3-5-6.4v-4z" />
      <path d="M5.8 8.1l1.6 1.6 2.9-3" />
    </Glyph>
  ),
};

function Toggle({
  checked,
  disabled,
  onChange,
  label,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (v: boolean) => void;
  label: string;
}) {
  return (
    <label className={`toggle${disabled ? " disabled" : ""}`}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        aria-label={label}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="toggle-slider" />
    </label>
  );
}

function Row({
  icon,
  title,
  sub,
  children,
  onClick,
  disabled,
}: {
  icon: ReactNode;
  title: string;
  sub: ReactNode;
  children?: ReactNode;
  onClick?: () => void;
  disabled?: boolean;
}) {
  const body = (
    <>
      <span className="srow-icon">{icon}</span>
      <span className="srow-text">
        <span className="srow-title">{title}</span>
        <span className="srow-sub">{sub}</span>
      </span>
      {children}
    </>
  );
  return onClick ? (
    <button className="srow srow-button" onClick={onClick} disabled={disabled}>
      {body}
      <span className="chevron">›</span>
    </button>
  ) : (
    <div className="srow">{body}</div>
  );
}

export type SettingsPanelProps = {
  onClose: () => void;
  // behaviour
  greedy: boolean;
  notifyOnly: boolean;
  onGreedy: (v: boolean) => void;
  onNotifyOnly: (v: boolean) => void;
  // schedule
  scheduleEnabled: boolean;
  scheduleLocked: boolean;
  hour: number;
  minute: number;
  onToggleSchedule: (enabled: boolean) => void;
  onChangeTime: (hour: number, minute: number) => void;
  scheduleError: string | null;
  scheduleConfirm: boolean;
  scheduledCount: number;
  onConfirmSchedule: () => void;
  onCancelSchedule: () => void;
  lastRun: ScheduledRunSummary | null;
  // skipped versions
  ignored: Set<string>;
  onUnskip: (key: string) => void;
  onClearSkipped: () => void;
  // tools
  verifying: boolean;
  verifyPct: number;
  onAdopt: () => void;
  onVerify: () => void;
  // security
  checkVulns: boolean;
  onCheckVulns: (v: boolean) => void;
  snapshots: Snapshot[];
  onRestore: (token: string) => Promise<string>;
  // general
  launchAtLogin: boolean;
  onLaunchAtLogin: (v: boolean) => void;
  onShowHistory: () => void;
  appVersion: string;
  appUpdate: AppUpdate | null;
  // Resolves to a status line ("Freshet is up to date").
  onCheckAppUpdate: () => Promise<string>;
};

function VersionFooter({
  version,
  update,
  onCheck,
}: {
  version: string;
  update: AppUpdate | null;
  onCheck: () => Promise<string>;
}) {
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <div className="settings-foot">
      <span>Freshet {version || "…"}</span>
      {update ? (
        <span className="ok-text"> · {update.version} available</span>
      ) : (
        <>
          {" · "}
          <button
            className="label-action"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              onCheck()
                .then(setStatus)
                .catch((e: unknown) => setStatus(String(e)))
                .finally(() => setBusy(false));
            }}
          >
            {busy ? "Checking…" : (status ?? "Check for updates")}
          </button>
        </>
      )}
      <div className="settings-foot-sub">Settings and logs live in ~/.freshet</div>
    </div>
  );
}

function SnapshotRow({ snap, onRestore }: { snap: Snapshot; onRestore: (token: string) => Promise<string> }) {
  const [state, setState] = useState<{ status: "idle" | "busy" | "done" | "error"; message: string }>({
    status: "idle",
    message: "",
  });
  const when = new Date(snap.created_at * 1000).toLocaleDateString([], { month: "short", day: "numeric" });
  return (
    <div className="srow skipped-row">
      <span className="srow-icon">{icons.undo}</span>
      <span className="srow-text">
        <span className="srow-title">{snap.apps.join(", ").replace(/\.app/g, "") || snap.token}</span>
        <span className={`srow-sub${state.status === "error" ? " fail-text" : ""}`}>
          {state.status === "done" || state.status === "error"
            ? state.message
            : `${snap.version ? `${snap.version} · ` : ""}saved ${when}`}
        </span>
      </span>
      {state.status !== "done" && (
        <button
          className="btn-ghost btn-compact"
          disabled={state.status === "busy"}
          onClick={() => {
            setState({ status: "busy", message: "" });
            onRestore(snap.token)
              .then((message) => setState({ status: "done", message }))
              .catch((e: unknown) => setState({ status: "error", message: String(e) }));
          }}
        >
          {state.status === "busy" ? "Restoring…" : "Restore"}
        </button>
      )}
    </div>
  );
}

export function SettingsPanel(p: SettingsPanelProps) {
  // Re-render every 30s so "Next run today at …" turns into "tomorrow" once the time passes.
  const [, setTick] = useState(0);
  useEffect(() => {
    const timer = window.setInterval(() => setTick((t) => t + 1), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  const skipped = parseSkipped(p.ignored);
  const lastRunNeedsYou =
    p.lastRun !== null &&
    (p.lastRun.failed.length > 0 || p.lastRun.needs_admin.length > 0 || p.lastRun.signer_changed.length > 0);

  return (
    <div className="results-overlay settings">
      <div className="settings-header" data-tauri-drag-region>
        <button className="icon-btn" onClick={p.onClose} title="Back" aria-label="Back" data-tauri-drag-region="false">
          <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="currentColor" strokeWidth="1.6">
            <path d="M10 3.5 5.5 8l4.5 4.5" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
        <div className="results-title">Settings</div>
      </div>

      <div className="settings-body">
        {/* ---- schedule: the hero card ---- */}
        <section className={`schedule-card${p.scheduleEnabled ? " on" : ""}`}>
          <div className="schedule-top">
            <span className="schedule-icon">{icons.clock}</span>
            <div className="schedule-heading">
              <div className="schedule-title">Daily Homebrew run</div>
              <div className="schedule-status">
                {p.scheduleEnabled ? (
                  <>
                    <span className="status-pip on" />
                    Next run {nextRunText(p.hour, p.minute, new Date())}
                  </>
                ) : (
                  <>
                    <span className="status-pip" />
                    Off
                  </>
                )}
              </div>
            </div>
            <Toggle
              checked={p.scheduleEnabled}
              // Until the first check lands, the first-run count (and its confirmation) isn't known.
              disabled={p.scheduleLocked && !p.scheduleEnabled}
              onChange={p.onToggleSchedule}
              label="Daily Homebrew run"
            />
          </div>

          <p className="schedule-desc">
            {p.notifyOnly
              ? "Checks Homebrew each day and notifies you. Notify-only is on, so nothing is installed."
              : "Updates Homebrew, upgrades what's listed here (skipped versions excluded), then cleans up. Runs even when Freshet is closed."}
          </p>

          <div className="schedule-time">
            <span className="schedule-time-label">Time · {formatClock(p.hour, p.minute)}</span>
            <div className="time-select">
              <select
                aria-label="Hour"
                value={p.hour}
                onChange={(e) => p.onChangeTime(parseInt(e.target.value, 10), p.minute)}
              >
                {Array.from({ length: 24 }, (_, i) => (
                  <option key={i} value={i}>
                    {hourLabel(i)}
                  </option>
                ))}
              </select>
              <span className="time-colon">:</span>
              <select
                aria-label="Minute"
                value={p.minute}
                onChange={(e) => p.onChangeTime(p.hour, parseInt(e.target.value, 10))}
              >
                {Array.from({ length: 12 }, (_, i) => i * 5).map((m) => (
                  <option key={m} value={m}>
                    {pad(m)}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {p.lastRun && (
            <div className={`last-run${lastRunNeedsYou ? " attention" : ""}`}>
              <span className={`status-pip ${lastRunNeedsYou ? "warn" : "on"}`} />
              <span className="last-run-text">
                <span className="last-run-when">
                  Last run{" "}
                  {new Date(p.lastRun.finished_at * 1000).toLocaleString([], {
                    month: "short",
                    day: "numeric",
                    hour: "numeric",
                    minute: "2-digit",
                  })}
                </span>
                <span>{lastRunText(p.lastRun)}</span>
              </span>
            </div>
          )}

          {p.scheduleError && <div className="error-msg">{p.scheduleError}</div>}

          {p.scheduleConfirm && (
            <div className="schedule-confirm">
              <p>
                The first run will upgrade the <strong>{p.scheduledCount}</strong> Homebrew{" "}
                {p.scheduledCount === 1 ? "package" : "packages"} outdated now. Skip any you want to keep first, or turn
                on notify-only. Anything that needs your password is left for you to finish here.
              </p>
              <div className="schedule-confirm-actions">
                <button className="btn-ghost" onClick={p.onCancelSchedule}>
                  Cancel
                </button>
                <button className="btn-primary btn-compact" onClick={p.onConfirmSchedule}>
                  Turn on
                </button>
              </div>
            </div>
          )}
        </section>

        {/* ---- behaviour ---- */}
        <div className="settings-label">Updates</div>
        <div className="sgroup">
          <Row
            icon={icons.power}
            title="Open at login"
            sub="Keeps the menu-bar icon and update count around after a restart"
          >
            <Toggle checked={p.launchAtLogin} onChange={p.onLaunchAtLogin} label="Open at login" />
          </Row>
          <Row
            icon={icons.bell}
            title="Notify only"
            sub={notifyOnlySub(p.notifyOnly)}
          >
            <Toggle checked={p.notifyOnly} onChange={p.onNotifyOnly} label="Notify only" />
          </Row>
          <Row
            icon={icons.refresh}
            title="Include self-updating apps"
            sub={p.greedy ? "Showing apps like Chrome and Slack that update themselves" : "Apps that update themselves are hidden"}
          >
            <Toggle checked={p.greedy} onChange={p.onGreedy} label="Include self-updating apps" />
          </Row>
        </div>

        {/* ---- skipped versions ---- */}
        <div className="settings-label">
          Skipped versions
          {skipped.length > 0 && (
            <button className="label-action" onClick={p.onClearSkipped}>
              Unskip all
            </button>
          )}
        </div>
        <div className="sgroup">
          {skipped.length === 0 ? (
            <Row icon={icons.skip} title="Nothing skipped" sub="Use ⓘ › Skip on an update to hide that version" />
          ) : (
            skipped.map((s) => (
              <div key={s.key} className="srow skipped-row">
                <span className="skipped-kind">{s.label}</span>
                <span className="srow-text">
                  <span className="srow-title">{s.id}</span>
                  <span className="srow-sub mono">{s.version}</span>
                </span>
                <button className="btn-ghost btn-compact" onClick={() => p.onUnskip(s.key)}>
                  Unskip
                </button>
              </div>
            ))
          )}
        </div>

        {/* ---- security ---- */}
        <div className="settings-label">Security</div>
        <div className="sgroup">
          <Row
            icon={icons.lock}
            title="Publisher check before install"
            sub="Always on. An app update signed by a different developer than the installed app is blocked."
          >
            <span className="always-on">On</span>
          </Row>
          <Row
            icon={icons.bug}
            title="Flag known vulnerabilities"
            sub="Checks npm updates against osv.dev. Sends package names and versions there."
          >
            <Toggle checked={p.checkVulns} onChange={p.onCheckVulns} label="Flag known vulnerabilities" />
          </Row>
        </div>

        <div className="settings-label">Rollback</div>
        <div className="sgroup">
          {p.snapshots.length === 0 ? (
            <Row
              icon={icons.undo}
              title="No snapshots yet"
              sub="Each app update saves the previous version here for 30 days, using no extra disk until it changes."
            />
          ) : (
            p.snapshots.map((snap) => <SnapshotRow key={snap.token} snap={snap} onRestore={p.onRestore} />)
          )}
        </div>

        {/* ---- tools ---- */}
        <div className="settings-label">Tools</div>
        <div className="sgroup">
          <Row
            icon={icons.history}
            title="Update history"
            sub="Everything Freshet updated, and when"
            onClick={p.onShowHistory}
          />
          <Row
            icon={icons.adopt}
            title="Adopt apps into Homebrew"
            sub="Let Homebrew update apps you installed by hand"
            onClick={p.onAdopt}
          />
          <Row
            icon={icons.shield}
            title={p.verifying ? "Verifying signatures…" : "Verify app signatures"}
            sub={
              p.verifying ? (
                <span className="progress">
                  <span className="progress-bar" style={{ width: `${Math.round(p.verifyPct * 100)}%` }} />
                </span>
              ) : (
                "Flag apps whose publisher changed: a sign of a hijacked update"
              )
            }
            onClick={p.onVerify}
            disabled={p.verifying}
          />
        </div>

        <VersionFooter version={p.appVersion} update={p.appUpdate} onCheck={p.onCheckAppUpdate} />
      </div>
    </div>
  );
}

function notifyOnlySub(on: boolean): string {
  return on ? "Updates are listed but never installed, here or on schedule" : "Freshet can install updates";
}
