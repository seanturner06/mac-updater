# 06 — Security Design

Freshet runs commands that replace software on your machine, and sometimes asks for your admin
password. That makes it a high-trust tool, and the security posture has to be proportionate.

For reporting a vulnerability, see [`../SECURITY.md`](../SECURITY.md). The most recent full audit
write-up is [`../tests/security-audit.txt`](../tests/security-audit.txt).

## The architectural claim

**Freshet never downloads or executes an update artifact.**

It is an orchestrator. `brew`, `mas`, and `npm` download their own artifacts and perform their
own integrity verification — Homebrew checks SHA-256 against the cask/formula definition, npm
verifies registry integrity hashes, the App Store does its own thing entirely. Freshet's job
stops at constructing an argv.

This is the single most important thing to preserve. Most of the classic updater vulnerability
class — unsigned downloads, TLS downgrade on the artifact, checksum bypass, "verify then run" race
conditions — does not exist here because there is no artifact in our hands. **A change that makes
Freshet download and run something is not an incremental feature; it relocates the entire threat
model.**

The one direct network path is fetching Sparkle appcast XML, and that path is **detection-only**:
it parses a version string to tell the user an app can update itself. It never downloads a DMG.

## Trust boundaries

Four inputs are untrusted. Everything else is derived from them.

```
┌─ 1. The webview ────────────────────────────────────────────────────────┐
│  Cask tokens, npm names, mas ids, search queries, URLs arriving via IPC │
│  → validated at every command entry, before any argv is built           │
└─────────────────────────────────────────────────────────────────────────┘
┌─ 2. Installed apps' Info.plist ─────────────────────────────────────────┐
│  SUFeedURL is a URL chosen by any app on the machine                    │
│  → is_safe_feed_url at harvest AND again at fetch                       │
└─────────────────────────────────────────────────────────────────────────┘
┌─ 3. Network responses ──────────────────────────────────────────────────┐
│  Appcast XML from third-party servers                                   │
│  → size-capped, timed out, https-only across redirects, hand-parsed     │
└─────────────────────────────────────────────────────────────────────────┘
┌─ 4. Other local users ──────────────────────────────────────────────────┐
│  ~/.freshet holds the trust baseline and an executable helper script    │
│  → 0700 dir, 0600 data, 0700 scripts, set explicitly not via umask      │
└─────────────────────────────────────────────────────────────────────────┘
```

### Why the webview is untrusted

We ship the frontend, so in the nominal case its inputs are ours. Treating it as untrusted anyway
is cheap defence-in-depth against the case where it isn't: a bug in the webview, a CSP failure, a
compromised dependency in the frontend chain, a malicious value that arrived from a *source* and
round-tripped through the UI. The validators cost microseconds. The absence of them costs argv
injection into a command that may run under sudo.

## Threat model

| # | Threat | Mitigation | Status |
|---|---|---|---|
| T1 | Shell/command injection via an identifier | No shell anywhere; `Command::new` + argv; per-kind validators | Mitigated |
| T2 | Argument injection — a value starting with `-` read as a flag | Validators reject leading `-`; tokens must start alphanumeric | Mitigated |
| T3 | Arbitrary install spec via npm (git URL, tarball, path) | `is_valid_npm_package` rejects `://`, leading `.`/`_`/`-`, uppercase | Mitigated |
| T4 | SSRF — hostile app points `SUFeedURL` at internal infra | `is_safe_feed_url`: https-only, private/loopback/link-local/CGNAT/`.local` refused | Mitigated (DNS rebinding excepted) |
| T5 | TLS downgrade via redirect on a feed fetch | `curl --proto =https --proto-redir =https`; never `-k` | Mitigated |
| T6 | Memory exhaustion from a hostile feed | `--max-filesize 5242880`, `--max-time 8` | Partial — curl enforces the size cap only with a `Content-Length` |
| T7 | Tampering with the trust baseline to suppress a hijack warning | `signing.json` at `0600` in a `0700` dir | Mitigated locally |
| T8 | Swapping `askpass.sh` to capture the sudo password | `0700` in a `0700` dir; rewritten each process | Mitigated locally |
| T9 | Password exposure during elevation | `SUDO_ASKPASS` → native macOS dialog; the app never sees or stores it | Mitigated |
| T10 | Over-broad webview capabilities | `core:default` + window-drag only; no shell plugin, no opener plugin | Mitigated |
| T11 | Remote content execution in the webview | Strict CSP, `default-src 'self'`, self-hosted fonts, no remote scripts | Mitigated |
| T12 | Malicious update artifact | Delegated to brew/mas/npm's own verification | Delegated |
| T13 | Hijacked-but-validly-signed release | **Signer-drift detection** — see [07](07-provenance.md) | Mitigated (post-hoc) |
| T14 | Vulnerable dependency | `cargo audit` in CI; strict lints; small dep tree | Ongoing |
| T15 | Malicious `PATH` — a planted `brew`/`curl` earlier in `PATH` | None | **Accepted** |
| T16 | Race on `signing.json` across processes | In-process mutex only | **Accepted** (last writer wins) |

### Accepted limitations, stated plainly

- **DNS rebinding.** `is_safe_feed_url` checks the host as written. A hostname that resolves to a
  public IP at validation time and a private one at fetch time defeats it. Closing this requires
  resolving and pinning the address ourselves, which means not using `curl`. Judged not worth it
  for a detection-only path.
- **`PATH` hijacking.** Freshet resolves `brew`, `curl`, `mas`, `npm`, `codesign`, `spctl`,
  `defaults`, `launchctl`, and `open` through `PATH`. An attacker who can write to a directory
  early in the user's `PATH` already has code execution as that user, so this is not a
  privilege boundary Freshet can meaningfully defend. Using absolute paths for the system tools
  (`/usr/bin/codesign`, `/usr/bin/curl`, …) would still be a cheap hardening step and is on the
  list.
- **First-run trust gap.** Whatever Team IDs exist when Freshet first scans become the baseline.
  A machine already compromised is baselined as clean. See [07](07-provenance.md).

## The controls

### No shell, ever

Every external process is `Command::new(program).args([...])`. There is no `sh -c`, no
`format!`-into-a-command-string, no `shell-escape`. Grep for it; the absence is the invariant.

This is what makes the validators defence-in-depth rather than the sole line of defence: even
with a validator bug, an argv element is an argv element — `; rm -rf ~` is a single, harmless,
nonexistent package name. What the validators actually stop is **argument** injection: a token
like `--force` or `--cask` would be a real flag.

### Validation at the boundary

| Value | Validator | Reachable commands |
|---|---|---|
| Cask / formula token | `is_valid_cask_token` | `brew upgrade`, `brew upgrade --cask`, `brew fetch`, `brew info`, `brew install --cask --adopt` |
| mas ADAM ID | `is_numeric_id` | `mas upgrade` |
| npm package | `is_valid_npm_package` | `npm install -g` |
| Search query | `is_safe_search_query` | `brew search --cask` |
| URL to open | `is_safe_url` | `open` |
| Feed URL to fetch | `is_safe_feed_url` | `curl` |

`build_update_command` is the chokepoint: **every update path goes through it, and every arm is
gated on its validator.** `fetch_batches` re-validates independently so the prefetch path can't
be reached with something the install path would reject. An unvalidated new source must not get
an arm.

Every one of these validators is a pure function with unit tests, including adversarial cases —
leading dashes, `://`, control characters, over-length inputs, `..`, userinfo in URLs.

### SSRF defence

`SUFeedURL` is read from `Info.plist` of apps we did not write. `is_safe_feed_url` requires:

- scheme is exactly `https://` (not merely "http or https" — plaintext is MITM-able and there is
  no reason a modern appcast needs it),
- length ≤ 2048, no control characters or spaces,
- the host, extracted by `url_host`, is not local or private.

`is_local_or_private_host` refuses `localhost`, `*.local`, `*.localhost`, `*.internal`; IPv4
private, loopback, link-local, unspecified, broadcast, and **CGNAT 100.64.0.0/10** (which is not
in std's helpers and is exactly what a shared-infrastructure attacker would use); IPv6 loopback,
unspecified, unique-local `fc00::/7`, and link-local `fe80::/10`.

The concrete attack this stops: a malicious app sets `SUFeedURL` to
`https://169.254.169.254/latest/meta-data/iam/security-credentials/` and waits for Freshet to
fetch it. TLS does not help — the request would be perfectly valid TLS to an internal address.

`url_host` handles userinfo (`https://evil.com@127.0.0.1/`), ports, and bracketed IPv6 literals,
because each of those is a standard way to smuggle a host past a naive check.

**Validation happens twice** — in `check_sparkle` when harvesting, and again inside `fetch_url`
before spawning curl. The second check is what protects any future caller that forgets the first.

### Elevation

Some casks need admin rights (anything shipping a `.pkg`, declaring `sudo: true`, or writing to
`/Library`). In a GUI app spawned with no controlling terminal, `sudo` cannot prompt and simply
fails.

`run_update_cmd` sets `SUDO_ASKPASS` to a generated helper that runs `osascript` to show a native
macOS password dialog. Homebrew invokes `sudo -A`, so it consults the helper and the user gets a
proper system dialog. **Freshet never sees, handles, or stores the password** — `sudo` reads it
from the helper's stdout directly.

The helper lives at `~/.freshet/askpass.sh`, mode `0700`, in a `0700` directory, rewritten each
process. If another local user could write that file, they would own every sudo password the user
ever types into Freshet.

**Known hole:** `mas` calls `sudo` *without* `-A`, so it never consults the helper and fails with
"sudo: a password is required" for App Store apps that need elevation. The failure is clean (that
item fails, the batch continues) but the dialog never appears. Tracked in
[12 — Roadmap](12-roadmap.md).

### Filesystem

`freshet_dir()` creates `~/.freshet` and forces `0700` on **every call**, not just at creation —
so a directory that already exists with looser permissions is corrected. `write_private()` writes
and then chmods explicitly rather than relying on the process umask.

| File | Mode | Why it matters |
|---|---|---|
| `signing.json` | 0600 | Writable ⇒ pre-seed a hijacker's Team ID ⇒ the warning never fires |
| `askpass.sh` | 0700 | Writable ⇒ capture the user's sudo password |
| `run.sh` | 0700 | Writable ⇒ code execution on a daily launchd schedule |

The threat here is other local accounts and any process running as another user — not a remote
attacker. On a single-user Mac it is nearly moot; it costs nothing and is correct on a shared
machine.

### Tauri surface

The webview gets the minimum:

```json
"permissions": ["core:default", "core:window:allow-start-dragging"]
```

No `shell` plugin (which would make `open_url` trivial and the injection surface enormous), no
`opener` plugin, no `fs` plugin, no `http` plugin. `open_url` is a custom command precisely so
the URL is validated in Rust before reaching `open`.

CSP:

```
default-src 'self';
img-src 'self' data: asset: https://asset.localhost;
style-src 'self' 'unsafe-inline';
font-src 'self';
connect-src 'self' ipc: http://ipc.localhost
```

No remote scripts, no remote fonts, no remote connections. Fonts are self-hosted `woff2` in
`src/assets/fonts/` specifically so `font-src 'self'` can hold. `style-src 'unsafe-inline'` is
required by React's inline style attributes and is the one relaxation.

The dev CSP additionally allows the Vite HMR websocket and `unsafe-eval`; that applies only to
`pnpm tauri dev`.

### Dependencies

The Rust tree is deliberately small: `tauri`, `tauri-plugin-positioner`, `serde`, `serde_json`,
`semver`, `dirs`. The frontend has three runtime dependencies: `react`, `react-dom`,
`@tauri-apps/api`.

`cargo audit` runs in CI. One advisory is ignored with a documented, verified justification:

```
RUSTSEC-2024-0429 — glib unsoundness, reached only through the Linux GTK/WebKitGTK chain that
Tauri pulls in on Ubuntu. Verified absent from the macOS graph:
  cargo tree --target aarch64-apple-darwin | grep -iE 'glib|gtk|webkit2gtk'   → 0 matches
Freshet ships macOS-only. Remove this ignore if a Linux target is ever added.
```

**Adding an ignore requires that shape of evidence**, not an assertion. A prior audit found two
high-severity `quick-xml` DoS advisories reachable through `plist`; they were fixed by a
dependency bump, not an ignore.

`unsafe_code = "deny"` at the crate level. There is no unsafe code and no FFI.

## Reviewing a security-relevant change

If a pull request touches any of the following, it needs security review:

- **Anything that builds an argv** — new command, new arm in `build_update_command`, new
  `Command::new`. Is every interpolated value validated? Does it go through the chokepoint?
- **Anything that fetches a URL** — does it use `fetch_url`? Does the URL pass
  `is_safe_feed_url`? Is there a size cap and a timeout?
- **Anything that writes to `~/.freshet`** — does it use `write_private` with the right mode?
- **Any new Tauri capability or plugin** — what exactly does it grant the webview, and is there a
  narrower custom command that would do?
- **Any loosened validator** — with a test demonstrating the newly-allowed input is safe.
- **Any new dependency** — what does it pull in transitively, and is it maintained?
- **Anything that makes Freshet download or execute an artifact** — this is the architectural
  line; it needs a design discussion, not a code review.

The repository-level guarantee that backs all of this is that the checks are mechanical:
`cargo clippy -- -D warnings` at pedantic level, `cargo audit`, and 50 Rust unit tests including
the adversarial validator cases, all run in CI on every push and pull request.

## Dependency audit exceptions

`pnpm audit` must be clean; anything it flags is fixed or listed here with the reason.

- **GHSA-vfj7-8cjw-p6xm — `braces` ≤ 3.0.3, stack exhaustion on deeply nested patterns.**
  No patched release exists (3.0.3 is the latest). It's reached only through `type-coverage`
  (dev/CI only, never shipped), which globs the project's own sources with fixed patterns — the
  attack needs an attacker-supplied pattern. Ignored via `pnpm.auditConfig.ignoreGhsas` in
  `package.json`. Revisit if `braces` ships a fix or `type-coverage` changes its glob library.

Transitive dev-tool dependencies with available fixes are pinned to patched floors with scoped
`pnpm.overrides` (e.g. `"postcss@8": "^8.5.23"`), each limited to the affected major line.

