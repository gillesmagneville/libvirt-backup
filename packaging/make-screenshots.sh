#!/usr/bin/env bash
# Régénère les captures d'écran du README : screenshots/en/*.png et screenshots/fr/*.png.
#
# Besoin : xvfb-run, ImageMagick (import, convert) et la chaîne de compilation avec libgtk-4-dev.
# Les fenêtres sont rendues par la vraie interface sur une fausse racine (tests/gui_shots.rs), sous un
# écran virtuel : rien ne touche au système, et les captures sont reproductibles.
set -euo pipefail

cd "$(dirname "$0")/.."
for tool in xvfb-run import convert; do
    command -v "$tool" >/dev/null || { echo "Il manque « $tool »." >&2; exit 1; }
done

raw=$(mktemp -d)
trap 'rm -rf "$raw"' EXIT

# l'icône de l'application doit être trouvable par GTK (elle n'est pas installée sur la machine de dev)
mkdir -p "$raw/share/icons"
cp -r data/icons/hicolor "$raw/share/icons/"
export XDG_DATA_DIRS="$raw/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"

# recadre une fenêtre posée sur une autre : la zone qui diffère entre « avant » et « après »
crop_diff() { # avant après sortie
    local box
    box=$(convert "$1" "$2" -compose difference -composite -format '%@' info:)
    convert "$2" -crop "$box" +repage "$3"
}

for lang in en fr; do
    mkdir -p "$raw/$lang" "screenshots/$lang"
    LB_SHOTS_LANG=$lang xvfb-run -a -s "-screen 0 1280x900x24" \
        cargo test --locked --features gui --test gui_shots -- "$raw/$lang"
    out="screenshots/$lang"
    for page in main:backups service:service settings:settings; do
        convert "$raw/$lang/${page%%:*}.png" -trim +repage "$out/${page##*:}.png"
    done
    crop_diff "$raw/$lang/main-bis.png" "$raw/$lang/about.png" "$out/about.png"
    crop_diff "$raw/$lang/main-bis.png" "$raw/$lang/prefs.png" "$out/preferences.png"
    crop_diff "$raw/$lang/quit-before.png" "$raw/$lang/quit.png" "$out/quit.png"
done

# icône du README (l'icône vectorielle de l'application)
cp data/icons/hicolor/256x256/apps/io.github.gillesmagneville.LibvirtBackup.png screenshots/github-readme-icon.png
echo "Captures écrites dans screenshots/{en,fr}/."
