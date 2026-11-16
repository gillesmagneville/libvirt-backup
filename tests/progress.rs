//! Jauge de progression de la sauvegarde : événements `progress` du protocole JSON et jauge du terminal.

mod common;

use std::fs;

use common::*;

/// Fractions des événements `progress` d'une sortie JSON, dans l'ordre.
fn fractions(stdout: &str) -> Vec<f64> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event"] == "progress")
        .map(|v| v["fraction"].as_f64().unwrap())
        .collect()
}

#[test]
fn json_backup_reports_a_monotonic_progress_from_zero_to_one() {
    let env = Env::new();
    // Un contenu volumineux et peu compressible : la phase de compression envoie de nombreux paliers.
    let mut blob = vec![0u8; 24 << 20];
    let mut seed = 0x2545F4914F6CDD1Du64;
    for chunk in blob.chunks_mut(8) {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        chunk.copy_from_slice(&seed.to_le_bytes()[..chunk.len()]);
    }
    fs::write(env.root.join("etc/libvirt/qemu/blob.bin"), &blob).unwrap();
    env.set_config(&[("lean_backup", "false")]);

    let run = env.cli(&["backup", "--json"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let values = fractions(&run.stdout);
    assert_eq!(values.first(), Some(&0.0), "la jauge démarre à 0 : {values:?}");
    assert_eq!(values.last(), Some(&1.0), "la jauge se termine à 100 % : {values:?}");
    assert!(values.windows(2).all(|w| w[0] < w[1]), "la progression ne recule jamais et ne se répète pas : {values:?}");
    // 24 Mio de données : bien plus que les quelques jalons fixes (2, 8, 10, 30, 45, 90, 98 et 100 %)
    let during_compression = values.iter().filter(|f| **f > 0.45 && **f < 0.90).count();
    assert!(during_compression >= 10, "{during_compression} paliers seulement pendant la compression : {values:?}");
    assert!(values.len() <= 210, "la jauge ne doit pas inonder la sortie : {} événements", values.len());
    // le protocole garde ses autres événements, et le fichier est bien archivé
    assert!(run.stdout.contains("\"event\":\"step\""), "{}", run.stdout);
    assert!(names(&env.backup()).contains(&"etc/libvirt/qemu/blob.bin".to_string()));
}

#[test]
fn progress_events_do_not_reach_the_text_output_of_other_operations() {
    let env = Env::new();
    assert_eq!(env.cli(&["backup"]).code, 0);
    // une opération sans jauge (liste, vérification) n'émet aucun événement de progression
    let run = env.cli(&["verify", "--all", "--json"]);
    assert_eq!(run.code, 0, "{}", run.all());
    assert!(fractions(&run.stdout).is_empty(), "{}", run.stdout);
}

#[test]
fn small_backup_still_ends_at_one_hundred_percent() {
    let env = Env::new();
    let run = env.cli(&["backup", "--json"]);
    assert_eq!(run.code, 0, "{}", run.all());
    let values = fractions(&run.stdout);
    assert_eq!(values.first(), Some(&0.0));
    assert_eq!(values.last(), Some(&1.0));
    assert!(values.windows(2).all(|w| w[0] < w[1]), "{values:?}");
}

#[test]
fn text_gauge_bar_fills_proportionally() {
    use libvirt_backup::report::gauge_bar;
    let width = |s: &str| s.chars().count();
    let empty = gauge_bar(0.0, 10);
    let half = gauge_bar(0.5, 10);
    let full = gauge_bar(1.0, 10);
    assert!(width(&empty) == 12 && width(&half) == 12 && width(&full) == 12, "{empty} {half} {full}");
    let filled = |s: &str| s.chars().filter(|c| *c == '█' || *c == '#').count();
    assert_eq!((filled(&empty), filled(&half), filled(&full)), (0, 5, 10));
    assert_eq!(filled(&gauge_bar(7.0, 10)), 10, "au-delà de 1, la barre reste pleine");
}
