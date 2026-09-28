#!/usr/bin/env bash
# Renders assets/demo.gif from assets/demo.tape as a translucent window over a generated wallpaper.
set -euo pipefail
cd "$(dirname "$0")/.."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
vhs -q -o "$tmp/demo.gif" assets/demo.tape

# Must match Width, Height, Margin, BorderRadius and MarginFill in the tape.
width=1200 height=780 margin=20 radius=10 fill=0x808080
# Catppuccin Mocha base as rendered in the VHS GIF, and the tint left over the wallpaper.
terminal_bg=0x1c1c2c tint=0x1e1e2e tint_opacity=0.72

blob() { echo "exp(-(pow(X-$1,2)+pow(Y-$2,2))/$3)"; }
warm=$(blob 220 640 180000) cool=$(blob 1020 120 160000) pink=$(blob 760 760 120000)
ffmpeg -v error -y -f lavfi -i "color=black:s=${width}x${height},format=rgb24" -vf "geq=\
r='min(255,24+190*$warm+20*$cool+150*$pink)':\
g='min(255,32+105*$warm+150*$cool+50*$pink)':\
b='min(255,58+30*$warm+170*$cool+150*$pink)'" -frames:v 1 "$tmp/wallpaper.png"

inner_x=$((margin + radius)) inner_y=$((margin + radius))
# 1. Window: key out the margin fill outside the window only, so terminal colors survive.
# 2. Glass: darken the wallpaper inside the window, then show it where the terminal background was.
filter="[1:v]split=3[key][wide][tall];\
[key]colorkey=${fill}:0.12:0[keyed];\
[wide]crop=$((width - 2 * inner_x)):$((height - 2 * margin)):${inner_x}:${margin}[wide_crop];\
[tall]crop=$((width - 2 * margin)):$((height - 2 * inner_y)):${margin}:${inner_y}[tall_crop];\
[keyed][wide_crop]overlay=${inner_x}:${margin}[partial];\
[partial][tall_crop]overlay=${margin}:${inner_y},format=rgba,split[shape][window];\
[shape]alphaextract,format=gray[mask];\
color=c=${tint}:s=${width}x${height},format=rgba[tint_color];\
[tint_color][mask]alphamerge,colorchannelmixer=aa=${tint_opacity}[glass];\
[0:v]format=rgba[wallpaper];\
[wallpaper][glass]overlay=format=rgb[backdrop];\
color=c=${terminal_bg}:s=${width}x${height}[flat];\
[flat][window]overlay=shortest=1:format=rgb,format=rgba,colorkey=${terminal_bg}:0.005:0.02[text];\
[backdrop][text]overlay=shortest=1:format=rgb,format=rgb24,split[frames][palette_source];\
[palette_source]palettegen=stats_mode=full[palette];\
[frames][palette]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle[out]"
ffmpeg -v error -y -loop 1 -framerate 24 -i "$tmp/wallpaper.png" -i "$tmp/demo.gif" \
  -filter_complex "$filter" -map "[out]" "$tmp/composited.gif"
gifsicle -O3 "$tmp/composited.gif" -o assets/demo.gif 2>/dev/null
