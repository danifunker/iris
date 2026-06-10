# Pipeline + App Store Handoff

## Repository layout

- `main` — upstream mirror of `techomancer/iris`. Nightly rebase keeps it in sync.
  Also holds the three workflow files so GitHub Actions can schedule and display them.
- `build-pipeline-danifunker` — working branch. All pipeline additions live here:
  `.github/workflows/`, `installer/`, `iris-gui/Cargo.toml` metadata,
  `iris-gui/iris-gui.desktop`, `iris-gui/iris-gui.entitlements`.

The release build always checks out `build-pipeline-danifunker` (or whatever
`BUILD_BRANCH` repo variable is set to).

---

## Workflow summary

| Workflow | File | Trigger |
|---|---|---|
| Sync Upstream | `sync-upstream.yml` | Nightly 2am UTC + manual |
| Release | `release.yml` | After sync finds new commits + manual |
| App Store | `appstore.yml` | Manual only |
| Rust (original CI) | `rust.yml` | Push / PR to main |

**Important:** `release.yml`, `sync-upstream.yml`, and `appstore.yml` must exist on
`main` (the default branch) for GitHub Actions to display them in the UI and run
scheduled jobs. When you update these files on `build-pipeline-danifunker`, copy
them to `main` as well:

```bash
git switch main
git checkout build-pipeline-danifunker -- .github/workflows/<changed-file>.yml
git commit -m "sync workflow file from build branch"
git push
git switch -
```

**Repo variable required:**
- `BUILD_BRANCH` = `build-pipeline-danifunker` (Settings → Secrets and variables → Actions → Variables)

---

## What the release pipeline builds

### iris-gui (GUI emulator)
- Windows x64: Inno Setup installer + portable zip (standard + lightning)
- macOS arm64 + x64: DMG with signed .app bundle (standard + lightning)
- Linux x64 + arm64: AppImage + deb + rpm + Arch pkg (standard + lightning)

### iris (headless CLI emulator)
- Windows x64 + x86: zip (standard + lightning)
- macOS arm64 + x64: tar.gz (standard + lightning)
- Linux x64 + arm64: tar.gz (standard + lightning)

### chd_extract (one-shot CHD → raw converter)
- Windows x64 + x86, macOS arm64 + x64, Linux x64 + arm64: zip/tar.gz

**Lightning builds** enable `iris/lightning` feature (disables debug paths, maximum speed).
All builds include `--features chd` — `libchdman-rs` (GPL-3.0).

**GPL-3.0 note:** All distributed binaries link `libchdman-rs` and must be treated as
GPL-3.0. Each archive ships `LICENSE` (BSD-3-Clause, iris source) and `LICENSE-GPL3.txt`
(downloaded from gnu.org at build time). The Windows installer shows a combined license
screen. See `docs/appstore-build.md` for the App Store licensing discussion.

---

## macOS signing secrets (for release pipeline)

Without these, builds use ad-hoc signing and are not notarized (Gatekeeper warns on launch).

| Secret | How to get it |
|---|---|
| `MACOS_CERTIFICATE` | Export Developer ID Application cert from Keychain as .p12, `base64 -i cert.p12 \| pbcopy` |
| `MACOS_CERTIFICATE_PWD` | Password set during .p12 export |
| `MACOS_NOTARIZE_APPLE_ID` | Your Apple ID email |
| `MACOS_NOTARIZE_PASSWORD` | App-specific password from appleid.apple.com |
| `MACOS_TEAM_ID` | 10-char Team ID from developer.apple.com → Membership |

---

## App Store pipeline secrets

All 8 must be populated before `appstore.yml` can run successfully.

| Secret | How to get it |
|---|---|
| `APP_STORE_APP_CERT` | Mac App Distribution cert → export from Keychain as .p12, base64 encode |
| `APP_STORE_APP_CERT_PWD` | Password set during export |
| `APP_STORE_INSTALLER_CERT` | Mac Installer Distribution cert → same process |
| `APP_STORE_INSTALLER_CERT_PWD` | Password set during export |
| `APP_STORE_PROVISIONING_PROFILE` | Mac App Store Connect .provisionprofile → base64 encode |
| `APP_STORE_CONNECT_KEY_ID` | App Store Connect → Users and Access → Integrations → API |
| `APP_STORE_CONNECT_ISSUER_ID` | Same page |
| `APP_STORE_CONNECT_PRIVATE_KEY` | .p8 file from same page (download once only), base64 encode |

App Store Connect setup required before first submission:
1. App ID `io.github.danifunker.iris` created at developer.apple.com ✅
2. Mac App Distribution provisioning profile created — **needs .provisionprofile not .mobileprovision**
3. App record created in App Store Connect (name: IRIS, bundle ID: `io.github.danifunker.iris`)
4. App Store Connect API key generated

---

## Known pipeline issues to fix

### ~~1. CFBundleVersion format rejected by App Store~~ ✅ Fixed
`appstore.yml` now converts `2025-06-09-02-00` → `20250609.0200` via `sed` before
writing Info.plist. `CFBundleVersion` and `CFBundleShortVersionString` both use the
converted `BUNDLE_VER`.

### ~~2. Provisioning profile type mismatch~~ ✅ Fixed (user)
Correct `.provisionprofile` created on developer.apple.com under **Mac App Store Connect**.

**Also fixed:** All three `actions/checkout` steps in `appstore.yml` now use
`vars.BUILD_BRANCH || 'build-pipeline-danifunker'` instead of `github.ref_name`, so
triggering from `main` no longer checks out the wrong branch and loses `installer/`.

### 3. Linux AppImage + packages untested end-to-end
The `build-linux-appimage` and `build-linux-packages` jobs have not had a successful run
verified. Watch the first run for:
- `cargo-deb` asset path resolution for workspace member (`-p iris-gui`)
- `cargo-generate-rpm` asset paths
- `quick-sharun` / `appimagetool` working for both standard and lightning builds
- Missing Linux library dependencies (`libv4l-dev`, wayland headers, etc.)

### ~~4. build-pipeline-danifunker not auto-rebased after nightly sync~~ ✅ Fixed
`sync-upstream.yml` now rebases `BUILD_BRANCH` onto `main` immediately after each
upstream sync. On conflict it aborts cleanly and files a GitHub issue instead of
failing silently.

### 5. x86 Windows build untested
The `i686-pc-windows-msvc` build for iris CLI and chd_extract has not been verified.
Confirm the first release run produces valid x86 zips.

---

## App changes needed for App Store acceptance

### 1. App Sandbox compatibility (most work)
The app must run correctly under macOS App Sandbox. The entitlements are declared in
`installer/iris-gui.entitlements` but the app has not been tested under sandbox.

Test locally:
```bash
# Temporarily sign with sandbox entitlements and run
codesign --force --entitlements installer/iris-gui.entitlements --sign - target/release/iris-gui
./target/release/iris-gui
```

Known likely sandbox failures:
- **TUN/TAP networking**: `/dev/tun*` access is blocked under sandbox. The Ethernet
  emulator (SEEQ 8003 / HPC3) will need a fallback to userspace/slirp-style networking
  when running sandboxed, or a graceful error message.
- **Serial port access**: `/dev/tty*` devices require `com.apple.security.device.serial`
  entitlement (add to `iris-gui.entitlements` if needed).
- **Arbitrary file access**: Only files the user explicitly opens via NSOpenPanel are
  accessible. Disk image paths saved in config may not be re-openable on next launch
  without security-scoped bookmarks.

### 2. Security-scoped bookmarks for disk images
When a user selects a disk image file, the sandbox only grants access for that session.
On next launch, the saved path in config is inaccessible unless the app stores a
security-scoped bookmark alongside the path and resolves it on load.
`rfd` (the file picker) does not handle this automatically — needs code changes in
iris-gui's config/disk image loading logic.

### 3. JIT entitlement review note
When submitting, add a note to the App Review team in App Store Connect:
> "This app emulates a MIPS R4400 CPU and requires the com.apple.security.cs.allow-jit
> entitlement for the Cranelift JIT compiler used in the lightning build. Without JIT,
> the app falls back to the interpreter. JIT is declared in the app's entitlements."

### 4. App Store description must state IRIX media requirement
Apple may reject the app if it appears to require unlicensed software to function.
The App Store description must clearly state:
> "Requires original SGI IRIX installation media. IRIX media is not included and is
> not available from this developer."

### 5. App category and age rating
In App Store Connect when creating the build submission:
- Category: **Utilities** (or Developer Tools if available on Mac)
- Age rating: 4+ (no objectionable content)

---

## Files added in this session

```
.github/workflows/release.yml          Full release pipeline
.github/workflows/sync-upstream.yml    Nightly upstream sync
.github/workflows/appstore.yml         Mac App Store build + submission
installer/iris-gui.iss                 Inno Setup script (Windows)
installer/iris-gui.entitlements        macOS App Sandbox entitlements
iris-gui/iris-gui.desktop              Linux .desktop entry
iris-gui/Cargo.toml                    Added description, license, deb/rpm metadata
docs/appstore-build.md                 App Store licensing discussion
docs/handoff-pipeline.md               This file
```
