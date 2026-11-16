//! Suppression d'archives choisies : garde-fous, confirmation, lien « latest ».

mod common;

use std::fs;

use common::*;

fn latest_target(env: &Env) -> Option<String> {
    fs::read_link(env.backups.join("libvirt-config-latest.tar.gz")).ok().map(|t| t.to_string_lossy().into_owned())
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// Trois archives, de la plus ancienne à la plus récente.
fn three_archives(env: &Env) -> Vec<std::path::PathBuf> {
    for _ in 0..3 {
        env.backup();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let list = archives(&env.backups);
    assert_eq!(list.len(), 3);
    list
}

#[test]
fn delete_by_name_number_and_latest_then_the_link_follows() {
    let env = Env::new();
    let list = three_archives(&env);
    assert_eq!(latest_target(&env).as_deref(), Some(file_name(&list[2]).as_str()));

    // la plus récente : le lien est repointé sur la précédente
    let run = env.cli(&["delete", "latest", "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(!list[2].exists());
    assert_eq!(latest_target(&env).as_deref(), Some(file_name(&list[1]).as_str()), "{}", run.all());
    assert!(run.all().contains("Archive supprimée"), "{}", run.all());

    // par nom
    let run = env.cli(&["delete", &file_name(&list[0]), "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(!list[0].exists());
    assert!(list[1].exists());
    assert_eq!(latest_target(&env).as_deref(), Some(file_name(&list[1]).as_str()), "le lien ne change pas");

    // la dernière : plus de sauvegarde, plus de lien pendant
    let run = env.cli(&["delete", "1", "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(archives(&env.backups).is_empty());
    assert!(fs::symlink_metadata(env.backups.join("libvirt-config-latest.tar.gz")).is_err(), "le lien doit être retiré");
}

#[test]
fn several_archives_at_once_and_duplicates_are_counted_once() {
    let env = Env::new();
    let list = three_archives(&env);
    let (a, b) = (file_name(&list[0]), file_name(&list[1]));
    let run = env.cli(&["delete", &a, &b, "1", &a, "--yes", "--json"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(archives(&env.backups).is_empty());
    let result = run.events().into_iter().find(|e| e["event"] == "result").expect("résultat");
    assert_eq!(result["data"]["removed"].as_array().unwrap().len(), 3);
    assert!(result["data"]["freed"].as_u64().unwrap() > 0);
    assert_eq!(result["data"]["latest"], "removed");
}

#[test]
fn confirmation_is_required_without_yes_and_json_never_prompts() {
    let env = Env::new();
    let list = three_archives(&env);
    let run = env.cli(&["delete", "1"]);
    assert_ne!(run.code, 0);
    assert!(run.all().contains("--yes"), "{}", run.all());
    let run = env.cli(&["delete", "1", "--json"]);
    assert_ne!(run.code, 0);
    assert_eq!(archives(&env.backups).len(), list.len(), "rien ne doit être supprimé sans confirmation");
}

#[test]
fn confirmation_word_in_a_terminal_and_warning_for_the_last_archive() {
    let env = Env::new();
    env.backup();
    let Some(wrong) = env.cli_tty(&["delete", "latest"], "oui\n") else { return };
    assert_eq!(wrong.code, 0, "{}", wrong.all());
    assert!(wrong.all().contains("AUCUNE sauvegarde automatique"), "avertissement attendu :\n{}", wrong.all());
    assert!(wrong.all().contains("DÉFINITIVE"), "{}", wrong.all());
    assert!(wrong.all().contains("annulée"), "{}", wrong.all());
    assert_eq!(archives(&env.backups).len(), 1, "une mauvaise réponse ne supprime rien");

    let right = env.cli_tty(&["delete", "latest"], "SUPPRIMER\n").unwrap();
    assert_eq!(right.code, 0, "{}", right.all());
    assert!(archives(&env.backups).is_empty());
}

#[test]
fn only_archives_of_the_backup_folder_can_be_deleted() {
    let env = Env::new();
    let list = three_archives(&env);
    let elsewhere = env.tmp.path().join("usb");
    fs::create_dir_all(&elsewhere).unwrap();
    let copy = elsewhere.join(file_name(&list[0]));
    fs::copy(&list[0], &copy).unwrap();

    let run = env.cli(&["delete", copy.to_str().unwrap(), "--yes"]);
    assert_ne!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("n'est pas dans le dossier des sauvegardes"), "{}", run.all());
    assert!(copy.exists() && list[0].exists());

    // un fichier quelconque, un numéro hors limite, le lien lui-même
    for bad in [env.root.join("etc/fstab").to_str().unwrap().to_string(), "9".to_string()] {
        let run = env.cli(&["delete", &bad, "--yes"]);
        assert_ne!(run.code, 0, "{bad}");
    }
    assert!(env.root.join("etc/fstab").exists());
    let run = env.cli(&["delete", "libvirt-config-latest.tar.gz", "--yes"]);
    assert_ne!(run.code, 0, "{}", run.all());
    assert_eq!(archives(&env.backups).len(), 3);

    // le chemin complet d'une archive du dossier est accepté
    let run = env.cli(&["delete", list[0].to_str().unwrap(), "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(!list[0].exists());
    assert!(copy.exists(), "la copie hors du dossier n'est jamais touchée");
}

#[test]
fn a_running_backup_blocks_the_deletion() {
    let env = Env::new();
    env.backup();
    let mut holder = libvirt_backup::sys::FileLock::new(&env.root.join("run/lock/libvirt-config-backup.lock"));
    assert!(holder.try_acquire().unwrap());
    let run = env.cli(&["delete", "1", "--yes"]);
    assert_ne!(run.code, 0);
    assert!(run.all().contains("en cours"), "{}", run.all());
    assert_eq!(archives(&env.backups).len(), 1);
    holder.release();
    assert_eq!(env.cli(&["delete", "1", "--yes"]).code, 0);
}

#[test]
fn safety_archives_can_be_deleted_and_leave_the_link_alone() {
    let env = Env::new();
    env.backup();
    env.mutate();
    assert_eq!(env.cli(&["restore", "--latest", "--yes"]).code, 0);
    let safety = safety_archives(&env.backups);
    assert_eq!(safety.len(), 1);
    let before = latest_target(&env);

    let Some(ask) = env.cli_tty(&["delete", &file_name(&safety[0])], "non\n") else { return };
    assert!(ask.all().contains("archive de sécurité"), "avertissement attendu :\n{}", ask.all());
    assert!(safety[0].exists());

    let run = env.cli(&["delete", &file_name(&safety[0]), "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(safety_archives(&env.backups).is_empty());
    assert_eq!(archives(&env.backups).len(), 1);
    assert_eq!(latest_target(&env), before);
}

#[test]
fn delete_needs_at_least_one_archive() {
    let env = Env::new();
    env.backup();
    let run = env.cli(&["delete", "--yes"]);
    assert_ne!(run.code, 0);
    assert_eq!(archives(&env.backups).len(), 1);
}
