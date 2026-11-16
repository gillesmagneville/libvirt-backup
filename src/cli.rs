//! Interface en ligne de commande de libvirt-backup.

use crate::tr;
use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use clap::{Command as ClapCommand, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::archive::{expand_home, inspect_archive, list_archives, resolve_archive, ArchiveInfo, Kind};
use crate::backup::{create_backup, preview_backup};
use crate::bail;
use crate::config::{
    check_backup_dir, confirm_word, delete_word, group_ids, is_confirm_word, is_delete_word, normalize_dir, save_config, validate,
    CONFIG_KEYS, GROUPS, SERVICE_NAME,
};
use crate::context::Context;
use crate::delete::{plan_delete, run_delete};
use crate::i18n;
use crate::report::{JsonReporter, Reporter, TextReporter};
use crate::restore::{parse_groups, plan_restore, run_restore};
use crate::selfinstall;
use crate::service::{self, InstallOptions};
use crate::sys::{human_size, interrupted_message, is_root, which, AppError, InterruptGuard, Result};
use crate::VERSION;

/// Écrit sur la sortie standard sans jamais paniquer (tube fermé par le lecteur).
macro_rules! outln {
    () => { { let _ = writeln!(std::io::stdout()); } };
    ($($arg:tt)*) => { { let _ = writeln!(std::io::stdout(), $($arg)*); } };
}

const QUERY_COMMANDS: &[&str] = &["list", "info", "status", "groups"];

#[derive(Parser)]
#[command(
    name = "libvirt-backup",
    version = VERSION,
    about = "Backup and restore of the virtualization configuration (libvirt/QEMU, AppArmor, VFIO, systemd, network, ZFS). Graphical interface: \"libvirt-backup gui\".",
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true,
    subcommand_help_heading = "Commands",
    next_help_heading = "Options",
    override_usage = "libvirt-backup [OPTIONS] [COMMAND]"
)]
struct Cli {
    /// Show the help
    #[arg(short = 'h', long, global = true, action = clap::ArgAction::Help, help_heading = "Global options")]
    help: Option<bool>,

    /// Show the version
    #[arg(short = 'V', long, action = clap::ArgAction::Version, help_heading = "Global options")]
    version: Option<bool>,

    /// Machine-readable output (one JSON event per line)
    #[arg(long, global = true, help_heading = "Global options")]
    json: bool,

    /// System root (tests)
    #[arg(long, global = true, hide = true, value_name = "DIR")]
    root: Option<PathBuf>,

    /// Archive folder for this run (with "install", it becomes the saved folder)
    #[arg(long, global = true, value_name = "FOLDER", help_heading = "Global options")]
    backup_dir: Option<String>,

    /// Language of the messages for this run: system, en, fr, de or es (default: the "language" setting, then the system language)
    #[arg(long, global = true, value_name = "LANGUAGE", help_heading = "Global options")]
    language: Option<String>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create an archive now
    Backup {
        /// Do nothing if the configuration is identical to the last archive
        #[arg(long)]
        if_changed: bool,
        /// Simulation: show what would be archived (files, sizes, skipped) without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Archive everything for this run (without the lean backup, see the lean_backup setting)
        #[arg(long)]
        full: bool,
    },
    /// List the archives
    List,
    /// Details of an archive (content by group, README)
    Info {
        /// Number, name or path; default: the most recent
        archive: Option<String>,
    },
    /// Check the integrity and format of one or all archives
    Verify {
        archive: Option<String>,
        /// Verify all the automatic archives
        #[arg(long)]
        all: bool,
    },
    /// Delete selected archives (irreversible)
    Delete {
        /// Numbers (see "list"), names or "latest"; at least one
        #[arg(required = true, value_name = "ARCHIVE")]
        archives: Vec<String>,
        /// Do not ask for the "DELETE" confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Restore an archive (merge, with a prior safety archive)
    Restore {
        /// Number, name or path (USB drive, copy...); otherwise interactive selection
        archive: Option<String>,
        /// Take the most recent automatic archive
        #[arg(long)]
        latest: bool,
        /// Restore only these groups (comma-separated)
        #[arg(long, value_name = "GROUPS")]
        only: Option<String>,
        /// Exclude these groups
        #[arg(long, value_name = "GROUPS")]
        skip: Option<String>,
        /// Do not ask for the "RESTORE" confirmation
        #[arg(long)]
        yes: bool,
        /// Simulation: show the plan, change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Install the tool and the watcher service, and make a first backup
    Install {
        /// Owner of the archives (default: the calling user)
        #[arg(long)]
        user: Option<String>,
        /// Number of archives kept (default: 30)
        #[arg(long)]
        retention: Option<u32>,
        /// Do not make a first backup
        #[arg(long)]
        no_first_backup: bool,
        /// Ask nothing: keep the proposed folder
        #[arg(short, long)]
        yes: bool,
    },
    /// Stop and remove the watcher service
    Uninstall {
        /// Also remove the program and the settings (never the archives)
        #[arg(long)]
        purge: bool,
    },
    /// Control the watcher service
    Service {
        #[arg(value_enum)]
        action: ServiceAction,
    },
    /// State of the installation, the service and the archives
    Status,
    /// Show or change the settings (including backup_dir, the location of the backups)
    Config {
        #[arg(value_enum)]
        action: ConfigAction,
        /// Assignments, for example backup_dir=/mnt/backups/virtualization
        #[arg(value_name = "KEY=VALUE")]
        pairs: Vec<String>,
        /// Restart the service after the change
        #[arg(long)]
        restart_service: bool,
    },
    /// List the groups of backed-up paths
    Groups,
    /// Watcher daemon (started by systemd)
    Watch,
    /// Open the GTK4 graphical interface
    Gui,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum ServiceAction {
    Status,
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

impl ServiceAction {
    fn word(self) -> &'static str {
        match self {
            ServiceAction::Status => "status",
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
            ServiceAction::Restart => "restart",
            ServiceAction::Enable => "enable",
            ServiceAction::Disable => "disable",
        }
    }
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum ConfigAction {
    Show,
    Set,
}

impl Cmd {
    fn name(&self) -> &'static str {
        match self {
            Cmd::Backup { .. } => "backup",
            Cmd::List => "list",
            Cmd::Info { .. } => "info",
            Cmd::Verify { .. } => "verify",
            Cmd::Delete { .. } => "delete",
            Cmd::Restore { .. } => "restore",
            Cmd::Install { .. } => "install",
            Cmd::Uninstall { .. } => "uninstall",
            Cmd::Service { .. } => "service",
            Cmd::Status => "status",
            Cmd::Config { .. } => "config",
            Cmd::Groups => "groups",
            Cmd::Watch => "watch",
            Cmd::Gui => "gui",
        }
    }
}

// --------------------------------------------------------------------------
// Outils
// --------------------------------------------------------------------------

fn interactive() -> bool {
    std::io::stdin().is_terminal()
}

fn read_answer(question: &str) -> Option<String> {
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
    }
}

/// Chemin donné par l'utilisateur (`~/x`, relatif ou absolu) → chemin absolu normalisé.
pub fn resolve_dir(raw: &str) -> Result<PathBuf> {
    let expanded = expand_home(raw.trim());
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir().map_err(|e| AppError(tr!("current folder not accessible: {}", e)))?.join(expanded)
    };
    normalize_dir(&absolute.to_string_lossy())
}

/// Relance la commande via sudo si besoin (comme l'ancien script), sauf en mode JSON ou sans terminal.
fn ensure_root(cli: &Cli) -> Result<()> {
    if is_root() || cli.root.is_some() {
        return Ok(());
    }
    if cli.json || !interactive() {
        bail!("This operation requires administrator rights (root).");
    }
    let Some(sudo) = which("sudo") else {
        bail!("sudo not found; cannot obtain administrator privileges.");
    };
    let exe = std::env::current_exe().map_err(|e| AppError(tr!("program not found: {}", e)))?;
    outln!("{}", tr!("Administrator privileges required; asking for sudo authentication..."));
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let error = Command::new(sudo).arg("--").arg(exe).args(args).exec();
    bail!("cannot re-run the command through sudo: {}", error)
}

fn print_archive_list(ctx: &Context, archives: &[ArchiveInfo]) {
    outln!("{}", tr!("\nBackups available in {}\n", ctx.backup_dir().display()));
    let first_auto = archives.iter().position(|a| a.kind == Kind::Auto);
    for (index, info) in archives.iter().enumerate() {
        let marker = if Some(index) == first_auto { tr!("  <== LATEST BACKUP (default choice)") } else { "" };
        outln!("  [{:2}] {}", index + 1, info.name());
        outln!(
            "{}",
            tr!("       Date: {0} | Size: {1} | {2}{3}", info.created.display(), human_size(info.size), info.kind.label(), marker)
        );
    }
    outln!();
}

// --------------------------------------------------------------------------
// Commandes
// --------------------------------------------------------------------------

fn cmd_backup(cli: &Cli, ctx: &Context, if_changed: bool, dry_run: bool, full: bool) -> Result<i32> {
    ensure_root(cli)?;
    let mut ctx = ctx.clone();
    if full {
        ctx.config.lean_backup = false;
    }
    let ctx = &ctx;
    if dry_run {
        preview_backup(ctx)?;
        return Ok(0);
    }
    let _guard = InterruptGuard::new();
    let result = create_backup(ctx, if_changed)?;
    Ok(if result.path.is_some() || result.skipped.is_some() { 0 } else { 1 })
}

fn cmd_list(cli: &Cli, ctx: &Context) -> Result<i32> {
    let archives = list_archives(&ctx.backup_dir());
    if cli.json {
        let list: Vec<Value> = archives
            .iter()
            .enumerate()
            .map(|(i, a)| {
                json!({"index": i + 1, "name": a.name(), "path": a.path.to_string_lossy(), "kind": a.kind.json(),
                       "size": a.size, "created": a.created.iso()})
            })
            .collect();
        outln!("{}", Value::Array(list));
        return Ok(0);
    }
    if archives.is_empty() {
        eprintln!("{}", tr!("No libvirt-config-*.tar.gz archive found in {}.", ctx.backup_dir().display()));
        return Ok(1);
    }
    print_archive_list(ctx, &archives);
    Ok(0)
}

fn cmd_info(cli: &Cli, ctx: &Context, spec: Option<&str>) -> Result<i32> {
    let info = resolve_archive(&ctx.backup_dir(), spec)?;
    let rep = inspect_archive(&info.path)?;
    if cli.json {
        let groups: serde_json::Map<String, Value> =
            GROUPS.iter().map(|g| (g.id.to_string(), json!({"label": g.label(), "count": rep.group_count(g.id)}))).collect();
        outln!(
            "{}",
            json!({"name": info.name(), "path": info.path.to_string_lossy(), "size": info.size, "entries": rep.entries,
                   "valid": rep.valid(), "groups": groups, "manifest": rep.manifest, "readme": rep.readme})
        );
        return Ok(0);
    }
    outln!("{}", tr!("Archive: {}", info.path.display()));
    outln!(
        "{}",
        tr!(
            "Size    : {0} | Entries: {1} | Valid: {2}",
            human_size(info.size),
            rep.entries,
            if rep.valid() { tr!("yes") } else { tr!("NO") }
        )
    );
    for (key, label) in [("created", "Created"), ("version", "Version"), ("hostname", "Hostname"), ("kernel", "Kernel")] {
        if let Some(value) = rep.manifest_str(key) {
            outln!("{label:<8}: {value}");
        }
    }
    outln!("{}", tr!("\nContent by group:"));
    for g in GROUPS {
        outln!("  {:9} {:5}  {}", g.id, rep.group_count(g.id), g.label());
    }
    if !rep.unknown.is_empty() || !rep.dangerous.is_empty() {
        let refused: Vec<&str> = rep.unknown.iter().chain(rep.dangerous.iter()).take(10).map(String::as_str).collect();
        outln!("{}", tr!("\nRefused entries: {}", refused.join(", ")));
    }
    if !rep.readme.is_empty() {
        outln!("\n{}", rep.readme);
    }
    Ok(0)
}

fn cmd_verify(ctx: &Context, archive: Option<&str>, all: bool) -> Result<i32> {
    let rep = ctx.rep.clone();
    let dir = ctx.backup_dir();
    let targets: Vec<ArchiveInfo> = if all {
        list_archives(&dir).into_iter().filter(|a| a.kind == Kind::Auto).collect()
    } else {
        vec![resolve_archive(&dir, archive)?]
    };
    if targets.is_empty() {
        bail!("No archive to verify in {}.", dir.display());
    }
    let mut bad = 0;
    for info in &targets {
        match inspect_archive(&info.path) {
            Ok(result) if result.valid() && !result.system_members.is_empty() => {
                rep.info(&tr!("OK       {0} ({1} entries, {2})", info.name(), result.entries, human_size(info.size)));
            }
            Ok(result) => {
                bad += 1;
                let why: Vec<&str> = result.unknown.iter().chain(result.dangerous.iter()).take(3).map(String::as_str).collect();
                let why = if why.is_empty() { tr!("no system path").to_string() } else { why.join(", ") };
                rep.error(&tr!("{0}: unexpected format ({1})", info.name(), why));
            }
            Err(e) => {
                bad += 1;
                rep.error(&e.to_string());
            }
        }
    }
    rep.result(json!({"checked": targets.len(), "failed": bad}));
    Ok(if bad > 0 { 1 } else { 0 })
}

fn choose_interactively(ctx: &Context) -> Result<String> {
    let archives = list_archives(&ctx.backup_dir());
    if archives.is_empty() {
        bail!("No libvirt-config-*.tar.gz archive found in {}.", ctx.backup_dir().display());
    }
    print_archive_list(ctx, &archives);
    let answer = read_answer(tr!("Number of the backup to restore [1]: ")).unwrap_or_default();
    let choice = if answer.trim().is_empty() { "1".to_string() } else { answer.trim().to_string() };
    match choice.parse::<usize>() {
        Ok(n) if (1..=archives.len()).contains(&n) => Ok(choice),
        _ => bail!("invalid selection."),
    }
}

fn cmd_delete(cli: &Cli, ctx: &Context, archives: &[String], yes: bool) -> Result<i32> {
    ensure_root(cli)?;
    let plan = plan_delete(ctx, archives)?;
    if !yes {
        if cli.json || !interactive() {
            bail!("Interactive confirmation is impossible without a terminal: use --yes.");
        }
        outln!("{}", tr!("\nArchives to delete, in {}:\n", ctx.backup_dir().display()));
        for target in &plan.targets {
            outln!("  - {}  ({}, {}, {})", target.name(), target.created.display(), human_size(target.size), target.kind.label());
        }
        outln!("{}", tr!("\nWarning: the deletion is PERMANENT (no trash), {} will be freed.", human_size(plan.total_size)));
        for warning in &plan.warnings {
            outln!("{}", tr!("WARNING: {}", warning));
        }
        let word = delete_word();
        let answer = read_answer(&tr!("\nTo confirm, type exactly {}: ", word)).unwrap_or_default();
        if !is_delete_word(&answer) {
            outln!("{}", tr!("Deletion cancelled; no archive was deleted."));
            return Ok(0);
        }
    }
    let _guard = InterruptGuard::new();
    let result = run_delete(ctx, &plan)?;
    if result.latest == "relinked" {
        ctx.rep.info(tr!("The link \"libvirt-config-latest.tar.gz\" now points to the most recent remaining archive."));
    } else if result.latest == "removed" {
        ctx.rep.info(tr!("No automatic backup left: the link \"libvirt-config-latest.tar.gz\" was removed."));
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_restore(
    cli: &Cli,
    ctx: &Context,
    archive: Option<&str>,
    latest: bool,
    only: Option<&str>,
    skip: Option<&str>,
    yes: bool,
    dry_run: bool,
) -> Result<i32> {
    if !dry_run {
        ensure_root(cli)?;
    }
    let spec: String = if latest {
        "latest".into()
    } else if let Some(a) = archive {
        a.to_string()
    } else {
        if cli.json || !interactive() {
            bail!("Interactive selection is impossible without a terminal: give an ARCHIVE or use --latest.");
        }
        choose_interactively(ctx)?
    };
    let info = resolve_archive(&ctx.backup_dir(), Some(&spec))?;
    let groups = parse_groups(only, skip)?;
    let plan = plan_restore(&info.path, Some(&groups))?;

    if !yes && !dry_run {
        if cli.json || !interactive() {
            bail!("Interactive confirmation is impossible without a terminal: use --yes.");
        }
        outln!("{}", tr!("\nSelected backup: {}", info.path.display()));
        outln!("{}", tr!("Size: {}\n", human_size(info.size)));
        outln!("{}", tr!("The restore will:"));
        outln!("{}", tr!("  - replace the configuration files present in this archive;"));
        outln!("{}", tr!("  - restore the hooks, scripts, libvirt XML, AppArmor and systemd settings;"));
        outln!("{}", tr!("  - restore the NVRAM/TPM states only if they are in the archive;"));
        outln!("{}", tr!("  - redefine the libvirt domains, networks and pools from the exported XML;"));
        outln!("{}", tr!("  - keep the current files that are not in the archive (no global deletion)."));
        outln!("{}", tr!("\nItems restored: {}", plan.groups.join(", ")));
        outln!("{}", tr!("\nA safety archive of the current state will be created before any change."));
        outln!("{}", tr!("All VMs must be completely shut down. No virtual disk or ZFS dataset/zvol will be restored.\n"));
        let word = confirm_word();
        let answer = read_answer(&tr!("To confirm, type exactly {}: ", word)).unwrap_or_default();
        if !is_confirm_word(&answer) {
            outln!("{}", tr!("Restore cancelled; no change was made."));
            return Ok(0);
        }
    }
    let _guard = InterruptGuard::new();
    run_restore(ctx, &plan, dry_run)?;
    Ok(0)
}

/// Propose le dossier des archives (Entrée = valeur proposée) ; redemande si le choix est refusé.
fn ask_backup_dir(ctx: &Context, user: Option<&str>) -> Result<Option<String>> {
    let mut cfg = ctx.config.clone();
    if let Some(u) = user {
        cfg.backup_user = u.to_string();
    }
    let proposed = cfg.resolved_dir();
    outln!("{}", tr!("Folder where the archives will be stored. A disk or USB drive separate from the system is preferable:"));
    outln!("{}", tr!("otherwise a failure of the system disk would take the backups with it."));
    loop {
        let Some(answer) = read_answer(&tr!("Backup folder [{}]: ", proposed.display())) else {
            return Ok(None);
        };
        if answer.trim().is_empty() {
            return Ok(None);
        }
        match resolve_dir(&answer).and_then(|d| check_backup_dir(&d, ctx.is_system_root()).map(|_| d)) {
            Ok(dir) => return Ok(Some(dir.to_string_lossy().into_owned())),
            Err(e) => eprintln!("{e}"),
        }
    }
}

fn cmd_install(
    cli: &Cli,
    ctx: &mut Context,
    user: Option<String>,
    retention: Option<u32>,
    no_first_backup: bool,
    yes: bool,
) -> Result<i32> {
    ensure_root(cli)?;
    let mut backup_dir = match &cli.backup_dir {
        Some(raw) => Some(resolve_dir(raw)?.to_string_lossy().into_owned()),
        None => None,
    };
    if backup_dir.is_none() && !yes && !cli.json && interactive() {
        backup_dir = ask_backup_dir(ctx, user.as_deref())?;
    }
    let opts = InstallOptions { user, backup_dir, retention, skip_first_backup: no_first_backup };
    service::install(ctx, &opts)?;
    Ok(0)
}

fn cmd_service(cli: &Cli, ctx: &Context, action: ServiceAction) -> Result<i32> {
    if action == ServiceAction::Status {
        let state = service::unit_state();
        outln!(
            "{}",
            tr!(
                "Service {0}: installed={1} active={2} at boot={3}",
                SERVICE_NAME,
                if state.installed { "True" } else { "False" },
                state.active,
                state.enabled
            )
        );
        return Ok(if state.active == "active" { 0 } else { 3 });
    }
    ensure_root(cli)?;
    service::control(ctx, action.word())?;
    Ok(0)
}

fn cmd_status(cli: &Cli, ctx: &Context) -> Result<i32> {
    let data = service::status(ctx);
    if cli.json {
        outln!("{data}");
        return Ok(0);
    }
    let text = |v: &Value| v.as_str().unwrap_or("").to_string();
    let state = &data["service"];
    let yes_no = |v: &Value| if v.as_bool().unwrap_or(false) { tr!("yes") } else { tr!("no") };
    let how = if text(&data["layout"]) == "package" { tr!("dpkg package (/usr/bin)") } else { tr!("local installation (/usr/local)") };
    outln!("{}", tr!("Tool installed         : {0} — {1}", yes_no(&data["installed"]), how));
    if data["local_shadow"].as_bool().unwrap_or(false) {
        outln!(
            "{}", tr!("WARNING: an old local installation (/usr/local) hides the package in the PATH. Run \"sudo /usr/bin/libvirt-backup install\": it will be removed."
        ));
    }
    outln!(
        "{}",
        tr!(
            "Watcher service        : {0} | active: {1} | at boot: {2}",
            if state["installed"].as_bool().unwrap_or(false) { tr!("installed") } else { tr!("absent") },
            text(&state["active"]),
            text(&state["enabled"])
        )
    );
    if data["legacy_unit"].as_bool().unwrap_or(false) {
        outln!("{}", tr!("                        (unit of the old shell scripts: run \"libvirt-backup install\" to replace it)"));
    }
    outln!("{}", tr!("User                   : {}", text(&data["backup_user"])));
    outln!("{}", tr!("Archive folder         : {0} ({1} archive(s))", text(&data["backup_dir"]), data["archives"]));
    if !data["latest"].is_null() {
        let l = &data["latest"];
        outln!("{}", tr!("Latest backup          : {0} ({1}, {2})", text(&l["name"]), text(&l["created"]), text(&l["size_human"])));
    }
    Ok(0)
}

fn cmd_config(cli: &Cli, ctx: &mut Context, action: ConfigAction, pairs: &[String], restart_service: bool) -> Result<i32> {
    match action {
        ConfigAction::Show => {
            for key in CONFIG_KEYS {
                outln!("{key}={}", ctx.config.get(key));
            }
            outln!("{}", tr!("# effective folder: {}", ctx.backup_dir().display()));
            Ok(0)
        }
        ConfigAction::Set => {
            ensure_root(cli)?;
            if pairs.is_empty() && cli.backup_dir.is_none() {
                bail!("Give at least one KEY=VALUE assignment.");
            }
            let mut new = ctx.config.clone();
            if let Some(raw) = &cli.backup_dir {
                new.backup_dir = resolve_dir(raw)?.to_string_lossy().into_owned();
            }
            for pair in pairs {
                let Some((key, value)) = pair.split_once('=') else {
                    bail!("Invalid assignment: {} (expected KEY=VALUE).", pair);
                };
                let key = key.trim();
                if key == "backup_dir" && !value.trim().is_empty() {
                    new.backup_dir = resolve_dir(value)?.to_string_lossy().into_owned();
                } else {
                    new.set(key, value)?;
                }
            }
            validate(&new)?;
            if !new.backup_dir.is_empty() {
                check_backup_dir(Path::new(&new.backup_dir), ctx.is_system_root())?;
            }
            ctx.config = new.clone();
            let path = save_config(&ctx.root, &new)?;
            ctx.rep.info(&tr!("Settings saved in {}.", path.display()));
            if restart_service && service::unit_state().active == "active" {
                service::control(ctx, "restart")?;
            }
            ctx.rep.result(json!({"path": path.to_string_lossy()}));
            Ok(0)
        }
    }
}

fn cmd_groups(cli: &Cli) -> Result<i32> {
    if cli.json {
        let list: Vec<Value> =
            GROUPS.iter().map(|g| json!({"id": g.id, "label": g.label(), "paths": g.paths, "caution": g.caution()})).collect();
        outln!("{}", Value::Array(list));
        return Ok(0);
    }
    for g in GROUPS {
        if g.caution().is_empty() {
            outln!("{:9} {}", g.id, g.label());
        } else {
            outln!("{:9} {}  [{}]", g.id, g.label(), g.caution());
        }
        for path in g.paths {
            outln!("            /{path}");
        }
    }
    Ok(0)
}

fn cmd_watch(cli: &Cli, ctx: &Context) -> Result<i32> {
    ensure_root(cli)?;
    let root = ctx.root.clone();
    let user_dir = cli.backup_dir.clone();
    // Les réglages sont relus avant chaque sauvegarde : un nouvel emplacement enregistré par
    // « config set backup_dir=... » s'applique sans redémarrer le service.
    let mut backup_fn = move |c: &Context| -> Result<()> {
        let mut fresh = c.clone();
        if user_dir.is_none() {
            if let Ok(cfg) = crate::config::load_config(&root) {
                if validate(&cfg).is_ok() && check_backup_dir(&cfg.resolved_dir(), c.is_system_root()).is_ok() {
                    fresh.config = cfg;
                }
            }
        }
        create_backup(&fresh, false).map(|_| ())
    };
    service::run_watch(ctx, None, &mut backup_fn, &|state| crate::sys::sd_notify(state))
}

fn cmd_gui() -> Result<i32> {
    let bins = selfinstall::own_binaries().ok();
    let gui = bins.and_then(|b| b.gui).or_else(|| which(crate::config::GUI_NAME));
    let Some(gui) = gui else {
        if selfinstall::Layout::detect().programs_owned_by_dpkg() {
            bail!("The graphical interface is not installed: sudo apt install libvirt-backup-gui");
        }
        bail!(
            "The graphical interface is not installed (libvirt-backup-gui not found). Build it with \"make gui\" (the libgtk-4-dev package is required)."
        );
    };
    let error = Command::new(gui).exec();
    bail!("cannot start the graphical interface: {}", error)
}

// --------------------------------------------------------------------------
// Point d'entrée
// --------------------------------------------------------------------------

/// Choisit la langue avant toute sortie (l'aide elle-même est traduite) :
/// `--language`, puis `LIBVIRT_BACKUP_LANG`, puis le réglage `language`, puis la langue du système.
fn init_language(args: &[OsString]) {
    let mut explicit: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        let text = arg.to_string_lossy();
        if text == "--" {
            break;
        } else if let Some(value) = text.strip_prefix("--language=") {
            explicit = Some(value.to_string());
        } else if text == "--language" {
            explicit = it.next().map(|v| v.to_string_lossy().into_owned());
        } else if let Some(value) = text.strip_prefix("--root=") {
            root = Some(PathBuf::from(value));
        } else if text == "--root" {
            root = it.next().map(PathBuf::from);
        }
    }
    let env = std::env::var(i18n::ENV_LANGUAGE).ok();
    let configured = crate::config::load_config(root.as_deref().unwrap_or(Path::new("/"))).ok().map(|c| c.language);
    let preference = i18n::pick_preference(explicit.as_deref(), env.as_deref(), configured.as_deref());
    i18n::set_language(&preference);
}

/// Traduit les textes visibles (aide, en-têtes, noms de valeurs) de la commande et de ses sous-commandes.
/// Les textes écrits dans le code sont les libellés anglais, clés des tables de `src/locales`.
fn localize(cmd: ClapCommand) -> ClapCommand {
    let template = format!("{{about-with-newline}}\n{} {{usage}}\n\n{{all-args}}{{after-help}}", i18n::tr("Usage:"));
    localize_with(cmd, &template).override_usage(i18n::tr("libvirt-backup [OPTIONS] [COMMAND]"))
}

fn localize_with(mut cmd: ClapCommand, template: &str) -> ClapCommand {
    if let Some(text) = cmd.get_about().map(|s| s.to_string()).and_then(|s| i18n::tr_dynamic(&s)) {
        cmd = cmd.about(text);
    }
    if let Some(text) = cmd.get_subcommand_help_heading().and_then(i18n::tr_dynamic) {
        cmd = cmd.subcommand_help_heading(text);
    }
    if let Some(text) = cmd.get_next_help_heading().and_then(i18n::tr_dynamic) {
        cmd = cmd.next_help_heading(text);
    }
    cmd = cmd.help_template(template.to_string()).mut_args(|mut arg| {
        if let Some(text) = arg.get_help().map(|s| s.to_string()).and_then(|s| i18n::tr_dynamic(&s)) {
            arg = arg.help(text);
        }
        match arg.get_help_heading().map(str::to_string) {
            Some(heading) => {
                if let Some(text) = i18n::tr_dynamic(&heading) {
                    arg = arg.help_heading(text);
                }
            }
            None if arg.is_positional() => arg = arg.help_heading(i18n::tr("Arguments")),
            None => arg = arg.help_heading(i18n::tr("Options")),
        }
        let name = arg.get_value_names().and_then(|names| names.first()).map(|n| n.to_string());
        if let Some(text) = name.and_then(|n| i18n::tr_dynamic(&n)) {
            arg = arg.value_name(text);
        }
        arg
    });
    let template = template.to_string();
    cmd.mut_subcommands(move |sub| localize_with(sub, &template))
}

/// Commande clap avec ses libellés anglais d'origine, avant traduction (test de complétude des tables).
#[doc(hidden)]
pub fn untranslated_command() -> ClapCommand {
    Cli::command()
}

fn parse_cli(args: Vec<OsString>) -> Cli {
    init_language(&args);
    let command = localize(Cli::command()).after_help(tr!("Restorable groups (--only/--skip): {}", group_ids().join(", ")));
    let matches = command.get_matches_from(args);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if let Some(language) = &cli.language {
        if !i18n::is_valid_preference(&language.trim().to_lowercase()) {
            let message = tr!("Invalid language: {0} (choose from: {1}).", language, i18n::PREFERENCES.join(", "));
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
    cli
}

fn dispatch(cli: &Cli, rep: Arc<dyn Reporter>) -> Result<i32> {
    let cmd = cli.command.as_ref().expect("command present");
    if matches!(cmd, Cmd::Gui) {
        return cmd_gui();
    }
    let validate_config = !matches!(cmd, Cmd::Install { .. } | Cmd::Groups | Cmd::Service { .. } | Cmd::Status | Cmd::Config { .. });
    // « install » et « config » enregistrent le dossier (--backup-dir) au lieu de le surcharger.
    let persistent = matches!(cmd, Cmd::Install { .. } | Cmd::Config { .. } | Cmd::Groups | Cmd::Service { .. });
    let override_dir = match (&cli.backup_dir, persistent) {
        (Some(raw), false) => Some(resolve_dir(raw)?),
        _ => None,
    };
    let mut ctx = if matches!(cmd, Cmd::Restore { .. }) {
        Context::create_lenient(rep, cli.root.as_deref(), validate_config, override_dir.as_deref())?
    } else {
        Context::create(rep, cli.root.as_deref(), validate_config, override_dir.as_deref())?
    };
    match cmd {
        Cmd::Backup { if_changed, dry_run, full } => cmd_backup(cli, &ctx, *if_changed, *dry_run, *full),
        Cmd::List => cmd_list(cli, &ctx),
        Cmd::Info { archive } => cmd_info(cli, &ctx, archive.as_deref()),
        Cmd::Verify { archive, all } => cmd_verify(&ctx, archive.as_deref(), *all),
        Cmd::Delete { archives, yes } => cmd_delete(cli, &ctx, archives, *yes),
        Cmd::Restore { archive, latest, only, skip, yes, dry_run } => {
            cmd_restore(cli, &ctx, archive.as_deref(), *latest, only.as_deref(), skip.as_deref(), *yes, *dry_run)
        }
        Cmd::Install { user, retention, no_first_backup, yes } => {
            cmd_install(cli, &mut ctx, user.clone(), *retention, *no_first_backup, *yes)
        }
        Cmd::Uninstall { purge } => {
            ensure_root(cli)?;
            service::uninstall(&ctx, *purge)?;
            Ok(0)
        }
        Cmd::Service { action } => cmd_service(cli, &ctx, *action),
        Cmd::Status => cmd_status(cli, &ctx),
        Cmd::Config { action, pairs, restart_service } => cmd_config(cli, &mut ctx, *action, pairs, *restart_service),
        Cmd::Groups => cmd_groups(cli),
        Cmd::Watch => cmd_watch(cli, &ctx),
        Cmd::Gui => unreachable!("handled above"),
    }
}

/// Exécute la ligne de commande ; renvoie le code de sortie du processus.
pub fn run() -> i32 {
    run_with(std::env::args_os().collect())
}

pub fn run_with(args: Vec<OsString>) -> i32 {
    let cli = parse_cli(args);
    let Some(cmd) = cli.command.as_ref() else {
        let _ = Cli::command().print_help();
        return 2;
    };
    let action_json = cli.json && !QUERY_COMMANDS.contains(&cmd.name());
    let rep: Arc<dyn Reporter> = if action_json { Arc::new(JsonReporter::new()) } else { Arc::new(TextReporter::new()) };
    let code = match dispatch(&cli, rep.clone()) {
        Ok(code) => code,
        Err(e) if e.0 == interrupted_message() => {
            rep.error(&e.0);
            130
        }
        Err(e) => {
            rep.error(&e.0);
            1
        }
    };
    if action_json {
        rep.end(code);
    }
    code
}
