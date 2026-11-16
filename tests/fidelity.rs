//! Fidélité aux scripts d'origine et unités de base.

mod common;

use std::os::unix::net::UnixDatagram;
use std::process::Stdio;
use std::time::Duration;

use common::*;
use libvirt_backup::archive::{classify, Class};
use libvirt_backup::config::{group_of, Config, APPARMOR_PROFILES};
use libvirt_backup::selfinstall::{unit_text, Layout};
use libvirt_backup::sys::sd_notify;

#[test]
fn defaults_match_original_scripts() {
    let cfg = Config::default();
    assert_eq!(cfg.retention, 30, "RETENTION=30");
    assert_eq!(cfg.safety_retention, 0, "l'ancien restaurateur ne supprimait jamais ses archives de sécurité");
    assert_eq!(APPARMOR_PROFILES, ["usr.sbin.libvirtd"]);
    // Même unité dans les deux dispositions, hors chemin de la commande.
    let packaged = unit_text(Layout::Package);
    assert_eq!(packaged.replace("/usr/bin/libvirt-backup watch", "/usr/local/bin/libvirt-backup watch"), unit_text(Layout::Local));
    let unit = unit_text(Layout::Local);
    for line in [
        "Type=notify",
        "NotifyAccess=main",
        "TimeoutStartSec=300",
        "User=root",
        "UMask=0077",
        "Restart=always",
        "RestartSec=5",
        "After=local-fs.target libvirtd.service",
        "WantedBy=multi-user.target",
        "ExecStart=/usr/local/bin/libvirt-backup watch",
    ] {
        assert!(unit.lines().any(|l| l == line), "ligne manquante : {line}");
    }
}

#[test]
fn safety_archives_are_never_rotated_by_default() {
    let env = Env::new();
    env.set_config(&[("safety_retention", "0")]);
    env.backup();
    for _ in 0..4 {
        env.write("etc/default/grub", "CHANGED\n");
        assert_eq!(env.cli(&["restore", "--latest", "--yes", "--only", "boot"]).code, 0);
    }
    assert_eq!(safety_archives(&env.backups).len(), 4);
}

#[test]
fn safety_retention_can_be_enabled() {
    let env = Env::new();
    env.backup();
    for _ in 0..4 {
        env.write("etc/default/grub", "CHANGED\n");
        assert_eq!(env.cli(&["restore", "--latest", "--yes", "--only", "boot"]).code, 0);
    }
    assert_eq!(safety_archives(&env.backups).len(), 2, "safety_retention=2 dans la configuration de test");
}

#[test]
fn classification_of_archive_members() {
    assert_eq!(classify("etc/libvirt/qemu/vm.xml"), Class::System);
    assert_eq!(classify("./etc/default/grub"), Class::System, "ancien format avec préfixe ./");
    assert_eq!(classify("etc/libvirt/"), Class::System);
    assert_eq!(classify("README.txt"), Class::Meta);
    assert_eq!(classify("host-inventory/manifest.json"), Class::Meta);
    assert_eq!(classify("virsh-exports/domains/a.persistent.xml"), Class::Meta);
    assert_eq!(classify("../x"), Class::Dangerous);
    assert_eq!(classify("/etc/passwd"), Class::Dangerous);
    assert_eq!(classify("etc/passwd"), Class::Unknown);
    assert_eq!(classify("etc/libvirtX/a"), Class::Unknown, "préfixe de chemin, pas de dossier");
    assert_eq!(group_of("etc/libvirt-backup/config.json"), Some("tool"));
    assert_eq!(group_of("etc/libvirt/qemu/a.xml"), Some("libvirt"));
}

#[test]
fn sd_notify_sends_datagram() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notify.sock");
    let server = UnixDatagram::bind(&path).unwrap();
    server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    std::env::set_var("NOTIFY_SOCKET", &path);
    assert!(sd_notify("READY=1"));
    let mut buf = [0u8; 100];
    let n = server.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"READY=1");
    std::env::remove_var("NOTIFY_SOCKET");
    assert!(!sd_notify("READY=1"));
}

#[test]
fn administrator_rights_are_required_and_json_mode_never_prompts() {
    if libvirt_backup::sys::is_root() {
        return; // ce test n'a de sens que pour un utilisateur ordinaire
    }
    let out = std::process::Command::new(CLI).args(["backup", "--json"]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("root"), "{}", String::from_utf8_lossy(&out.stdout));
    let out = std::process::Command::new(CLI).args(["backup"]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "sans terminal : jamais d'invite sudo");
}

#[test]
fn sudo_is_used_when_a_terminal_is_available() {
    if libvirt_backup::sys::is_root() || !std::path::Path::new("/usr/bin/script").exists() {
        return;
    }
    let env = Env::new();
    // faux sudo : consigne ses arguments au lieu de demander un mot de passe
    let log = env.tmp.path().join("sudo.log");
    write_file(&env.stubs.join("sudo"), &format!("#!/bin/sh\necho \"$@\" > {}\n", shell_quote(&log.to_string_lossy())), 0o755);
    let mut line = format!("{} backup --if-changed", shell_quote(CLI));
    line.push_str(" 2>&1");
    let out = std::process::Command::new("script")
        .args(["-qec", &line, "/dev/null"])
        .env("PATH", env.path_env())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = out;
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logged.starts_with("-- ") && logged.contains("libvirt-backup") && logged.trim_end().ends_with("backup --if-changed"),
        "{logged:?}"
    );
}
