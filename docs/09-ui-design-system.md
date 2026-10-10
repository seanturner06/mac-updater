# 09 — UI Design System

Everything visual lives in `src/App.css` (713 lines, no framework, no preprocessor, no CSS
modules). One file is the right size for one 420px window, and it keeps the token block and its
consumers visible together.

## Design goal

**Calm, minimal, and quiet.** The consistent complaint about tools in this category is that they
overwhelm — MacUpdater was described as "a car crash of a billion options"; the tool people
praise, Latest, wins on elegance rather than features. Restraint is a competitive position here,
not just taste.

Concretely, that means:

- One window, five collapsed sections, two buttons. Nothing is visible until you ask for it.
- Semantic colours are **muted and desaturated**. Nothing shouts. The one place that is allowed
  to shout is a signer-drift warning, because that is the one thing that should interrupt you.
- Dark-first. A menu-bar popover sits against the desktop, not a document.

## Tokens

Every colour in the app is a CSS custom property in `:root`. Swapping the entire theme is
replacing that one block — which is exactly how the palette has been iterated.

| Token | Role |
|---|---|
| `--bg` | Window background (with alpha, over `backdrop-filter`) |
| `--bg-elev` | Raised surfaces: inputs, secondary buttons, checkboxes |
| `--bg-hover` | Hover state for rows and buttons |
| `--border` | Every 1px divider and outline |
| `--text` / `--text-dim` / `--text-faint` | Three-step text hierarchy |
| `--accent` | Primary action, title, checked states |
| `--on-accent` | Foreground on an accent fill (checkmarks, primary button label) |
| `--ok` / `--warn` / `--err` | Semantic foregrounds |
| `--ok-bg` / `--warn-bg` / `--err-bg` / `--err-border` | Their low-alpha surfaces |
| `--font-display` / `--font-body` / `--font-mono` | Type roles |

### The surface-tinting rule

Early palettes used **neutral white-alpha** surfaces (`rgba(255,255,255,0.045)` and friends).
The result: swapping the palette visibly changed only the toggle and the primary button, while
panels, hovers, and dividers stayed identical across every theme. The palette had no presence.

**The rule now: tint `--bg-elev`, `--bg-hover`, and `--border` with the accent's RGB**, so
surfaces carry the palette's tone. `.title` also uses `var(--accent)` globally. Any restored
older palette needs the same treatment to feel like a theme rather than a button colour.

Current palette — **Gothic Noir**: black `#000000` → bg, brown-gray `#5c4e4e` → surfaces, silver
`#d1d0d0` → text, mauve-gray `#988686` → accent and dim text. `--err` is a crimson `#c46666`,
the only colour outside the brand palette, because failure has to be legible as failure.

Several alternate palettes (Cobalt Sky, Sage & Stone, Slate & Sky, Pine & Mist, Lavender,
Obsidian & Electric Blue) are recorded as complete token blocks in the project's design notes and
can be restored by pasting one over `:root`.

## Typography

Four self-hosted variable `woff2` faces in `src/assets/fonts/`. **Self-hosting is a security
requirement, not a preference** — it is what lets the CSP say `font-src 'self'` and forbid
external requests entirely ([06 — Security](06-security.md)).

| Role | Face | Used for |
|---|---|---|
| Display | **Fraunces** (`opsz 144`) | The "Freshet" title, 21px, `--accent` |
| Body | **Inter Tight** | All UI text, 13px base |
| Mono | **JetBrains Mono** | Versions, counts, package names, logs |

Instrument Serif is bundled as a kept revert option for the display face.

**Mono for identifiers is a functional choice.** Version strings and package names are compared
character by character (`4.36.140` → `4.36.141`), and proportional digits make that harder.
Category bodies are mono at 11px for exactly this reason.

### Scale

The type scale uses the golden ratio where it is perceptible rather than imposing it everywhere:
body 13px × 1.618 ≈ **21px title**. The window is 420 × 680, ≈ 1.618. Intermediate sizes
(12.5px, 11.5px, 11px, 10px) are chosen by eye — a strict modular scale at this size produces
steps too small to see and too rigid to work with.

## Layout

```
┌──────────────────────────────────────────┐  420 × 680, radius 12, 1px border
│ ● Freshet                        ⟳  ⚙   │  header — drag region
│   3 updates available                    │  16px padding, 1px bottom border
├──────────────────────────────────────────┤
│  [3] Homebrew                         ›  │  categories — flex: 1, overflow-y: auto
│  [0] Applications                     ›  │  4px 8px padding, 8px radius rows
│  [1] App Store                        ›  │
│  [0] npm globals                      ›  │
│  [2] Other apps                       ›  │
├──────────────────────────────────────────┤
│  ┌────────────────────────────────────┐  │  actions — NOT a drag region
│  │        Update selected (3)         │  │  full-width primary
│  └────────────────────────────────────┘  │
│  ┌────────────────────────────────────┐  │  full-width secondary
│  │             Update all             │  │
│  └────────────────────────────────────┘  │
├──────────────────────────────────────────┤
│ last checked 14:32                       │  footer — drag region
└──────────────────────────────────────────┘
```

The window is a flex column with `max-height: 680px`; only the categories region scrolls. Header,
actions, and footer are fixed, so the primary button never scrolls out of reach.

Overlays (Settings, Adopt, Results) cover the whole window with their own header / body / footer
structure. They are mutually exclusive and dismissed by an `✕` or a **Done** button.

### The window material

```css
.app {
  background: var(--bg);                                   /* alpha ~0.94 */
  backdrop-filter: blur(40px) saturate(180%);
  border-radius: 12px;
  border: 1px solid var(--border);
}
```

This requires `transparent: true` and `macOSPrivateApi: true` in `tauri.conf.json`, plus
`background: transparent` on `html`, `body`, and `#root` so the blur reaches the desktop. Getting
the popover material right is what makes the window read as a system component rather than a
web page in a frameless box.

## Interaction rules

**Fitts's law, applied where it is felt.** Three concrete applications:

- Primary and secondary actions are **full-width**.
- An updatable row is a `<label>` wrapping its checkbox, so the **whole row** is the target —
  the 15px checkbox is never the thing you have to hit.
- The small glyph buttons (`↗` notes, `✕` skip) are 13px glyphs in **24 × 24** grid-centred hit
  areas.

**Every state is visible.**

| Signal | Rendering |
|---|---|
| Status | A glowing 8px dot: `ok` (accent), `warn`, `idle` (faint), with `box-shadow: 0 0 12px currentColor` |
| Count per category | A mono pill, `pill-ok` at zero / `pill-warn` above |
| Version change | `4.36.140` struck through in `--text-faint` → `→` → `4.36.141` in `--warn` |
| Disabled | `opacity: 0.4` + `cursor: not-allowed`; the primary button also drops to `--bg-elev` |
| In-flight rows | `.item-locked` — 0.55 opacity, `not-allowed`, checkbox disabled |
| Partial selection | A tri-state checkbox with a custom `:indeterminate` dash |

**Transitions are 0.12–0.2s**, and only on `background`, `color`, `border-color`, `transform`,
and `opacity`. Nothing animates layout. There are no entrance animations — a menu-bar popover
that animates its contents in feels slow, because you opened it to read something.

**The header subtitle is a priority-ordered status line**, not a stack of indicators:

```
verifying signatures… 47%   →  checking…  →  everything is current  →  N updates available
```

## Custom controls

Native form controls do not fit a dark translucent popover, so three are rebuilt:

- **Checkbox** — `appearance: none`, 15px, 4px radius, `--bg-elev` fill. `:checked` uses a
  rotated bordered pseudo-element as the tick; `:indeterminate` uses a 7px dash. Both fill with
  `--accent` and draw in `--on-accent`.
- **Toggle** — 34 × 20 iOS-style pill, 16px knob translated 14px on `:checked`. Built from a
  hidden input plus a `.toggle-slider` sibling.
- **Chevron** — a `›` character rotated 90° via inline `transform`, transitioning at 0.15s.

## Drag regions

A `decorations: false` window has no title bar, so dragging is CSS-driven:

```jsx
<div className="header" data-tauri-drag-region>            {/* draggable */}
  <div className="header-actions" data-tauri-drag-region="false">  {/* buttons work */}
```

Header and footer are drag regions. **The actions row is deliberately not one** — a drag region
swallows clicks, and the primary button living inside one would be maddening. Every overlay
header is a drag region with its close button opted out.

This requires the `core:window:allow-start-dragging` capability, which is the only permission
beyond `core:default`.

## Accessibility — current state

Honestly: thin.

**Present:** semantic `<button>` and `<label>` elements throughout; `title` on icon buttons;
`aria-label` on the cask info button; real focusable controls; status conveyed by text as well as
by the colour of the dot.

**Missing:**
- No `Escape`-to-close, no focus trap, and no `aria-modal` on the overlays. Focus stays behind
  the panel.
- Most icon buttons have `title` but no `aria-label`. `title` is not reliably announced.
- Results and progress changes are not in a live region — a screen-reader user gets no
  announcement when a check finishes.
- No visible focus ring is defined; the default is inconsistent against these backgrounds.
- No light mode and no `prefers-reduced-motion` handling.

The overlay focus management and `aria-label` coverage are small, well-defined contributions if
someone wants them.

## Adding a light mode

The token structure already supports it. The work is one block plus the gotchas:

```css
@media (prefers-color-scheme: light) {
  :root { /* … */ }
}
```

- `--bg-elev`, `--bg-hover`, and `--border` must flip from light-alpha to **dark**-alpha, or
  every surface disappears.
- `--accent` needs to deepen for contrast against a light background, and `--on-accent` inverts.
- Semantic colours need darkening — the muted pastels that read as calm on near-black are
  illegible on near-white.
- `.toggle-slider`'s unchecked background is a hardcoded `rgba(255,255,255,0.15)` and the knob is
  hardcoded `white`. Both need tokenising first.

A light palette was tried once and rejected as too washed out; the direction if revisited is
higher contrast, not lighter.

## Rules for UI contributions

1. **Use tokens.** A literal colour in a rule is a bug unless it is genuinely non-thematic.
   (`.toggle-slider` is the standing exception and should be fixed.)
2. **Do not add a CSS framework or a component library.** See
   [13 — Decisions](13-decisions.md).
3. **Keep the window at 420 × 680.** It is not resizable, and it is the design constraint that
   keeps the product restrained.
4. **New surfaces reuse `.results-overlay`** and its header/body/footer structure rather than
   inventing a fourth panel shape.
5. **Hit targets ≥ 24px**, even for small glyphs.
6. **Don't animate layout**, and don't add entrance animations.
7. **If you add a control, add both its states** — hover and disabled. Half the polish in this
   file is `:disabled` rules.
