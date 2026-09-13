# Stria Launcher — Agent Instructions

## Purpose

The Stria Launcher is the single entry point to the Stria platform. One small
native window per OS that installs Stria Works + Stria-Pi, prompts workspace
registration on first run, pairs the machine to the Stria portal, and hands
off to the desktop app.

**This is the repo the striasystems.com/download buttons pull from.** Release
artifacts must carry these exact names (the website matches them exactly):

| Platform | Asset name |
| --- | --- |
| macOS (Apple silicon) | `Stria-Launcher-macos-arm64.dmg` |
| Windows (x86_64) | `Stria-Launcher-windows-x64-setup.exe` |
| Linux (x86_64) | `Stria-Launcher-linux-x86_64.AppImage` |
| Linux (Debian/Ubuntu) | `Stria-Launcher-linux-x86_64.deb` |
| Checksums | `sha256sums.txt` |

## Stack

Tauri 2 (Rust core + system webview): ~3 MB installers instead of ~100 MB
Electron, native auto-update, per-OS signing story that actually works.

## Repository Layout

```
.
├── src-tauri/          # Rust backend (Tauri commands, auto-update, signing)
│   ├── src/main.rs     # Entry point + command handlers
│   ├── Cargo.toml      # Rust dependencies
│   └── tauri.conf.json # Tauri config (bundle ID, endpoints, permissions)
├── ui/                 # Frontend (static HTML/CSS/JS — no build step)
│   └── index.html      # Registration + pairing flow
├── .github/workflows/
│   ├── ci.yml          # fmt · clippy · test · frontend sanity
│   └── release.yml     # Build all 3 platforms, sign, publish
└── package.json        # npm scripts for tauri dev/build
```

## CI/CD

### CI (`ci.yml`)
- Runs on push to main and PRs
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --workspace`
- Frontend sanity: verify `ui/index.html` and `tauri.conf.json` exist

### Release (`release.yml`)
- Triggers on tags `v*`
- Builds macOS, Windows, Linux (AppImage + Deb)
- **Fail-closed gates**: all signing secrets must exist, all contract assets must exist
- Publishes to GitHub Releases with `generate_release_notes: true`

## Development

```sh
npm install
npm run tauri dev    # native window, hot reload
```

## First-run flow

1. Detect existing Stria Works / Stria-Pi installs; install or update if missing
2. Prompt workspace registration → `POST https://portal.striasystems.com/api/auth/register` (or login)
3. Mint a pairing code in the portal → pair this machine → machine token stored in OS keychain
4. Hand off: launch Stria Works, keep tray icon + updater alive

## Conventions

- Bundle ID: `com.striasystems.launcher`
- Tauri version: 2.x
- All portal API calls go through `src-tauri/src/portal_client.rs`
- Keychain access: `src-tauri/src/keychain.rs` (per-OS: macOS Keychain, Windows Credential Vault, Linux Secret Service)
- Auto-update: Tauri v2 updater with GitHub Releases as the source

## Testing

- Unit tests: `cargo test --workspace`
- E2E: manual verification of first-run flow on each OS
- CI does not run E2E (no GUI in runners)

## Security

- No secrets in the repo — all signing via GitHub Secrets
- Fail-closed: release blocked if any signing secret is missing
- Checksums published alongside artifacts
