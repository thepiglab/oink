#!/usr/bin/env bash
# Builds "Oink.app" and a .dmg into dist/.
# The app is universal (Apple silicon + Intel) when both Rust targets are installed:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="dist/Oink.app"

targets=$(rustup target list --installed 2>/dev/null || true)
if grep -q aarch64-apple-darwin <<<"$targets" && grep -q x86_64-apple-darwin <<<"$targets"; then
  rustup run stable cargo build --release --target aarch64-apple-darwin
  rustup run stable cargo build --release --target x86_64-apple-darwin
  mkdir -p target/universal
  lipo -create -output target/universal/oink \
    target/aarch64-apple-darwin/release/oink target/x86_64-apple-darwin/release/oink
  BIN=target/universal/oink
else
  cargo build --release
  BIN=target/release/oink
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/oink"

# The icon is drawn by the app itself.
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  "$BIN" --write-icon "$ICONSET/icon_${s}x${s}.png" "$s"
  "$BIN" --write-icon "$ICONSET/icon_${s}x${s}@2x.png" "$((s * 2))"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Oink</string>
  <key>CFBundleDisplayName</key><string>Oink</string>
  <key>CFBundleIdentifier</key><string>io.github.mkieffer1107.oink</string>
  <key>CFBundleExecutable</key><string>oink</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.medical</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>EEG recording</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Default</string>
      <key>LSItemContentTypes</key>
      <array><string>org.edfplus.edf</string><string>com.biosemi.bdf</string></array>
    </dict>
  </array>
  <key>UTImportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>org.edfplus.edf</string>
      <key>UTTypeDescription</key><string>European Data Format recording</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string></array>
      <key>UTTypeTagSpecification</key>
      <dict><key>public.filename-extension</key><array><string>edf</string><string>rec</string></array></dict>
    </dict>
    <dict>
      <key>UTTypeIdentifier</key><string>com.biosemi.bdf</string>
      <key>UTTypeDescription</key><string>BioSemi Data Format recording</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string></array>
      <key>UTTypeTagSpecification</key>
      <dict><key>public.filename-extension</key><array><string>bdf</string></array></dict>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Ad-hoc signature: enough to run on this Mac. Distributing to other Macs needs a Developer ID
# signature and notarisation.
codesign --force --sign - "$APP"

DMG="dist/Oink-${VERSION}-macos.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "Oink" -srcfolder "$APP" -ov -format UDZO "$DMG"
echo "Built $APP and $DMG"
