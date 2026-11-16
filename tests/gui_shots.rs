//! Captures d'écran du README (GTK 4 sous Xvfb) : ce n'est pas un test, il ne vérifie rien.
//!
//!     xvfb-run -a cargo test --features gui --test gui_shots -- DOSSIER_BRUT
//!
//! Le langage vient de `LB_SHOTS_LANG` (en ou fr). Chaque capture est un écran entier ; le script
//! `packaging/make-screenshots.sh` les recadre. Sans écran (pas de DISPLAY), le programme ne fait rien.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use libvirt_backup::gui::testing::{run, Driver};

fn shot(dir: &Path, name: &str) {
    let _ = Command::new("import").args(["-window", "root"]).arg(dir.join(format!("{name}.png"))).status();
}

fn main() {
    std::env::set_var("GSK_RENDERER", "cairo");
    std::env::set_var("GDK_BACKEND", "x11");
    if std::env::var_os("DISPLAY").is_none() {
        println!("captures ignorées : aucun écran (lance-les avec xvfb-run -a)");
        return;
    }
    let shots: PathBuf = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gui-shots-raw"));
    fs::create_dir_all(&shots).unwrap();
    let lang = std::env::var("LB_SHOTS_LANG").unwrap_or_else(|_| "en".to_string());

    let env = Env::new();
    std::env::set_var("PATH", env.path_env());
    std::env::set_var("STUB_DIR", &env.stubs);
    std::env::set_var("STUB_LOG", env.log());
    std::env::set_var("LIBVIRT_BACKUP_CLI", CLI);
    std::env::set_var("XDG_CONFIG_HOME", env.tmp.path().join("xdg"));
    std::env::set_var("LIBVIRT_BACKUP_LANG", &lang); // après Env::new, qui impose le français aux tests
    let quit_dir = env.tmp.path().join("quit");
    // un faux virsh qui attend tant que le fichier « slow » existe : la sauvegarde reste en cours le temps de la capture
    let slow = env.stubs.join("slow");
    let virsh = env.stubs.join("virsh");
    fs::rename(&virsh, env.stubs.join("virsh.real")).unwrap();
    fs::write(&virsh, format!("#!/bin/sh\n[ -e '{}' ] && sleep 4\nexec \"$(dirname \"$0\")/virsh.real\" \"$@\"\n", slow.display()))
        .unwrap();
    fs::set_permissions(&virsh, fs::Permissions::from_mode(0o755)).unwrap();

    let (root, backups) = (env.root.clone(), env.backups.clone());
    let shots_dir = shots.clone();
    let mut step = 0usize;
    let mut waited = 0u32;
    run(Some(root), move |d: &Driver| {
        waited += 1;
        if waited > 200 {
            return true;
        }
        match step {
            0 if (d.idle() && !d.status().is_null()) || waited > 80 => {
                d.backup_now();
                step = 1;
                waited = 0;
            }
            1 if d.idle() && waited > 2 => {
                d.refresh();
                step = 2;
                waited = 0;
            }
            2 if d.idle() && d.archive_rows() == 1 => {
                d.select_first();
                step = 3;
                waited = 0;
            }
            3 if waited > 3 => {
                shot(&shots, "main");
                d.show_page("service");
                step = 4;
                waited = 0;
            }
            4 if waited > 4 => {
                shot(&shots, "service");
                d.show_page("settings");
                step = 5;
                waited = 0;
            }
            5 if waited > 4 => {
                shot(&shots, "settings");
                d.show_page("backups");
                step = 6;
                waited = 0;
            }
            6 if waited > 4 => {
                shot(&shots, "main-bis"); // état de référence pour recadrer les fenêtres
                d.show_about();
                step = 7;
                waited = 0;
            }
            7 if waited > 4 => {
                shot(&shots, "about");
                d.close_extra_windows();
                let _ = d.open_preferences();
                step = 8;
                waited = 0;
            }
            8 if waited > 4 => {
                shot(&shots, "prefs");
                d.close_extra_windows();
                fs::create_dir_all(&quit_dir).unwrap();
                fs::write(&slow, "").unwrap();
                d.backup_to(&quit_dir);
                step = 9;
                waited = 0;
            }
            9 if waited > 1 => {
                d.set_gauge(0.45);
                step = 10;
                waited = 0;
            }
            10 if waited > 2 => {
                shot(&shots, "quit-before");
                let _ = d.can_quit_now(); // l'opération tourne : la boîte « Quitter ? » s'ouvre
                step = 11;
                waited = 0;
            }
            11 if waited > 3 => {
                shot(&shots, "quit");
                let _ = fs::remove_file(&slow);
                let _ = &backups;
                return true;
            }
            _ => {}
        }
        false
    });
    println!("captures brutes ({lang}) dans {}", shots_dir.display());
}
