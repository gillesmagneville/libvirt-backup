//! Contenu de l'archive (mode allégé), simulation, manifeste en tête et appels `virsh` en parallèle.

mod common;

use std::fs;
use std::time::Instant;

use common::*;

const ELF: &[u8] = b"\x7fELF\x02\x01\x01\x00 pretend compiled program";

/// Dépose, dans la fausse racine, un script, un programme compilé, un gros script et un gros fichier.
fn add_heavy_files(env: &Env) {
    write_file(&env.root.join("usr/local/bin/small-tool.sh"), "#!/bin/sh\necho hello\n", 0o755);
    fs::write(env.root.join("usr/local/bin/compiled"), ELF).unwrap();
    fs::write(env.root.join("usr/local/sbin/big-script.sh"), vec![b'#'; 1_572_864]).unwrap();
    let blob = fs::File::create(env.root.join("etc/libvirt/qemu/blob.bin")).unwrap();
    blob.set_len(17 << 20).unwrap();
}

#[test]
fn lean_backup_skips_compiled_programs_and_large_files() {
    let env = Env::new();
    add_heavy_files(&env);
    let run = env.cli(&["backup"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let archive = env.backup();
    let members = names(&archive);
    // les scripts et la configuration sont là
    assert!(members.contains(&"usr/local/bin/small-tool.sh".to_string()));
    assert!(members.contains(&"usr/local/sbin/hook.sh".to_string()));
    assert!(members.contains(&"etc/libvirt/qemu/vm1.xml".to_string()));
    // les programmes compilés et les fichiers volumineux sont écartés
    assert!(!members.contains(&"usr/local/bin/compiled".to_string()), "{members:?}");
    assert!(!members.contains(&"usr/local/sbin/big-script.sh".to_string()));
    assert!(!members.contains(&"etc/libvirt/qemu/blob.bin".to_string()));
    // l'utilisateur en est informé, avec le moyen de tout archiver
    let text = run.all();
    assert!(text.contains("Ignoré (sauvegarde allégée) : 3 fichier(s)"), "{text}");
    assert!(text.contains("/usr/local/bin/compiled (exécutable compilé"), "{text}");
    assert!(text.contains("--full"), "{text}");
    assert!(text.contains("Durées :"), "{text}");
    // le manifeste et le README le consignent
    let manifest: serde_json::Value = serde_json::from_str(&member(&archive, "host-inventory/manifest.json").unwrap()).unwrap();
    assert_eq!(manifest["lean_backup"], true);
    assert_eq!(manifest["skipped_count"], 3);
    assert_eq!(manifest["skipped"].as_array().unwrap().len(), 3);
    assert!(member(&archive, "README.txt").unwrap().contains("Mode allégé"));
    // l'archive reste valide pour la restauration
    let verify = env.cli(&["verify", "--all"]);
    assert_eq!(verify.code, 0, "{}", verify.all());
}

#[test]
fn full_option_and_setting_archive_everything_like_the_original_script() {
    let env = Env::new();
    add_heavy_files(&env);

    let run = env.cli(&["backup", "--full"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let members = names(archives(&env.backups).last().unwrap());
    for expected in ["usr/local/bin/compiled", "usr/local/sbin/big-script.sh", "etc/libvirt/qemu/blob.bin"] {
        assert!(members.contains(&expected.to_string()), "{expected} absent : --full doit tout archiver");
    }

    // le réglage persistant a le même effet, sans option
    fs::remove_file(archives(&env.backups).last().unwrap()).unwrap();
    env.set_config(&[("lean_backup", "false")]);
    let run = env.cli(&["backup"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let latest = archives(&env.backups).last().unwrap().clone();
    assert!(names(&latest).contains(&"usr/local/bin/compiled".to_string()));
    assert!(member(&latest, "README.txt").unwrap().contains("Mode complet"));
    assert!(run.all().contains("Sauvegarde complète"), "{}", run.all());
}

#[test]
fn lean_backup_setting_is_readable_and_writable() {
    let env = Env::new();
    let show = env.cli(&["config", "show"]);
    assert!(show.stdout.contains("lean_backup=true"), "{}", show.stdout);
    let set = env.cli(&["config", "set", "lean_backup=non"]);
    assert_eq!(set.code, 0, "{}", set.all());
    assert!(env.cli(&["config", "show"]).stdout.contains("lean_backup=false"));
    let bad = env.cli(&["config", "set", "lean_backup=peut-etre"]);
    assert_ne!(bad.code, 0);
    assert!(bad.all().contains("Valeur invalide pour lean_backup"), "{}", bad.all());
}

#[test]
fn manifest_is_the_first_member_so_it_reads_without_unpacking_everything() {
    let env = Env::new();
    add_heavy_files(&env);
    env.backup();
    let archive = archives(&env.backups).last().unwrap().clone();
    assert_eq!(names(&archive).first().map(String::as_str), Some("host-inventory/manifest.json"));
    // le manifeste n'apparaît qu'une fois, et le répertoire host-inventory/ n'est pas dupliqué
    let members = names(&archive);
    assert_eq!(members.iter().filter(|n| n.as_str() == "host-inventory/manifest.json").count(), 1);
    assert_eq!(members.iter().filter(|n| n.as_str() == "host-inventory").count(), 1);
    let manifest = libvirt_backup::archive::read_manifest(&archive);
    assert_eq!(manifest["tool"], "libvirt-backup");
}

#[test]
fn if_changed_ignores_files_that_are_not_archived() {
    let env = Env::new();
    add_heavy_files(&env);
    assert_eq!(env.cli(&["backup"]).code, 0);
    assert_eq!(archives(&env.backups).len(), 1);

    // un programme compilé qui change n'est pas de la configuration : pas de nouvelle archive
    fs::write(env.root.join("usr/local/bin/compiled"), [ELF, b" v2"].concat()).unwrap();
    let run = env.cli(&["backup", "--if-changed"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(run.all().contains("inchangée"), "{}", run.all());
    assert_eq!(archives(&env.backups).len(), 1);

    // un script qui change, si
    env.write("usr/local/bin/small-tool.sh", "#!/bin/sh\necho changed\n");
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(env.cli(&["backup", "--if-changed"]).code, 0);
    assert_eq!(archives(&env.backups).len(), 2);
}

#[test]
fn dry_run_describes_the_content_and_writes_nothing() {
    let env = Env::new();
    add_heavy_files(&env);
    let run = env.cli(&["backup", "--dry-run"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let text = run.all();
    for expected in ["Simulation de sauvegarde", "libvirt", "scripts", "Plus gros fichiers retenus", "Ignoré (sauvegarde allégée) : 3"] {
        assert!(text.contains(expected), "« {expected} » absent :\n{text}");
    }
    assert!(archives(&env.backups).is_empty(), "une simulation ne crée aucune archive");
    assert!(!env.backups.join("libvirt-config-latest.tar.gz").exists());
    let leftovers: Vec<_> = fs::read_dir(env.root.join("run")).map(|rd| rd.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
    assert!(leftovers.iter().all(|n: &std::ffi::OsString| !n.to_string_lossy().starts_with("libvirt-config-backup.")), "{leftovers:?}");

    // protocole JSON : totaux par groupe et liste des fichiers ignorés
    let json = env.cli(&["backup", "--dry-run", "--json"]);
    let result = json.events().into_iter().find(|e| e["event"] == "result").expect("événement result");
    let data = &result["data"];
    assert_eq!(data["dry_run"], true);
    assert_eq!(data["skipped"].as_array().unwrap().len(), 3);
    let scripts = data["groups"].as_array().unwrap().iter().find(|g| g["id"] == "scripts").unwrap();
    assert_eq!(scripts["skipped"], 2);
    assert!(data["files"].as_u64().unwrap() > 5);

    // --full dans la simulation : plus rien d'écarté
    let full = env.cli(&["backup", "--dry-run", "--full", "--json"]);
    let result = full.events().into_iter().find(|e| e["event"] == "result").unwrap();
    assert_eq!(result["data"]["skipped"].as_array().unwrap().len(), 0);
}

#[test]
fn virsh_exports_run_in_parallel_and_stay_complete() {
    let env = Env::new();
    let domains: Vec<String> = (1..=8).map(|n| format!("vm{n}")).collect();
    env.write_stub("domains", &domains.iter().map(|d| format!("{d}\n")).collect::<String>());
    env.set_states(&domains.iter().map(|d| format!("{d}=shut off\n")).collect::<String>());
    // chaque appel à virsh dure 0,3 s : en série, les 29 appels demanderaient plus de 8 secondes
    let stub = fs::read_to_string(env.stubs.join("virsh")).unwrap();
    write_file(&env.stubs.join("virsh"), &stub.replacen('\n', "\nsleep 0.3\n", 1), 0o755);

    let begin = Instant::now();
    let run = env.cli(&["backup"]);
    let elapsed = begin.elapsed();
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(elapsed.as_secs_f64() < 6.0, "appels virsh en série ? {elapsed:?}\n{}", run.all());

    let archive = archives(&env.backups).last().unwrap().clone();
    let members = names(&archive);
    for d in &domains {
        for kind in ["persistent", "current"] {
            let member = format!("virsh-exports/domains/{d}.{kind}.xml");
            assert!(members.contains(&member), "{member} absent");
        }
    }
    assert!(members.contains(&"virsh-exports/networks/default.xml".to_string()));
    assert!(members.contains(&"virsh-exports/pools/images.xml".to_string()));
    assert_eq!(member(&archive, "virsh-exports/domains.list").unwrap().lines().count(), 8);
    assert_eq!(member(&archive, "virsh-exports/pools.list").unwrap().trim(), "images");
}

#[test]
fn files_removed_during_the_backup_do_not_make_it_fail() {
    // tar reçoit une liste explicite : un fichier disparu entre le parcours et la lecture (ici, supprimé
    // par le faux virsh pendant l'export) doit être un avertissement, pas un échec de la sauvegarde.
    let env = Env::new();
    let victim = env.root.join("etc/libvirt/qemu/vm2.xml");
    let stub = fs::read_to_string(env.stubs.join("virsh")).unwrap();
    // seulement pendant l'export (dumpxml), c'est-à-dire après le parcours des fichiers
    let hook = format!("\ncase \"$*\" in *dumpxml*) rm -f '{}' ;; esac\n", victim.display());
    write_file(&env.stubs.join("virsh"), &stub.replacen('\n', &hook, 1), 0o755);

    let run = env.cli(&["backup"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(!victim.exists(), "le faux virsh aurait dû supprimer le fichier");
    assert!(run.all().contains("vm2.xml"), "l'avertissement de tar doit être montré :\n{}", run.all());
    let archive = archives(&env.backups).last().unwrap().clone();
    assert!(names(&archive).contains(&"etc/libvirt/qemu/vm1.xml".to_string()));
    assert_eq!(env.cli(&["verify", "--all"]).code, 0);
}
