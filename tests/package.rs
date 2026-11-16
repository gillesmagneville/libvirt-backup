//! Installation par paquet .deb : les programmes appartiennent à dpkg (/usr/bin), l'outil n'y touche pas.
//!
//! `LIBVIRT_BACKUP_LAYOUT=package` simule un programme lancé depuis /usr/bin ; la fausse racine reçoit les
//! fichiers que dpkg aurait posés.
mod common;

use std::fs;

use common::*;
use libvirt_backup::selfinstall::{unit_text, Layout};

const PKG: &[(&str, &str)] = &[("LIBVIRT_BACKUP_LAYOUT", "package")];
const UNIT: &str = "etc/systemd/system/libvirt-config-backup-watch.service";
const POLICY: &str = "usr/share/polkit-1/actions/io.github.gillesmagneville.libvirt-backup.run.policy";

/// Pose les fichiers du paquet dans la fausse racine, comme le ferait dpkg.
fn dpkg_files(env: &Env) {
    write_file(&env.root.join("usr/bin/libvirt-backup"), "#!/bin/sh\n# faux programme du paquet\n", 0o755);
    write_file(&env.root.join("usr/bin/libvirt-backup-gui"), "#!/bin/sh\n", 0o755);
    write_file(&env.root.join(POLICY), "<policyconfig>paquet</policyconfig>\n", 0o644);
}

fn status(env: &Env, vars: &[(&str, &str)]) -> serde_json::Value {
    serde_json::from_str(env.cli_env(&["status", "--json"], vars).stdout.trim()).unwrap()
}

#[test]
fn install_only_creates_settings_and_service() {
    let env = Env::new();
    dpkg_files(&env);
    let run = env.cli_env(&["install", "--yes"], PKG);
    assert_eq!(run.code, 0, "{}", run.all());
    assert_eq!(env.read(UNIT), unit_text(Layout::Package));
    assert!(env.read(UNIT).contains("ExecStart=/usr/bin/libvirt-backup watch"));
    // rien n'est écrit à la place de dpkg ni sous /usr/local
    assert!(!env.root.join("usr/local/lib/libvirt-backup").exists());
    assert!(!env.root.join("usr/local/bin/libvirt-backup").exists());
    assert!(!env.root.join("usr/local/share/applications").exists());
    assert_eq!(env.read("usr/bin/libvirt-backup"), "#!/bin/sh\n# faux programme du paquet\n");
    assert_eq!(env.read(POLICY), "<policyconfig>paquet</policyconfig>\n");
    assert!(env.root.join("etc/libvirt-backup/config.json").is_file());
    let calls = env.calls();
    assert!(calls.iter().any(|c| c == "systemctl enable libvirt-config-backup-watch.service"));
    assert!(calls.iter().any(|c| c == "systemctl restart libvirt-config-backup-watch.service"));

    let data = status(&env, PKG);
    assert_eq!(data["installed"], true);
    assert_eq!(data["layout"], "package");
    assert_eq!(data["legacy_unit"], false);
}

#[test]
fn archive_carries_a_local_layout_copy_of_the_tool() {
    let env = Env::new();
    dpkg_files(&env);
    assert_eq!(env.cli_env(&["install", "--yes", "--no-first-backup"], PKG).code, 0);
    let run = env.cli_env(&["backup"], PKG);
    assert_eq!(run.code, 0, "{}", run.all());
    let archive = archives(&env.backups).pop().unwrap();
    let members = names(&archive);
    for expected in
        ["usr/local/lib/libvirt-backup/libvirt-backup", "usr/local/bin/libvirt-backup", POLICY, "etc/libvirt-backup/config.json"]
    {
        assert!(members.iter().any(|m| m == expected), "{expected} absent de l'archive");
    }
    assert!(!members.iter().any(|m| m.starts_with("usr/bin/")), "rien de /usr/bin dans l'archive : {members:?}");
    // le programme archivé est le vrai binaire (pas le faux fichier de la racine) et reste exécutable
    let listing = std::process::Command::new("tar").arg("-tvzf").arg(&archive).output().unwrap();
    let listing = String::from_utf8_lossy(&listing.stdout);
    let line = listing.lines().find(|l| l.ends_with("usr/local/lib/libvirt-backup/libvirt-backup")).expect("programme archivé");
    assert!(line.starts_with("-rwxr-xr-x"), "{line}");
    let manifest: serde_json::Value = serde_json::from_str(&member(&archive, "host-inventory/manifest.json").unwrap()).unwrap();
    assert_eq!(manifest["tool_layout"], "package");
}

#[test]
fn restore_from_a_package_machine_keeps_dpkg_files_untouched() {
    let env = Env::new();
    dpkg_files(&env);
    assert_eq!(env.cli_env(&["install", "--yes"], PKG).code, 0);
    let archive = archives(&env.backups).pop().unwrap();
    env.mutate();
    env.set_config(&[("retention", "7")]);
    // une copie locale périmée ne doit pas non plus être ressuscitée par la restauration
    let run = env.cli_env(&["restore", archive.to_str().unwrap(), "--yes"], PKG);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("fourni par un paquet dpkg"), "{}", run.all());
    assert!(!env.root.join("usr/local/lib/libvirt-backup").exists(), "les programmes de l'archive ne sont pas extraits");
    assert!(!env.root.join("usr/local/bin/libvirt-backup").exists());
    assert_eq!(env.read(POLICY), "<policyconfig>paquet</policyconfig>\n");
    let cfg: serde_json::Value = serde_json::from_str(&env.read("etc/libvirt-backup/config.json")).unwrap();
    assert_eq!(cfg["retention"], 3, "les réglages sont restaurés ({cfg})");
    assert!(env.read("etc/libvirt/qemu/vm1.xml").contains("vm1"));
    assert_eq!(env.read(UNIT), unit_text(Layout::Package));
}

#[test]
fn archive_from_a_package_machine_installs_the_tool_locally_on_a_bare_machine() {
    // machine A : paquet ; machine B (vierge, sans paquet) : restauration avec le binaire extrait de l'archive
    let a = Env::new();
    dpkg_files(&a);
    assert_eq!(a.cli_env(&["install", "--yes"], PKG).code, 0);
    let archive = archives(&a.backups).pop().unwrap();

    let b = Env::new();
    fs::remove_dir_all(b.root.join("etc/libvirt")).unwrap();
    let run = b.cli(&["restore", archive.to_str().unwrap(), "--yes", "--backup-dir", b.backups.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(b.root.join("usr/local/lib/libvirt-backup/libvirt-backup").is_file());
    assert!(b.root.join("usr/local/bin/libvirt-backup").is_file());
    // l'unité de la machine A appelait /usr/bin : elle est réécrite pour les programmes réellement présents
    assert_eq!(b.read(UNIT), unit_text(Layout::Local));
    assert!(run.all().contains("Unité du service de surveillance adaptée"), "{}", run.all());
    assert_eq!(status(&b, &[])["layout"], "local");
}

#[test]
fn install_replaces_a_previous_local_installation() {
    let env = Env::new();
    assert_eq!(env.cli(&["install", "--yes", "--no-first-backup"]).code, 0);
    assert!(env.root.join("usr/local/lib/libvirt-backup/libvirt-backup").is_file());
    write_file(&env.root.join("usr/local/sbin/mine.sh"), "#!/bin/sh\n", 0o755);
    dpkg_files(&env);
    // une version compilée refuse de s'installer par-dessus le paquet
    let refused = env.cli(&["install", "--yes", "--no-first-backup"]);
    assert_ne!(refused.code, 0);
    assert!(refused.all().contains("paquet libvirt-backup est installé"), "{}", refused.all());
    // le paquet, lui, reprend la main et nettoie /usr/local
    let run = env.cli_env(&["install", "--yes", "--no-first-backup"], PKG);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("Ancienne installation locale retirée"), "{}", run.all());
    assert!(!env.root.join("usr/local/lib/libvirt-backup").exists());
    assert!(!env.root.join("usr/local/bin/libvirt-backup").exists());
    assert!(env.root.join("usr/local/sbin/mine.sh").exists(), "les scripts de l'utilisateur ne sont pas touchés");
    assert_eq!(env.read(POLICY), "<policyconfig>paquet</policyconfig>\n");
    assert_eq!(env.read(UNIT), unit_text(Layout::Package));
}

#[test]
fn uninstall_purge_leaves_the_programs_to_dpkg() {
    let env = Env::new();
    dpkg_files(&env);
    assert_eq!(env.cli_env(&["install", "--yes"], PKG).code, 0);
    let plain = env.cli_env(&["uninstall"], PKG);
    assert_eq!(plain.code, 0, "{}", plain.all());
    assert!(plain.all().contains("apt remove libvirt-backup"), "{}", plain.all());
    assert!(!env.root.join(UNIT).exists());
    assert!(env.root.join("etc/libvirt-backup/config.json").exists());
    let run = env.cli_env(&["uninstall", "--purge"], PKG);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(!env.root.join("etc/libvirt-backup").exists());
    for kept in ["usr/bin/libvirt-backup", "usr/bin/libvirt-backup-gui", POLICY] {
        assert!(env.root.join(kept).exists(), "{kept} appartient à dpkg");
    }
    assert!(!archives(&env.backups).is_empty(), "les archives sont conservées");
}

#[test]
fn legacy_shell_unit_is_still_detected_in_package_mode() {
    let env = Env::new();
    dpkg_files(&env);
    env.write(UNIT, "[Service]\nExecStart=/usr/local/sbin/watch-libvirt-config\n");
    assert_eq!(status(&env, PKG)["legacy_unit"], true);
    // une restauration ne réécrit pas l'unité d'un autre outil
    assert!(!libvirt_backup::selfinstall::align_unit(&env.root, Layout::Package).unwrap());
    assert!(env.read(UNIT).contains("watch-libvirt-config"));
}

#[test]
fn status_warns_when_a_local_install_shadows_the_package() {
    let env = Env::new();
    assert_eq!(env.cli(&["install", "--yes", "--no-first-backup"]).code, 0);
    dpkg_files(&env);
    let data = status(&env, PKG);
    assert_eq!(data["local_shadow"], true);
    let text = env.cli_env(&["status"], PKG);
    assert!(text.all().contains("masque le paquet"), "{}", text.all());
    // une fois le paquet pris en charge par « install », l'avertissement disparaît
    assert_eq!(env.cli_env(&["install", "--yes", "--no-first-backup"], PKG).code, 0);
    assert_eq!(status(&env, PKG)["local_shadow"], false);
    assert!(!env.cli_env(&["status"], PKG).all().contains("masque le paquet"));
    // et jamais d'avertissement en installation locale
    let local = Env::new();
    assert_eq!(local.cli(&["install", "--yes", "--no-first-backup"]).code, 0);
    assert_eq!(status(&local, &[])["local_shadow"], false);
}
