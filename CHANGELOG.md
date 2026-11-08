# Changelog — Stria Launcher

All notable changes to the Launcher. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The first tagged release is
pending; the `0.1.0` section is the release candidate baseline.

## Unreleased

### Changed

- macOS first-run redesign: overlay title bar with a draggable, traffic-light
  aware header; the Portal's paper form surface beside the obsidian brand panel.
- Sign-in is the default tab and paired Macs show **Welcome back** with the
  account, workspace, prefilled email, **Open hub** and **Use another account**.
- Email copy resets when switching between Sign in and Create workspace, so
  paired-account wording never leaks into a fresh sign-in.
- Release readiness gate: `VERSION`, `package.json`, `tauri.conf.json`, the
  README metadata block, the changelog section and the tag must agree.

### Added

- Browser → launcher handoff: the Portal Downloads page mints a single-use
  `stria://pair?code=…` link, pairs without a second password, and keeps the
  code visible as a manual fallback.
- `cargo test` guards the inline UI script hash against the Tauri CSP, so a UI
  edit can no longer ship a blank window.

## 0.1.0 — first release (pending tag)

### Added

- One native first-run window for macOS (Apple silicon), Windows x64 and
  Linux x64, built with Tauri 2.
- Detects existing Stria Works / Stria-Pi installs and installs or updates the
  suite, verifying a downloaded asset against its SHA-256.
- Organization sign-in and registration against the Portal, with a
  workspace-required TOTP second factor and one-time recovery codes.
- Machine pairing: a Portal-minted single-use code stores a machine token in
  the OS keychain (macOS Keychain, Windows Credential Vault, Linux Secret
  Service).
- `stria://pair?code=…` deep link: a Portal machine row wakes the Launcher and
  pairs without typing, from a cold start or an already-running instance.
- Handoff to Stria Works with a tray icon and updater kept alive.
- Origin pinning through `STRIA_PORTAL_ORIGIN`; the header pill says
  Production / Staging / Dev.
- Fail-closed 3-OS release pipeline with signing, notarization, `signtool`
  verification, exact asset names and `sha256sums.txt`.

### Security

- Origin validation is pinned; the window CSP is explicit.
- No signing or keychain material is stored in the repository.
