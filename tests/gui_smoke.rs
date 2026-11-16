//! Test graphique de bout en bout (GTK 4 sous Xvfb), sans bibliothèque de test (GTK exige le thread principal) :
//!
//!     xvfb-run -a cargo test --features gui --test gui_smoke [-- DOSSIER_CAPTURES]
//!
//! La vraie interface pilote la vraie ligne de commande (sous-processus JSON) sur une fausse racine système.

mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use common::*;
use libvirt_backup::config::{AUTHOR_NAME, BUILD_DATE, REPO_URL};
use libvirt_backup::gui::testing::{run, Driver};
use libvirt_backup::i18n::{self, Lang};

fn shot(dir: &Path, name: &str) {
    let _ = Command::new("import").args(["-window", "root"]).arg(dir.join(format!("{name}.png"))).status();
}

fn main() {
    std::env::set_var("GSK_RENDERER", "cairo");
    std::env::set_var("GDK_BACKEND", "x11");
    if std::env::var_os("DISPLAY").is_none() {
        println!("test graphique ignoré : aucun écran (lance-le avec xvfb-run -a)");
        return;
    }
    let shots: PathBuf =
        std::env::args().skip(1).find(|a| !a.starts_with('-')).map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("gui-shots"));
    fs::create_dir_all(&shots).unwrap();

    let env = Env::new();
    std::env::set_var("PATH", env.path_env());
    std::env::set_var("STUB_DIR", &env.stubs);
    std::env::set_var("STUB_LOG", env.log());
    std::env::set_var("LIBVIRT_BACKUP_CLI", CLI);
    // les préférences de la fenêtre (gui.json) vont dans un dossier jetable, pas dans le vrai ~/.config
    let xdg = env.tmp.path().join("xdg");
    std::env::set_var("XDG_CONFIG_HOME", &xdg);
    let gui_prefs = xdg.join("libvirt-backup/gui.json");
    let other = env.tmp.path().join("cle-usb-virtualisation");
    let (german_dir, quit_dir) = (env.tmp.path().join("sauvegarde-allemande"), env.tmp.path().join("sauvegarde-quitter"));
    let failures: Rc<RefCell<Vec<String>>> = Rc::default();
    let check = {
        let failures = failures.clone();
        move |ok: bool, text: &str| {
            println!("{} {text}", if ok { "OK  " } else { "FAIL" });
            if !ok {
                failures.borrow_mut().push(text.to_string());
            }
        }
    };

    let (root, backups, grub, cfg_path) =
        (env.root.clone(), env.backups.clone(), env.root.join("etc/default/grub"), env.root.join("etc/libvirt-backup/config.json"));
    let other2 = other.clone();
    let shots2 = shots.clone();
    let mut step = 0usize;
    let mut waited = 0u32;
    let code = run(Some(root.clone()), move |d: &Driver| {
        waited += 1;
        let ready = |d: &Driver, waited: u32| (d.idle() && !d.status().is_null()) || waited > 80;
        if waited > 160 {
            check(false, &format!("délai dépassé à l'étape {step}"));
            return true;
        }
        match step {
            0 if ready(d, waited) => {
                check(d.status()["installed"] == false, "état initial : outil non installé");
                check(d.banner().contains("n'est pas installé"), "bannière : outil non installé");
                check(d.folder_label().contains(&*backups.to_string_lossy()), "dossier enregistré affiché");
                check(!d.sensitive("restore"), "« Restaurer » inactif sans sélection");
                check(!d.sensitive("delete"), "« Supprimer… » inactif sans sélection");
                step = 1;
                waited = 0;
            }
            1 => {
                d.backup_now();
                step = 2;
                waited = 0;
            }
            2 if d.idle() && waited > 2 => {
                check(archives(&backups).len() == 1, "sauvegarde créée dans le dossier enregistré");
                check(d.log_text().contains("[1/5]") && d.log_text().contains("[5/5]"), "journal : étapes 1/5 à 5/5");
                check((d.progress() - 1.0).abs() < 1e-9, "barre de progression terminée");
                check(d.progress_text().contains("100"), "jauge : 100 % affiché à la fin");
                check(d.progress_events() >= 5, "jauge : la sauvegarde envoie plusieurs événements de progression");
                check(d.activity().contains("terminé"), "activité : terminé");
                step = 3;
                waited = 0;
                d.refresh();
            }
            3 if d.idle() && d.archive_rows() == 1 => {
                d.select_first();
                check(d.sensitive("restore"), "« Restaurer » actif après sélection");
                check(d.sensitive("delete"), "« Supprimer… » actif après sélection");
                if Path::new("/usr/share/icons/Adwaita").exists() {
                    check(d.button_icons() == 8, "les 8 boutons de la page Sauvegardes ont une icône");
                }
                shot(&shots2, "1-sauvegardes");
                step = 4;
                waited = 0;
            }
            4 => {
                fs::create_dir(&other2).unwrap();
                d.backup_to(&other2);
                step = 5;
                waited = 0;
            }
            5 if d.idle() && waited > 2 => {
                check(archives(&other2).len() == 1, "« Sauvegarder vers… » écrit dans le dossier choisi");
                check(archives(&backups).len() == 1, "le dossier enregistré n'a pas bougé");
                step = 6;
                waited = 0;
            }
            6 => {
                d.set_view_dir(Some(other2.clone()));
                step = 7;
                waited = 0;
            }
            7 if waited > 4 && d.folder_label().contains("affichage temporaire") => {
                check(d.folder_label().contains(&*other2.to_string_lossy()), "dossier affiché = dossier choisi");
                check(d.sensitive("saved_dir"), "bouton « Dossier enregistré » visible");
                shot(&shots2, "2-autre-dossier");
                d.set_view_dir(None);
                step = 8;
                waited = 0;
            }
            8 if waited > 4 && !d.folder_label().contains("affichage temporaire") => {
                check(!d.sensitive("saved_dir"), "retour au dossier enregistré");
                fs::write(&grub, "CHANGED\n").unwrap();
                let path = archives(&backups).last().cloned().unwrap();
                d.run_operation("Restauration", &["restore", path.to_str().unwrap(), "--only", "boot", "--yes"]);
                step = 9;
                waited = 0;
            }
            9 if d.idle() && waited > 2 => {
                check(fs::read_to_string(&grub).unwrap().starts_with("GRUB_CMDLINE"), "restauration lancée depuis l'interface");
                check(d.log_text().contains("[7/7]"), "journal : étapes de restauration 1/7 à 7/7");
                check(!safety_archives(&backups).is_empty(), "archive de sécurité créée");
                d.show_page("service");
                step = 90;
                waited = 0;
            }
            90 if waited > 4 => {
                shot(&shots2, "3-surveillance");
                d.show_page("settings");
                step = 10;
                waited = 0;
            }
            10 if waited > 4 => {
                shot(&shots2, "4-reglages");
                check(d.dir_entry() == backups.to_string_lossy(), "réglages : dossier enregistré affiché");
                check(d.lean_checked(), "réglages : sauvegarde allégée cochée par défaut");
                d.set_lean_checked(false);
                d.set_dir_entry(&other2.to_string_lossy());
                d.save_settings();
                step = 11;
                waited = 0;
            }
            11 if d.idle() && waited > 2 => {
                let saved: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
                check(saved["backup_dir"] == other2.to_string_lossy().as_ref(), "réglages enregistrés : nouveau dossier des archives");
                check(saved["lean_backup"] == false, "réglages enregistrés : sauvegarde allégée décochée");
                d.show_page("backups");
                d.refresh();
                step = 110;
                waited = 0;
            }
            // --- suppression d'une archive, avec avertissement ---
            110 if d.idle() && waited > 3 => {
                check(d.archive_rows() == 1, "le nouveau dossier affiche son unique archive");
                d.select_first();
                check(d.sensitive("delete"), "« Supprimer… » actif sur l'archive sélectionnée");
                let (title, detail) = d.delete_warning().expect("une archive est sélectionnée");
                check(title.contains("Supprimer"), "boîte de confirmation : question posée");
                check(detail.contains("DÉFINITIVE"), "boîte de confirmation : suppression définitive signalée");
                check(detail.contains("AUCUNE sauvegarde automatique"), "boîte de confirmation : dernière archive signalée");
                d.open_delete_dialog();
                step = 111;
                waited = 0;
            }
            111 if waited > 4 => {
                shot(&shots2, "6-suppression");
                check(archives(&other2).len() == 1, "rien n'est supprimé tant que la confirmation n'est pas donnée");
                d.confirm_delete();
                step = 112;
                waited = 0;
            }
            112 if d.idle() && waited > 2 => {
                check(archives(&other2).is_empty(), "l'archive est supprimée après confirmation");
                check(d.log_text().contains("Archive supprimée"), "journal : archive supprimée");
                check(d.archive_rows() == 0, "la liste est actualisée");
                step = 20;
                waited = 0;
            }
            // --- menu, « À propos », préférences ---
            20 => {
                check(d.menu_labels() == ["Préférences", "À propos", "Quitter"], "menu : Préférences, À propos, Quitter");
                let about = d.about_text();
                let date = i18n::format_date(Lang::Fr, BUILD_DATE);
                check(about.contains("libvirt Backup"), "À propos : nom du logiciel");
                check(
                    about.contains(&format!("Version {}  —  Build du {date}", libvirt_backup::VERSION)),
                    "À propos : version et date de compilation",
                );
                check(about.contains(&format!("Développé par {AUTHOR_NAME}")), "À propos : nom du développeur");
                check(about.contains("Conçu avec l'aide de Claude.ai (Anthropic)"), "À propos : aide de Claude.ai");
                check(about.contains("Distribué sous licence GNU GPL v3"), "À propos : licence");
                check(about.contains(REPO_URL.trim_start_matches("https://")), "À propos : lien du dépôt GitHub");
                d.show_about();
                step = 21;
                waited = 0;
            }
            21 if waited > 4 => {
                shot(&shots2, "7-a-propos");
                d.close_extra_windows();
                let (names, selected) = d.open_preferences();
                check(names == ["Langue système", "English", "Français", "Deutsch", "Español"], "préférences : liste des langues");
                check(selected == 0, "préférences : langue du système par défaut");
                step = 22;
                waited = 0;
            }
            22 if waited > 4 => {
                shot(&shots2, "8-preferences");
                check(d.choose_language(3), "préférences : avis de redémarrage après un changement de langue");
                let saved: serde_json::Value = serde_json::from_str(&fs::read_to_string(&gui_prefs).unwrap()).unwrap();
                check(saved["language"] == "de", "préférences : langue enregistrée dans gui.json");
                check(d.cli_language() == "de", "préférences : la ligne de commande suivante utilise la nouvelle langue");
                d.close_extra_windows();
                fs::create_dir(&german_dir).unwrap();
                d.run_operation("Sicherung", &["backup", "--backup-dir", german_dir.to_str().unwrap()]);
                step = 23;
                waited = 0;
            }
            23 if d.idle() && waited > 2 => {
                check(archives(&german_dir).len() == 1, "sauvegarde lancée depuis l'interface en allemand");
                let german = i18n::lookup(Lang::De, "Preparing and locking...").expect("traduction allemande");
                check(d.log_text().contains(german), "journal : messages de la ligne de commande en allemand");
                let (_, _) = d.open_preferences();
                check(d.choose_language(2), "préférences : retour au français");
                check(d.cli_language() == "fr", "préférences : français transmis à la ligne de commande");
                d.close_extra_windows();
                step = 24;
                waited = 0;
            }
            // --- quitter pendant une opération ---
            24 => {
                check(d.can_quit_now(), "quitter : possible tout de suite quand rien ne tourne");
                fs::create_dir(&quit_dir).unwrap();
                d.backup_to(&quit_dir);
                let before = d.toplevels();
                check(!d.can_quit_now(), "quitter : refusé tant qu'une opération tourne (question posée)");
                check(!d.idle(), "quitter : l'opération continue");
                d.set_toplevels_before(before);
                step = 25;
                waited = 0;
            }
            25 if waited > 2 => {
                check(d.toplevels() > d.toplevels_before(), "quitter : boîte de confirmation affichée");
                d.set_gauge(0.45);
                check(d.progress_text().contains("45"), "jauge : pourcentage affiché sur la barre");
                shot(&shots2, "9-quitter");
                d.close_extra_windows(); // « Annuler »
                step = 26;
                waited = 0;
            }
            26 if d.idle() && waited > 2 => {
                check(archives(&quit_dir).len() == 1, "quitter : l'opération annulée a bien été menée à son terme");
                check(d.can_quit_now(), "quitter : de nouveau possible une fois l'opération terminée");
                step = 12;
                waited = 0;
            }
            12 => {
                d.run_operation("Sauvegarde", &["backup", "--backup-dir", "/dossier/\u{0}invalide"]);
                step = 13;
                waited = 0;
            }
            13 if d.idle() && waited > 2 => {
                check(
                    d.activity().contains("échec") || d.log_text().contains("ERREUR") || d.log_text().contains("introuvable"),
                    "une erreur est signalée dans l'interface",
                );
                shot(&shots2, "5-erreur");
                return true;
            }
            _ => {}
        }
        false
    });
    let _ = code;
    let failed = failures.borrow();
    if failed.is_empty() {
        println!("Test graphique : tout est conforme (captures dans {}).", shots.display());
    } else {
        println!("{} échec(s) : {:?}", failed.len(), *failed);
        std::process::exit(1);
    }
}
