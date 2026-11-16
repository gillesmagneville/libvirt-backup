#!/usr/bin/env bash
# Construit les paquets Debian à partir des binaires de target/release :
#   libvirt-backup      ligne de commande + service de surveillance (aucune dépendance GTK)
#   libvirt-backup-gui  interface GTK4, règle polkit, entrée de menu, fiche AppStream, icônes (SVG, PNG, symbolique)
#
# Usage : packaging/build-deb.sh [dossier-de-sortie]        (défaut : dist/)
#   DEB_REVISION=2         révision Debian (défaut 1)
#   DEB_MAINTAINER="N <m>" champ Maintainer
#   SOURCE_DATE_EPOCH=...  dates fixes (construction reproductible)
# Compile d'abord : cargo build --release --locked --features gui   (ou « make deb »)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
OUT=${1:-dist}
REVISION=${DEB_REVISION:-1}
MAINTAINER=${DEB_MAINTAINER:-gillesmagneville <gillesmagneville@users.noreply.github.com>}
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
HOMEPAGE=$(sed -n 's/^repository *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
FULL="$VERSION-$REVISION"
EPOCH=${SOURCE_DATE_EPOCH:-$(date +%s)}
DATE_RFC=$(date -R -d "@$EPOCH")
DATE_MAN=$(date -u -d "@$EPOCH" +%Y-%m-%d)
BIN=target/release
APP_ID=io.github.gillesmagneville.LibvirtBackup
POLICY_FILE=io.github.gillesmagneville.libvirt-backup.run.policy

command -v dpkg-deb >/dev/null || { echo "dpkg-deb est introuvable (paquet dpkg)." >&2; exit 1; }
[ -n "$VERSION" ] || { echo "version introuvable dans Cargo.toml" >&2; exit 1; }
[ -x "$BIN/libvirt-backup" ] || { echo "$BIN/libvirt-backup est absent : lance « cargo build --release --locked --features gui »." >&2; exit 1; }
ARCH=$(dpkg --print-architecture)
HAVE_GUI=1
if [ ! -x "$BIN/libvirt-backup-gui" ]; then
    HAVE_GUI=0
    echo "ATTENTION : $BIN/libvirt-backup-gui est absent : seul le paquet libvirt-backup sera construit." >&2
fi

umask 022
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

# Dépendances de bibliothèques (dpkg-shlibdeps si disponible, liste de repli sinon).
shlibs() {
    local binary tmp deps="" fallback="$2"
    binary=$(realpath "$1")
    if command -v dpkg-shlibdeps >/dev/null; then
        tmp=$(mktemp -d)
        mkdir -p "$tmp/debian"
        printf 'Source: libvirt-backup\n\nPackage: libvirt-backup\nArchitecture: any\n' >"$tmp/debian/control"
        deps=$(cd "$tmp" && dpkg-shlibdeps -O "$binary" 2>/dev/null | sed -n 's/^shlibs:Depends=//p') || deps=""
        rm -rf "$tmp"
    fi
    if [ -z "$deps" ]; then
        echo "ATTENTION : dpkg-shlibdeps indisponible ou muet pour $binary ; dépendances de repli utilisées." >&2
        deps="$fallback"
    fi
    echo "$deps"
}

fill() { # remplace les marqueurs @...@ d'un modèle
    sed -e "s|@VERSION@|$VERSION|g" -e "s|@REVISION@|$REVISION|g" -e "s|@MAINTAINER@|$MAINTAINER|g" \
        -e "s|@DATE@|$DATE_RFC|g" -e "s|@DATE_MAN@|$DATE_MAN|g" "$1"
}

finish() { # finish <dossier> <nom-du-paquet>
    local dir="$1" name="$2"
    find "$dir" -exec touch -h -d "@$EPOCH" {} +
    local size
    size=$(du -sk --exclude=DEBIAN "$dir" | cut -f1)
    sed -i "s|^Installed-Size: .*|Installed-Size: $size|" "$dir/DEBIAN/control"
    (cd "$dir" && find . -type f ! -path './DEBIAN/*' -printf '%P\0' | LC_ALL=C sort -z | xargs -0 md5sum) >"$dir/DEBIAN/md5sums"
    touch -d "@$EPOCH" "$dir/DEBIAN/md5sums"
    dpkg-deb --root-owner-group -Zxz --build "$dir" "$OUT/${name}_${FULL}_${ARCH}.deb" >/dev/null
    echo "→ $OUT/${name}_${FULL}_${ARCH}.deb"
}

# ------------------------------------------------------------------ libvirt-backup
P="$WORK/libvirt-backup"
install -d "$P/DEBIAN" "$P/usr/bin" "$P/usr/share/doc/libvirt-backup" "$P/usr/share/man/man1"
install -m 755 "$BIN/libvirt-backup" "$P/usr/bin/libvirt-backup"
fill packaging/debian/changelog | gzip -9n >"$P/usr/share/doc/libvirt-backup/changelog.Debian.gz"
install -m 644 packaging/debian/copyright "$P/usr/share/doc/libvirt-backup/copyright"
# Documentation du projet, en anglais (fichier par défaut) et en français (.fr).
for doc in README CHANGELOG SECURITY CODE_OF_CONDUCT; do
    gzip -9n <"$doc.md" >"$P/usr/share/doc/libvirt-backup/$doc.md.gz"
    gzip -9n <"$doc.fr.md" >"$P/usr/share/doc/libvirt-backup/$doc.fr.md.gz"
done
install -d "$P/usr/share/man/fr/man1"
fill packaging/libvirt-backup.1 | gzip -9n >"$P/usr/share/man/man1/libvirt-backup.1.gz"
fill packaging/libvirt-backup.fr.1 | gzip -9n >"$P/usr/share/man/fr/man1/libvirt-backup.1.gz"
install -m 755 packaging/debian/postinst packaging/debian/prerm packaging/debian/postrm "$P/DEBIAN/"
DEPS_CLI=$(shlibs "$BIN/libvirt-backup" "libc6 (>= 2.34), libgcc-s1")
cat >"$P/DEBIAN/control" <<CONTROL
Package: libvirt-backup
Version: $FULL
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: 0
Depends: $DEPS_CLI, tar, inotify-tools
Recommends: sudo, libvirt-clients
Suggests: libvirt-daemon-system
Section: admin
Priority: optional
Homepage: $HOMEPAGE
Description: back up and restore a libvirt/QEMU host configuration
 Backs up libvirt definitions, networks, storage pools, UEFI NVRAM and TPM
 state, AppArmor profiles, systemd units, VFIO/GRUB/initramfs settings and
 local scripts into archives, automatically (inotify watcher service) or on
 demand, and restores them with the original owners, modes and ACLs.
 The archive location is user-selectable (internal disk, USB drive, network
 share).
 .
 Every archive also contains the tool itself, so a bare machine can be
 restored from a single archive. This package holds the command line and the
 watcher service; run "sudo libvirt-backup install" to enable it.
CONTROL
finish "$P" libvirt-backup

# -------------------------------------------------------------- libvirt-backup-gui
if [ "$HAVE_GUI" = 1 ]; then
    G="$WORK/libvirt-backup-gui"
    install -d "$G/DEBIAN" "$G/usr/bin" "$G/usr/share/applications" "$G/usr/share/doc" "$G/usr/share/metainfo" \
        "$G/usr/share/icons/hicolor/scalable/apps" "$G/usr/share/polkit-1/actions"
    install -m 755 "$BIN/libvirt-backup-gui" "$G/usr/bin/libvirt-backup-gui"
    # Les modèles de data/ décrivent l'installation locale (/usr/local) ; le paquet vise /usr/bin.
    sed 's|/usr/local/bin/libvirt-backup-gui|/usr/bin/libvirt-backup-gui|' data/app.desktop >"$G/usr/share/applications/$APP_ID.desktop"
    sed 's|/usr/local/bin/libvirt-backup|/usr/bin/libvirt-backup|' data/app.policy >"$G/usr/share/polkit-1/actions/$POLICY_FILE"
    install -m 644 data/app.svg "$G/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
    # Icônes raster (16 à 512 px, générées par packaging/make-icons.sh) et icône symbolique monochrome.
    for dir in data/icons/hicolor/*x*/apps; do
        size=$(basename "$(dirname "$dir")")
        install -d "$G/usr/share/icons/hicolor/$size/apps"
        install -m 644 "$dir/$APP_ID.png" "$G/usr/share/icons/hicolor/$size/apps/$APP_ID.png"
    done
    install -d "$G/usr/share/icons/hicolor/symbolic/apps"
    install -m 644 data/app-symbolic.svg "$G/usr/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
    install -m 644 "metainfo/$APP_ID.metainfo.xml" "$G/usr/share/metainfo/$APP_ID.metainfo.xml"
    chmod 644 "$G/usr/share/applications/$APP_ID.desktop" "$G/usr/share/polkit-1/actions/$POLICY_FILE"
    ln -s libvirt-backup "$G/usr/share/doc/libvirt-backup-gui"
    DEPS_GUI=$(shlibs "$BIN/libvirt-backup-gui" "libc6 (>= 2.34), libgcc-s1, libgtk-4-1 (>= 4.10), libglib2.0-0")
    # L'interface utilise l'API GTK 4.10 (feature v4_10 de gtk4-rs) : minimum explicite, sans doublon.
    if printf '%s' "$DEPS_GUI" | grep -q 'libgtk-4-1'; then
        DEPS_GUI=$(printf '%s' "$DEPS_GUI" | sed -E 's/libgtk-4-1 \([^)]*\)/libgtk-4-1 (>= 4.10)/')
    else
        DEPS_GUI="$DEPS_GUI, libgtk-4-1 (>= 4.10)"
    fi
    cat >"$G/DEBIAN/control" <<CONTROL
Package: libvirt-backup-gui
Version: $FULL
Architecture: $ARCH
Maintainer: $MAINTAINER
Installed-Size: 0
Depends: libvirt-backup (= $FULL), $DEPS_GUI, pkexec | policykit-1
Recommends: adwaita-icon-theme, librsvg2-common
Section: admin
Priority: optional
Homepage: $HOMEPAGE
Description: GTK4 interface for libvirt-backup
 Graphical front-end of libvirt-backup: browse and verify archives, back up
 now with a progress gauge (to the saved folder or to any other one), restore
 selected groups with a dry-run preview, drive the watcher service and edit
 the settings, including the backup location. Available in English, French,
 German and Spanish. Privileged operations go through pkexec.
CONTROL
    finish "$G" libvirt-backup-gui
fi
