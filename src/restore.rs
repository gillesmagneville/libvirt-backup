//! Restauration prudente par fusion (équivalent de `restore-libvirt-backup.sh`).
//!
//! Rien n'est supprimé : les fichiers actuels absents de l'archive sont conservés. Une archive de
//! sécurité de l'état courant est créée avant toute modification.

use crate::tr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::archive::{inspect_archive, strip_name, ArchiveReport, Kind};
use crate::backup::{ensure_backup_dir, rotate, stamp_now, write_archive, UmaskGuard};
use crate::bail;
use crate::config::{group_by_id, group_ids, APPARMOR_PROFILES, GROUPS, LIBVIRT_UNITS, SAFETY_PREFIX, SERVICE_NAME};
use crate::context::Context;
use crate::report::busy;
use crate::selfinstall;
use crate::service::{systemctl, unit_exists};
use crate::sys::{human_size, log, mkdtemp, run, which, Context as _, FileLock, Result, Tag};
use crate::virsh::{DefineKind, Virsh};

#[derive(Debug)]
pub struct RestorePlan {
    pub archive: PathBuf,
    pub report: ArchiveReport,
    pub groups: Vec<&'static str>,
    /// Noms tels que stockés dans l'archive, à extraire.
    pub members: Vec<String>,
    pub per_group: Vec<(&'static str, usize)>,
    pub skipped_groups: Vec<(&'static str, usize)>,
    pub notes: Vec<String>,
}

impl RestorePlan {
    pub fn defines_libvirt(&self) -> bool {
        self.groups.contains(&"libvirt")
    }
}

/// Traduit `--only` / `--skip` (listes séparées par des virgules) en identifiants de groupes.
pub fn parse_groups(only: Option<&str>, skip: Option<&str>) -> Result<Vec<&'static str>> {
    let split = |value: Option<&str>| -> Result<Vec<&'static str>> {
        let mut out = Vec::new();
        for item in value.unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match group_by_id(item) {
                Some(g) => out.push(g.id),
                None => bail!("Unknown group: {1} (groups: {0})", group_ids().join(", "), item),
            }
        }
        Ok(out)
    };
    let only = split(only)?;
    let selected = if only.is_empty() { group_ids() } else { only };
    let excluded = split(skip)?;
    let result: Vec<&'static str> = group_ids().into_iter().filter(|g| selected.contains(g) && !excluded.contains(g)).collect();
    if result.is_empty() {
        bail!("No group left to restore after applying --only/--skip.");
    }
    Ok(result)
}

pub fn plan_restore(archive: &Path, groups: Option<&[&str]>) -> Result<RestorePlan> {
    let report = inspect_archive(archive)?;
    if !report.dangerous.is_empty() {
        let shown: Vec<&str> = report.dangerous.iter().take(5).map(String::as_str).collect();
        bail!("Dangerous path in the archive: {}\nNothing was extracted.", shown.join(", "));
    }
    if !report.unknown.is_empty() {
        let shown: Vec<&str> = report.unknown.iter().take(5).map(String::as_str).collect();
        bail!(
            "Unrecognised entry in the archive: {}\nThis archive does not match the expected format; nothing was extracted.",
            shown.join(", ")
        );
    }
    let selected: Vec<&str> = match groups {
        Some(g) if !g.is_empty() => g.to_vec(),
        _ => group_ids(),
    };
    if report.system_members.is_empty() {
        bail!("The archive contains no recognised system path.");
    }
    // Installé par paquet, l'outil ne laisse pas une archive réécrire ses programmes : dpkg en est le
    // propriétaire. Les réglages (/etc/libvirt-backup) et le reste de la configuration sont restaurés.
    let by_package = selfinstall::Layout::detect().programs_owned_by_dpkg();
    let mut members: Vec<String> = Vec::new();
    let mut counts: Vec<(&'static str, usize)> = Vec::new();
    let mut ignored_programs = 0;
    for (name, group) in report.system_members.iter().filter(|(_, g)| selected.contains(g)) {
        if by_package && selfinstall::is_program_path(strip_name(name)) {
            ignored_programs += 1;
            continue;
        }
        if !members.contains(name) {
            members.push(name.clone());
            match counts.iter_mut().find(|(g, _)| g == group) {
                Some((_, n)) => *n += 1,
                None => counts.push((group, 1)),
            }
        }
    }
    if members.is_empty() {
        bail!("The archive contains nothing for the selected groups.");
    }
    let ordered: Vec<&'static str> = group_ids().into_iter().filter(|g| selected.contains(g)).collect();
    let per_group = ordered.iter().filter_map(|g| counts.iter().find(|(c, _)| c == g).copied()).collect();
    let skipped_groups =
        group_ids().into_iter().filter(|g| !selected.contains(g)).map(|g| (g, report.group_count(g))).filter(|(_, n)| *n > 0).collect();
    let mut notes = Vec::new();
    if ignored_programs > 0 {
        notes.push(tr!(
            "the program is provided by a dpkg package: {} program file(s) in the archive are ignored (only the settings are restored).",
            ignored_programs
        ));
    }
    let manifest_present = report.manifest.as_object().map(|m| !m.is_empty()).unwrap_or(false);
    let omitted = if manifest_present {
        report.manifest.get("vm_state_included") == Some(&Value::Bool(false))
    } else {
        report.readme.contains("NVRAM et TPM omis")
    };
    if omitted {
        notes.push(
            tr!("this archive states that the UEFI/NVRAM and TPM states were omitted (VMs were running at backup time).").to_string(),
        );
    }
    Ok(RestorePlan { archive: archive.to_path_buf(), report, groups: ordered, members, per_group, skipped_groups, notes })
}

// --------------------------------------------------------------------------
// Contrôles préalables
// --------------------------------------------------------------------------

pub fn qemu_processes(proc_dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(proc_dir) else { return found };
    for entry in entries.flatten() {
        let pid = entry.file_name().to_string_lossy().into_owned();
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(raw) = fs::read(entry.path().join("cmdline")) else { continue };
        let args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        let Some(first) = args.first() else { continue };
        let base = first.rsplit('/').next().unwrap_or(first);
        if base.starts_with("qemu-system-") || base.starts_with("qemu-kvm") {
            let line: String = format!("{pid} {}", args.join(" ")).chars().take(160).collect();
            found.push(line);
        }
    }
    found.sort();
    found
}

/// (erreurs bloquantes, avertissements).
pub fn check_vms_stopped(ctx: &Context) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let procs = qemu_processes(&ctx.proc_dir);
    if !procs.is_empty() {
        errors.push(tr!("at least one QEMU process is still running:\n  {}", procs.join("\n  ")));
    }
    let virsh = Virsh::new(None);
    if virsh.available() {
        match virsh.names(&["list", "--all", "--name"]) {
            None => warnings.push(tr!("virsh cannot reach qemu:///system; QEMU processes were checked instead.").to_string()),
            Some(names) => {
                for name in names {
                    let state = virsh.domstate(&name);
                    if state != "shut off" {
                        let shown = if state.is_empty() { "inconnu" } else { state.as_str() };
                        errors.push(tr!("the VM '{0}' is not shut off (state: {1}).", name, shown));
                    }
                }
            }
        }
    }
    (errors, warnings)
}

// --------------------------------------------------------------------------
// Restauration
// --------------------------------------------------------------------------

fn tar_extract(archive: &Path, directory: &Path, names: &[String], tmp: &Path, system: bool) -> Result<()> {
    let list_file = tmp.join(if system { "restore-files.list" } else { "metadata-files.list" });
    let mut data = Vec::new();
    for n in names {
        data.extend_from_slice(n.as_bytes());
        data.push(0);
    }
    fs::write(&list_file, data).ctx(tr!("writing {}", list_file.display()))?;
    let mut cmd = Command::new("tar");
    // --no-recursion : seuls les membres nommés sont extraits. Sans lui, GNU tar traite le nom d'un
    // dossier comme un préfixe et répond « Not found in archive » (code 2) pour chaque fichier nommé ensuite.
    cmd.arg("--extract").arg("--gzip").arg("--file").arg(archive).arg("--directory").arg(directory).arg("--no-recursion");
    if system {
        cmd.args(["--same-owner", "--same-permissions", "--acls", "--xattrs", "--numeric-owner", "--delay-directory-restore"]);
    } else {
        cmd.arg("--no-same-owner");
    }
    cmd.args(["--null", "--files-from"]).arg(&list_file);
    let output = match cmd.stdin(std::process::Stdio::null()).output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("tar (GNU tar) not found."),
        Err(e) => bail!("tar could not start: {}", e),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.trim().lines().collect();
        let tail = lines[lines.len().saturating_sub(4)..].join(" | ");
        bail!("the extraction failed (code {0}): {1}\nThe safety backup is kept.", output.status.code().unwrap_or(-1), tail);
    }
    Ok(())
}

fn make_safety_archive(ctx: &Context, plan: &RestorePlan) -> Result<Option<PathBuf>> {
    let (uid, gid) = ensure_backup_dir(ctx, false)?;
    let mut current: Vec<String> = Vec::new();
    for id in &plan.groups {
        let Some(group) = group_by_id(id) else { continue };
        for rel in group.paths {
            let path = ctx.root.join(rel);
            if (path.exists() || fs::symlink_metadata(&path).is_ok()) && !current.iter().any(|c| c == rel) {
                current.push((*rel).to_string());
            }
        }
    }
    if current.is_empty() {
        ctx.rep.info(tr!("No current configuration file to back up."));
        return Ok(None);
    }
    let name = format!("{SAFETY_PREFIX}{}.tar.gz", stamp_now());
    let dir = ctx.backup_dir();
    let final_path = dir.join(&name);
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut args = vec!["-C".to_string(), ctx.root.to_string_lossy().into_owned()];
    args.extend(current);
    let result = write_archive(&tmp, &args, (uid, gid));
    let finished = match result {
        Ok(warnings) => {
            for line in warnings {
                ctx.rep.warn(&line);
            }
            fs::rename(&tmp, &final_path).ctx(tr!("creating {}", final_path.display()))
        }
        Err(e) => Err(e),
    };
    let _ = fs::remove_file(&tmp);
    finished?;
    rotate(ctx, Kind::Safety, ctx.config.safety_retention);
    Ok(Some(final_path))
}

fn libvirt_unit() -> Option<&'static str> {
    LIBVIRT_UNITS.iter().copied().find(|unit| systemctl(&["cat", unit], 20).code == 0)
}

fn define_from_exports(ctx: &Context, metadata: &Path) -> usize {
    let rep = &ctx.rep;
    let virsh = Virsh::new(None);
    if !virsh.reachable(20, Duration::from_secs(1)) {
        rep.warn(tr!(
            "libvirt is not reachable; the exported XML files remain available in the archive and the libvirt files were restored."
        ));
        return 0;
    }
    let mut failures = 0;
    let base = metadata.join("virsh-exports");
    for (kind, sub, suffix, label) in [
        (DefineKind::Network, "networks", ".xml", tr!("Network defined")),
        (DefineKind::Pool, "pools", ".xml", tr!("Pool defined")),
        (DefineKind::Domain, "domains", ".persistent.xml", tr!("Domain defined")),
    ] {
        let mut files: Vec<PathBuf> = fs::read_dir(base.join(sub)).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        files.sort();
        for xml in files {
            let file_name = xml.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some(name) = file_name.strip_suffix(suffix) else { continue };
            if virsh.define(kind, &xml) {
                rep.info(&tr!("{0}: {1}", label, name));
            } else {
                rep.warn(&tr!("cannot define \"{0}\" from {1}", name, file_name));
                failures += 1;
            }
        }
    }
    failures
}

struct RestoreCleanup<'a> {
    ctx: &'a Context,
    tmp: PathBuf,
    watcher_stopped: bool,
    lock: FileLock,
}

impl Drop for RestoreCleanup<'_> {
    fn drop(&mut self) {
        if self.watcher_stopped {
            self.ctx.rep.warn(tr!("re-enabling the watcher service..."));
            systemctl(&["daemon-reload"], 60);
            if unit_exists() {
                systemctl(&["restart", SERVICE_NAME], 300);
            }
        }
        let _ = fs::remove_dir_all(&self.tmp);
        self.lock.release();
    }
}

pub fn run_restore(ctx: &Context, plan: &RestorePlan, dry_run: bool) -> Result<Value> {
    let rep = ctx.rep.clone();
    let report = &plan.report;
    rep.step(1, 7, &tr!("Checking the archive: {0} entries, {1} to restore.", report.entries, plan.members.len()));
    crate::sys::check_interrupted()?;
    for group in GROUPS {
        if let Some((_, n)) = plan.per_group.iter().find(|(g, _)| *g == group.id) {
            rep.info(&tr!("  - {0}: {1} item(s)", group.label(), n));
        }
    }
    if !plan.skipped_groups.is_empty() {
        let labels: Vec<&str> = plan.skipped_groups.iter().filter_map(|(g, _)| group_by_id(g)).map(|g| g.label()).collect();
        rep.info(&tr!("  Not selected: {}", labels.join(", ")));
    }
    for note in &plan.notes {
        rep.warn(note);
    }

    if dry_run {
        let (errors, warnings) = check_vms_stopped(ctx);
        for text in warnings.iter().chain(errors.iter()) {
            rep.warn(text);
        }
        rep.info(tr!("Simulation finished: no change was made."));
        rep.result(json!({"dry_run": true, "members": plan.members.len(), "groups": plan.groups, "vm_blockers": errors.len()}));
        return Ok(json!({"dry_run": true}));
    }

    // AVANT l'extraction : le programme en cours d'exécution peut être remplacé par celui de l'archive.
    // Installé par paquet, il appartient à dpkg et n'est ni lu ni réécrit.
    let layout = selfinstall::Layout::detect();
    let payload = match layout {
        selfinstall::Layout::Local => Some(selfinstall::build_payload(&selfinstall::own_binaries()?)?),
        selfinstall::Layout::Package => None,
    };
    fs::create_dir_all(&ctx.run_dir).ctx(tr!("creating {}", ctx.run_dir.display()))?;
    let tmp = mkdtemp(&ctx.run_dir, "libvirt-config-restore.")?;
    let _umask = UmaskGuard::set(0o077);
    let mut cleanup = RestoreCleanup { ctx, tmp: tmp.clone(), watcher_stopped: false, lock: FileLock::new(&ctx.lock_path()) };

    if !cleanup.lock.try_acquire()? {
        bail!("an automatic backup is currently running. Try again when it has finished.");
    }

    rep.step(2, 7, tr!("Checking that the virtual machines are shut off..."));
    crate::sys::check_interrupted()?;
    let (errors, warnings) = check_vms_stopped(ctx);
    for text in &warnings {
        rep.warn(text);
    }
    if !errors.is_empty() {
        bail!("{}\nShut down all the VMs completely before restoring their configuration.", errors.join("\n"));
    }

    rep.step(3, 7, tr!("Creating a safety backup of the current state..."));
    crate::sys::check_interrupted()?;
    let safety = make_safety_archive(ctx, plan)?;
    if let Some(s) = &safety {
        let size = fs::metadata(s).map(|m| m.len()).unwrap_or(0);
        rep.info(&tr!("Safety backup: {0} ({1})", s.display(), human_size(size)));
    }
    let metadata = tmp.join("metadata");
    fs::create_dir(&metadata)?;
    let meta_names: Vec<String> = report.meta_members.iter().filter(|n| strip_name(n).starts_with("virsh-exports")).cloned().collect();
    if plan.defines_libvirt() && !meta_names.is_empty() {
        tar_extract(&plan.archive, &metadata, &meta_names, &tmp, false)?;
    }

    rep.step(4, 7, tr!("Temporarily stopping the automatic watcher..."));
    crate::sys::check_interrupted()?;
    if systemctl(&["is-active", "--quiet", SERVICE_NAME], 20).code == 0 {
        systemctl(&["stop", SERVICE_NAME], 120);
        cleanup.watcher_stopped = true;
    }

    rep.step(5, 7, tr!("Restoring the configuration files..."));
    crate::sys::check_interrupted()?;
    busy(rep.as_ref(), tr!("Extracting the archive..."), || tar_extract(&plan.archive, &ctx.root, &plan.members, &tmp, true))?;
    // L'outil en cours d'exécution est réinstallé par-dessus la copie de l'archive (éventuellement plus
    // ancienne), comme le faisait l'ancien script pour lui-même.
    if let Some(payload) = &payload {
        selfinstall::write_payload(&ctx.root, payload)?;
    }
    // L'unité restaurée doit appeler les programmes réellement présents sur cette machine.
    if selfinstall::align_unit(&ctx.root, layout)? {
        rep.info(tr!("Watcher service unit adapted to this machine's installation."));
    }

    rep.step(6, 7, tr!("Reloading the AppArmor policies and systemd..."));
    crate::sys::check_interrupted()?;
    if which("apparmor_parser").is_some() {
        for profile in APPARMOR_PROFILES {
            let path = ctx.root.join("etc/apparmor.d").join(profile);
            if path.is_file() {
                if run(&["apparmor_parser", "-r", &path.to_string_lossy()], Some(Duration::from_secs(120))).code == 0 {
                    rep.info(&tr!("AppArmor profile {} reloaded.", profile));
                } else {
                    rep.warn(&tr!("failed to reload the AppArmor profile {}.", profile));
                }
            }
        }
    }
    systemctl(&["daemon-reload"], 60);
    let unit = libvirt_unit();
    match unit {
        Some(u) => {
            rep.info(&tr!("Reloading the service {}...", u));
            let restart = systemctl(&["is-active", "--quiet", u], 20).code == 0;
            if systemctl(&[if restart { "restart" } else { "start" }, u], 180).code != 0 {
                rep.warn(&if restart { tr!("could not restart {}.", u) } else { tr!("could not start {}.", u) });
            }
        }
        None => rep
            .warn(tr!("no libvirtd/virtqemud service found; the XML definitions will be imported once libvirt is installed and started.")),
    }
    let mut failures = 0;
    if plan.defines_libvirt() && unit.is_some() && !meta_names.is_empty() {
        failures = define_from_exports(ctx, &metadata);
    }
    if systemctl(&["cat", "numad.service"], 20).code == 0 && systemctl(&["is-active", "--quiet", "numad.service"], 20).code == 0 {
        rep.info(tr!("Restarting numad to apply its restored configuration..."));
        if systemctl(&["restart", "numad.service"], 60).code != 0 {
            rep.warn(tr!("numad could not be restarted."));
        }
    }

    rep.step(7, 7, tr!("Re-enabling the automatic watcher..."));
    crate::sys::check_interrupted()?;
    if unit_exists() {
        systemctl(&["enable", SERVICE_NAME], 60);
        if systemctl(&["restart", SERVICE_NAME], 300).code == 0 {
            cleanup.watcher_stopped = false;
            rep.info(tr!("Watcher active."));
        } else {
            rep.warn(tr!("the restore is done, but the watcher service did not restart."));
        }
        if !selfinstall::unit_uses_this_tool(&ctx.root) {
            rep.warn(tr!("the restored service is the one from the old shell scripts: run \"libvirt-backup install\" to replace it with this tool's."));
        }
    } else {
        cleanup.watcher_stopped = false;
        rep.warn(tr!("the watcher service does not exist in this restore; run \"libvirt-backup install\" to create it."));
    }
    drop(cleanup);

    log(Tag::Restore, &tr!("Restore finished from: {}", plan.archive.display()));
    rep.info("");
    rep.info(&tr!("Restore finished from: {}", plan.archive.display()));
    if let Some(s) = &safety {
        rep.info(&tr!("Safety backup of the previous state: {}", s.display()));
    }
    if failures > 0 {
        rep.warn(&tr!("{} virsh XML definition(s) could not be re-imported.", failures));
    }
    rep.info(tr!("The VMs were not started. Virtual disks and ZFS datasets/zvols were not restored."));
    rep.info(tr!("Files that are not in the archive were not deleted."));
    if plan.groups.contains(&"boot") {
        rep.info(tr!("Check the GRUB/initramfs settings and run update-grub or update-initramfs if needed before rebooting the host."));
    }
    let safety_str = safety.as_ref().map(|p| p.to_string_lossy().into_owned());
    rep.result(json!({"archive": plan.archive.to_string_lossy(), "safety": safety_str, "define_failures": failures}));
    Ok(json!({"archive": plan.archive.to_string_lossy(), "safety": safety_str, "define_failures": failures}))
}
