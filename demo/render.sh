#!/bin/sh
# Renders the demo film to demo/out/freshet-demo.mp4 (1920×1080, 30 fps, H.264).
#   demo/render.sh            the whole film
#   demo/render.sh 25 34      just seconds 25–34 (quick preview of one scene)
# Needs Node 20+ (Vite 7), Xcode's swiftc and ffmpeg. Takes a few minutes; a small window shows
# the film while it renders.
set -eu
cd "$(dirname "$0")/.."
FPS=30
OUT=demo/out
FRAMES="$OUT/frames"
rm -rf "$FRAMES" && mkdir -p "$FRAMES"

# Real app icons for the list rows, taken from this Mac's /Applications (not committed: they're
# the apps' own artwork). Rows without one show a letter, as in the app.
mkdir -p demo/assets/icons
for app in "Visual Studio Code" Docker Figma Postman zoom.us Xcode; do
  [ -f "demo/assets/icons/$app.png" ] && continue
  icns=$(defaults read "/Applications/$app.app/Contents/Info" CFBundleIconFile 2>/dev/null) || continue
  sips -s format png -Z 128 "/Applications/$app.app/Contents/Resources/${icns%.icns}.icns" \
    --out "demo/assets/icons/$app.png" >/dev/null 2>&1 || true
done

if ! curl -s -o /dev/null http://localhost:1499/demo/film.html; then
  pnpm vite --config demo/vite.demo.mjs > "$OUT/vite.log" 2>&1 &
  VITE=$!
  trap 'kill $VITE 2>/dev/null' EXIT
  until curl -s -o /dev/null http://localhost:1499/demo/film.html; do sleep 0.5; done
fi

swiftc -O demo/render.swift -o "$OUT/render" 2>&1 | grep -v warning || true
"$OUT/render" "http://localhost:1499/demo/film.html" "$FRAMES" "$FPS" "${1:-0}" ${2:+"$2"}

NAME=freshet-demo
[ $# -gt 0 ] && NAME="freshet-demo-$1-${2:-end}"
ffmpeg -y -loglevel error -framerate "$FPS" -start_number "$(ls "$FRAMES" | head -1 | tr -dc 0-9 | sed 's/^0*//;s/^$/0/')" \
  -i "$FRAMES/f%05d.png" -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p -movflags +faststart "$OUT/$NAME.mp4"
echo "$OUT/$NAME.mp4"
