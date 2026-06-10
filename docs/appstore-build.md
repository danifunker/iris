# Mac App Store Build

## Blocker: GPL3 is incompatible with the Mac App Store

`libchdman-rs` (the CHD feature) is GPL-3.0 licensed. The Mac App Store Terms of Service
require Apple's DRM and redistribution restrictions, which directly conflict with GPL-3.0's
requirement that users be free to run modified versions. **Apple will not accept a binary
that statically links GPL-3.0 code**, and in Rust all crates are statically linked.

This means the App Store build **cannot include `--features chd`**. Your options:

| Option | Feasibility |
|---|---|
| Ship without CHD on the App Store | ✅ Straightforward — CHD is a power-user feature; direct downloads can still have it |
| Find a non-GPL CHD library | ⚠️ MAME/chdman is GPL-2.0+ — no BSD/MIT alternative exists today |
| Obtain a GPL exception from all copyright holders | ❌ Impractical |

The recommended path: submit a **no-CHD lightning build** to the App Store. Users who need
CHD can download the direct release binary from GitHub. Document this clearly in the App
Store description.

---

## Yes, GitHub Actions can build and submit to the App Store

The full pipeline (build → sign → package → submit) is automatable. The submission step
uses the App Store Connect API, which is the modern replacement for `altool`.

---

## How it differs from the existing Developer ID build

| | Developer ID (current) | Mac App Store |
|---|---|---|
| Certificate type | Developer ID Application | Mac App Distribution |
| Packaging | `hdiutil` → `.dmg` | `productbuild` → `.pkg` |
| Submission | `notarytool` | App Store Connect API |
| Sandbox required | No | **Yes** |
| Provisioning profile | Not needed | **Required** |
| JIT allowed | Yes (hardened runtime) | Yes (with entitlement, since 2024) |

---

## App Sandbox entitlements

IRIS needs these entitlements in an `iris-gui.entitlements` plist at the repo root:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <!-- Required for App Store -->
    <key>com.apple.security.app-sandbox</key>
    <true/>

    <!-- Cranelift JIT (lightning build). Apple permits this in the App Store as of 2024. -->
    <key>com.apple.security.cs.allow-jit</key>
    <true/>

    <!-- IndyCam / VINO emulation -->
    <key>com.apple.security.device.camera</key>
    <true/>

    <!-- Reading/writing disk image files (.raw, .img) the user selects -->
    <key>com.apple.security.files.user-selected.read-write</key>
    <true/>

    <!-- Ethernet emulation (SEEQ 8003 / HPC3) -->
    <key>com.apple.security.network.client</key>
    <true/>
    <key>com.apple.security.network.server</key>
    <true/>
</dict>
</plist>
```

### Known sandbox risk: TUN/TAP networking

If IRIS uses a tun/tap interface for Ethernet emulation, that **will be blocked** by the
App Sandbox — system network extensions require a separate entitlement
(`com.apple.security.network.packet-filter`) that Apple almost never grants to App Store
apps. Test with the sandbox applied before submitting. The emulator may need to fall back
to userspace networking (e.g. slirp-style) when sandboxed.

---

## New secrets required

Add these to your GitHub repository secrets alongside the existing `MACOS_CERTIFICATE_*` set:

| Secret | What it is |
|---|---|
| `APP_STORE_CERTIFICATE` | Base64-encoded **Mac App Distribution** `.p12` — export from Keychain, `base64 -i cert.p12 \| pbcopy` |
| `APP_STORE_CERTIFICATE_PWD` | Password for the `.p12` |
| `APP_STORE_PROVISIONING_PROFILE` | Base64-encoded `.provisionprofile` — download from developer.apple.com → Profiles |
| `APP_STORE_CONNECT_KEY_ID` | Key ID from App Store Connect → Users and Access → Integrations → App Store Connect API |
| `APP_STORE_CONNECT_ISSUER_ID` | Issuer ID from the same page |
| `APP_STORE_CONNECT_PRIVATE_KEY` | Contents of the `.p8` key file (base64-encoded) |

**Getting the App Store Connect API key:**
1. Go to [App Store Connect → Users and Access → Integrations → App Store Connect API](https://appstoreconnect.apple.com/access/integrations/api)
2. Generate a key with **Developer** role (sufficient for uploads)
3. Download the `.p8` — you only get one chance to download it
4. `base64 -i AuthKey_XXXXXXXXXX.p8 | pbcopy` → paste as `APP_STORE_CONNECT_PRIVATE_KEY`

---

## What the GitHub Actions workflow step looks like

This would be a new job in `release.yml` (or a separate `appstore.yml`), running after
`build-macos` on a `workflow_dispatch` with an explicit opt-in input — you don't want
every nightly triggering an App Store submission.

```yaml
- name: Build iris-gui (App Store — no CHD, lightning)
  run: |
    cargo build --release --target ${{ matrix.target }} -p iris-gui \
      --features iris/lightning \
      --no-default-features \
      # iris-gui workspace dep already gates chd/camera/jit/rex-jit;
      # pass iris features explicitly without chd:
      # NOTE: requires iris-gui/Cargo.toml feature passthrough — see below

- name: Sign for App Store
  run: |
    /usr/bin/codesign \
      --force \
      --options runtime \
      --entitlements iris-gui.entitlements \
      --sign "$APP_STORE_IDENTITY" \
      "IRIS.app"

- name: Package with productbuild
  run: |
    productbuild \
      --component IRIS.app /Applications \
      --sign "$INSTALLER_IDENTITY" \
      "IRIS-appstore-${{ matrix.arch }}-${VER}.pkg"

- name: Submit to App Store Connect
  run: |
    # Write the .p8 key to a temp file
    echo "$APP_STORE_CONNECT_PRIVATE_KEY" | base64 --decode > /tmp/asc_key.p8
    xcrun altool --upload-app \
      --type osx \
      --file "IRIS-appstore-${{ matrix.arch }}-${VER}.pkg" \
      --apiKey    "$APP_STORE_CONNECT_KEY_ID" \
      --apiIssuer "$APP_STORE_CONNECT_ISSUER_ID" \
      --apiPrivateKeyPath /tmp/asc_key.p8
    rm /tmp/asc_key.p8
```

---

## Feature flag issue: excluding CHD from iris-gui

`iris-gui/Cargo.toml` currently hard-wires `iris = { features = ["chd", ...] }`.
To build without CHD for the App Store you need a way to opt out. Two approaches:

**Option A** — Add an App Store feature to iris-gui that overrides the chd dep:
```toml
# iris-gui/Cargo.toml
[features]
appstore = []  # disables chd at build time via cfg

[dependencies]
iris = { path = "..", features = ["camera", "jit", "rex-jit"] }  # remove chd here
```
Then conditionalize CHD in iris-gui's code with `#[cfg(not(feature = "appstore"))]`.
Cleanest long-term but requires code changes in iris-gui.

**Option B** — Separate workspace member `iris-gui-appstore` that lists iris features
without `chd`. More isolation, more maintenance.

**Option C** — Patch `iris-gui/Cargo.toml` in CI before building the App Store target.
Hacky but zero code changes:
```bash
sed -i '' 's/"chd", //' iris-gui/Cargo.toml
cargo build --release -p iris-gui --features iris/lightning
```

Option C is the quickest path to a first submission while you decide on a longer-term approach.

---

## App Store review considerations

- **Emulators**: Apple updated guidelines in April 2024 (Rule 4.7) to allow emulators.
  IRIS is a workstation emulator, not a gaming emulator — it may fall in a grey area.
  Frame it as a "vintage computer emulator / educational tool" in the App Store description.
- **IRIX media**: Users must supply their own IRIX install media. State this clearly in
  the App Store description to avoid a rejection for distributing unlicensed software.
- **JIT**: Explicitly mention the `com.apple.security.cs.allow-jit` entitlement in your
  review notes — reviewers sometimes flag it without reading the entitlements.
