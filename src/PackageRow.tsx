// One outdated package in the main list: icon, name + description, version change, and an
// expandable detail card explaining what it is before you update it.
import { invoke } from "@tauri-apps/api/core";
import { versionBump, type OutdatedItem, type PackageInfo, type VulnReport } from "./logic";

const openUrl = (url: string) => void invoke("open_url", { url });

// Category glyphs, drawn at 16px on a 16-unit grid with the same stroke weight as the header icons.
export function KindIcon({ kind }: { kind: string }) {
  const common = {
    viewBox: "0 0 16 16",
    width: 16,
    height: 16,
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.3,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  switch (kind) {
    case "brew": // terminal prompt
      return (
        <svg {...common}>
          <rect x="1.5" y="2.5" width="13" height="11" rx="2.5" />
          <path d="M4.5 6.5l2 1.75-2 1.75M8.5 10.5h3" />
        </svg>
      );
    case "cask": // app grid
      return (
        <svg {...common}>
          <rect x="2" y="2" width="5" height="5" rx="1.4" />
          <rect x="9" y="2" width="5" height="5" rx="1.4" />
          <rect x="2" y="9" width="5" height="5" rx="1.4" />
          <rect x="9" y="9" width="5" height="5" rx="1.4" />
        </svg>
      );
    case "mas": // shopping bag
      return (
        <svg {...common}>
          <path d="M3 5.5h10l-.8 8H3.8z" />
          <path d="M5.8 5.5V4.4a2.2 2.2 0 0 1 4.4 0v1.1" />
        </svg>
      );
    case "npm": // package cube
      return (
        <svg {...common}>
          <path d="M8 1.8l5.5 3v6.4L8 14.2l-5.5-3V4.8z" />
          <path d="M2.5 4.8L8 7.8l5.5-3M8 7.8v6.4" />
        </svg>
      );
    default: // sparkle
      return (
        <svg {...common}>
          <path d="M8 2v3.2M8 10.8V14M2 8h3.2M10.8 8H14M4 4l1.9 1.9M10.1 10.1L12 12M12 4l-1.9 1.9M5.9 10.1L4 12" />
        </svg>
      );
  }
}

export function InfoIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.3">
      <circle cx="8" cy="8" r="6.3" />
      <path d="M8 7.3v3.6" strokeLinecap="round" />
      <circle cx="8" cy="5" r="0.7" fill="currentColor" stroke="none" />
    </svg>
  );
}

// The app's own icon when we have one; otherwise a monogram tile so every row has an anchor.
function PackageIcon({ name, icon, dim }: { name: string; icon?: string | null; dim: boolean }) {
  if (icon) return <img className="pkg-icon" src={icon} alt="" draggable={false} />;
  const letter = name.replace(/^@[^/]+\//, "").charAt(0).toUpperCase() || "?";
  return <span className={`pkg-icon pkg-monogram${dim ? " dim" : ""}`}>{letter}</span>;
}

type RowProps = {
  kind: string;
  item: OutdatedItem;
  info?: PackageInfo;
  vuln?: VulnReport;
  icon?: string | null;
  selectable: boolean;
  checked: boolean;
  locked: boolean;
  open: boolean;
  onToggle: () => void;
  onToggleInfo: () => void;
  onSkip: () => void;
};

export function PackageRow({
  kind,
  item,
  info,
  vuln,
  icon,
  selectable,
  checked,
  locked,
  open,
  onToggle,
  onToggleInfo,
  onSkip,
}: RowProps) {
  const bump = versionBump(item.current, item.latest);
  const desc = info?.desc ?? "";
  // Only Homebrew rows get descriptions; reserve the line for them while details load.
  const describable = kind === "brew" || kind === "cask";
  const stop = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
  };

  const content = (
    <>
      {selectable && (
        <input
          type="checkbox"
          className="check"
          checked={checked}
          onChange={onToggle}
          disabled={locked}
          aria-label={`Select ${item.name}`}
        />
      )}
      <PackageIcon name={item.name} icon={icon} dim={info?.dependency ?? false} />
      <span className="pkg-main">
        <span className="pkg-name">
          <span className="pkg-name-text">{item.name}</span>
          {bump === "major" && <span className="chip chip-major">major</span>}
          {vuln && vuln.fixed.length > 0 && (
            <span className="chip chip-vuln" title="Known vulnerabilities this update fixes (osv.dev)">
              fixes {vuln.fixed.length} {vuln.fixed.length === 1 ? "vuln" : "vulns"}
            </span>
          )}
          {item.self_updating && <span className="chip">self-updating</span>}
        </span>
        {desc ? (
          <span className="pkg-desc">{desc}</span>
        ) : (
          describable && !info && <span className="pkg-desc pkg-desc-empty" aria-hidden />
        )}
      </span>
      <span className="pkg-ver" title={`${item.current || "?"} → ${item.latest}`}>
        <span className="pkg-ver-new">{item.latest}</span>
        {item.current && <span className="pkg-ver-old">from {item.current}</span>}
      </span>
      <button
        className={`pkg-info${open ? " active" : ""}`}
        onClick={(e) => {
          stop(e);
          onToggleInfo();
        }}
        title="What is this?"
        aria-label={`About ${item.name}`}
        aria-expanded={open}
      >
        <InfoIcon />
      </button>
    </>
  );

  return (
    <div className={`pkg${open ? " open" : ""}${locked ? " locked" : ""}`}>
      {selectable ? (
        <label className="pkg-row">{content}</label>
      ) : (
        <div className="pkg-row pkg-row-readonly" onClick={onToggleInfo}>
          {content}
        </div>
      )}
      {open && <PackageDetail kind={kind} item={item} info={info} vuln={vuln} bump={bump} onSkip={onSkip} />}
    </div>
  );
}

function PackageDetail({
  kind,
  item,
  info,
  vuln,
  bump,
  onSkip,
}: {
  kind: string;
  item: OutdatedItem;
  info?: PackageInfo;
  vuln?: VulnReport;
  bump: ReturnType<typeof versionBump>;
  onSkip: () => void;
}) {
  const chips: string[] = [];
  if (info?.dependency) chips.push("dependency of another formula");
  if (info?.auto_updates || item.self_updating) chips.push("updates itself");
  if (info?.license) chips.push(info.license.length > 32 ? `${info.license.slice(0, 30)}…` : info.license);
  const safe = (u?: string) => (u && /^https?:\/\//.test(u) ? u : "");
  const homepage = safe(info?.homepage);
  const notes = safe(info?.release_notes) || safe(item.notes_url);

  return (
    <div className="pkg-detail">
      {/* The row's own description line un-truncates while open, so it isn't repeated here. */}
      {!info?.desc && (
        <p className="pkg-detail-desc muted">
          {(kind === "brew" || kind === "cask") && !info ? "Loading details…" : "No description available."}
        </p>
      )}
      {bump === "major" && (
        <p className="pkg-detail-warn">Major version: may include breaking changes. Check the release notes.</p>
      )}
      {vuln && vuln.current.length > 0 && (
        <div className="pkg-vulns">
          <p className={vuln.fixed.length > 0 ? "pkg-detail-danger" : "pkg-detail-warn"}>
            {item.current} has {vuln.current.length} known{" "}
            {vuln.current.length === 1 ? "vulnerability" : "vulnerabilities"}
            {vuln.fixed.length > 0 ? `; ${item.latest} fixes ${vuln.fixed.length}.` : "; updating doesn't fix them."}
          </p>
          <div className="pkg-chips">
            {vuln.current.map((id) => (
              <button
                key={id}
                className={`chip chip-link${vuln.fixed.includes(id) ? " fixed" : ""}`}
                onClick={() => openUrl(`https://osv.dev/vulnerability/${encodeURIComponent(id)}`)}
                title={vuln.fixed.includes(id) ? "Fixed by this update" : "Not fixed by this update"}
              >
                {id} ↗
              </button>
            ))}
          </div>
        </div>
      )}
      {chips.length > 0 && (
        <div className="pkg-chips">
          {chips.map((c) => (
            <span key={c} className="chip">
              {c}
            </span>
          ))}
        </div>
      )}
      <div className="pkg-detail-actions">
        {homepage && (
          <button className="link-btn" onClick={() => openUrl(homepage)}>
            Homepage ↗
          </button>
        )}
        {notes && (
          <button className="link-btn" onClick={() => openUrl(notes)}>
            Release notes ↗
          </button>
        )}
        <span className="spacer" />
        <button className="link-btn link-btn-quiet" onClick={onSkip} title="Hide until a newer version ships">
          Skip {item.latest}
        </button>
      </div>
    </div>
  );
}

// Placeholder rows while the first check runs.
export function SkeletonRows({ count }: { count: number }) {
  return (
    <div className="skeletons" aria-hidden>
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="skeleton-row">
          <span className="skeleton skeleton-icon" />
          <span className="skeleton-lines">
            <span className="skeleton skeleton-line" style={{ width: `${45 + ((i * 17) % 35)}%` }} />
            <span className="skeleton skeleton-line short" style={{ width: `${25 + ((i * 11) % 25)}%` }} />
          </span>
        </div>
      ))}
    </div>
  );
}
