//! Préférences de l'interface graphique (`gui.json`) : lecture, écriture, valeurs invalides.
//!
//! Un seul test : il modifie l'environnement du processus (`XDG_CONFIG_HOME`).

use std::fs;

use libvirt_backup::guiprefs;

#[test]
fn the_language_preference_is_saved_per_user_and_validated() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", tmp.path());
    let file = tmp.path().join("libvirt-backup/gui.json");
    assert_eq!(guiprefs::path().as_deref(), Some(file.as_path()));

    assert_eq!(guiprefs::load_language(), None, "aucune préférence au départ");
    guiprefs::save_language("de").unwrap();
    assert_eq!(guiprefs::load_language().as_deref(), Some("de"));
    guiprefs::save_language("system").unwrap();
    assert_eq!(guiprefs::load_language().as_deref(), Some("system"));

    assert!(guiprefs::save_language("klingon").is_err(), "une langue inconnue est refusée");
    assert_eq!(guiprefs::load_language().as_deref(), Some("system"), "et ne change rien");

    // les autres préférences du fichier sont conservées
    fs::write(&file, r#"{"language": "es", "autre": 42}"#).unwrap();
    guiprefs::save_language("fr").unwrap();
    let saved: serde_json::Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["language"], "fr");
    assert_eq!(saved["autre"], 42);

    // fichier illisible ou valeur invalide : comme s'il n'y avait pas de préférence
    fs::write(&file, "pas du JSON").unwrap();
    assert_eq!(guiprefs::load_language(), None);
    fs::write(&file, r#"{"language": "xx"}"#).unwrap();
    assert_eq!(guiprefs::load_language(), None);
    fs::write(&file, r#"{"language": "FR "}"#).unwrap();
    assert_eq!(guiprefs::load_language().as_deref(), Some("fr"), "casse et espaces tolérés");

    // XDG_CONFIG_HOME relatif ou vide : repli sur ~/.config
    std::env::set_var("HOME", tmp.path().join("home"));
    std::env::set_var("XDG_CONFIG_HOME", "relatif");
    assert_eq!(guiprefs::path().unwrap(), tmp.path().join("home/.config/libvirt-backup/gui.json"));
    std::env::set_var("XDG_CONFIG_HOME", "");
    assert_eq!(guiprefs::path().unwrap(), tmp.path().join("home/.config/libvirt-backup/gui.json"));
}
