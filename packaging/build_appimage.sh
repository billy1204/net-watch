#!/usr/bin/env bash
# Build Net-Watch-x86_64.AppImage.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOL="${APPIMAGETOOL:-$(command -v appimagetool || echo "$HOME/.cache/kraken-rgb-tools/appimagetool-x86_64.AppImage")}"
[ -x "$TOOL" ] || { echo "Set APPIMAGETOOL to an appimagetool binary"; exit 1; }
cargo build --release
APPDIR=target/AppDir
rm -rf "$APPDIR" && mkdir -p "$APPDIR/usr/bin"
install -m755 target/release/net-watch "$APPDIR/usr/bin/"
install -m755 packaging/AppRun "$APPDIR/AppRun"
cp packaging/net-watch.desktop "$APPDIR/"
cp assets/icon.png "$APPDIR/net-watch.png"
ln -s net-watch.png "$APPDIR/.DirIcon"
install -Dm644 assets/icon.png "$APPDIR/usr/share/icons/hicolor/256x256/apps/net-watch.png"
install -Dm644 packaging/net-watch.desktop "$APPDIR/usr/share/applications/net-watch.desktop"
install -Dm644 -t "$APPDIR/usr/share/doc/net-watch" LICENSE THIRD-PARTY-LICENSES.md
OUT=Net-Watch-x86_64.AppImage
rm -f "$OUT.tmp"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 "$TOOL" --no-appstream "$APPDIR" "$OUT.tmp"
mv -f "$OUT.tmp" "$OUT"
echo "Built $OUT"
