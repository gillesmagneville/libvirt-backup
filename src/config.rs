//! Constantes, groupes de chemins sauvegardés et réglages de libvirt-backup.
//!
//! Les noms (archives, service, verrou, étiquettes syslog) sont volontairement identiques à ceux des
//! anciens scripts shell `install-libvirt-autobackup.sh` et `restore-libvirt-backup.sh` : les archives
//! existantes restent restaurables, le service remplace l'ancien sans doublon et le verrou est partagé.

use crate::tr_noop;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::bail;
use crate::sys::{self, atomic_write, current_uid, user_by_name, user_by_uid, AppError, Result};
use crate::tr;

pub const APP_NAME: &str = "libvirt-backup";
/// Nom affiché dans « À propos » (mêmes mentions que Planche-Contact).
pub const AUTHOR_NAME: &str = "Gilles MAGNEVILLE";
/// Dépôt du projet.
pub const REPO_SLUG: &str = "gillesmagneville/libvirt-backup";
pub const REPO_URL: &str = "https://github.com/gillesmagneville/libvirt-backup";
/// Date de compilation (AAAA-MM-JJ, UTC), fixée par `build.rs`.
pub const BUILD_DATE: &str = env!("LB_BUILD_DATE");
pub const APP_ID: &str = "io.github.gillesmagneville.LibvirtBackup";
pub const POLKIT_ACTION: &str = "io.github.gillesmagneville.libvirt-backup.run";

pub const SERVICE_NAME: &str = "libvirt-config-backup-watch.service";
pub const LOCK_NAME: &str = "libvirt-config-backup.lock";

pub const ARCHIVE_PREFIX: &str = "libvirt-config-";
pub const SAFETY_PREFIX: &str = "pre-restore-safety-";
pub const LATEST_LINK: &str = "libvirt-config-latest.tar.gz";
/// Mots canoniques (français) de confirmation : toujours acceptés, quelle que soit la langue, pour que
/// les scripts écrits pour l'ancienne version continuent de fonctionner.
pub const CONFIRM_WORD_CANONICAL: &str = "RESTAURER";
pub const DELETE_WORD_CANONICAL: &str = "SUPPRIMER";

/// Mot à saisir pour confirmer une restauration, dans la langue active.
pub fn confirm_word() -> &'static str {
    tr!("RESTORE")
}

/// Mot à saisir pour confirmer une suppression, dans la langue active.
pub fn delete_word() -> &'static str {
    tr!("DELETE")
}

/// Vrai si la réponse confirme la restauration (mot localisé ou mot canonique français).
pub fn is_confirm_word(answer: &str) -> bool {
    answer == confirm_word() || answer == CONFIRM_WORD_CANONICAL
}

/// Vrai si la réponse confirme la suppression (mot localisé ou mot canonique français).
pub fn is_delete_word(answer: &str) -> bool {
    answer == delete_word() || answer == DELETE_WORD_CANONICAL
}

// Emplacements de l'outil installé (relatifs à la racine du système).
pub const CLI_NAME: &str = "libvirt-backup";
pub const GUI_NAME: &str = "libvirt-backup-gui";
pub const INSTALL_DIR: &str = "usr/local/lib/libvirt-backup";
pub const CLI_BIN_REL: &str = "usr/local/lib/libvirt-backup/libvirt-backup";
pub const GUI_BIN_REL: &str = "usr/local/lib/libvirt-backup/libvirt-backup-gui";
pub const CONFIG_DIR: &str = "etc/libvirt-backup";
pub const CONFIG_REL: &str = "etc/libvirt-backup/config.json";
pub const LAUNCHER_REL: &str = "usr/local/bin/libvirt-backup";
pub const GUI_LAUNCHER_REL: &str = "usr/local/bin/libvirt-backup-gui";
pub const DESKTOP_REL: &str = "usr/local/share/applications/io.github.gillesmagneville.LibvirtBackup.desktop";
pub const ICON_REL: &str = "usr/local/share/icons/hicolor/scalable/apps/io.github.gillesmagneville.LibvirtBackup.svg";
pub const ICON_SYMBOLIC_REL: &str = "usr/local/share/icons/hicolor/symbolic/apps/io.github.gillesmagneville.LibvirtBackup-symbolic.svg";
pub const POLICY_REL: &str = "usr/share/polkit-1/actions/io.github.gillesmagneville.libvirt-backup.run.policy";
pub const UNIT_REL: &str = "etc/systemd/system/libvirt-config-backup-watch.service";
/// Installation par paquet .deb : les programmes appartiennent à dpkg et vivent dans /usr/bin.
pub const PKG_CLI_REL: &str = "usr/bin/libvirt-backup";
pub const PKG_GUI_REL: &str = "usr/bin/libvirt-backup-gui";

/// Profils AppArmor rechargés après une restauration (comme l'ancien script).
pub const APPARMOR_PROFILES: &[&str] = &["usr.sbin.libvirtd"];
pub const LIBVIRT_UNITS: &[&str] = &["libvirtd.service", "virtqemud.service"];

/// Ensemble cohérent de chemins, restaurable séparément.
#[derive(Debug)]
pub struct Group {
    pub id: &'static str,
    /// Libellé anglais (clé de traduction) : utiliser [`Group::label`].
    pub label_key: &'static str,
    pub paths: &'static [&'static str],
    /// Avertissement anglais (clé de traduction, vide si aucun) : utiliser [`Group::caution`].
    pub caution_key: &'static str,
}

impl Group {
    /// Libellé du groupe dans la langue active.
    pub fn label(&self) -> &'static str {
        crate::i18n::tr(self.label_key)
    }

    /// Avertissement du groupe dans la langue active (vide s'il n'y en a pas).
    pub fn caution(&self) -> &'static str {
        crate::i18n::tr(self.caution_key)
    }
}

pub static GROUPS: &[Group] = &[
    Group {
        id: "libvirt",
        label_key: tr_noop!("libvirt configuration (VMs, networks, pools, secrets)"),
        paths: &["etc/libvirt", "var/lib/libvirt/secrets"],
        caution_key: "",
    },
    Group {
        id: "nvram",
        label_key: tr_noop!("UEFI/NVRAM and TPM states of the VMs"),
        paths: &["var/lib/libvirt/qemu/nvram", "var/lib/libvirt/swtpm", "var/lib/swtpm"],
        caution_key: "",
    },
    Group { id: "apparmor", label_key: tr_noop!("AppArmor profiles and rules"), paths: &["etc/apparmor.d"], caution_key: "" },
    Group {
        id: "systemd",
        label_key: tr_noop!("systemd units, sysctl, polkit, limits"),
        paths: &["etc/systemd/system", "etc/tmpfiles.d", "etc/sysctl.d", "etc/security/limits.d", "etc/polkit-1/rules.d"],
        caution_key: "",
    },
    Group {
        id: "scripts",
        label_key: tr_noop!("Local scripts (/usr/local/sbin, /usr/local/bin)"),
        paths: &["usr/local/sbin", "usr/local/bin"],
        caution_key: "",
    },
    Group {
        id: "boot",
        label_key: tr_noop!("Kernel, VFIO, GRUB, initramfs, udev, NUMA"),
        paths: &[
            "etc/modprobe.d",
            "etc/modules-load.d",
            "etc/modules",
            "etc/udev/rules.d",
            "etc/default",
            "etc/grub.d",
            "etc/initramfs-tools",
            "etc/kernel",
            "etc/numad.conf",
        ],
        caution_key: "",
    },
    Group {
        id: "network",
        label_key: tr_noop!("Host network (netplan, NetworkManager)"),
        paths: &["etc/netplan", "etc/network", "etc/NetworkManager/system-connections"],
        caution_key: tr_noop!("untick if the host or its network cards have changed"),
    },
    Group {
        id: "storage",
        label_key: tr_noop!("Storage (fstab, ZFS)"),
        paths: &["etc/zfs", "etc/fstab"],
        caution_key: tr_noop!("untick if the disks or pools have changed"),
    },
    Group {
        id: "tool",
        label_key: tr_noop!("libvirt-backup tool (program, settings)"),
        paths: &[INSTALL_DIR, CONFIG_DIR, DESKTOP_REL, ICON_REL, ICON_SYMBOLIC_REL, POLICY_REL],
        caution_key: "",
    },
];

pub fn group_ids() -> Vec<&'static str> {
    GROUPS.iter().map(|g| g.id).collect()
}

pub fn group_by_id(id: &str) -> Option<&'static Group> {
    GROUPS.iter().find(|g| g.id == id)
}

/// Répertoires surveillés par le service (relatifs à la racine du système).
pub static WATCH_DIRS: &[&str] = &[
    "etc/libvirt",
    "etc/apparmor.d",
    "etc/systemd/system",
    "usr/local/sbin",
    "usr/local/bin",
    "usr/local/lib/libvirt-backup",
    "etc/libvirt-backup",
    "etc/modprobe.d",
    "etc/modules-load.d",
    "etc/udev/rules.d",
    "etc/sysctl.d",
    "etc/tmpfiles.d",
    "etc/security/limits.d",
    "etc/polkit-1/rules.d",
    "etc/default",
    "etc/grub.d",
    "etc/initramfs-tools",
    "etc/netplan",
    "etc/network",
    "etc/NetworkManager/system-connections",
    "etc/zfs",
];

// Entrées d'archive non système mais attendues (format historique conservé : le manifeste vit dans
// host-inventory/ pour rester lisible par l'ancien script).
pub const META_FILES: &[&str] = &["README.txt"];
pub const META_DIRS: &[&str] = &["virsh-exports", "host-inventory"];
pub const MANIFEST_MEMBER: &str = "host-inventory/manifest.json";

/// Identifiant du groupe qui contient ce chemin relatif, sinon `None`.
pub fn group_of(path: &str) -> Option<&'static str> {
    for group in GROUPS {
        for root in group.paths {
            if path == *root || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/')) {
                return Some(group.id);
            }
        }
    }
    None
}

// --------------------------------------------------------------------------
// Réglages persistants
// --------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub backup_user: String,
    /// Vide = `~/virtualisation` de l'utilisateur propriétaire.
    pub backup_dir: String,
    pub retention: u32,
    /// 0 = tout conserver (comportement de l'ancien script).
    pub safety_retention: u32,
    pub debounce: f64,
    /// Sauvegarde allégée : ignore les binaires compilés des dossiers de scripts et les fichiers très
    /// volumineux (voir `selection`). Désactivée, tout le contenu des chemins est archivé, comme
    /// l'ancien script.
    pub lean_backup: bool,
    /// Langue des messages : `system` (défaut, suit le système) ou `en`, `fr`, `de`, `es`.
    pub language: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            backup_user: String::new(),
            backup_dir: String::new(),
            retention: 30,
            safety_retention: 0,
            debounce: 2.0,
            lean_backup: true,
            language: "system".to_string(),
        }
    }
}

pub const CONFIG_KEYS: &[&str] = &["backup_user", "backup_dir", "retention", "safety_retention", "debounce", "lean_backup", "language"];

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "oui" | "on" | "vrai" => Some(true),
        "0" | "false" | "no" | "non" | "off" | "faux" => Some(false),
        _ => None,
    }
}

impl Config {
    pub fn resolved_dir(&self) -> PathBuf {
        if !self.backup_dir.is_empty() {
            return PathBuf::from(&self.backup_dir);
        }
        let home = user_by_name(&self.backup_user).map(|u| u.home).unwrap_or_else(|| PathBuf::from(format!("/home/{}", self.backup_user)));
        home.join("virtualisation")
    }

    pub fn get(&self, key: &str) -> String {
        match key {
            "backup_user" => self.backup_user.clone(),
            "backup_dir" => self.backup_dir.clone(),
            "retention" => self.retention.to_string(),
            "safety_retention" => self.safety_retention.to_string(),
            "debounce" => format!("{:?}", self.debounce),
            "lean_backup" => self.lean_backup.to_string(),
            "language" => self.language.clone(),
            _ => String::new(),
        }
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let value = value.trim();
        let invalid = || AppError(tr!("Invalid value for {1}: {0}", format!("{value:?}"), key));
        match key {
            "backup_user" => self.backup_user = value.to_string(),
            "backup_dir" => self.backup_dir = value.to_string(),
            "retention" => self.retention = value.parse().map_err(|_| invalid())?,
            "safety_retention" => self.safety_retention = value.parse().map_err(|_| invalid())?,
            "debounce" => self.debounce = value.replace(',', ".").parse().map_err(|_| invalid())?,
            "lean_backup" => self.lean_backup = parse_bool(value).ok_or_else(invalid)?,
            "language" => {
                let value = value.to_lowercase();
                if !crate::i18n::is_valid_preference(&value) {
                    return Err(invalid());
                }
                self.language = value;
            }
            _ => bail!("Unknown key: {1} (keys: {0})", CONFIG_KEYS.join(", "), key),
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "backup_user": self.backup_user,
            "backup_dir": self.backup_dir,
            "retention": self.retention,
            "safety_retention": self.safety_retention,
            "debounce": self.debounce,
            "lean_backup": self.lean_backup,
            "language": self.language,
        })
    }
}

/// Utilisateur propriétaire des archives : celui qui a lancé sudo/pkexec.
pub fn default_user() -> String {
    if let Ok(name) = std::env::var("SUDO_USER") {
        if !name.is_empty() && name != "root" {
            return name;
        }
    }
    if let Ok(uid) = std::env::var("PKEXEC_UID") {
        if let Ok(uid) = uid.parse::<u32>() {
            if uid != 0 {
                if let Some(user) = user_by_uid(uid) {
                    return user.name;
                }
            }
        }
    }
    if !sys::is_root() {
        if let Some(user) = user_by_uid(current_uid()) {
            return user.name;
        }
    }
    user_by_uid(1000).map(|u| u.name).unwrap_or_default()
}

/// Contrôle les valeurs ; erreur si l'une d'elles est inutilisable.
pub fn validate(cfg: &Config) -> Result<()> {
    if cfg.backup_user.is_empty() {
        bail!("No user owns the backups: use --user.");
    }
    if !cfg.backup_dir.is_empty() {
        let p = Path::new(&cfg.backup_dir);
        if !p.is_absolute() || cfg.backup_dir.contains('\0') {
            bail!("The backup folder must be an absolute path: {}", format!("{:?}", cfg.backup_dir));
        }
    }
    if !(1..=10000).contains(&cfg.retention) {
        bail!("The retention must be between 1 and 10000.");
    }
    if cfg.safety_retention > 10000 {
        bail!("The retention of safety archives must be between 0 (keep all) and 10000.");
    }
    if !(0.2..=300.0).contains(&cfg.debounce) {
        bail!("The debounce delay must be between 0.2 and 300 seconds.");
    }
    Ok(())
}

/// Normalise un chemin absolu sans toucher au disque (retire `.` et les `/` finaux, refuse `..`).
pub fn normalize_dir(raw: &str) -> Result<PathBuf> {
    let p = Path::new(raw.trim());
    if raw.trim().is_empty() || !p.is_absolute() || raw.contains('\0') {
        bail!("The backup folder must be an absolute path: {}", format!("{:?}", raw));
    }
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(n) => out.push(n),
            Component::ParentDir => bail!("The backup folder must not contain \"..\": {}", raw),
            Component::Prefix(_) => bail!("Invalid path: {}", raw),
        }
    }
    Ok(out)
}

/// Vérifie qu'un emplacement de sauvegarde est raisonnable.
///
/// Sur le vrai système (`system_root`), il ne doit ni se trouver dans un chemin sauvegardé ou surveillé
/// (la sauvegarde se déclencherait elle-même en boucle), ni en contenir un, ni être volatil.
pub fn check_backup_dir(dir: &Path, system_root: bool) -> Result<()> {
    let dir = normalize_dir(&dir.to_string_lossy())?;
    if dir.exists() && !dir.is_dir() {
        bail!("{} exists but is not a folder.", dir.display());
    }
    if !system_root {
        return Ok(());
    }
    if dir == Path::new("/") {
        bail!("The backup folder cannot be the root of the system.");
    }
    for volatile in ["/proc", "/sys", "/dev", "/run"] {
        if dir.starts_with(volatile) {
            bail!("{} is a volatile location (emptied at every boot): choose another folder.", dir.display());
        }
    }
    let mut protected: Vec<&str> = WATCH_DIRS.to_vec();
    for g in GROUPS {
        protected.extend_from_slice(g.paths);
    }
    for rel in protected {
        let abs = Path::new("/").join(rel);
        if dir.starts_with(&abs) {
            bail!(
                "{0} is inside a backed-up or watched location ({1}): each archive would trigger a new backup. Choose another folder.",
                dir.display(),
                abs.display()
            );
        }
        if abs.starts_with(&dir) {
            bail!("{0} contains backed-up paths ({1}): choose a more specific folder.", dir.display(), abs.display());
        }
    }
    Ok(())
}

/// Lit la configuration ; les valeurs absentes prennent leur défaut.
pub fn load_config(root: &Path) -> Result<Config> {
    let mut cfg = Config { backup_user: default_user(), ..Config::default() };
    let path = root.join(CONFIG_REL);
    if path.is_file() {
        let text = std::fs::read_to_string(&path).map_err(|e| AppError(tr!("Configuration unreadable ({0}): {1}", path.display(), e)))?;
        let data: Value =
            serde_json::from_str(&text).map_err(|e| AppError(tr!("Configuration unreadable ({0}): {1}", path.display(), e)))?;
        let Value::Object(map) = data else {
            bail!("Configuration invalid ({}): a JSON object is expected.", path.display());
        };
        let bad = |key: &str, v: &Value| AppError(tr!("Configuration invalid ({0}): {1}={2}", path.display(), key, v));
        for (key, value) in &map {
            match key.as_str() {
                "backup_user" => cfg.backup_user = value.as_str().ok_or_else(|| bad(key, value))?.to_string(),
                "backup_dir" => cfg.backup_dir = value.as_str().ok_or_else(|| bad(key, value))?.to_string(),
                "retention" => cfg.retention = value.as_u64().ok_or_else(|| bad(key, value))? as u32,
                "safety_retention" => cfg.safety_retention = value.as_u64().ok_or_else(|| bad(key, value))? as u32,
                "debounce" => cfg.debounce = value.as_f64().ok_or_else(|| bad(key, value))?,
                "lean_backup" => cfg.lean_backup = value.as_bool().ok_or_else(|| bad(key, value))?,
                "language" => {
                    let lang = value.as_str().ok_or_else(|| bad(key, value))?;
                    if crate::i18n::is_valid_preference(lang) {
                        cfg.language = lang.to_string();
                    }
                }
                _ => {}
            }
        }
    }
    Ok(cfg)
}

pub fn save_config(root: &Path, cfg: &Config) -> Result<PathBuf> {
    validate(cfg)?;
    let path = root.join(CONFIG_REL);
    let text = serde_json::to_string_pretty(&cfg.to_json()).map_err(|e| AppError(e.to_string()))? + "\n";
    atomic_write(&path, text.as_bytes(), 0o644, Some((0, 0)))?;
    Ok(path)
}
