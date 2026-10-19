#!/bin/sh
set -eu

version="${1:-}"
if [ -z "$version" ]; then
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
fi
if [ -z "$version" ]; then
    echo "could not determine package version" >&2
    exit 1
fi

case "$(uname -s)" in
    Darwin) os_name=macos ;;
    Linux) os_name=linux ;;
    *) os_name=other ;;
esac
arch="$(uname -m)"
archive_base="warraqcraft-$version-$os_name-$arch"
stage="dist/$archive_base"
tarball="$stage.tar.gz"

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    cargo build --release -p warraqcraft -p warraqcraft-cli
fi

rm -rf "$stage"
rm -f "$tarball" "dist/$archive_base.dmg" "dist/$archive_base.deb"
mkdir -p "$stage"

cp target/release/warraqcraft target/release/warraqcraft-cli "$stage/"
cp README.md LICENSE-APACHE LICENSE-MIT NOTICE "$stage/"
cp -R templates "$stage/templates"
if [ "${INCLUDE_TECTONIC:-0}" = "1" ] && [ -f bin/tectonic ]; then
    mkdir -p "$stage/bin"
    cp bin/tectonic "$stage/bin/tectonic"
fi

tar -C dist -czf "$tarball" "$archive_base"
echo "$tarball"

if [ "$os_name" = "macos" ]; then
    app="dist/WarraqCraft.app"
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp target/release/warraqcraft "$app/Contents/MacOS/warraqcraft"
    cp target/release/warraqcraft-cli "$app/Contents/MacOS/warraqcraft-cli"
    cp -R templates "$app/Contents/Resources/templates"
    cp README.md LICENSE-APACHE LICENSE-MIT NOTICE "$app/Contents/Resources/"
    cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>WarraqCraft</string>
<key>CFBundleDisplayName</key><string>WarraqCraft</string>
<key>CFBundleIdentifier</key><string>dev.warraqcraft.studio</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleExecutable</key><string>warraqcraft</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
EOF
    if command -v hdiutil >/dev/null 2>&1; then
        hdiutil create -volname WarraqCraft -srcfolder "$app" -ov -format UDZO "dist/$archive_base.dmg" >/dev/null
        echo "dist/$archive_base.dmg"
    fi
elif [ "$os_name" = "linux" ] && command -v dpkg-deb >/dev/null 2>&1; then
    # Stage on a native filesystem: drvfs/network checkouts report 777
    # and ignore chmod, and dpkg-deb refuses DEBIAN dirs above 0775.
    stage_root="$(mktemp -d)"
    pkgroot="$stage_root/deb-root"
    mkdir -p "$pkgroot/DEBIAN" "$pkgroot/usr/bin" "$pkgroot/usr/share/applications" \
        "$pkgroot/usr/share/warraqcraft" "$pkgroot/usr/share/doc/warraqcraft"
    cp target/release/warraqcraft target/release/warraqcraft-cli "$pkgroot/usr/bin/"
    cp -R templates "$pkgroot/usr/share/warraqcraft/templates"
    cp README.md LICENSE-APACHE LICENSE-MIT NOTICE "$pkgroot/usr/share/doc/warraqcraft/"
    cat > "$pkgroot/usr/share/applications/warraqcraft.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=WarraqCraft
Comment=Local-first LaTeX authoring and typesetting studio
Exec=warraqcraft %F
Terminal=false
Categories=Office;Development;
MimeType=text/x-tex;
Keywords=LaTeX;TeX;editor;
StartupNotify=true
EOF
    cat > "$pkgroot/DEBIAN/control" <<EOF
Package: warraqcraft
Version: $version
Section: editors
Priority: optional
Architecture: amd64
Maintainer: WarraqCraft contributors
Description: Local-first LaTeX authoring and typesetting studio
EOF
    # Normalize permissions for the staged copy.
    find "$pkgroot" -type d -exec chmod 755 {} +
    find "$pkgroot" -type f -exec chmod 644 {} +
    chmod 755 "$pkgroot/usr/bin/warraqcraft" "$pkgroot/usr/bin/warraqcraft-cli"
    dpkg-deb --build "$pkgroot" "dist/$archive_base.deb" >/dev/null
    rm -rf "$stage_root"
    echo "dist/$archive_base.deb"
fi
