# Releasing the Stria Launcher

**Release standard v1 — internal.** CI enforces the version/changelog/tag
contract; the release workflow enforces signing and the artifact contract.

## Cadence

| What | When |
|---|---|
| Weekly train | Every **Thursday at 16:00 UTC** |
| Freeze | **Wednesday at 16:00 UTC**. After freeze, only release-blocking fixes. |
| Hotfix | Any time, same gates, patch bump. |
| Minor / major | First train of the month. |

The Launcher ships only when all three operating systems build and verify.
There is no "partial" release: a missing platform blocks the tag.

## Versioning

[Semantic Versioning](https://semver.org/). Four places must agree:

1. `VERSION`
2. `package.json` → `version`
3. `src-tauri/tauri.conf.json` → `version`
4. `README.md` → the `release-meta` block (the human-facing copy)

CI fails on any disagreement. Tags are `vX.Y.Z` (for example `v0.1.0`).

## Channels

| Channel | Source | Audience |
|---|---|---|
| `main` | every merge | engineering, unsigned builds only |
| `vX.Y.Z-rc.N` | a release PR | staging / design partners |
| `vX.Y.Z` | the Thursday train | production, published on GitHub Releases |

`striasystems.com/download` resolves the latest release. A build that is not
tagged is not downloadable and must not be announced as one.

## Release checklist

1. **Freeze and confirm scope.** The release owner reads `CHANGELOG.md` under
   `## Unreleased` and confirms the train is complete.
2. **Bump the version.** Update `VERSION`, `package.json` and
   `src-tauri/tauri.conf.json` together. Move the changelog entries into
   `## X.Y.Z — YYYY-MM-DD`.
3. **Run the gates locally.**
   ```sh
   cd src-tauri
   cargo fmt --all -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test --workspace
   ```
4. **Confirm the signing secrets exist** in repository settings:
   `APPLE_CERTIFICATE_P12`, `APPLE_CERTIFICATE_PWD`,
   `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`,
   `APPLE_TEAM_ID`, `KEYCHAIN_PASSWORD`, `WINDOWS_CERT_PFX`,
   `WINDOWS_CERT_PWD`. `release.yml` fails closed without them.
5. **Tag and push.**
   ```sh
   git tag -a v0.1.0 -m "Stria Launcher 0.1.0"
   git push origin v0.1.0
   ```
   `release-readiness.yml` requires `v$(cat VERSION)`.
6. **Watch the matrix.** macOS (arm64 dmg), Windows (x64 setup exe), Linux
   (AppImage + deb). The publish job checks that every contract asset exists,
   writes `sha256sums.txt`, and creates the GitHub release.
7. **Verify the download path.** From a cold browser:
   - `https://github.com/Stria-Systems/launcher/releases/latest` resolves to
     the new tag.
   - Each asset name is exactly the contract name.
   - `sha256sums.txt` downloads and matches a local `shasum -a 256`.
8. **Smoke first run on each platform** using the staging Portal:
   install → sign in → 2FA → pair → hand off. The origin pill must read
   `Staging` for the staging run and `Production` for the production build.
9. **Announce.** Post the changelog section and the release URL in the team
   release channel.

## Required gates

| Gate | Workflow | What it protects |
|---|---|---|
| fmt / clippy / tests | `ci.yml` | the Rust core compiles and passes |
| `portal.json` v2 contract | `ci.yml` → `identity_tests` | org/employee/device separation |
| Frontend sanity | `ci.yml` | `ui/index.html` and `tauri.conf.json` exist |
| Version / README / changelog | `release-readiness.yml` | the record matches the build |
| Signed 3-OS matrix | `release.yml` | no unsigned or missing artifact |
| Checksums | `release.yml` | installs are verifiable |

## Ownership

The release owner rotates weekly across the platform group. Signing material
is held by the platform lead; the release owner may run the workflow but not
export a certificate. A blocked signing gate escalates to the platform lead —
never to the download page.

## Rollback

Tags are immutable. To roll back, re-point the website's download manifest to
the last good release where the product supports it, and cut a new patch from
the last good commit. Never delete or move a published tag.
