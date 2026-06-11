#!/bin/bash
# Build iris-gui as a local .app bundle for macOS testing.
#
# Running the binary directly (cargo run / ./iris-gui) will always open
# inside your current Terminal window. This script wraps it in a proper
# .app bundle so you can launch it with  open IRIS.app  — just like users
# do — and the Terminal window stays closed.
#
# Usage:
#   ./scripts/build-macos.sh            # standard build
#   ./scripts/build-macos.sh lightning  # enable iris/lightning feature
#
# After it finishes:
#   open IRIS.app

set -e

VARIANT="${1:-standard}"

# ── Architecture ────────────────────────────────────────────────────────────

ARCH=$(uname -m)
if [ "$ARCH" = "arm64" ]; then
    TARGET="aarch64-apple-darwin"
elif [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-apple-darwin"
else
    echo "Unsupported architecture: $ARCH" >&2
    exit 1
fi

# ── Bundle ID — derived from the git remote so any fork gets the right ID ──
# git@github.com:owner/repo.git  →  io.github.owner.repo
# https://github.com/owner/repo  →  io.github.owner.repo

REMOTE_URL=$(git remote get-url origin 2>/dev/null || echo "")
if [[ "$REMOTE_URL" =~ github\.com[:/]([^/]+)/([^/.]+) ]]; then
    BUNDLE_ID="io.github.${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
else
    # Fallback: read from Cargo.toml if present, otherwise use a default
    BUNDLE_ID=$(grep -m1 '^bundle_id\s*=' iris-gui/Cargo.toml 2>/dev/null \
        | sed 's/.*"\(.*\)".*/\1/' || echo "io.github.unknown.iris")
fi

echo "Building iris-gui ($VARIANT) for macOS ($ARCH)..."
echo "  Bundle ID: $BUNDLE_ID"

# ── Build ───────────────────────────────────────────────────────────────────

if [ "$VARIANT" = "lightning" ]; then
    cargo build --release --target "$TARGET" -p iris-gui --features iris/lightning
else
    cargo build --release --target "$TARGET" -p iris-gui
fi

VERSION=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
    | python3 -c "import sys,json; pkgs=json.load(sys.stdin)['packages']; \
      print(next(p['version'] for p in pkgs if p['name']=='iris-gui'))" 2>/dev/null \
    || echo "0.0.0-local")

# ── Bundle ──────────────────────────────────────────────────────────────────

BUNDLE="IRIS.app"
rm -rf "$BUNDLE"
mkdir -p "${BUNDLE}/Contents/MacOS" "${BUNDLE}/Contents/Resources"

cp "target/${TARGET}/release/iris-gui" "${BUNDLE}/Contents/MacOS/iris-gui"
chmod +x "${BUNDLE}/Contents/MacOS/iris-gui"

if [ -f "iris-gui/assets/icons/icon.icns" ]; then
    cp "iris-gui/assets/icons/icon.icns" "${BUNDLE}/Contents/Resources/AppIcon.icns"
fi

cat > "${BUNDLE}/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>IRIS</string>
    <key>CFBundleDisplayName</key><string>IRIS</string>
    <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleExecutable</key><string>iris-gui</string>
    <key>CFBundleIconFile</key><string>AppIcon.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSMinimumSystemVersion</key><string>10.13</string>
    <key>NSCameraUsageDescription</key><string>Provides the IndyCam video input for SGI Indy emulation (VINO device).</string>
</dict>
</plist>
EOF

# ── Sign ────────────────────────────────────────────────────────────────────

echo "Signing bundle..."
if [ -f "installer/iris-gui.entitlements" ]; then
    codesign --force --deep --sign - --entitlements installer/iris-gui.entitlements "${BUNDLE}"
else
    codesign --force --deep --sign - "${BUNDLE}"
fi

echo ""
echo "Done: ${BUNDLE} (${VARIANT}, bundle ID: ${BUNDLE_ID})"
echo ""
echo "Launch without Terminal:"
echo "  open ${BUNDLE}"
echo ""
echo "Or double-click IRIS.app in Finder."
