#!/bin/sh
# Package the macOS executable as a double-clickable .app inside a zip.
# A bare Mach-O opened from Finder launches Terminal and carries no icon;
# only a bundle gets the Dock icon and a normal application launch.
set -eu

binary=$1   # path to the built executable
version=$2  # e.g. 0.1.0
label=$3    # e.g. macos-arm64

root=$(cd "$(dirname "$0")/.." && pwd)
# Assemble under target/: dist/ must hold publishable archives only, never a
# half-built bundle that a later step could pick up or publish by mistake.
stage=$root/target/macos-bundle
app=$stage/HandyBox.app
archive=$root/dist/handybox-$version-$label.zip

rm -rf "$stage"
mkdir -p "$root/dist" "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/handybox"
chmod 755 "$app/Contents/MacOS/handybox"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>HandyBox</string>
<key>CFBundleDisplayName</key><string>HandyBox</string>
<key>CFBundleIdentifier</key><string>dev.handybox.HandyBox</string>
<key>CFBundleExecutable</key><string>handybox</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>1</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
<key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict></plist>
PLIST

# PNG -> .icns with the tools that ship with macOS; no extra dependency.
iconset=$stage/AppIcon.iconset
rm -rf "$iconset"
mkdir -p "$iconset"
# Up to 256@2x = 512px, matching the source; larger variants would
# only upscale and would inflate the .icns.
for size in 16 32 128 256; do
    sips -z "$size" "$size" "$root/assets/macos-icon.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$root/assets/macos-icon.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

# Ad-hoc signature only: no Developer ID and no notarization, so first launch
# still needs the Gatekeeper override described in the README.
codesign --force --sign - "$app"

# ditto, not zip: it preserves the bundle's executable bits and symlinks, and is
# the command Apple's own notarization workflow uses.
rm -f "$archive"
ditto -c -k --keepParent "$app" "$archive"
# Only the archive is published; the expanded bundle is scratch.
rm -rf "$stage"
echo "Built $archive"
