//! Jeu d'icônes : fichiers présents et cohérents, installés avec l'outil, restaurés avec le groupe « outil ».

mod common;

use std::fs;
use std::path::Path;

use common::*;
use libvirt_backup::archive::{classify, Class};
use libvirt_backup::config::{group_of, APP_ID, ICON_REL, ICON_SYMBOLIC_REL};
use libvirt_backup::selfinstall::{build_payload, is_program_path, Binaries, Layout};

const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

fn data(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Dimensions lues dans l'en-tête IHDR d'un PNG.
fn png_size(path: &Path) -> (u32, u32, u8) {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("{} : {e}", path.display()));
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{} n'est pas un PNG", path.display());
    let be = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap());
    (be(16), be(20), bytes[25]) // largeur, hauteur, type de couleur (6 = RGBA)
}

#[test]
fn raster_icons_exist_at_every_standard_size_with_transparency() {
    for size in SIZES {
        let path = data(&format!("data/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"));
        let (w, h, color) = png_size(&path);
        assert_eq!((w, h), (size, size), "{}", path.display());
        assert_eq!(color, 6, "{} : un fond transparent (RGBA) est attendu pour les coins arrondis", path.display());
    }
}

#[test]
fn svg_icons_are_wellformed_and_the_symbolic_one_is_recolorable() {
    for name in ["data/app.svg", "data/app-small.svg", "data/app-symbolic.svg"] {
        let text = fs::read_to_string(data(name)).unwrap();
        assert!(text.contains("<svg") && text.contains("viewBox"), "{name}");
        assert!(text.trim_end().ends_with("</svg>"), "{name}");
    }
    // GTK recolore les icônes symboliques en remplaçant le remplissage (fill) ; un contour (stroke) ou un
    // dégradé resterait de sa couleur d'origine, illisible en thème sombre.
    let symbolic = fs::read_to_string(data("data/app-symbolic.svg")).unwrap();
    for forbidden in ["stroke", "gradient", "filter", "opacity"] {
        assert!(!symbolic.contains(forbidden), "l'icône symbolique ne doit pas contenir « {forbidden} »");
    }
    assert!(symbolic.contains("width=\"16\"") && symbolic.contains("viewBox=\"0 0 16 16\""));
}

#[test]
fn desktop_entry_and_policy_point_to_existing_icon_and_files() {
    let desktop = fs::read_to_string(data("data/app.desktop")).unwrap();
    assert!(desktop.contains(&format!("Icon={APP_ID}")), "l'entrée de menu doit nommer l'icône par son identifiant");
    assert!(desktop.contains(&format!("StartupWMClass={APP_ID}")));
}

#[test]
fn icon_paths_belong_to_the_tool_group_and_are_dpkg_owned_in_package_mode() {
    for rel in [ICON_REL, ICON_SYMBOLIC_REL] {
        assert_eq!(group_of(rel), Some("tool"), "{rel}");
        assert_eq!(classify(rel), Class::System, "{rel}");
        assert!(is_program_path(rel), "{rel} appartient au paquet en disposition Package");
    }
    assert!(ICON_SYMBOLIC_REL.ends_with(&format!("{APP_ID}-symbolic.svg")));
    assert!(ICON_SYMBOLIC_REL.contains("/hicolor/symbolic/apps/"));
}

#[test]
fn local_payload_carries_both_svg_icons() {
    let env = Env::new();
    let bins = Binaries { cli: std::path::PathBuf::from(CLI), gui: None };
    let payload = build_payload(&bins).unwrap();
    for rel in [ICON_REL, ICON_SYMBOLIC_REL] {
        let (_, bytes, mode) = payload.files.iter().find(|(r, _, _)| r == rel).unwrap_or_else(|| panic!("{rel} absent de la charge utile"));
        assert_eq!(*mode, 0o644);
        assert!(String::from_utf8_lossy(bytes).contains("<svg"), "{rel}");
    }
    drop(env);
}

#[test]
fn install_writes_the_icons_archives_them_and_uninstall_removes_them() {
    let env = Env::new();
    let run = env.cli_env(&["install", "--yes", "--no-first-backup"], &[("LIBVIRT_BACKUP_LAYOUT", "local")]);
    assert_eq!(run.code, 0, "{}", run.all());
    for rel in [ICON_REL, ICON_SYMBOLIC_REL] {
        assert!(env.root.join(rel).is_file(), "{rel} non installé");
    }
    let run = env.cli_env(&["backup"], &[("LIBVIRT_BACKUP_LAYOUT", "local")]);
    assert_eq!(run.code, 0, "{}", run.all());
    let archive = archives(&env.backups).last().unwrap().clone();
    let members = names(&archive);
    for rel in [ICON_REL, ICON_SYMBOLIC_REL] {
        assert!(members.contains(&rel.to_string()), "{rel} absent de l'archive");
    }
    let run = env.cli_env(&["uninstall", "--purge"], &[("LIBVIRT_BACKUP_LAYOUT", "local")]);
    assert_eq!(run.code, 0, "{}", run.all());
    for rel in [ICON_REL, ICON_SYMBOLIC_REL] {
        assert!(!env.root.join(rel).exists(), "{rel} aurait dû être retiré");
    }
    assert_eq!(Layout::Local.id(), "local");
}
