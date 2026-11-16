//! Préférences propres à l'interface graphique, enregistrées pour chaque utilisateur
//! (`$XDG_CONFIG_HOME/libvirt-backup/gui.json`, par défaut `~/.config/libvirt-backup/gui.json`).
//!
//! Elles sont distinctes du réglage `language` de `/etc/libvirt-backup/config.json`, qui concerne le
//! service et la ligne de commande exécutés en root : la langue de la fenêtre est un choix de la personne
//! qui l'utilise, pas du système.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::i18n;

const KEY_LANGUAGE: &str = "language";

/// Chemin du fichier de préférences, si le dossier personnel est connu.
pub fn path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() && PathBuf::from(&dir).is_absolute() => PathBuf::from(dir),
        _ => {
            let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
            PathBuf::from(home).join(".config")
        }
    };
    Some(base.join("libvirt-backup").join("gui.json"))
}

fn read_object() -> Map<String, Value> {
    path()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// Langue choisie dans les préférences (`system`, `en`, `fr`, `de` ou `es`), si elle est valide.
pub fn load_language() -> Option<String> {
    let object = read_object();
    let value = object.get(KEY_LANGUAGE)?.as_str()?.trim().to_lowercase();
    i18n::is_valid_preference(&value).then_some(value)
}

/// Enregistre la langue choisie (les autres préférences éventuelles du fichier sont conservées).
pub fn save_language(preference: &str) -> io::Result<()> {
    if !i18n::is_valid_preference(preference) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid language: {preference}")));
    }
    let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    let mut object = read_object();
    object.insert(KEY_LANGUAGE.to_string(), json!(preference));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(object))? + "\n")?;
    fs::rename(&tmp, &path)
}
