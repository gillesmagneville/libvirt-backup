mod common;

use std::fs;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use common::*;

struct Watch {
    child: Child,
    socket: UnixDatagram,
}

impl Watch {
    /// `fake_inotify` : corps d'un faux `inotifywait` ; vide = le vrai programme.
    fn start(env: &Env, fake_inotify: &str) -> Watch {
        if !fake_inotify.is_empty() {
            write_file(&env.stubs.join("inotifywait"), &format!("#!/bin/bash\n{fake_inotify}"), 0o755);
        }
        let sock_path = env.tmp.path().join("notify.sock");
        let socket = UnixDatagram::bind(&sock_path).unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let child = env
            .command(&["watch"])
            .env("NOTIFY_SOCKET", &sock_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Watch { child, socket }
    }

    fn notifications(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = [0u8; 512];
        while let Ok(n) = self.socket.recv(&mut buf) {
            out.push(String::from_utf8_lossy(&buf[..n]).into_owned());
        }
        out
    }

    fn wait_exit(&mut self, secs: u64) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap_or(-1);
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("le démon de surveillance ne s'est pas arrêté");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn terminate(&self) {
        // SAFETY: envoi d'un signal à notre propre processus enfant.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn readiness_debounce_and_unconditional_backup() {
    let env = Env::new();
    let mut watch = Watch::start(
        &env,
        r#"
echo "Setting up watches.  Beware: since -r was given, this may take a while!" >&2
echo "/x/ignored-before-ready CREATE"
sleep 0.2
echo "Watches established." >&2
sleep 0.2
echo "/x/a.xml CLOSE_WRITE,CLOSE"
echo "/x/b.xml CREATE"
echo "/x/c.xml DELETE"
sleep 1.5
echo "/x/d.xml ATTRIB"
sleep 1.0
exit 0
"#,
    );
    assert_eq!(watch.wait_exit(40), 1, "inotifywait arrêté -> code non nul pour que systemd relance le service");
    let notes = watch.notifications();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("READY=1"), "{notes:?}");
    // rafale de 3 événements = 1 sauvegarde, puis d.xml = 1 autre ; contenu identique mais sauvegarde inconditionnelle
    assert_eq!(archives(&env.backups).len(), 2);
}

#[test]
fn backup_failure_does_not_stop_the_watcher() {
    let env = Env::new();
    let parent = env.tmp.path().join("support-absent");
    let target = parent.join("sauvegardes");
    env.set_config(&[("backup_dir", &target.to_string_lossy())]);
    let mut watch = Watch::start(
        &env,
        &format!(
            r#"
echo "Watches established." >&2
sleep 0.2
echo "/x/a.xml CREATE"
sleep 1.5
mkdir -p {parent}
echo "/x/b.xml CREATE"
sleep 1.5
exit 0
"#,
            parent = shell_quote(&parent.to_string_lossy())
        ),
    );
    assert_eq!(watch.wait_exit(40), 1);
    // la 1re sauvegarde échoue (support non monté : le dossier parent n'existe pas), la 2e réussit
    assert_eq!(archives(&target).len(), 1);
    assert_eq!(watch.notifications().len(), 1);
}

#[test]
fn watcher_applies_a_new_backup_dir_without_restart() {
    let env = Env::new();
    let second = env.tmp.path().join("autre-dossier");
    fs::create_dir(&second).unwrap();
    let new_cfg = serde_json::json!({"backup_user": env.user, "backup_dir": second.to_string_lossy(), "retention": 3,
                                     "safety_retention": 2, "debounce": 0.4})
    .to_string();
    let cfg_path = env.root.join("etc/libvirt-backup/config.json");
    let mut watch = Watch::start(
        &env,
        &format!(
            r#"
echo "Watches established." >&2
sleep 0.2
echo "/x/a.xml CREATE"
sleep 1.5
printf '%s' {cfg} > {path}
echo "/x/b.xml CREATE"
sleep 1.5
exit 0
"#,
            cfg = shell_quote(&new_cfg),
            path = shell_quote(&cfg_path.to_string_lossy())
        ),
    );
    assert_eq!(watch.wait_exit(40), 1);
    assert_eq!(archives(&env.backups).len(), 1, "avant le changement de réglage");
    assert_eq!(archives(&second).len(), 1, "après « config set backup_dir=... », sans redémarrage");
}

#[test]
fn sigterm_stops_the_watcher_cleanly() {
    let env = Env::new();
    let mut watch = Watch::start(&env, "echo \"Watches established.\" >&2\nexec sleep 60\n");
    assert!(wait_for(10, || !watch.notifications().is_empty() || watch.child.try_wait().unwrap().is_some()));
    std::thread::sleep(Duration::from_millis(200));
    watch.terminate();
    assert_eq!(watch.wait_exit(10), 0, "arrêt demandé par systemd = succès");
}

#[test]
fn missing_inotifywait_is_reported() {
    let env = Env::new();
    let out = env.command(&["watch"]).env("PATH", &env.stubs).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("inotifywait"), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn end_to_end_with_real_inotifywait() {
    if !Path::new("/usr/bin/inotifywait").exists() {
        return;
    }
    let env = Env::new();
    let mut watch = Watch::start(&env, "");
    assert!(wait_for(20, || !watch.notifications().is_empty()), "READY=1 jamais reçu");
    env.write("etc/libvirt/qemu/vm1.xml", "<domain>MODIFIED</domain>\n");
    assert!(wait_for(20, || !archives(&env.backups).is_empty()), "aucune sauvegarde déclenchée par inotify");
    let found: Vec<PathBuf> = archives(&env.backups);
    assert!(member(&found[0], "etc/libvirt/qemu/vm1.xml").unwrap().contains("MODIFIED"));
    watch.terminate();
    assert_eq!(watch.wait_exit(15), 0);
}
