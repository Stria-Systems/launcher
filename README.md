# Stria Launcher

The single entry point to the Stria platform. One small native window per OS
that installs Stria Works + Stria-Pi, prompts workspace registration on first
run, pairs the machine to the Stria portal, and hands off to the desktop app.

**This is the repo the striasystems.com/download buttons pull from.** Release
artifacts must carry these exact names (the website matches them exactly):

| Platform | Asset name |
| --- | --- |
| macOS (Apple silicon) | `Stria-Launcher-macos-arm64.dmg` |
| Windows (x86_64) | `Stria-Launcher-windows-x64-setup.exe` |
| Linux (x86_64) | `Stria-Launcher-linux-x86_64.AppImage` |
| Linux (Debian/Ubuntu) | `Stria-Launcher-linux-x86_64.deb` |
| Checksums | `sha256sums.txt` |

Tag a release (`v0.1.0`, …) and GitHub Actions builds all three platforms and
publishes the assets above. The website resolves `releases/latest` and every
"Download for …" button becomes a direct download.

## Stack

Tauri 2 (Rust core + system webview): ~3 MB installers instead of ~100 MB
Electron, native auto-update, per-OS signing story that actually works.

## Development

```sh
npm install
npm run tauri dev    # native window, hot reload
```

## First-run flow (implemented in src-tauri/src/main.rs commands + UI)

1. Detect existing Stria Works / Stria-Pi installs; install or update if missing
2. Sign in to your organization → `POST /api/auth/login` (or register)
3. Two-factor: every fresh sign-in on an enrolled account asks for a new
   authenticator code (`verify_two_factor`); workspaces can require
   enrollment per company (owner/admin in portal Settings → Sign-in
   security). No trusted-device bypass — relogin after logout, expired
   session, or a new machine pairing all challenge again.
4. Mint a pairing code in the portal → pair this machine → machine token stored in OS keychain
5. Hand off: launch Stria Works, keep tray icon + updater alive

The header pill always shows which portal the launcher signs into
(Production / Staging / Dev) — it reads `STRIA_PORTAL_ORIGIN`, so the
developer login flow never changes, only the target does.

## Staging sign-in validation (before trusting production logins)

```sh
# Point the real launcher binary at the staging portal build:
STRIA_PORTAL_ORIGIN=https://<staging-portal-pages-url> npm run tauri dev
```

Same code, same portal build family — a green staging pass validates prod:

- [ ] Password login with no 2FA → straight to hub (unchanged path)
- [ ] Enrolled operator: login → 6-digit step → hub; wrong code → clear error
- [ ] Recovery code signs in once; reuse fails
- [ ] `require_2fa` workspace, unenrolled operator: setup panel → secret →
      confirm → recovery codes shown once → re-login challenges
- [ ] 5 wrong codes → 15-minute lockout, then a fresh login works
- [ ] Pairing codes still single-use (replay 401s)
- [ ] Portal Machines → "Open in launcher" (`stria://pair?code=…`) wakes the
      launcher and pairs without typing (manual OS-click check per platform;
      cold start and already-running both land in the same validated flow)
- [ ] Pill reads `Staging · …` the whole time; production build reads `Production`

## Release

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` builds macOS (aarch64), Windows (x64), and
Linux (x86_64 AppImage + deb), writes `sha256sums.txt`, and attaches
everything to the release. macOS signing/notarization and Windows signing
activate when certificates are added as repo secrets; unsigned builds still
publish and download.
