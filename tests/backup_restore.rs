mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::*;
use libvirt_backup::archive::{inspect_archive, list_archives, resolve_archive, Kind};
use libvirt_backup::config::{group_ids, MANIFEST_MEMBER, POLICY_REL};
use libvirt_backup::restore::{parse_groups, plan_restore};
use libvirt_backup::selfinstall::{build_payload, write_payload, Binaries};

// ---------------------------------------------------------------- sauvegarde

#[test]
fn backup_layout_permissions_and_latest_link() {
    let env = Env::new();
    let path = env.backup();
    assert_eq!(mode(&path), 0o600);
    let latest = env.backups.join("libvirt-config-latest.tar.gz");
    assert!(fs::symlink_metadata(&latest).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(&latest).unwrap().to_string_lossy(), path.file_name().unwrap().to_string_lossy());
    let members = names(&path);
    for expected in [
        "etc/libvirt/qemu/vm1.xml",
        "etc/libvirt/qemu/autostart/vm1.xml",
        "usr/local/sbin/hook.sh",
        "README.txt",
        MANIFEST_MEMBER,
        "host-inventory/host-report.txt",
        "virsh-exports/domains/vm1.persistent.xml",
        "virsh-exports/domains/vm1.current.xml",
        "virsh-exports/networks/default.xml",
        "virsh-exports/pools/images.xml",
    ] {
        assert!(members.iter().any(|m| m == expected), "{expected} absent de {members:?}");
    }
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(&path).unwrap()));
    let link = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap())
        .find(|e| e.path().unwrap().to_string_lossy() == "etc/libvirt/qemu/autostart/vm1.xml")
        .unwrap();
    assert!(link.header().entry_type().is_symlink());
    assert_eq!(link.link_name().unwrap().unwrap().to_string_lossy(), "../vm1.xml");

    let report = inspect_archive(&path).unwrap();
    assert!(report.valid());
    for group in ["libvirt", "apparmor", "scripts", "boot", "storage", "tool"] {
        assert!(report.group_count(group) > 0, "groupe {group} vide");
    }
    assert_eq!(report.manifest["vm_state_included"], true);
}

#[test]
fn nvram_included_only_when_all_vms_stopped() {
    let env = Env::new();
    let first = env.backup();
    assert!(names(&first).iter().any(|m| m == "var/lib/libvirt/qemu/nvram/vm1_VARS.fd"));

    env.set_states("vm1=running\nvm2=shut off\n");
    let second = env.backup();
    assert!(!names(&second).iter().any(|m| m == "var/lib/libvirt/qemu/nvram/vm1_VARS.fd"));
    assert!(inspect_archive(&second).unwrap().readme.contains("omis"));

    env.set_states("vm1=paused\nvm2=shut off\n");
    assert_eq!(inspect_archive(&env.backup()).unwrap().group_count("nvram"), 0);

    fs::write(env.stubs.join("virsh_down"), "").unwrap();
    assert_eq!(inspect_archive(&env.backup()).unwrap().group_count("nvram"), 0);
}

#[test]
fn retention_keeps_newest() {
    let env = Env::new();
    let created: Vec<PathBuf> = (0..5).map(|_| env.backup()).collect();
    let left = archives(&env.backups);
    assert_eq!(left, created[2..].to_vec());
    let temporary: Vec<_> =
        fs::read_dir(&env.backups).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(temporary.is_empty());
}

#[test]
fn if_changed_skips_identical_and_ignores_touch() {
    let env = Env::new();
    assert_eq!(env.cli(&["backup", "--if-changed"]).code, 0);
    assert_eq!(archives(&env.backups).len(), 1);
    // un simple touch ne change pas le contenu
    let grub = env.root.join("etc/default/grub");
    let content = fs::read(&grub).unwrap();
    fs::write(&grub, content).unwrap();
    let again = env.cli(&["backup", "--if-changed", "--json"]);
    assert_eq!(again.code, 0, "{}", again.all());
    assert_eq!(archives(&env.backups).len(), 1, "aucune nouvelle archive attendue");
    env.write("etc/default/grub", "changed\n");
    assert_eq!(env.cli(&["backup", "--if-changed"]).code, 0);
    assert_eq!(archives(&env.backups).len(), 2);
}

#[test]
fn busy_lock_skips_backup() {
    let env = Env::new();
    let mut holder = libvirt_backup::sys::FileLock::new(&env.root.join("run/lock/libvirt-config-backup.lock"));
    assert!(holder.try_acquire().unwrap());
    let run = env.cli(&["backup"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("déjà en cours"), "{}", run.all());
    assert!(archives(&env.backups).is_empty());
    holder.release();
    assert_eq!(env.cli(&["backup"]).code, 0);
    assert_eq!(archives(&env.backups).len(), 1);
}

// -------------------------------------------------------------- restauration

#[test]
fn restore_roundtrip_merge_safety_payload_and_virsh() {
    let env = Env::new();
    let backed = env.backup();
    env.mutate();
    let run = env.cli(&["restore", "--latest", "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());

    assert_eq!(env.read("etc/libvirt/qemu/vm1.xml"), "<domain><name>vm1</name></domain>\n");
    let hook = env.root.join("usr/local/sbin/hook.sh");
    assert!(fs::read_to_string(&hook).unwrap().starts_with("#!/bin/sh"));
    assert_ne!(mode(&hook) & 0o111, 0, "hook.sh doit rester exécutable");
    let link = env.root.join("etc/libvirt/qemu/autostart/vm1.xml");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(&link).unwrap().to_string_lossy(), "../vm1.xml");
    assert!(env.root.join("etc/libvirt/qemu/extra.xml").exists(), "aucune suppression globale");
    assert_eq!(env.read("etc/default/grub"), "GRUB_CMDLINE_LINUX=\"intel_iommu=on\"\n");

    let safeties = safety_archives(&env.backups);
    assert_eq!(safeties.len(), 1);
    assert!(member(&safeties[0], "etc/default/grub").unwrap().contains("CHANGED"), "état d'AVANT la restauration");
    assert_eq!(archives(&env.backups), vec![backed]);

    // l'outil est réinstallé par la restauration
    for rel in ["usr/local/lib/libvirt-backup/libvirt-backup", "usr/local/bin/libvirt-backup", POLICY_REL] {
        assert!(env.root.join(rel).exists(), "{rel} absent");
    }

    let calls = env.calls();
    assert!(calls.iter().any(|c| c.starts_with("virsh") && c.contains("net-define")));
    assert!(calls.iter().any(|c| c.contains("pool-define")));
    assert_eq!(calls.iter().filter(|c| c.contains(" define ")).count(), 2, "vm1 et vm2 (persistent.xml)");
    assert!(calls.iter().any(|c| c == "systemctl restart libvirtd.service"));
    let stop = calls.iter().position(|c| c == "systemctl stop libvirt-config-backup-watch.service").unwrap();
    let restart = calls.iter().position(|c| c == "systemctl restart libvirt-config-backup-watch.service").unwrap();
    assert!(stop < restart);
}

#[test]
fn restore_refuses_running_vm_and_qemu_process() {
    let env = Env::new();
    env.backup();
    env.write("etc/default/grub", "CHANGED\n");
    env.set_states("vm1=running\nvm2=shut off\n");
    let run = env.cli(&["restore", "--latest", "--yes"]);
    assert_eq!(run.code, 1);
    assert!(run.all().contains("vm1"), "{}", run.all());
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n", "rien ne doit être modifié");
    assert!(safety_archives(&env.backups).is_empty());

    env.set_states("vm1=shut off\nvm2=shut off\n");
    write_file(&env.root.join("proc/4242/cmdline"), "/usr/bin/qemu-system-x86_64\0-name\0vm1\0", 0o644);
    let run = env.cli(&["restore", "--latest", "--yes"]);
    assert_eq!(run.code, 1);
    assert!(run.all().contains("QEMU"), "{}", run.all());
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n");
}

#[test]
fn restore_only_selected_groups() {
    let env = Env::new();
    env.backup();
    env.mutate();
    env.write("etc/libvirt-backup/config.json", "CHANGED");
    // réglages illisibles : la restauration doit tout de même pouvoir s'exécuter (comme sous sudo)
    let backups = env.backups.to_string_lossy().into_owned();
    let user = env.user.clone();
    let run = env.cli_env(&["restore", "--latest", "--yes", "--only", "libvirt", "--backup-dir", &backups], &[("SUDO_USER", &user)]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("réglages par défaut"), "{}", run.all());
    assert_eq!(env.read("etc/libvirt-backup/config.json"), "CHANGED", "le préfixe etc/libvirt n'est pas etc/libvirt-backup");
    assert_eq!(env.read("etc/libvirt/qemu/vm1.xml"), "<domain><name>vm1</name></domain>\n");
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n");
    assert!(!env.root.join("usr/local/sbin/hook.sh").exists());
    assert!(parse_groups(Some("inexistant"), None).is_err());
    assert!(!parse_groups(None, Some("network")).unwrap().contains(&"network"));
    assert!(parse_groups(Some("boot"), Some("boot")).is_err());
}

#[test]
fn dry_run_changes_nothing() {
    let env = Env::new();
    env.backup();
    env.mutate();
    let run = env.cli(&["restore", "--latest", "--dry-run"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n");
    assert!(safety_archives(&env.backups).is_empty());
    assert!(!env.calls().iter().any(|c| c.contains("stop")));
}

#[test]
fn restore_from_legacy_archive_without_manifest() {
    // Format produit par l'ancien script shell : mêmes chemins, sans manifest.json.
    let env = Env::new();
    let stage = env.tmp.path().join("stage");
    write_file(&stage.join("virsh-exports/domains/vm1.persistent.xml"), "<domain><name>vm1</name></domain>", 0o644);
    write_file(&stage.join("host-inventory/host-report.txt"), "x", 0o644);
    write_file(&stage.join("README.txt"), "Sauvegarde automatique ... NVRAM et TPM omis", 0o644);
    fs::create_dir_all(&env.backups).unwrap();
    let legacy = env.backups.join("libvirt-config-20260101-120000-123456789.tar.gz");
    let status = Command::new("tar")
        .args(["-czpf"])
        .arg(&legacy)
        .arg("-C")
        .arg(&env.root)
        .args(["etc/libvirt", "usr/local/sbin", "-C"])
        .arg(&stage)
        .args(["README.txt", "virsh-exports", "host-inventory"])
        .status()
        .unwrap();
    assert!(status.success());
    env.write("etc/libvirt/qemu/vm1.xml", "CHANGED");
    let plan = plan_restore(&legacy, None).unwrap();
    assert!(plan.notes.iter().any(|n| n.contains("omis")));
    let run = env.cli(&["restore", legacy.to_str().unwrap(), "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(env.read("etc/libvirt/qemu/vm1.xml").starts_with("<domain>"));
    assert!(env.calls().iter().any(|c| c.contains(" define ")));
}

#[test]
fn malicious_archives_are_refused() {
    let env = Env::new();
    for evil in ["../evil", "etc/../../evil", "/etc/passwd", "foo/bar", "etc/libvirt/../../x"] {
        let bad = env.tmp.path().join("bad.tar.gz");
        raw_archive(&bad, &[("etc/libvirt/ok.xml", b"ok", None), (evil, b"x", None)]);
        assert!(plan_restore(&bad, None).is_err(), "{evil} aurait dû être refusé");
        let run = env.cli(&["restore", bad.to_str().unwrap(), "--yes"]);
        assert_eq!(run.code, 1, "{evil} : {}", run.all());
        assert!(!env.root.join("evil").exists());
    }
}

#[test]
fn hardlink_outside_whitelist_refused() {
    let env = Env::new();
    let bad = env.tmp.path().join("bad.tar.gz");
    raw_archive(&bad, &[("etc/libvirt/shadow", b"", Some(("etc/shadow", true)))]);
    let err = plan_restore(&bad, None).unwrap_err();
    assert!(err.to_string().contains("dangereux"), "{err}");
    // un lien symbolique relatif reste admis (autostart/vm1.xml -> ../vm1.xml)
    let good = env.tmp.path().join("good.tar.gz");
    raw_archive(&good, &[("etc/libvirt/qemu/autostart/a.xml", b"", Some(("../a.xml", false))), ("etc/libvirt/qemu/a.xml", b"x", None)]);
    assert!(plan_restore(&good, None).is_ok());
}

#[test]
fn corrupted_archive_is_detected() {
    let env = Env::new();
    let backed = env.backup();
    let mut data = fs::read(&backed).unwrap();
    let middle = data.len() / 2;
    data[middle] ^= 0xFF;
    fs::write(&backed, data).unwrap();
    let err = inspect_archive(&backed).unwrap_err();
    assert!(err.to_string().contains("illisible"), "{err}");
    let run = env.cli(&["verify", "--all"]);
    assert_eq!(run.code, 1, "{}", run.all());
}

#[test]
fn resolve_archive_specs() {
    let env = Env::new();
    let first = env.backup();
    let second = env.backup();
    let dir = &env.backups;
    assert_eq!(resolve_archive(dir, None).unwrap().path, second);
    assert_eq!(resolve_archive(dir, Some("latest")).unwrap().path, second);
    assert_eq!(resolve_archive(dir, Some("2")).unwrap().path, first);
    assert_eq!(resolve_archive(dir, Some(&first.file_name().unwrap().to_string_lossy())).unwrap().path, first);
    assert_eq!(resolve_archive(dir, Some(first.to_str().unwrap())).unwrap().path, first);
    assert!(resolve_archive(dir, Some("9")).is_err());
    assert!(resolve_archive(dir, Some("n-existe-pas")).is_err());
    let listing = list_archives(dir);
    assert_eq!(listing.len(), 2);
    assert!(listing.iter().all(|a| a.kind == Kind::Auto));
}

// ------------------------------------------------------ auto-installation

fn cli_only() -> Binaries {
    Binaries { cli: PathBuf::from(CLI), gui: None }
}

#[test]
fn payload_is_runnable_and_included_in_backups() {
    let env = Env::new();
    let payload = build_payload(&cli_only()).unwrap();
    write_payload(&env.root, &payload).unwrap();
    let installed = env.root.join("usr/local/lib/libvirt-backup/libvirt-backup");
    let out = Command::new(&installed).args(["groups", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let groups: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ids: Vec<&str> = groups.as_array().unwrap().iter().map(|g| g["id"].as_str().unwrap()).collect();
    assert_eq!(ids, group_ids());
    let version = Command::new(&installed).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("libvirt-backup"));
    assert_eq!(mode(&env.root.join("usr/local/bin/libvirt-backup")), 0o755);
    let launcher = fs::read_to_string(env.root.join("usr/local/bin/libvirt-backup")).unwrap();
    assert!(launcher.starts_with("#!/bin/sh") && launcher.contains("exec /usr/local/lib/libvirt-backup/libvirt-backup"));

    let members = names(&env.backup());
    for expected in ["usr/local/lib/libvirt-backup/libvirt-backup", "usr/local/bin/libvirt-backup", POLICY_REL] {
        assert!(members.iter().any(|m| m == expected), "{expected} absent");
    }
}

#[test]
fn extracted_binary_can_restore_on_a_bare_machine() {
    // Amorçage d'un système vierge : seul le programme extrait de l'archive est disponible (ni Python, ni GTK).
    let env = Env::new();
    write_payload(&env.root, &build_payload(&cli_only()).unwrap()).unwrap();
    let backed = env.backup();
    let boot = env.tmp.path().join("boot");
    fs::create_dir(&boot).unwrap();
    let status = Command::new("tar").arg("-xzf").arg(&backed).arg("-C").arg(&boot).arg("usr/local/lib/libvirt-backup").status().unwrap();
    assert!(status.success());
    env.write("etc/default/grub", "CHANGED\n");
    let out = Command::new(boot.join("usr/local/lib/libvirt-backup/libvirt-backup"))
        .args(["restore", backed.to_str().unwrap(), "--yes", "--json", "--root"])
        .arg(&env.root)
        .env("PATH", env.path_env())
        .env("STUB_DIR", &env.stubs)
        .env("STUB_LOG", env.log())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    let last = text.lines().last().unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(last).unwrap(), serde_json::json!({"event": "end", "code": 0}));
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"));
}

// ---------------------------------------------------------------- CLI

#[test]
fn cli_json_protocol_and_list() {
    let env = Env::new();
    let run = env.cli(&["backup", "--json"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let events = run.events();
    let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
    // la jauge démarre à 0 % avant la première étape (événement `progress`, voir tests/progress.rs)
    assert!(matches!(kinds[0], "progress" | "step"), "{kinds:?}");
    assert!(kinds.contains(&"step") && kinds.contains(&"progress"));
    assert!(kinds.contains(&"result"));
    assert_eq!(*kinds.last().unwrap(), "end");
    assert_eq!(events.last().unwrap()["code"], 0);

    let list = env.cli(&["list", "--json"]);
    assert_eq!(list.code, 0);
    let listing: serde_json::Value = serde_json::from_str(list.stdout.trim()).unwrap();
    assert_eq!(listing[0]["index"], 1);
    assert_eq!(listing[0]["kind"], "auto");
}

#[test]
fn cli_restore_requires_confirmation_without_tty() {
    let env = Env::new();
    env.backup();
    env.write("etc/default/grub", "CHANGED\n");
    let run = env.cli(&["restore", "--latest"]);
    assert_eq!(run.code, 1);
    assert!(run.all().contains("--yes"), "{}", run.all());
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n");
}

#[test]
fn cli_restore_and_verify() {
    let env = Env::new();
    env.backup();
    env.write("etc/default/grub", "CHANGED\n");
    assert_eq!(env.cli(&["verify", "--all"]).code, 0);
    assert_eq!(env.cli(&["restore", "--latest", "--yes", "--only", "boot"]).code, 0);
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"));
}

#[test]
fn cli_config_set_validates() {
    let env = Env::new();
    assert_eq!(env.cli(&["config", "set", "retention=7", "debounce=1,5"]).code, 0);
    let saved: serde_json::Value = serde_json::from_str(&env.read("etc/libvirt-backup/config.json")).unwrap();
    assert_eq!(saved["retention"], 7);
    assert_eq!(saved["debounce"], 1.5);
    assert_eq!(env.cli(&["config", "set", "retention=0"]).code, 1);
    assert_eq!(env.cli(&["config", "set", "inconnu=1"]).code, 1);
    assert_eq!(env.cli(&["config", "set", "pas-une-affectation"]).code, 1);
    let shown = env.cli(&["config", "show"]);
    assert!(shown.stdout.contains("retention=7"), "{}", shown.all());
}

#[test]
fn interactive_confirmation_word() {
    let env = Env::new();
    env.backup();
    env.write("etc/default/grub", "CHANGED\n");
    let Some(cancelled) = env.cli_tty(&["restore", "--latest"], "restaurer\n") else { return };
    assert_eq!(cancelled.code, 0, "{}", cancelled.all());
    assert!(cancelled.all().contains("annulée"), "{}", cancelled.all());
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n");
    let done = env.cli_tty(&["restore", "--latest"], "RESTAURER\n").unwrap();
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"));
}

#[test]
fn interactive_selection_of_an_archive() {
    let env = Env::new();
    let first = env.backup();
    env.write("etc/default/grub", "SECOND\n");
    env.backup();
    env.write("etc/default/grub", "CHANGED\n");
    // choix « 2 » = la plus ancienne
    let Some(run) = env.cli_tty(&["restore"], "2\nRESTAURER\n") else { return };
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains(&*first.file_name().unwrap().to_string_lossy()));
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"));
}

// ------------------------------------------------------- service installé

#[test]
fn install_and_uninstall_flow() {
    let env = Env::new();
    let run = env.cli(&["install", "--yes"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let unit = env.read("etc/systemd/system/libvirt-config-backup-watch.service");
    assert_eq!(unit.matches("libvirt-backup watch").count(), 1);
    assert!(env.root.join("usr/local/lib/libvirt-backup/libvirt-backup").is_file());
    let first = archives(&env.backups);
    assert_eq!(first.len(), 1);
    assert!(names(&first[0]).iter().any(|m| m == "usr/local/lib/libvirt-backup/libvirt-backup"), "la 1re archive contient l'outil");
    let calls = env.calls();
    assert!(calls.iter().any(|c| c == "systemctl enable libvirt-config-backup-watch.service"));
    assert!(calls.iter().any(|c| c == "systemctl restart libvirt-config-backup-watch.service"));

    let status = env.cli(&["status", "--json"]);
    let data: serde_json::Value = serde_json::from_str(status.stdout.trim()).unwrap();
    assert_eq!(data["installed"], true);
    assert_eq!(data["legacy_unit"], false);

    let run = env.cli(&["uninstall", "--purge"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(!env.root.join("usr/local/bin/libvirt-backup").exists());
    assert!(!env.root.join("etc/libvirt-backup").exists());
    assert!(!env.root.join("usr/local/lib/libvirt-backup").exists());
    assert!(!archives(&env.backups).is_empty(), "les archives sont conservées");
}

#[test]
fn status_flags_legacy_unit() {
    let env = Env::new();
    env.write("etc/systemd/system/libvirt-config-backup-watch.service", "[Service]\nExecStart=/usr/local/sbin/watch-libvirt-config\n");
    let data: serde_json::Value = serde_json::from_str(env.cli(&["status", "--json"]).stdout.trim()).unwrap();
    assert_eq!(data["legacy_unit"], true);
    env.write(
        "etc/systemd/system/libvirt-config-backup-watch.service",
        &libvirt_backup::selfinstall::unit_text(libvirt_backup::selfinstall::Layout::Local),
    );
    let data: serde_json::Value = serde_json::from_str(env.cli(&["status", "--json"]).stdout.trim()).unwrap();
    assert_eq!(data["legacy_unit"], false);
}

#[test]
fn gui_binary_is_optional_and_reported() {
    // sans interface graphique installée à côté du programme, l'installation continue avec un avertissement
    let env = Env::new();
    let run = env.cli(&["install", "--yes", "--no-first-backup"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let bin_dir = PathBuf::from(CLI).parent().unwrap().to_path_buf();
    let gui_next_to_cli = bin_dir.join("libvirt-backup-gui").is_file();
    assert_eq!(env.root.join("usr/local/bin/libvirt-backup-gui").exists(), gui_next_to_cli);
    if !gui_next_to_cli {
        assert!(run.all().contains("interface graphique non installée"), "{}", run.all());
    }
    let mode_launcher = fs::metadata(env.root.join("usr/local/bin/libvirt-backup")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_launcher, 0o755);
}
