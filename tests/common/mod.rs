//! Fausse racine système + faux `virsh` / `systemctl` partagés par les tests d'intégration.
//!
//! Toutes les opérations passent par le programme réel (`libvirt-backup --root FAUSSE_RACINE`) lancé
//! avec un PATH qui place les faux outils en tête : les tests s'exécutent donc en parallèle sans
//! partager la moindre variable d'environnement.
#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use serde_json::Value;

pub const CLI: &str = env!("CARGO_BIN_EXE_libvirt-backup");

const VIRSH_STUB: &str = r#"#!/bin/bash
echo "virsh $*" >> "$STUB_LOG"
[ -f "$STUB_DIR/virsh_down" ] && { echo "error: failed to connect" >&2; exit 1; }
shift 2
case "$1" in
  list)
    if [ "$2" = "--name" ] || [ "$3" = "--name" ]; then cat "$STUB_DIR/domains" 2>/dev/null; else echo " Id  Name  State"; fi ;;
  domstate) grep "^$2=" "$STUB_DIR/states" | cut -d= -f2- ;;
  dumpxml) echo "<domain><name>${@: -1}</name></domain>" ;;
  net-list) echo "default" ;;
  net-dumpxml) echo "<network><name>$2</name></network>" ;;
  pool-list) echo "images" ;;
  pool-dumpxml) echo "<pool><name>$2</name></pool>" ;;
  define|net-define|pool-define)
    [ -f "$STUB_DIR/define_fails" ] && { echo "error: define failed" >&2; exit 1; }
    echo "defined $2" ;;
  *) exit 0 ;;
esac
"#;

const SYSTEMCTL_STUB: &str = r#"#!/bin/bash
echo "systemctl $*" >> "$STUB_LOG"
case "$1" in
  cat) grep -qx "$2" "$STUB_DIR/units"; exit $? ;;
  is-active)
    if [ "$2" = "--quiet" ]; then unit="$3"; quiet=1; else unit="$2"; quiet=0; fi
    if grep -qx "$unit" "$STUB_DIR/active"; then [ $quiet = 0 ] && echo active; exit 0; fi
    [ $quiet = 0 ] && echo inactive; exit 3 ;;
  is-enabled) echo enabled; exit 0 ;;
  stop) sed -i "/^$2\$/d" "$STUB_DIR/active"; exit 0 ;;
  start|restart) grep -qx "$2" "$STUB_DIR/active" || echo "$2" >> "$STUB_DIR/active"; exit 0 ;;
  *) exit 0 ;;
esac
"#;

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
    /// Événements JSON (un par ligne) de la sortie standard.
    pub fn events(&self) -> Vec<Value> {
        self.stdout.lines().filter(|l| l.starts_with('{')).filter_map(|l| serde_json::from_str(l).ok()).collect()
    }
}

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub stubs: PathBuf,
    pub backups: PathBuf,
    pub user: String,
}

pub fn write_file(path: &Path, text: &str, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

impl Env {
    pub fn new() -> Env {
        // Les tests vérifient les textes français : la langue est imposée, quelle que soit la locale.
        static LANGUAGE: std::sync::Once = std::sync::Once::new();
        LANGUAGE.call_once(|| std::env::set_var("LIBVIRT_BACKUP_LANG", "fr"));
        libvirt_backup::i18n::set_current(libvirt_backup::i18n::Lang::Fr);
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();
        let root = base.join("root");
        let stubs = base.join("stubs");
        let backups = base.join("backups");
        write_file(&stubs.join("virsh"), VIRSH_STUB, 0o755);
        write_file(&stubs.join("systemctl"), SYSTEMCTL_STUB, 0o755);
        write_file(&stubs.join("units"), "libvirtd.service\nlibvirt-config-backup-watch.service\n", 0o644);
        write_file(&stubs.join("active"), "libvirtd.service\nlibvirt-config-backup-watch.service\n", 0o644);
        write_file(&stubs.join("domains"), "vm1\nvm2\n", 0o644);
        write_file(&stubs.join("states"), "vm1=shut off\nvm2=shut off\n", 0o644);

        write_file(&root.join("etc/libvirt/qemu/vm1.xml"), "<domain><name>vm1</name></domain>\n", 0o644);
        write_file(&root.join("etc/libvirt/qemu/vm2.xml"), "<domain><name>vm2</name></domain>\n", 0o644);
        fs::create_dir_all(root.join("etc/libvirt/qemu/autostart")).unwrap();
        std::os::unix::fs::symlink("../vm1.xml", root.join("etc/libvirt/qemu/autostart/vm1.xml")).unwrap();
        write_file(&root.join("etc/apparmor.d/local/usr.sbin.libvirtd"), "# local\n", 0o644);
        write_file(&root.join("etc/apparmor.d/usr.sbin.libvirtd"), "profile libvirtd {}\n", 0o644);
        write_file(&root.join("usr/local/sbin/hook.sh"), "#!/bin/sh\necho hook\n", 0o755);
        write_file(&root.join("etc/default/grub"), "GRUB_CMDLINE_LINUX=\"intel_iommu=on\"\n", 0o644);
        write_file(&root.join("etc/modprobe.d/vfio.conf"), "options vfio-pci ids=10de:1234\n", 0o644);
        write_file(&root.join("etc/fstab"), "UUID=abc / ext4 defaults 0 1\n", 0o644);
        write_file(&root.join("var/lib/libvirt/qemu/nvram/vm1_VARS.fd"), "NVRAMDATA", 0o644);
        fs::create_dir_all(root.join("proc")).unwrap();

        let user = libvirt_backup::sys::user_by_uid(libvirt_backup::sys::current_uid()).map(|u| u.name).unwrap_or_else(|| "root".into());
        let env = Env { tmp, root, stubs, backups, user };
        env.set_config(&[("retention", "3"), ("safety_retention", "2"), ("debounce", "0.4")]);
        env
    }

    /// Écrit la configuration de la fausse racine (valeurs de test + surcharges).
    pub fn set_config(&self, overrides: &[(&str, &str)]) {
        let mut cfg = serde_json::json!({
            "backup_user": self.user,
            "backup_dir": self.backups.to_string_lossy(),
            "retention": 3,
            "safety_retention": 2,
            "debounce": 0.4,
        });
        for (key, value) in overrides {
            cfg[*key] = match *key {
                "lean_backup" => Value::from(value.parse::<bool>().unwrap()),
                "retention" | "safety_retention" => Value::from(value.parse::<u64>().unwrap()),
                "debounce" => Value::from(value.parse::<f64>().unwrap()),
                _ => Value::from(*value),
            };
        }
        write_file(&self.root.join("etc/libvirt-backup/config.json"), &cfg.to_string(), 0o644);
    }

    pub fn log(&self) -> PathBuf {
        self.tmp.path().join("calls.log")
    }

    pub fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.log()).map(|t| t.lines().map(String::from).collect()).unwrap_or_default()
    }

    pub fn path_env(&self) -> String {
        format!("{}:{}", self.stubs.display(), std::env::var("PATH").unwrap_or_default())
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(CLI);
        cmd.args(args)
            .arg("--root")
            .arg(&self.root)
            .env("PATH", self.path_env())
            .env("STUB_DIR", &self.stubs)
            .env("STUB_LOG", self.log())
            .env("LIBVIRT_BACKUP_LANG", "fr")
            .env_remove("NOTIFY_SOCKET")
            .env_remove("SUDO_USER")
            .env_remove("PKEXEC_UID");
        cmd
    }

    pub fn cli(&self, args: &[&str]) -> Run {
        self.cli_env(args, &[])
    }

    pub fn cli_env(&self, args: &[&str], vars: &[(&str, &str)]) -> Run {
        let mut cmd = self.command(args);
        for (k, v) in vars {
            cmd.env(k, v);
        }
        let out = cmd.stdin(Stdio::null()).output().expect("lancement du programme");
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// Même chose dans un pseudo-terminal (`script`), avec des réponses saisies au clavier.
    pub fn cli_tty(&self, args: &[&str], keyboard: &str) -> Option<Run> {
        if !Path::new("/usr/bin/script").exists() {
            return None;
        }
        let mut line = shell_quote(CLI);
        for a in args {
            line.push(' ');
            line.push_str(&shell_quote(a));
        }
        line.push_str(&format!(" --root {}", shell_quote(&self.root.to_string_lossy())));
        let mut child = self
            .command_base("script")
            .args(["-qefc", &line, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let _ = stdin.write_all(keyboard.as_bytes());
        let _ = stdin.flush();
        let mut out = String::new();
        let mut err = String::new();
        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let t1 = std::thread::spawn(move || {
            let _ = so.read_to_string(&mut out);
            out
        });
        let t2 = std::thread::spawn(move || {
            let _ = se.read_to_string(&mut err);
            err
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(s) = child.try_wait().ok()? {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("le programme ne s'est pas terminé dans le terminal");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        drop(stdin);
        Some(Run { code: status.code().unwrap_or(-1), stdout: t1.join().unwrap(), stderr: t2.join().unwrap() })
    }

    fn command_base(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("PATH", self.path_env())
            .env("STUB_DIR", &self.stubs)
            .env("STUB_LOG", self.log())
            .env("LIBVIRT_BACKUP_LANG", "fr")
            .env_remove("NOTIFY_SOCKET")
            .env_remove("SUDO_USER")
            .env_remove("PKEXEC_UID");
        cmd
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    pub fn write(&self, rel: &str, text: &str) {
        write_file(&self.root.join(rel), text, 0o644);
    }

    /// Crée une archive et renvoie son chemin.
    pub fn backup(&self) -> PathBuf {
        let run = self.cli(&["backup"]);
        assert_eq!(run.code, 0, "sauvegarde : {}", run.all());
        let list = archives(&self.backups);
        assert!(!list.is_empty(), "aucune archive après la sauvegarde");
        list.last().unwrap().clone()
    }

    /// Altère l'état courant : un fichier modifié, un supprimé, un lien supprimé, un fichier en trop.
    pub fn mutate(&self) {
        self.write("etc/libvirt/qemu/vm1.xml", "<domain>CHANGED</domain>\n");
        fs::remove_file(self.root.join("usr/local/sbin/hook.sh")).unwrap();
        fs::remove_file(self.root.join("etc/libvirt/qemu/autostart/vm1.xml")).unwrap();
        self.write("etc/libvirt/qemu/extra.xml", "extra\n");
        self.write("etc/default/grub", "CHANGED\n");
    }

    pub fn write_stub(&self, name: &str, text: &str) {
        fs::write(self.stubs.join(name), text).unwrap();
    }

    pub fn set_states(&self, text: &str) {
        fs::write(self.stubs.join("states"), text).unwrap();
    }
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Archives `libvirt-config-*.tar.gz` d'un dossier, de la plus ancienne à la plus récente.
pub fn archives(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().unwrap().to_string_lossy().into_owned();
                    n.starts_with("libvirt-config-") && n.ends_with(".tar.gz") && !n.contains("latest")
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

pub fn safety_archives(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("pre-restore-safety-")).collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Noms stockés dans une archive.
pub fn names(path: &Path) -> Vec<String> {
    let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(path).unwrap()));
    archive.entries().unwrap().map(|e| e.unwrap().path().unwrap().to_string_lossy().trim_end_matches('/').to_string()).collect()
}

/// Contenu d'un membre d'archive.
pub fn member(path: &Path, name: &str) -> Option<String> {
    let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(path).unwrap()));
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_string_lossy().trim_end_matches('/') == name {
            let mut s = String::new();
            entry.read_to_string(&mut s).unwrap();
            return Some(s);
        }
    }
    None
}

pub fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Attend qu'une condition devienne vraie (ou échoue après `secs` secondes).
pub fn wait_for(secs: u64, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    condition()
}

/// Écrit une archive .tar.gz avec des noms arbitraires (même `..` ou absolus, refusés par l'API usuelle).
/// Entrée brute : (nom, contenu, lien éventuel : (cible, lien physique ?)).
pub type RawEntry<'a> = (&'a str, &'a [u8], Option<(&'a str, bool)>);

pub fn raw_archive(path: &Path, entries: &[RawEntry]) {
    let file = fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (name, data, link) in entries {
        let mut header = tar::Header::new_gnu();
        {
            let old = header.as_old_mut();
            old.name[..name.len()].copy_from_slice(name.as_bytes());
        }
        header.set_mode(0o644);
        header.set_mtime(0);
        match link {
            Some((target, hard)) => {
                header.set_entry_type(if *hard { tar::EntryType::Link } else { tar::EntryType::Symlink });
                header.as_old_mut().linkname[..target.len()].copy_from_slice(target.as_bytes());
                header.set_size(0);
            }
            None => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(data.len() as u64);
            }
        }
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}
