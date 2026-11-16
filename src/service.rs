//! Service systemd de surveillance : état, installation, désinstallation, démon inotify.

use crate::tr;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::archive::{list_archives, Kind};
use crate::backup::{create_backup, ensure_backup_dir};
use crate::bail;
use crate::config::{check_backup_dir, save_config, validate, SERVICE_NAME, WATCH_DIRS};
use crate::context::Context;
use crate::report::{busy, QuietReporter};
use crate::selfinstall;
use crate::sys::{human_size, log, run, run_env, same_device_as_root, which, Output, Result, Tag};

// --------------------------------------------------------------------------
// systemctl
// --------------------------------------------------------------------------

pub fn systemctl(args: &[&str], timeout_secs: u64) -> Output {
    let mut argv = vec!["systemctl"];
    argv.extend_from_slice(args);
    run(&argv, Some(Duration::from_secs(timeout_secs)))
}

pub fn unit_exists() -> bool {
    systemctl(&["cat", SERVICE_NAME], 20).code == 0
}

#[derive(Debug, Clone)]
pub struct UnitState {
    pub installed: bool,
    pub active: String,
    pub enabled: String,
}

pub fn unit_state() -> UnitState {
    if which("systemctl").is_none() {
        return UnitState { installed: false, active: "indisponible".into(), enabled: "indisponible".into() };
    }
    if !unit_exists() {
        return UnitState { installed: false, active: "absent".into(), enabled: "absent".into() };
    }
    let word = |args: &[&str]| {
        let out = systemctl(args, 20).stdout.trim().to_string();
        if out.is_empty() {
            "inconnu".to_string()
        } else {
            out
        }
    };
    UnitState { installed: true, active: word(&["is-active", SERVICE_NAME]), enabled: word(&["is-enabled", SERVICE_NAME]) }
}

/// start | stop | restart | enable | disable (le démarrage attend la disponibilité du service).
pub fn control(ctx: &Context, action: &str) -> Result<()> {
    if !["start", "stop", "restart", "enable", "disable"].contains(&action) {
        bail!("Unknown service action: {}", action);
    }
    if !unit_exists() {
        bail!("The service {} is not installed: run \"libvirt-backup install\" first.", SERVICE_NAME);
    }
    let rep = ctx.rep.clone();
    let result = if action == "start" || action == "restart" {
        busy(rep.as_ref(), tr!("Starting the service: inotify is setting up the recursive watches..."), || {
            Ok::<_, crate::sys::AppError>(systemctl(&[action, SERVICE_NAME], 300))
        })?
    } else {
        systemctl(&[action, SERVICE_NAME], 300)
    };
    if result.code != 0 {
        let text = if result.stderr.trim().is_empty() { &result.stdout } else { &result.stderr };
        bail!("systemctl {1} failed: {0}", text.trim(), action);
    }
    rep.info(&tr!("Service {0}: {1} done.", SERVICE_NAME, action));
    Ok(())
}

// --------------------------------------------------------------------------
// Installation / désinstallation
// --------------------------------------------------------------------------

fn ensure_inotify(ctx: &Context) -> Result<()> {
    if which("inotifywait").is_some() {
        return Ok(());
    }
    ctx.rep.info(tr!("Installing the inotify-tools package..."));
    if which("apt-get").is_none() {
        bail!("inotifywait not found and apt-get is unavailable: install inotify-tools.");
    }
    let env = [("DEBIAN_FRONTEND", "noninteractive")];
    if run_env(&["apt-get", "update"], Some(Duration::from_secs(600)), &env).code != 0 {
        ctx.rep.warn(tr!("apt-get update failed; trying to install with the current indexes."));
    }
    let result = run_env(&["apt-get", "install", "-y", "inotify-tools"], Some(Duration::from_secs(900)), &env);
    if result.code != 0 || which("inotifywait").is_none() {
        let tail: String = result.stderr.trim().chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("cannot install inotify-tools: {}", tail);
    }
    Ok(())
}

#[derive(Default)]
pub struct InstallOptions {
    pub user: Option<String>,
    pub backup_dir: Option<String>,
    pub retention: Option<u32>,
    pub skip_first_backup: bool,
}

pub fn install(ctx: &mut Context, opts: &InstallOptions) -> Result<()> {
    let rep = ctx.rep.clone();
    let mut cfg = ctx.config.clone();
    if let Some(u) = &opts.user {
        cfg.backup_user = u.clone();
    }
    if let Some(d) = &opts.backup_dir {
        cfg.backup_dir = crate::config::normalize_dir(d)?.to_string_lossy().into_owned();
    }
    if let Some(r) = opts.retention {
        cfg.retention = r;
    }
    validate(&cfg)?;
    ctx.config = cfg.clone();
    ctx.owner_ids()?; // vérifie que le compte existe
    check_backup_dir(&ctx.backup_dir(), ctx.is_system_root())?;

    let layout = selfinstall::Layout::detect();
    rep.step(1, 5, tr!("Checking the prerequisites..."));
    if which("tar").is_none() {
        bail!("GNU tar not found.");
    }
    if layout == selfinstall::Layout::Local && selfinstall::package_present(&ctx.root) {
        bail!(
            "the libvirt-backup package is installed (/{}): run \"sudo libvirt-backup install\" from the package, or remove it (sudo apt remove libvirt-backup libvirt-backup-gui) before installing a compiled version.",
            crate::config::PKG_CLI_REL
        );
    }
    ensure_inotify(ctx)?;
    // Le programme à installer doit être lisible avant de toucher au système.
    let payload = match layout {
        selfinstall::Layout::Local => Some(selfinstall::build_payload(&selfinstall::own_binaries()?)?),
        selfinstall::Layout::Package => None,
    };

    rep.step(2, 5, tr!("Preparing the backup directory and the settings..."));
    ensure_backup_dir(ctx, true)?;
    save_config(&ctx.root, &cfg)?;
    rep.info(&tr!("Archive folder: {}", ctx.backup_dir().display()));
    if ctx.is_system_root() && same_device_as_root(&ctx.backup_dir()) {
        rep.warn(tr!("this folder is on the same disk as the system: a failure of that disk would take the backups with it. A separate disk or USB drive is preferable."));
    }

    match &payload {
        Some(payload) => {
            rep.step(3, 5, tr!("Installing the program, the launchers and the service..."));
            selfinstall::write_payload(&ctx.root, payload)?;
            selfinstall::write_unit(&ctx.root, layout)?;
            if !payload.has_gui {
                rep.warn(tr!("graphical interface not installed: libvirt-backup-gui was not found next to the program."));
            }
            if which("update-desktop-database").is_some() {
                let apps = ctx.root.join("usr/local/share/applications");
                run(&["update-desktop-database", &apps.to_string_lossy()], Some(Duration::from_secs(30)));
            }
        }
        None => {
            rep.step(3, 5, tr!("Creating the service (programs provided by the dpkg package)..."));
            let leftovers = selfinstall::remove_local_install(&ctx.root);
            if !leftovers.is_empty() {
                rep.info(&tr!("Old local installation removed ({} item(s) under /usr/local): the package replaces it.", leftovers.len()));
            }
            selfinstall::write_unit(&ctx.root, layout)?;
        }
    }

    rep.step(4, 5, tr!("Reloading systemd and making the first backup..."));
    systemctl(&["daemon-reload"], 60);
    if !opts.skip_first_backup {
        create_backup(ctx, false)?;
    }

    rep.step(5, 5, tr!("Enabling the automatic watcher service..."));
    if systemctl(&["enable", SERVICE_NAME], 60).code != 0 {
        bail!("systemctl enable failed.");
    }
    let result = busy(rep.as_ref(), tr!("Starting the service: inotify is setting up the recursive watches (a few moments)..."), || {
        Ok::<_, crate::sys::AppError>(systemctl(&["restart", SERVICE_NAME], 300))
    })?;
    if result.code != 0 {
        let journal = run(&["journalctl", "-u", SERVICE_NAME, "-n", "40", "--no-pager"], Some(Duration::from_secs(30))).stdout;
        bail!("systemd could not start the watcher service.\n{}", journal);
    }
    if systemctl(&["is-active", "--quiet", SERVICE_NAME], 30).code != 0 {
        bail!("The service is not active: journalctl -u {} -n 80 --no-pager", SERVICE_NAME);
    }

    let dir = ctx.backup_dir();
    rep.info("");
    rep.info(tr!("Installation complete. The service confirmed that the inotify watches are set up."));
    rep.info(&tr!("Archive directory: {}", dir.display()));
    rep.info(&tr!("Latest archive: {}/libvirt-config-latest.tar.gz", dir.display()));
    rep.result(json!({"backup_dir": dir.to_string_lossy(), "service": SERVICE_NAME}));
    Ok(())
}

pub fn uninstall(ctx: &Context, purge: bool) -> Result<()> {
    let rep = ctx.rep.clone();
    rep.step(1, 3, tr!("Stopping and disabling the service..."));
    if unit_exists() {
        systemctl(&["stop", SERVICE_NAME], 120);
        systemctl(&["disable", SERVICE_NAME], 60);
    }
    rep.step(2, 3, tr!("Removing the systemd unit..."));
    let unit = ctx.root.join("etc/systemd/system").join(SERVICE_NAME);
    if unit.is_file() {
        let _ = std::fs::remove_file(&unit);
    }
    systemctl(&["daemon-reload"], 60);
    let layout = selfinstall::Layout::detect();
    let by_package = layout.programs_owned_by_dpkg();
    rep.step(3, 3, if purge { tr!("Removing the installed files...") } else { tr!("Keeping the installed program.") });
    if purge {
        let removed = selfinstall::remove_files(&ctx.root, true, layout);
        rep.info(&tr!("{0} item(s) removed. The archives in {1} are kept.", removed.len(), ctx.backup_dir().display()));
        if by_package {
            rep.info(tr!("The program is provided by the dpkg package: \"sudo apt remove libvirt-backup libvirt-backup-gui\" removes it."));
        }
    } else if by_package {
        rep.info(tr!(
            "The program remains installed (dpkg package: \"sudo apt remove libvirt-backup\" removes it). The archives are kept."
        ));
    } else {
        rep.info(tr!("The program remains installed (use --purge to remove it). The archives are kept."));
    }
    Ok(())
}

// --------------------------------------------------------------------------
// Démon de surveillance
// --------------------------------------------------------------------------

static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERMINATE.store(true, Ordering::SeqCst);
}

pub fn inotify_command(ctx: &Context) -> Result<(Vec<String>, usize)> {
    let Some(exe) = which("inotifywait") else {
        bail!("inotifywait not found: install the inotify-tools package.");
    };
    let dirs: Vec<String> =
        WATCH_DIRS.iter().map(|rel| ctx.root.join(rel)).filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()).collect();
    if dirs.is_empty() {
        bail!("No configuration directory to watch.");
    }
    let mut cmd: Vec<String> = vec![exe.to_string_lossy().into_owned(), "--monitor".into(), "--recursive".into()];
    for ev in ["close_write", "create", "delete", "moved_to", "moved_from", "attrib"] {
        cmd.extend(["--event".into(), ev.into()]);
    }
    cmd.extend(["--format".into(), "%w%f %e".into()]);
    let count = dirs.len();
    cmd.extend(dirs);
    Ok((cmd, count))
}

/// Surveille la configuration et déclenche une sauvegarde après chaque rafale de modifications.
///
/// Renvoie un code de sortie non nul si inotifywait s'arrête (systemd relance alors le service). Le
/// service est déclaré prêt (READY=1) seulement quand tous les watches récursifs existent.
pub fn run_watch(
    ctx: &Context,
    command: Option<Vec<String>>,
    backup_fn: &mut dyn FnMut(&Context) -> Result<()>,
    notify: &dyn Fn(&str) -> bool,
) -> Result<i32> {
    let (cmd, root_count) = match command {
        Some(c) => (c, 0usize),
        None => inotify_command(ctx)?,
    };
    let quiet = Context { rep: Arc::new(QuietReporter), ..ctx.clone() };
    log(Tag::Watch, &tr!("Setting up the recursive watch of {} directory(ies).", if root_count > 0 { root_count } else { cmd.len() }));

    // stdout et stderr d'inotifywait partagent le même tube : « Watches established. » (stderr) reste
    // ainsi ordonné par rapport aux événements (stdout), comme avec `2>&1` dans le script d'origine.
    let (reader, writer) = std::io::pipe()?;
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .spawn()
        .map_err(|e| crate::sys::AppError(tr!("{0}: {1}", cmd[0], e)))?;
    let (tx, rx) = mpsc::channel::<Option<String>>();
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim_end_matches('\n').to_string();
                    if tx.send(Some(line)).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(None);
    });

    TERMINATE.store(false, Ordering::SeqCst);
    // SAFETY: le gestionnaire ne fait qu'écrire un booléen atomique.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    let debounce = Duration::from_secs_f64(ctx.config.debounce);
    let poll = Duration::from_millis(200);
    let mut ready = false;
    let mut terminated = false;
    'outer: loop {
        if TERMINATE.load(Ordering::SeqCst) {
            terminated = true;
            break;
        }
        let line = match rx.recv_timeout(poll) {
            Ok(Some(l)) => l,
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
        };
        if !line.starts_with('/') {
            if line.trim() == "Watches established." {
                ready = true;
                log(Tag::Watch, tr!("Watcher active: all the recursive watches are set up."));
                notify(&tr!("READY=1\nSTATUS=Watcher active: {} root folders watched", root_count));
            } else if !line.trim().is_empty() {
                log(Tag::Watch, &tr!("inotifywait message: {}", line));
            }
            continue;
        }
        if !ready {
            log(Tag::Watch, &tr!("Event received before the readiness confirmation: {}", line));
            continue;
        }
        // Regroupe la rafale : on attend `debounce` secondes sans nouvel événement.
        let mut last = line;
        let mut deadline = Instant::now() + debounce;
        let mut finished = false;
        loop {
            if TERMINATE.load(Ordering::SeqCst) {
                terminated = true;
                break 'outer;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining.min(poll)) {
                Ok(Some(next)) => {
                    if next.starts_with('/') {
                        last = next;
                    }
                    deadline = Instant::now() + debounce;
                }
                Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    finished = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        log(Tag::Watch, &tr!("Change detected ({}); backup triggered.", last));
        if let Err(e) = backup_fn(&quiet) {
            // Une erreur de sauvegarde ne doit jamais arrêter la surveillance.
            log(Tag::Watch, &tr!("ERROR: automatic backup failed: {}", e));
        }
        if finished {
            break;
        }
    }
    let code = match child.try_wait() {
        Ok(Some(status)) => status.code(),
        _ => {
            let _ = child.kill();
            child.wait().ok().and_then(|s| s.code())
        }
    };
    if terminated {
        return Ok(0);
    }
    let shown = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
    if !ready {
        log(Tag::Watch, &tr!("ERROR: inotifywait stopped before the watches were set up (code {}).", shown));
    } else {
        log(Tag::Watch, &tr!("ERROR: inotifywait stopped (code {}); systemd will restart the service.", shown));
    }
    Ok(1)
}

// --------------------------------------------------------------------------
// Statut global
// --------------------------------------------------------------------------

pub fn status(ctx: &Context) -> Value {
    let state = unit_state();
    let dir: PathBuf = ctx.backup_dir();
    let archives = list_archives(&dir);
    let latest = archives
        .iter()
        .find(|a| a.kind == Kind::Auto)
        .map(|a| json!({"name": a.name(), "created": a.created.iso(), "size": a.size, "size_human": human_size(a.size)}));
    let layout = selfinstall::Layout::detect();
    json!({
        "installed": selfinstall::is_installed(&ctx.root, layout),
        "layout": layout.id(),
        "local_shadow": layout.programs_owned_by_dpkg() && selfinstall::local_install_present(&ctx.root),
        "legacy_unit": state.installed && !selfinstall::unit_uses_this_tool(&ctx.root),
        "service": {"installed": state.installed, "active": state.active, "enabled": state.enabled},
        "backup_user": ctx.config.backup_user,
        "backup_dir": dir.to_string_lossy(),
        "backup_dir_exists": dir.is_dir(),
        "archives": archives.len(),
        "latest": latest,
        "config": ctx.config.to_json(),
    })
}
