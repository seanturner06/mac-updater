# Freshet — Design Documents

This directory is the design record for Freshet: what it is, how it is built, why it is built
that way, and where the edges are. It is written for two readers — a contributor trying to make
a change safely, and a future maintainer trying to understand a decision that looks strange.

Every document describes **the system as it exists today**. Where reality diverges from an
intention (a user story that isn't implemented, a feature with a known hole), that's called out
in a *Known gaps* section rather than quietly smoothed over. Planned work lives in
[12 — Roadmap & Deferred Work](12-roadmap.md), never disguised as current behaviour.

## The documents

| # | Document | What it covers |
|---|---|---|
| 01 | [Product design](01-product-design.md) | Problem, users, the scope boundary, non-goals, positioning |
| 02 | [System architecture](02-architecture.md) | Process model, the IPC boundary, layering, concurrency, state |
| 03 | [Update sources](03-update-sources.md) | The five sources, the source contract, how to add a sixth |
| 04 | [Backend design](04-backend.md) | Rust layering, command catalogue, parsers, error model |
| 05 | [Frontend design](05-frontend.md) | React state model, the pure-logic layer, component structure |
| 06 | [Security design](06-security.md) | Threat model, trust boundaries, every validator and why |
| 07 | [Provenance & signer drift](07-provenance.md) | The differentiator: what it catches that Gatekeeper doesn't |
| 08 | [Scheduling](08-scheduling.md) | The launchd agent, and how it deliberately differs from the app |
| 09 | [UI design system](09-ui-design-system.md) | Tokens, type scale, layout, menu-bar window mechanics |
| 10 | [Testing & quality](10-testing-and-quality.md) | The four gates, the pure/IO split, fixture discipline |
| 11 | [Performance](11-performance.md) | Where the seconds actually go, and how to measure |
| 12 | [Roadmap & deferred work](12-roadmap.md) | v1→v2→v3, and what is explicitly parked |
| 13 | [Decision log](13-decisions.md) | The load-bearing decisions, with their alternatives and costs |

Contributor-facing process docs live at the repository root:
[`CONTRIBUTING.md`](../CONTRIBUTING.md) and [`SECURITY.md`](../SECURITY.md).

## The one-paragraph version

Freshet is a macOS menu-bar app that shows everything outdated on your Mac in one list —
Homebrew formulae, Homebrew casks, Mac App Store apps, npm globals, and Sparkle-based apps — and
updates what you pick. It **does not maintain its own version database**; it orchestrates the
registries your Mac already trusts. Its one genuinely differentiated feature is **signer-drift
detection**: after an update it re-checks the code signature of the upgraded app and warns if the
Developer Team ID changed since last time, which is the fingerprint of a hijacked release and the
thing Gatekeeper does not check.

## Reading order

- **New contributor:** 02 → 04 or 05 (whichever side you're touching) → 10 → `CONTRIBUTING.md`.
- **Adding an update source:** 03 → 04 → 10.
- **Touching anything that runs a command, fetches a URL, or writes a file:** 06, first.
- **Trying to understand a weird choice:** 13.
