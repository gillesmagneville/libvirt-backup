//! Choix de l'emplacement des sauvegardes.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use libvirt_backup::config::check_backup_dir;

fn config_of(env: &Env) -> serde_json::Value {
    serde_json::from_str(&env.read("etc/libvirt-backup/config.json")).unwrap()
}

#[test]
fn backup_dir_option_overrides_for_one_run_only() {
    let env = Env::new();
    let usb = env.tmp.path().join("usb/virtualisation");
    fs::create_dir_all(usb.parent().unwrap()).unwrap();
    let run = env.cli(&["backup", "--backup-dir", usb.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert_eq!(archives(&usb).len(), 1);
    assert!(usb.join("libvirt-config-latest.tar.gz").exists());
    assert_eq!(mode(&usb), 0o700);
    assert!(archives(&env.backups).is_empty(), "le dossier configuré n'est pas utilisé");
    assert_eq!(config_of(&env)["backup_dir"], env.backups.to_string_lossy().as_ref(), "réglage inchangé");
    // la rétention s'applique dans le dossier choisi
    for _ in 0..4 {
        assert_eq!(env.cli(&["backup", "--backup-dir", usb.to_str().unwrap()]).code, 0);
    }
    assert_eq!(archives(&usb).len(), 3);
}

#[test]
fn missing_parent_is_refused_instead_of_writing_to_the_system_disk() {
    let env = Env::new();
    let target = env.tmp.path().join("cle-usb-non-montee/virtualisation");
    let run = env.cli(&["backup", "--backup-dir", target.to_str().unwrap()]);
    assert_eq!(run.code, 1, "{}", run.all());
    assert!(run.all().contains("parent n'existe pas"), "{}", run.all());
    assert!(!target.exists() && !target.parent().unwrap().exists());
}

#[test]
fn list_info_verify_follow_backup_dir() {
    let env = Env::new();
    let other = env.tmp.path().join("ailleurs");
    fs::create_dir(&other).unwrap();
    assert_eq!(env.cli(&["backup", "--backup-dir", other.to_str().unwrap()]).code, 0);
    let list = env.cli(&["list", "--json", "--backup-dir", other.to_str().unwrap()]);
    let data: serde_json::Value = serde_json::from_str(list.stdout.trim()).unwrap();
    assert_eq!(data.as_array().unwrap().len(), 1);
    assert_eq!(env.cli(&["list", "--json"]).stdout.trim(), "[]", "le dossier configuré est vide");
    assert_eq!(env.cli(&["verify", "--all", "--backup-dir", other.to_str().unwrap()]).code, 0);
    assert_eq!(env.cli(&["info", "--backup-dir", other.to_str().unwrap()]).code, 0);
    let status = env.cli(&["status", "--json", "--backup-dir", other.to_str().unwrap()]);
    let data: serde_json::Value = serde_json::from_str(status.stdout.trim()).unwrap();
    assert_eq!(data["backup_dir"], other.to_string_lossy().as_ref());
    assert_eq!(data["archives"], 1);
}

#[test]
fn config_set_backup_dir_is_normalized_persisted_and_validated() {
    let env = Env::new();
    let messy = format!("{}/usb/./virtualisation/", env.tmp.path().display());
    let run = env.cli(&["config", "set", &format!("backup_dir={messy}")]);
    assert_eq!(run.code, 0, "{}", run.all());
    let expected = env.tmp.path().join("usb/virtualisation");
    assert_eq!(config_of(&env)["backup_dir"], expected.to_string_lossy().as_ref());

    // chemin relatif : résolu depuis le dossier courant
    let out = env.command(&["config", "set", "backup_dir=relatif/sauvegardes"]).current_dir(env.tmp.path()).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(config_of(&env)["backup_dir"], env.tmp.path().join("relatif/sauvegardes").to_string_lossy().as_ref());

    // « .. » refusé, réglage précédent conservé
    let before = config_of(&env);
    let run = env.cli(&["config", "set", "backup_dir=/mnt/../etc"]);
    assert_eq!(run.code, 1);
    assert_eq!(config_of(&env), before);

    // l'option globale équivaut à backup_dir=
    let other = env.tmp.path().join("via-option");
    assert_eq!(env.cli(&["config", "set", "--backup-dir", other.to_str().unwrap()]).code, 0);
    assert_eq!(config_of(&env)["backup_dir"], other.to_string_lossy().as_ref());

    // vide = retour au dossier par défaut (~utilisateur/virtualisation)
    assert_eq!(env.cli(&["config", "set", "backup_dir="]).code, 0);
    assert_eq!(config_of(&env)["backup_dir"], "");
}

#[test]
fn unsafe_locations_are_refused_on_the_real_system() {
    for bad in [
        "/",
        "/etc",
        "/etc/libvirt",
        "/etc/libvirt/sauvegardes",
        "/usr",
        "/usr/local/bin/x",
        "/proc/1",
        "/run/x",
        "/var",
        "/etc/apparmor.d/s",
    ] {
        assert!(check_backup_dir(Path::new(bad), true).is_err(), "{bad} aurait dû être refusé");
    }
    for good in [
        "/mnt/usb/virtualisation",
        "/media/abel/CLE/virtualisation",
        "/home/abel/virtualisation",
        "/var/backups/virt",
        "/srv/sauvegardes",
        "/etc/ma-sauvegarde",
    ] {
        assert!(check_backup_dir(Path::new(good), true).is_ok(), "{good} aurait dû être accepté");
    }
    // un fichier à la place d'un dossier est toujours refusé
    let file = std::env::temp_dir().join("libvirt-backup-test-file");
    fs::write(&file, "x").unwrap();
    assert!(check_backup_dir(&file, false).is_err());
    let _ = fs::remove_file(&file);
}

#[test]
fn install_option_saves_location_creates_it_and_uses_it_for_the_first_backup() {
    let env = Env::new();
    let usb = env.tmp.path().join("usb");
    fs::create_dir(&usb).unwrap();
    let target = usb.join("virtualisation");
    let run = env.cli(&["install", "--yes", "--backup-dir", target.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert_eq!(config_of(&env)["backup_dir"], target.to_string_lossy().as_ref());
    assert_eq!(mode(&target), 0o700);
    assert_eq!(archives(&target).len(), 1);
    assert!(archives(&env.backups).is_empty());
    assert!(run.all().contains(&*target.to_string_lossy()), "le dossier choisi est rappelé");
}

#[test]
fn install_asks_for_the_location_in_a_terminal() {
    let env = Env::new();
    let chosen = env.tmp.path().join("choisi");
    let Some(run) = env.cli_tty(&["install", "--no-first-backup"], &format!("{}\n", chosen.display())) else { return };
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("Dossier des sauvegardes"), "{}", run.all());
    assert_eq!(config_of(&env)["backup_dir"], chosen.to_string_lossy().as_ref());
    assert!(chosen.is_dir());

    // Entrée seule : le dossier proposé (celui déjà configuré) est conservé
    let env = Env::new();
    let run = env.cli_tty(&["install", "--no-first-backup"], "\n").unwrap();
    assert_eq!(run.code, 0, "{}", run.all());
    assert_eq!(config_of(&env)["backup_dir"], env.backups.to_string_lossy().as_ref());
    assert!(env.backups.is_dir());
}

#[test]
fn restore_from_an_archive_on_removable_media() {
    let env = Env::new();
    let original = env.backup();
    let usb = env.tmp.path().join("cle-usb");
    fs::create_dir(&usb).unwrap();
    let copy = usb.join("sauvegarde-du-lundi.tar.gz");
    fs::copy(&original, &copy).unwrap();
    fs::remove_file(&original).unwrap();
    env.write("etc/default/grub", "CHANGED\n");
    let run = env.cli(&["restore", copy.to_str().unwrap(), "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"));
    assert_eq!(safety_archives(&env.backups).len(), 1, "l'archive de sécurité va dans le dossier configuré");
    assert!(copy.exists(), "l'archive externe n'est pas modifiée");
}
