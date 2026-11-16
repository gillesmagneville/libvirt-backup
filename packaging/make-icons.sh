#!/usr/bin/env bash
# Régénère les icônes PNG (data/icons/hicolor/NxN/apps/) à partir des SVG de data/ :
#   data/app.svg          icône complète       → 48, 64, 128, 256, 512 px
#   data/app-small.svg    variante simplifiée  → 16, 24, 32 px (formes plus épaisses, lisibles en petit)
#   data/app-symbolic.svg icône symbolique     (installée telle quelle, sans PNG)
# Les PNG sont versionnés : ce script n'est utile qu'après une modification des SVG.
# Outil requis : rsvg-convert (paquet librsvg2-bin) ou, à défaut, ImageMagick (convert).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
APP_ID=io.github.gillesmagneville.LibvirtBackup

render() { # render <svg> <taille> <png>
    mkdir -p "$(dirname "$3")"
    if command -v rsvg-convert >/dev/null; then
        rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
    elif command -v convert >/dev/null; then
        convert -background none -density 384 "$1" -resize "${2}x${2}" "$3"
    else
        echo "rsvg-convert (librsvg2-bin) ou ImageMagick est requis." >&2
        exit 1
    fi
}

for size in 16 24 32; do
    render data/app-small.svg "$size" "data/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
done
for size in 48 64 128 256 512; do
    render data/app.svg "$size" "data/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
done
echo "Icônes régénérées dans data/icons/hicolor/."
