//! Appels à `virsh` (connexion qemu:///system) avec journal d'erreurs.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::sys::{run, which};

pub const URI: &str = "qemu:///system";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DefineKind {
    Domain,
    Network,
    Pool,
}

pub struct Virsh {
    exe: Option<PathBuf>,
    error_log: Option<PathBuf>,
}

impl Virsh {
    pub fn new(error_log: Option<&Path>) -> Self {
        Virsh { exe: which("virsh"), error_log: error_log.map(Path::to_path_buf) }
    }

    pub fn available(&self) -> bool {
        self.exe.is_some()
    }

    pub fn log(&self, text: &str) {
        let Some(path) = &self.error_log else { return };
        if text.trim().is_empty() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            // Un seul write : les appels simultanés (export en parallèle) n'entrelacent pas leurs lignes.
            let mut line = text.to_string();
            if !line.ends_with('\n') {
                line.push('\n');
            }
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// (code de retour, sortie standard) ; la sortie d'erreur est ajoutée au journal.
    pub fn call(&self, args: &[&str]) -> (i32, String) {
        let Some(exe) = &self.exe else { return (127, String::new()) };
        let mut argv: Vec<&str> = vec![exe.to_str().unwrap_or("virsh"), "-c", URI];
        argv.extend_from_slice(args);
        let out = run(&argv, Some(Duration::from_secs(60)));
        self.log(&out.stderr);
        (out.code, out.stdout)
    }

    /// Noms listés par une sous-commande (`list --all --name`...), `None` si elle échoue.
    pub fn names(&self, args: &[&str]) -> Option<Vec<String>> {
        let (code, out) = self.call(args);
        (code == 0).then(|| out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
    }

    pub fn domstate(&self, domain: &str) -> String {
        let (code, out) = self.call(&["domstate", domain]);
        if code != 0 {
            return "unknown".into();
        }
        out.trim().lines().next().map(|l| l.trim().to_string()).unwrap_or_else(|| "unknown".into())
    }

    pub fn define(&self, kind: DefineKind, xml: &Path) -> bool {
        let command = match kind {
            DefineKind::Domain => "define",
            DefineKind::Network => "net-define",
            DefineKind::Pool => "pool-define",
        };
        self.call(&[command, &xml.to_string_lossy()]).0 == 0
    }

    pub fn reachable(&self, attempts: u32, delay: Duration) -> bool {
        if self.exe.is_none() {
            return false;
        }
        for attempt in 0..attempts.max(1) {
            if self.call(&["list", "--all"]).0 == 0 {
                return true;
            }
            if attempt + 1 < attempts {
                thread::sleep(delay);
            }
        }
        false
    }
}
