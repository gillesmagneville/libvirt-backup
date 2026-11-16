//! Installation de l'outil lui-même (programmes, lanceurs, service, icône, règle polkit).
//!
//! Deux dispositions :
//! - `Layout::Local` (« install » depuis un binaire compilé) : l'outil écrit ses programmes dans
//!   /usr/local/lib/libvirt-backup, ses lanceurs dans /usr/local/bin, etc. Tous ces fichiers vivent dans
//!   des chemins couverts par les sauvegardes (groupes `tool`, `scripts` et `systemd`) : l'outil se
//!   sauvegarde donc lui-même, et une restauration le réinstalle. Le contenu est lu en mémoire AVANT
//!   toute extraction, car une restauration peut remplacer le programme pendant qu'il s'exécute.
//! - `Layout::Package` (paquet .deb) : les programmes appartiennent à dpkg (/usr/bin) et l'outil n'y
//!   touche jamais ; « install » ne crée que les réglages et le service. Les archives gardent pourtant
//!   la disposition locale (copie des programmes ajoutée à chaque sauvegarde), si bien qu'une machine
//!   vierge se restaure avec la même recette dans les deux cas.

use crate::tr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::bail;
use crate::config::{
    CLI_BIN_REL, CLI_NAME, CONFIG_DIR, DESKTOP_REL, GUI_BIN_REL, GUI_LAUNCHER_REL, GUI_NAME, ICON_REL, ICON_SYMBOLIC_REL, INSTALL_DIR,
    LAUNCHER_REL, PKG_CLI_REL, PKG_GUI_REL, POLICY_REL, UNIT_REL,
};
use crate::sys::{atomic_write, is_root, Context as _, Result};

const LAUNCHER: &str =
    "#!/bin/sh\n# Lanceur généré par libvirt-backup ; réécrit à chaque installation ou restauration.\nexec /{target} \"$@\"\n";

const UNIT_TEXT: &str = "\
[Unit]
Description=Surveillance et sauvegarde automatique des configurations de virtualisation libvirt
After=local-fs.target libvirtd.service

[Service]
Type=notify
NotifyAccess=main
TimeoutStartSec=300
User=root
UMask=0077
ExecStart=/{command} watch
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
";

static DESKTOP: &[u8] = include_bytes!("../data/app.desktop");
static ICON: &[u8] = include_bytes!("../data/app.svg");
static ICON_SYMBOLIC: &[u8] = include_bytes!("../data/app-symbolic.svg");
static POLICY: &[u8] = include_bytes!("../data/app.policy");

/// Disposition de l'outil sur le système.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Installé par « libvirt-backup install » : programmes et lanceurs sous /usr/local.
    Local,
    /// Installé par un paquet .deb : programmes sous /usr/bin, propriété de dpkg.
    Package,
}

impl Layout {
    /// Disposition du programme en cours d'exécution : `Package` s'il s'exécute depuis /usr/bin.
    ///
    /// La variable `LIBVIRT_BACKUP_LAYOUT` (`package` ou `local`) sert aux tests.
    pub fn detect() -> Layout {
        match std::env::var("LIBVIRT_BACKUP_LAYOUT").as_deref() {
            Ok("package") => return Layout::Package,
            Ok("local") => return Layout::Local,
            _ => {}
        }
        let me = fs::read_link("/proc/self/exe").unwrap_or_default();
        let me = me.to_string_lossy();
        let me = me.trim_end_matches(" (deleted)");
        let root = Path::new("/");
        if Path::new(me) == root.join(PKG_CLI_REL) || Path::new(me) == root.join(PKG_GUI_REL) {
            Layout::Package
        } else {
            Layout::Local
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Layout::Local => "local",
            Layout::Package => "package",
        }
    }

    /// Commande invoquée par le service (relative à la racine) : lanceur en local, programme en paquet.
    pub fn command_rel(self) -> &'static str {
        match self {
            Layout::Local => LAUNCHER_REL,
            Layout::Package => PKG_CLI_REL,
        }
    }

    pub fn programs_owned_by_dpkg(self) -> bool {
        self == Layout::Package
    }
}

/// Vrai pour les fichiers de l'outil que dpkg possède en disposition `Package` (programmes, lanceurs
/// locaux, entrée de menu, icône, règle polkit) : ils ne sont ni écrits ni restaurés à partir d'une archive.
pub fn is_program_path(rel: &str) -> bool {
    let rel = rel.trim_start_matches("./").trim_end_matches('/');
    rel == INSTALL_DIR
        || rel.starts_with(&format!("{INSTALL_DIR}/"))
        || [LAUNCHER_REL, GUI_LAUNCHER_REL, DESKTOP_REL, ICON_REL, ICON_SYMBOLIC_REL, POLICY_REL].contains(&rel)
}

/// Fichiers de l'outil : (chemin relatif à la racine, contenu, mode).
pub struct Payload {
    pub files: Vec<(String, Vec<u8>, u32)>,
    /// Faux si le binaire de l'interface graphique n'a pas été trouvé.
    pub has_gui: bool,
}

/// Emplacement des binaires de l'outil en cours d'exécution.
pub struct Binaries {
    pub cli: PathBuf,
    pub gui: Option<PathBuf>,
}

/// Trouve le programme en ligne de commande et, à côté, l'interface graphique.
///
/// La variable `LIBVIRT_BACKUP_BIN` (chemin du programme en ligne de commande) sert aux tests.
pub fn own_binaries() -> Result<Binaries> {
    let cli = if let Some(forced) = std::env::var_os("LIBVIRT_BACKUP_BIN") {
        PathBuf::from(forced)
    } else {
        let me = fs::read_link("/proc/self/exe").or_else(|_| std::env::current_exe())?;
        let name = me.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = name.trim_end_matches(" (deleted)").to_string();
        if name == CLI_NAME {
            PathBuf::from("/proc/self/exe") // lisible même si le fichier a été remplacé entre-temps
        } else {
            me.parent().map(|d| d.join(CLI_NAME)).unwrap_or_default()
        }
    };
    if !cli.exists() {
        bail!("the program {1} cannot be found ({0}).", cli.display(), CLI_NAME);
    }
    let dir = fs::canonicalize(if cli.starts_with("/proc") { Path::new("/proc/self/exe") } else { &cli })
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let gui = dir.map(|d| d.join(GUI_NAME)).filter(|g| g.is_file());
    Ok(Binaries { cli, gui })
}

pub fn unit_text(layout: Layout) -> String {
    UNIT_TEXT.replace("{command}", layout.command_rel())
}

pub fn build_payload(bins: &Binaries) -> Result<Payload> {
    let mut files = vec![
        (CLI_BIN_REL.to_string(), fs::read(&bins.cli).ctx(tr!("reading {}", bins.cli.display()))?, 0o755),
        (LAUNCHER_REL.to_string(), LAUNCHER.replace("{target}", CLI_BIN_REL).into_bytes(), 0o755),
        (DESKTOP_REL.to_string(), DESKTOP.to_vec(), 0o644),
        (ICON_REL.to_string(), ICON.to_vec(), 0o644),
        (ICON_SYMBOLIC_REL.to_string(), ICON_SYMBOLIC.to_vec(), 0o644),
        (POLICY_REL.to_string(), POLICY.to_vec(), 0o644),
    ];
    let mut has_gui = false;
    if let Some(gui) = &bins.gui {
        files.push((GUI_BIN_REL.to_string(), fs::read(gui).ctx(tr!("reading {}", gui.display()))?, 0o755));
        files.push((GUI_LAUNCHER_REL.to_string(), LAUNCHER.replace("{target}", GUI_BIN_REL).into_bytes(), 0o755));
        has_gui = true;
    }
    Ok(Payload { files, has_gui })
}

/// Crée les dossiers manquants en 0755 (le umask 077 du service ne doit pas les fermer).
fn ensure_dir(path: &Path, root: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    while current != root && !current.exists() {
        missing.push(current.clone());
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    for dir in missing.iter().rev() {
        fs::create_dir(dir).ctx(tr!("creating {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
        if is_root() {
            let _ = std::os::unix::fs::chown(dir, Some(0), Some(0));
        }
    }
    Ok(())
}

pub fn write_payload(root: &Path, payload: &Payload) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (rel, data, mode) in &payload.files {
        let target = root.join(rel);
        ensure_dir(target.parent().unwrap_or(root), root)?;
        atomic_write(&target, data, *mode, Some((0, 0)))?;
        written.push(rel.clone());
    }
    Ok(written)
}

pub fn write_unit(root: &Path, layout: Layout) -> Result<PathBuf> {
    let target = root.join(UNIT_REL);
    ensure_dir(target.parent().unwrap_or(root), root)?;
    atomic_write(&target, unit_text(layout).as_bytes(), 0o644, Some((0, 0)))?;
    Ok(target)
}

/// Faux si l'unité présente est celle des anciens scripts shell (ou n'est pas celle de cet outil).
pub fn unit_uses_this_tool(root: &Path) -> bool {
    let Ok(text) = fs::read_to_string(root.join(UNIT_REL)) else { return false };
    text.lines().any(|l| [Layout::Local, Layout::Package].iter().any(|ly| l.trim() == format!("ExecStart=/{} watch", ly.command_rel())))
}

/// Remet l'unité de cet outil en accord avec la disposition courante.
///
/// Une archive faite sur une machine installée par paquet contient une unité qui appelle /usr/bin, et
/// inversement : après une restauration, l'unité doit appeler les programmes réellement présents.
/// Renvoie vrai si l'unité a été réécrite.
pub fn align_unit(root: &Path, layout: Layout) -> Result<bool> {
    if !unit_uses_this_tool(root) {
        return Ok(false);
    }
    let current = fs::read_to_string(root.join(UNIT_REL)).unwrap_or_default();
    if current == unit_text(layout) {
        return Ok(false);
    }
    write_unit(root, layout)?;
    Ok(true)
}

pub fn is_installed(root: &Path, layout: Layout) -> bool {
    match layout {
        Layout::Local => root.join(CLI_BIN_REL).is_file() && root.join(LAUNCHER_REL).is_file(),
        Layout::Package => root.join(PKG_CLI_REL).is_file(),
    }
}

/// Vrai si le paquet .deb est installé (un programme existe sous /usr/bin).
pub fn package_present(root: &Path) -> bool {
    root.join(PKG_CLI_REL).exists()
}

/// Vrai si une installation locale (/usr/local) de cet outil est présente.
///
/// Installé par paquet, une telle copie passe avant /usr/bin dans le PATH et masque le paquet.
pub fn local_install_present(root: &Path) -> bool {
    root.join(LAUNCHER_REL).exists() || root.join(INSTALL_DIR).exists()
}

/// Copie des programmes en disposition locale, écrite dans `stage_root`, pour les ajouter à une archive.
///
/// Renvoie les entrées de premier niveau à archiver (relatives à `stage_root`).
pub fn stage_tool_payload(stage_root: &Path, bins: &Binaries) -> Result<Vec<String>> {
    let payload = build_payload(bins)?;
    fs::create_dir_all(stage_root).ctx(tr!("creating {}", stage_root.display()))?;
    write_payload(stage_root, &payload)?;
    let mut entries: Vec<String> = Vec::new();
    for (rel, _, _) in &payload.files {
        let top = if rel.starts_with(&format!("{INSTALL_DIR}/")) { INSTALL_DIR } else { rel.as_str() };
        if !entries.iter().any(|e| e == top) {
            entries.push(top.to_string());
        }
    }
    Ok(entries)
}

/// Retire une ancienne installation locale (/usr/local) quand le paquet prend le relais.
///
/// La règle polkit n'est pas touchée : le paquet a pris la place de ce fichier.
pub fn remove_local_install(root: &Path) -> Vec<String> {
    let mut removed = Vec::new();
    for rel in [LAUNCHER_REL, GUI_LAUNCHER_REL, DESKTOP_REL, ICON_REL, ICON_SYMBOLIC_REL] {
        let path = root.join(rel);
        if fs::symlink_metadata(&path).map(|m| m.is_file() || m.file_type().is_symlink()).unwrap_or(false) && fs::remove_file(&path).is_ok()
        {
            removed.push(rel.to_string());
        }
    }
    let dir = root.join(INSTALL_DIR);
    if fs::symlink_metadata(&dir).map(|m| m.is_dir()).unwrap_or(false) && fs::remove_dir_all(&dir).is_ok() {
        removed.push(INSTALL_DIR.to_string());
    }
    removed
}

/// Supprime l'outil installé (jamais les archives). Renvoie les chemins retirés.
///
/// En disposition `Package`, les programmes appartiennent à dpkg (« apt remove ») : seuls le service et,
/// avec `purge_config`, les réglages sont retirés.
pub fn remove_files(root: &Path, purge_config: bool, layout: Layout) -> Vec<String> {
    let mut removed = Vec::new();
    let files: &[&str] = if layout.programs_owned_by_dpkg() {
        &[UNIT_REL]
    } else {
        &[LAUNCHER_REL, GUI_LAUNCHER_REL, DESKTOP_REL, ICON_REL, ICON_SYMBOLIC_REL, POLICY_REL, UNIT_REL]
    };
    for rel in files.iter().copied() {
        let path = root.join(rel);
        if fs::symlink_metadata(&path).map(|m| m.is_file() || m.file_type().is_symlink()).unwrap_or(false) && fs::remove_file(&path).is_ok()
        {
            removed.push(rel.to_string());
        }
    }
    // Chemins constants et dédiés à l'outil : suppression récursive sans risque pour le reste du système.
    let mut dirs: Vec<&str> = if layout.programs_owned_by_dpkg() { Vec::new() } else { vec![INSTALL_DIR] };
    if purge_config {
        dirs.push(CONFIG_DIR);
    }
    for rel in dirs {
        let dir = root.join(rel);
        let is_real_dir = fs::symlink_metadata(&dir).map(|m| m.is_dir()).unwrap_or(false);
        if is_real_dir && fs::remove_dir_all(&dir).is_ok() {
            removed.push(rel.to_string());
        }
    }
    removed
}
