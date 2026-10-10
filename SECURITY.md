# Security Policy

Freshet runs commands that replace software on your Mac, and sometimes asks for your
administrator password. Security reports are taken seriously and are welcome.

## Reporting a vulnerability

**Please do not open a public issue.**

Report privately via **[GitHub Security Advisories](https://github.com/seanturner06/mac-updater/security/advisories/new)**
("Report a vulnerability" on the Security tab).

Please include:

- What the issue is and why it matters
- Steps to reproduce, or a proof of concept
- The affected version or commit
- Your macOS version and chip
- Any suggested fix

**What to expect:** an acknowledgement within a few days, an assessment and a plan shortly after,
and credit in the advisory and release notes unless you would rather not be named. Freshet is
maintained by one person, so please allow reasonable time for a fix before public disclosure.

## Supported versions

Freshet is at v0.1 and in early development. Only the latest commit on `main` is supported.

## What is in scope

- Command or argument injection through any identifier reaching a subprocess
- SSRF or TLS bypass in the Sparkle appcast fetch path
- Ways to tamper with `~/.freshet/signing.json` (the trust baseline) or `~/.freshet/askpass.sh`
  (the sudo helper) from a lower-privileged context
- Ways to suppress or defeat signer-drift detection
- Exposure of the user's administrator password
- Escape from the webview's restricted capability set, or CSP bypass
- Privilege escalation via the launchd schedule or the generated run script
- Vulnerable dependencies with a demonstrable path to exploitation in the shipped macOS build

## What is out of scope

These are known and documented limitations, not undisclosed bugs. A report about one of them is
still useful if you have a practical fix.

- **DNS rebinding against `is_safe_feed_url`.** The host is validated as written; a name that
  resolves to a public address at validation time and a private one at fetch time defeats it.
  Closing this means resolving and pinning the address ourselves instead of using `curl`.
- **`PATH` manipulation.** Freshet resolves `brew`, `curl`, `mas`, `npm`, `codesign`, `spctl`,
  `defaults`, `launchctl`, and `open` through `PATH`. An attacker who can write to a directory
  early in the user's `PATH` already has code execution as that user. (Absolute paths for the
  system tools are still on the hardening list.)
- **The first-run trust gap.** The Team IDs present when Freshet first scans become the baseline.
  A machine that is already compromised is baselined as clean.
- **Cross-process races on `signing.json`.** Guarded by an in-process mutex only; two processes
  are last-writer-wins.
- **Vulnerabilities inside `brew`, `mas`, `npm`, or an app's own Sparkle updater.** Report those
  upstream.
- **Anything requiring root, physical access, or an already-compromised user account.**
- Missing hardening flags with no demonstrated impact, or a raw scanner report with no analysis.

## How Freshet is designed to be safe

Context for anyone assessing the attack surface. Full detail in
[`docs/06-security.md`](docs/06-security.md).

- **Freshet never downloads or executes an update artifact.** It invokes `brew`, `mas`, and
  `npm`, and each does its own downloading and integrity verification. The only direct network
  request is fetching Sparkle appcast XML, which is parsed for a version string and never acted
  on.
- **No shell, anywhere.** Every external process is `Command::new` with an argument vector.
  Nothing is interpolated into a shell string.
- **Everything from the webview is validated** against strict allowlist patterns before it can
  reach an argv, and `build_update_command` is the single chokepoint every update passes through.
- **Sparkle feeds are https-only across every redirect hop** (`curl --proto =https
  --proto-redir =https`), with private, loopback, link-local, CGNAT, and `.local`/`.internal`
  hosts refused, a timeout, and a size cap.
- **`~/.freshet` is `0700`**, with the trust baseline at `0600` and helper scripts at `0700`, set
  explicitly rather than relying on the umask.
- **Minimal Tauri surface** — `core:default` plus window-drag. No shell, opener, fs, or http
  plugin. Strict CSP with self-hosted fonts and no remote resources.
- **Least-privilege elevation** — `SUDO_ASKPASS` points at a native macOS password dialog.
  Freshet never sees or stores the password.
- **`unsafe_code = "deny"`.** No unsafe code, no FFI.
- **CI runs** `cargo clippy -- -D warnings` at pedantic level, `cargo audit`, and 50 Rust unit
  tests including adversarial validator cases, on every push and pull request.

The most recent full audit write-up is in
[`tests/security-audit.txt`](tests/security-audit.txt).

## A note on signer-drift detection

Freshet warns when an updated app is signed by a different Developer Team ID than the previous
version — the fingerprint of a hijacked release like 3CX, HandBrake, or Transmission.

Please understand its limits before relying on it:

- It is **post-hoc**. Drift is detected after the update is installed, not before.
- It catches signer **substitution**. An attacker who compromises a vendor's legitimate signing
  key produces no drift at all.
- The baseline is seeded on first sight, so a pre-existing compromise is invisible.
- Only cask updates are checked automatically; the full inventory scan is manual.

These are documented in [`docs/07-provenance.md`](docs/07-provenance.md). Improvements to any of
them are very welcome.
