#!/bin/sh
# Builds VectorCut.app in dist/: a release build of the app binary inside a minimal bundle,
# signed ad hoc so it runs on this Mac. Distributing it to others needs a Developer ID
# signature and notarization instead.
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release --locked -p vectorcut-app
app=dist/VectorCut.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/VectorCut "$app/Contents/MacOS/VectorCut"
sed "s/__VERSION__/$version/g" macos/Info.plist > "$app/Contents/Info.plist"
cp LICENSE "$app/Contents/Resources/LICENSE"

# The app icon: Icon.jpeg cropped to its centred square, at every size an .icns holds.
work=$(mktemp -d)
side=$(sips -g pixelHeight Icon.jpeg | awk '/pixelHeight/ {print $2}')
sips -s format png --cropToHeightWidth "$side" "$side" Icon.jpeg --out "$work/square.png" >/dev/null
mkdir "$work/AppIcon.iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size "$work/square.png" --out "$work/AppIcon.iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double "$work/square.png" --out "$work/AppIcon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/AppIcon.iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$work"
codesign --force --sign - "$app" >/dev/null
echo "Built $app ($version)"
