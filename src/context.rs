//! Contexte d'exécution : racine du système, réglages, rapporteur.
//!
//! Avec une racine différente de `/` (option cachée `--root`, utilisée par les tests), tous les chemins
//! du système sont relatifs à cette racine ; rien n'est modifié hors d'elle, sauf les appels à
//! `systemctl` / `virsh` qui passent par le PATH (remplaçables en test).

use crate::tr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::bail;
use crate::config::{check_backup_dir, default_user, load_config, validate, Config, LOCK_NAME};
use crate::report::Reporter;
use crate::sys::{user_by_name, Result};

#[derive(Clone)]
pub struct Context {
    pub config: Config,
    pub rep: Arc<dyn Reporter>,
    pub root: PathBuf,
    pub run_dir: PathBuf,
    pub proc_dir: PathBuf,
    pub sys_dir: PathBuf,
}

impl Context {
    /// Charge les réglages. `backup_dir` remplace, pour cette exécution seulement, le dossier configuré.
    pub fn create(rep: Arc<dyn Reporter>, root: Option<&Path>, validate_config: bool, backup_dir: Option<&Path>) -> Result<Context> {
        Self::create_with(rep, root, validate_config, backup_dir, false)
    }

    /// Comme [`Context::create`], mais un fichier de réglages illisible est remplacé par les valeurs par
    /// défaut (avec un avertissement) : la restauration doit pouvoir réparer précisément ce fichier.
    pub fn create_lenient(
        rep: Arc<dyn Reporter>,
        root: Option<&Path>,
        validate_config: bool,
        backup_dir: Option<&Path>,
    ) -> Result<Context> {
        Self::create_with(rep, root, validate_config, backup_dir, true)
    }

    fn create_with(
        rep: Arc<dyn Reporter>,
        root: Option<&Path>,
        validate_config: bool,
        backup_dir: Option<&Path>,
        lenient: bool,
    ) -> Result<Context> {
        let base = root.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
        let mut config = match load_config(&base) {
            Ok(c) => c,
            Err(e) if lenient => {
                rep.warn(&tr!("{}; default settings are used for this operation.", e));
                Config { backup_user: default_user(), ..Config::default() }
            }
            Err(e) => return Err(e),
        };
        if let Some(dir) = backup_dir {
            let normalized = crate::config::normalize_dir(&dir.to_string_lossy())?;
            check_backup_dir(&normalized, base == Path::new("/"))?;
            config.backup_dir = normalized.to_string_lossy().into_owned();
        }
        if validate_config {
            validate(&config)?;
        }
        Ok(if base == Path::new("/") {
            Context {
                config,
                rep,
                root: base,
                run_dir: PathBuf::from("/run"),
                proc_dir: PathBuf::from("/proc"),
                sys_dir: PathBuf::from("/sys"),
            }
        } else {
            Context { config, rep, run_dir: base.join("run"), proc_dir: base.join("proc"), sys_dir: base.join("sys"), root: base }
        })
    }

    pub fn is_system_root(&self) -> bool {
        self.root == Path::new("/")
    }

    pub fn backup_dir(&self) -> PathBuf {
        self.config.resolved_dir()
    }

    pub fn lock_path(&self) -> PathBuf {
        self.run_dir.join("lock").join(LOCK_NAME)
    }

    pub fn owner_ids(&self) -> Result<(u32, u32)> {
        match user_by_name(&self.config.backup_user) {
            Some(u) => Ok((u.uid, u.gid)),
            None => bail!("The account {} does not exist.", self.config.backup_user),
        }
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}
