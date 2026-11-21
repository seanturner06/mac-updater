// The header's status: the app icon's drop, drawn small. Full when everything is current, carrying
// the icon's up-chevron when updates are waiting, an outline while checking. The shape carries
// the meaning, so it reads even in themes where the ok and warn colors sit close together.
export type StatusState = "ok" | "warn" | "idle";

const LABEL: Record<StatusState, string> = {
  ok: "Everything is up to date",
  warn: "Updates available",
  idle: "Checking",
};

export function StatusMark({ state }: { state: StatusState }) {
  return (
    <svg className={`status-mark ${state}`} viewBox="0 0 16 18" width="14" height="16" role="img" aria-label={LABEL[state]}>
      <path className="status-drop" d="M8 1.2C8 1.2 2.4 7.6 2.4 11.3a5.6 5.6 0 0 0 11.2 0C13.6 7.6 8 1.2 8 1.2Z" />
      {state === "warn" && <path className="status-chevron" d="M5.4 12.6 8 10l2.6 2.6" />}
    </svg>
  );
}
