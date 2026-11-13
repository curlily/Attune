#!/usr/bin/env bash
# Render the installer's sidebar and header images from their HTML sources
# (src-tauri/installer/*.html) to the 24-bit BMPs NSIS needs. Windows only:
# uses Microsoft Edge (headless) and System.Drawing.
#
#   scripts/render-installer-images.sh
set -euo pipefail
cd "$(dirname "$0")/../src-tauri/installer"

EDGE="/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"
[ -x "$EDGE" ] || EDGE="/c/Program Files/Microsoft/Edge/Application/msedge.exe"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

render() { # name width height
  local name=$1 width=$2 height=$3
  "$EDGE" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
    --window-size="$width,$height" --screenshot="$(cygpath -w "$WORK/$name.png")" \
    "file:///$(cygpath -m "$PWD/$name.html")" >/dev/null 2>&1
  # NSIS loads plain 24-bit bitmaps; drop the PNG's alpha channel.
  powershell.exe -NoProfile -Command "
    Add-Type -AssemblyName System.Drawing
    \$png = [System.Drawing.Image]::FromFile('$(cygpath -w "$WORK/$name.png")')
    \$bmp = New-Object System.Drawing.Bitmap $width, $height, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    \$g = [System.Drawing.Graphics]::FromImage(\$bmp)
    \$g.DrawImage(\$png, 0, 0, $width, $height)
    \$bmp.Save('$(cygpath -w "$PWD/$name.bmp")', [System.Drawing.Imaging.ImageFormat]::Bmp)
    \$g.Dispose(); \$bmp.Dispose(); \$png.Dispose()"
  echo "Rendered $name.bmp (${width}x$height)"
}

render sidebar 328 628
render header 300 114
