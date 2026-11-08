# Stria Launcher

**Internal engineering reference.** Customer-facing copy lives in the website
repository. This file is the first thing a Stria engineer should read.

<!-- release-meta
surface: launcher
version: 0.1.0
-->

[![Launcher CI](https://github.com/Stria-Systems/launcher/actions/workflows/ci.yml/badge.svg)](https://github.com/Stria-Systems/launcher/actions/workflows/ci.yml)
[![Launcher release](https://github.com/Stria-Systems/launcher/actions/workflows/release.yml/badge.svg)](https://github.com/Stria-Systems/launcher/actions/workflows/release.yml)

The single entry point to the Stria platform: one small native window per OS
that installs Stria Works + Stria-Pi, registers the workspace, pairs the
machine to the Portal, and hands off to the desktop app.

| | |
|---|---|
| **Source version** | `0.1.0` |
| **Latest tag** | none yet — first train pending |
| **Distribution** | GitHub Releases → `striasystems.com/download` |
| **Release train** | Thursday 16:00 UTC · freeze Wednesday 16:00 UTC |
| **Next cut** | 2026-11-12 |
| **Release owner** | Platform rotation — [`RELEASING.md`](RELEASING.md) |
| **Change log** | [`CHANGELOG.md`](CHANGELOG.md) |

## What's important right now

- **Cut the first tagged release.** `v0.1.0` must publish all four contract
  assets plus `sha256sums.txt`. The website's download buttons resolve
  `releases/latest`; an untagged build is not downloadable.
- **No unsigned releases.** `release.yml` is fail-closed: every signing secret
  and every contract asset must exist or the publish job refuses to run.
  A macOS build must be signed, notarized, stapled and verified; a Windows
  build must pass `signtool verify`.
- **Pairing and sign-in are the product.** Install → sign in → 2FA (when the
  workspace requires it) → mint a pairing code in the Portal → pair → hand off.
  A failure here is the whole first-run experience.
- **Origin pinning.** The header pill reads the Portal origin from
  `STRIA_PORTAL_ORIGIN`. Staging validation must use the same binary and the
  same flow; production must read `Production`.
- **The asset names are a contract.** The website matches them exactly; do not
  rename an artifact to make a local script easier.

## What this is

Tauri 2 (Rust core + system webview): roughly 3 MB installers instead of
~100 MB Electron, native auto-update, and a per-OS signing story that works.

```
install / update Works + Pi
        │
        ▼
sign in to the organization  ──▶  Portal /api/auth/login
        │                         (TOTP challenge when enrolled)
        ▼
pair this machine  ──▶  machine token in the OS keychain
        │
        ▼
hand off to Stria Works   + tray icon / updater stay alive
```

### Browser → launcher handoff

A signed-in Portal session is already the identity. The Downloads page's
**Open in Stria Launcher** mints a single-use pairing code and opens
`stria://pair?code=…`; the launcher pairs and lands in the software hub without
asking for a password again. The same page keeps the code visible as a manual
fallback when the custom scheme is not installed.

On relaunch the launcher reads `~/.stria/portal.json` first. A valid machine
token goes straight to the hub; a stale one shows **Welcome back**, names the
paired account and workspace, prefills the email, and offers **Open hub** or
**Use another account**. Sign-in stays the default tab; Create workspace is the
secondary path.

### Asset contract

| Platform | Asset name |
|---|---|
| macOS (Apple silicon) | `Stria-Launcher-macos-arm64.dmg` |
| Windows (x86_64) | `Stria-Launcher-windows-x64-setup.exe` |
| Linux (x86_64) | `Stria-Launcher-linux-x86_64.AppImage` |
| Linux (Debian/Ubuntu) | `Stria-Launcher-linux-x86_64.deb` |
| Checksums | `sha256sums.txt` |

## Development

```sh
npm install
npm run tauri dev    # native window, hot reload
```

The frontend is static HTML/CSS/JS in `ui/` — no build step. The Rust core is
in `src-tauri/`; all Portal calls go through `src-tauri/src/portal_client.rs`,
and keychain access is per-OS in `keychain.rs`.

### Verify

```sh
cd src-tauri
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace identity_tests   # portal.json v2 contract
```

## Release and CI/CD

The cadence, versioning rules and cut checklist are in
**[`RELEASING.md`](RELEASING.md)**. Tags are `vX.Y.Z`.

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | push to `main`, PRs | fmt, clippy, identity contract, tests, frontend sanity |
| `release.yml` | tag `v*` | builds macOS + Windows + Linux, signs/notarizes, verifies, publishes |
| `release-readiness.yml` | PRs, `main`, tags | version ↔ README ↔ changelog ↔ tag, standard sections |

Staging sign-in validation (before trusting production logins):

```sh
STRIA_PORTAL_ORIGIN=https://<staging-portal-pages-url> npm run tauri dev
```

The release checklist covers: password login, enrolled TOTP, recovery code,
workspace-required enrollment, 5-code lockout, single-use pairing, the
`stria://pair?code=…` deep link (cold start and already-running), and the
origin pill.

## Security

- No secrets in the repository. Every signing material is a GitHub Secret.
- The publish job fails if any signing secret is missing; it never ships an
  unsigned artifact.
- Checksums are published alongside the assets; the Launcher verifies a
  downloaded suite asset before installing it.

## Related surfaces

| Surface | Where |
|---|---|
| Stria Works | `Stria-Systems/stria-works` — the Rust engine and native app |
| Portal | `Stria-Systems/stria-website` → `Portal/` — org, pairing, entitlements |
| Website | `Stria-Systems/stria-website` — download page and release manifest |

## Ownership

Owned by the platform group. Release owner rotates weekly; see
[`RELEASING.md`](RELEASING.md). Security reports follow the organization
policy; do not file a public issue for a signing or keychain defect.
