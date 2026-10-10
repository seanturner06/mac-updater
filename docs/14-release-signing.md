# 14 — Release Signing & Notarization

A security tool that asks for your admin password has to be signed and notarized itself.
Everything in the repo is set up; what's left needs your Apple Developer account.

## What's already in place

- `src-tauri/tauri.conf.json` → `bundle.macOS.hardenedRuntime: true` (required for notarization).
  `signingIdentity` is `null` so local dev builds stay unsigned; CI supplies it.
- `.github/workflows/release.yml` → on a `v*` tag: builds a universal (Apple Silicon + Intel) app,
  signs it with the hardened runtime, notarizes it with Apple, staples the ticket, attaches the
  `.dmg` to a **draft** GitHub release, then independently verifies the result with
  `codesign --verify --deep --strict`, `spctl --assess` and `stapler validate`. The workflow
  refuses to run if the signing secrets are missing, so an unsigned build can't be published by
  accident.

## One-time setup (yours)

1. **Apple Developer Program** membership ($99/yr): <https://developer.apple.com/programs/>.
2. **Developer ID Application certificate.** Xcode → Settings → Accounts → Manage Certificates →
   `+` → *Developer ID Application*. (Not "Apple Development" — that one can't be notarized.)
3. **Export it** from Keychain Access as a `.p12` with a password, then base64 it:
   ```sh
   base64 -i DeveloperID.p12 | pbcopy
   ```
4. **App-specific password** for notarization: <https://account.apple.com> → Sign-In and
   Security → App-Specific Passwords.
5. **Repository secrets** (GitHub → Settings → Secrets and variables → Actions):

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | the base64 from step 3 |
   | `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
   | `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID1234)` — `security find-identity -v -p codesigning` |
   | `APPLE_ID` | your Apple ID email |
   | `APPLE_PASSWORD` | the app-specific password from step 4 |
   | `APPLE_TEAM_ID` | your 10-character Team ID |
   | `KEYCHAIN_PASSWORD` | any random string (protects CI's temporary keychain) |
   | `TAURI_SIGNING_PRIVATE_KEY` | contents of `~/.tauri/freshet-updater.key` (see below) |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | **don't create it** while the key has no password (GitHub rejects empty secrets; a missing secret reads as empty, which is right) |

## Self-update signing key

Freshet updates itself with `tauri-plugin-updater`. Each release publishes `latest.json` and a
signed `Freshet.app.tar.gz`; the app checks `releases/latest/download/latest.json` daily and only
installs an archive whose signature verifies against `plugins.updater.pubkey` in
`tauri.conf.json`. This is separate from (and in addition to) Apple code signing.

A key pair was generated on 2026-10-10 at `~/.tauri/freshet-updater.key` (private, 0600) and
`~/.tauri/freshet-updater.key.pub` (public; already in `tauri.conf.json`).

- **Back up the private key** somewhere safe (a password manager). If it's lost, already-installed
  copies can never accept another update; users would have to reinstall by hand.
- To add a password, regenerate with
  `pnpm tauri signer generate -w ~/.tauri/freshet-updater.key -p '<password>' -f`, put the new
  `.pub` contents into `tauri.conf.json`, and update both secrets. Do this **before** the first
  public release — after it, rotating means shipping one release signed with the old key that
  carries the new public key.

## Releasing

```sh
# bump "version" in src-tauri/tauri.conf.json and package.json, commit, then:
git tag v0.2.0 && git push origin v0.2.0
```

Review and publish the draft release the workflow creates.

## Unsigned test releases (before you have an Apple Developer account)

`.github/workflows/test-release.yml` builds a test release without Apple signing. Run it from
GitHub → **Actions** → **Test release (unsigned)** → **Run workflow**. It needs only the
`TAURI_SIGNING_PRIVATE_KEY` secret.

- Tagged `v<version>-unsigned` and named "(unsigned test build)", created as a **draft**: review
  it under Releases and click **Publish**. It can't collide with a later signed `v<version>`.
- **Ad-hoc signed** (`signingIdentity: "-"`), so the bundle is properly sealed. Without that, a
  downloaded copy is reported as "damaged" and can't be opened at all.
- Not marked pre-release: self-update follows GitHub's "latest release", which skips
  pre-releases. Installed test builds therefore update to the next test build — and to the first
  signed release, which replaces them as "latest".
- Testers see "Apple could not verify Freshet is free of malware" once. The release notes tell
  them: click **Done**, then **System Settings → Privacy & Security → Open Anyway**. (On macOS 15
  and later, right-click → Open no longer bypasses this.)

To ship a new test build, bump the version (see Releasing) and run the workflow again.

## Verifying a build yourself

```sh
codesign --verify --deep --strict --verbose=2 Freshet.app
codesign -dvv Freshet.app 2>&1 | grep -E 'Authority|TeamIdentifier|Runtime'
spctl --assess --type execute --verbose=4 Freshet.app   # "source=Notarized Developer ID"
xcrun stapler validate Freshet.app
```

Freshet's own signer-drift check applies to Freshet too: once users have a signed build, a
release signed by any other Team ID would be flagged — keep the certificate safe.
