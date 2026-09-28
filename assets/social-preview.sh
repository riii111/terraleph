#!/usr/bin/env bash
# Renders assets/social-preview.png (1280x640, for the repository Social preview setting).
# Reuses the hidden setup and cleanup blocks of assets/demo.tape.
set -euo pipefail
cd "$(dirname "$0")/.."

chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Card, window placement on the card, and the recorded terminal (scaled by 0.8).
card_width=1280 card_height=640 window_x=372 window_y=32 window_width=880 window_height=576
width=1100 height=720 margin=20 radius=10 fill=0x808080
terminal_bg=0x1c1c2c tint=0x1e1e2e tint_opacity=0.72

{
  cat <<EOF
Set Shell bash
Set FontFamily "Menlo"
Set FontSize 13
Set LineHeight 1.2
Set Width $width
Set Height $height
Set Padding 16
Set Margin $margin
Set MarginFill "#808080"
Set WindowBar Colorful
Set BorderRadius $radius
Set Theme "Catppuccin Mocha"
EOF
  awk '/^Hide$/ { copying = 1 } /^Show$/ { exit } copying' assets/demo.tape
  cat <<EOF
Show
Type "2"
Sleep 400ms
Space
Sleep 800ms
Down
Sleep 400ms
Down
Sleep 1200ms
Screenshot "$tmp/window.png"
Sleep 500ms
Type "q"
Sleep 700ms
Enter
Sleep 1s
EOF
  awk '/^Hide$/ { block = "" } { block = block $0 "\n" } END { printf "%s", block }' assets/demo.tape
} >"$tmp/window.tape"
vhs -q -o "$tmp/window.gif" "$tmp/window.tape"

blob() { echo "exp(-(pow(X-$1,2)+pow(Y-$2,2))/$3)"; }
warm=$(blob 230 540 180000) cool=$(blob 1120 80 160000) pink=$(blob 840 660 120000)
ffmpeg -v error -y -f lavfi -i "color=black:s=${card_width}x${card_height},format=rgb24" -vf "geq=\
r='min(255,24+190*$warm+20*$cool+150*$pink)':\
g='min(255,32+105*$warm+150*$cool+50*$pink)':\
b='min(255,58+30*$warm+170*$cool+150*$pink)'" -frames:v 1 "$tmp/wallpaper.png"

# Same glass treatment as assets/demo.sh, drawn at full size and then scaled onto the card.
inner=$((margin + radius))
filter="[0:v]crop=${window_width}:${window_height}:${window_x}:${window_y},scale=${width}:${height},format=rgba[behind];\
[1:v]split=3[key][wide][tall];\
[key]colorkey=${fill}:0.12:0[keyed];\
[wide]crop=$((width - 2 * inner)):$((height - 2 * margin)):${inner}:${margin}[wide_crop];\
[tall]crop=$((width - 2 * margin)):$((height - 2 * inner)):${margin}:${inner}[tall_crop];\
[keyed][wide_crop]overlay=${inner}:${margin}[partial];\
[partial][tall_crop]overlay=${margin}:${inner},format=rgba,split[shape][window];\
[shape]alphaextract,format=gray[mask];\
color=c=${tint}:s=${width}x${height},format=rgba[tint_color];\
[tint_color][mask]alphamerge,colorchannelmixer=aa=${tint_opacity}[glass];\
[behind][glass]overlay=format=rgb[backdrop];\
color=c=${terminal_bg}:s=${width}x${height}[flat];\
[flat][window]overlay=format=rgb,format=rgba,colorkey=${terminal_bg}:0.005:0.02[text];\
[backdrop][text]overlay=format=rgb,scale=${window_width}:${window_height}:flags=lanczos[scaled];\
[0:v][scaled]overlay=${window_x}:${window_y},format=rgb24"
ffmpeg -v error -y -i "$tmp/wallpaper.png" -i "$tmp/window.png" -filter_complex "$filter" \
  -frames:v 1 "$tmp/card_bg.png"

# The Homebrew ffmpeg lacks drawtext, so the title is laid out by headless Chrome.
cat >"$tmp/card.html" <<'EOF'
<!doctype html><meta charset="utf-8">
<style>
html,body{margin:0;width:1280px;height:640px;overflow:hidden}
body{background:url(card_bg.png) no-repeat;font-family:-apple-system,"SF Pro Display","Helvetica Neue",sans-serif;color:#fff}
.text{position:absolute;left:52px;top:0;bottom:0;width:300px;display:flex;flex-direction:column;justify-content:center;text-shadow:0 2px 12px rgba(20,16,40,.45)}
h1{margin:0;font-size:60px;font-weight:700;letter-spacing:-1.5px;line-height:1}
p{margin:22px 0 0;font-size:24px;line-height:1.35;font-weight:500;color:rgba(255,255,255,.92)}
.tags{margin-top:28px;font:600 15px ui-monospace,Menlo,monospace;letter-spacing:.3px;color:rgba(255,255,255,.8)}
</style>
<div class="text">
<h1>Terraleph</h1>
<p>Review and apply Terraform or OpenTofu plans in your terminal.</p>
<div class="tags">compare · review · apply</div>
</div>
EOF
"$chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=${card_width},${card_height} --screenshot="$tmp/card.png" "file://$tmp/card.html" 2>/dev/null
cp "$tmp/card.png" assets/social-preview.png
