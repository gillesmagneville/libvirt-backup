//! Création des archives de configuration (équivalent de `backup-libvirt-config`).

use crate::tr_noop;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::archive::{inspect_archive_with, list_archives, read_manifest, Kind};
use crate::bail;
use crate::config::{APP_NAME, ARCHIVE_PREFIX, GROUPS, LATEST_LINK, MANIFEST_MEMBER};
use crate::context::Context;
use crate::report::{busy, Gauge};
use crate::selection::{Selection, MAX_FILE_BYTES, SCRIPT_MAX_BYTES};
use crate::selfinstall;
use crate::sys::{
    capture, hostname, human_duration, human_size, is_root, kernel_release, log, mkdtemp, now_local, par_map, run, safe_name, which,
    Context as _, FileLock, Result, Tag,
};
use crate::tr;
use crate::virsh::Virsh;
use crate::VERSION;

/// Nombre d'appels `virsh` / commandes d'inventaire menés de front.
const WORKERS: usize = 6;

/// Texte du README.txt stocké dans chaque archive, dans la langue active (repères `@VERSION@`, `@DATE@`, `@LEAN@`).
fn readme_text() -> &'static str {
    tr!("Automatic backup of the host virtualization configuration.\nCreated by libvirt-backup @VERSION@ on @DATE@.\n\nContent: libvirt configuration and XML of the domains, networks and pools;\nAppArmor configuration (local profiles and rules), local systemd\nconfiguration and local scripts; usual VFIO settings, kernel modules, udev\nrules, boot, network and ZFS settings when they exist; the libvirt-backup tool\nitself (program, launchers, service, settings).\n\nThe virsh exports distinguish the persistent definition of the domains\n(*.persistent.xml) from their current definition (*.current.xml).\n\nThis archive does NOT contain the virtual disks, the ZFS zvols/datasets,\nthe disk snapshots, or a complete image of the system.\n@LEAN@\nA hardware, NUMA, PCI/IOMMU, kernel and ZFS inventory is added to help\nrebuild the host environment.\n\nRestoring on a blank system (the tool is in the archive, no Python or GTK needed):\n  mkdir /tmp/lb && tar -xzf ARCHIVE -C /tmp/lb usr/local/lib/libvirt-backup\n  sudo /tmp/lb/usr/local/lib/libvirt-backup/libvirt-backup restore ARCHIVE\n")
}

#[derive(Debug, Default)]
pub struct BackupResult {
    pub path: Option<PathBuf>,
    /// `busy` | `unchanged`
    pub skipped: Option<&'static str>,
    pub size: u64,
    pub removed: usize,
    pub warnings: Vec<String>,
    /// Fichiers archivés et fichiers ignorés par le mode allégé.
    pub files: u64,
    pub ignored: usize,
}

// --------------------------------------------------------------------------
// Inventaire de l'hôte
// --------------------------------------------------------------------------

fn iommu_lines(sys_dir: &Path) -> Vec<String> {
    let base = sys_dir.join("kernel/iommu_groups");
    if !base.is_dir() {
        return vec![tr!("No /sys/kernel/iommu_groups directory").into()];
    }
    let mut groups: Vec<PathBuf> = fs::read_dir(&base).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    groups.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match name.parse::<u64>() {
            Ok(n) => (false, n, name),
            Err(_) => (true, 0, name),
        }
    });
    let mut lines = Vec::new();
    for group in groups {
        let devices = group.join("devices");
        if let Ok(rd) = fs::read_dir(&devices) {
            let mut list: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
            list.sort();
            for device in list {
                if let Ok(target) = fs::read_link(&device) {
                    lines.push(format!("{} -> {}", device.display(), target.display()));
                }
            }
        }
    }
    lines
}

/// Un élément d'une section du rapport : une commande, un texte ou des lignes déjà prêtes.
enum Job {
    Run(Vec<&'static str>, u64),
    Text(String),
    Lines(Vec<String>),
}

/// Les commandes d'inventaire sont indépendantes : elles sont lancées de front, la durée totale est
/// celle de la plus lente (souvent `zpool status` ou `lspci`) et non la somme de toutes.
pub fn build_host_report(ctx: &Context) -> String {
    let cmdline = fs::read(ctx.proc_dir.join("cmdline"))
        .map(|b| String::from_utf8_lossy(&b).trim_end_matches('\n').to_string())
        .unwrap_or_else(|e| tr!("(unreadable: {})", e));
    let optional = |tool: &'static str, argv: Vec<&'static str>, timeout: u64| {
        if which(tool).is_some() {
            Job::Run(argv, timeout)
        } else {
            Job::Text(tr!("{} not available", tool))
        }
    };
    let mut cpu = vec![Job::Run(vec!["lscpu"], 30)];
    if which("numactl").is_some() {
        cpu.push(Job::Run(vec!["numactl", "--hardware"], 30));
    }
    let sections: Vec<(&str, Vec<Job>)> = vec![
        (tr!("Kernel and command line"), vec![Job::Run(vec!["uname", "-a"], 30), Job::Text(cmdline)]),
        (tr!("CPU / NUMA"), cpu),
        (tr!("PCI and drivers"), vec![optional("lspci", vec!["lspci", "-nnk"], 30)]),
        (tr!("IOMMU groups"), vec![Job::Lines(iommu_lines(&ctx.sys_dir))]),
        (tr!("Kernel modules"), vec![Job::Run(vec!["lsmod"], 30)]),
        (tr!("Disks and mounts"), vec![Job::Run(vec!["lsblk", "-o", "NAME,TYPE,SIZE,FSTYPE,MOUNTPOINTS"], 30)]),
        (tr!("Network"), vec![Job::Run(vec!["ip", "-brief", "link"], 30), Job::Run(vec!["ip", "-brief", "address"], 30)]),
        (tr!("ZFS pools"), vec![optional("zpool", vec!["zpool", "status"], 60)]),
    ];
    let jobs: Vec<&Job> = sections.iter().flat_map(|(_, jobs)| jobs.iter()).collect();
    let mut results = par_map(jobs, WORKERS, |job| match job {
        Job::Run(argv, timeout) => vec![capture(argv, *timeout)],
        Job::Text(text) => vec![text.clone()],
        Job::Lines(lines) => lines.clone(),
    })
    .into_iter();
    let mut out: Vec<String> = vec![tr!("Date: {}", now_local().iso())];
    for (title, jobs) in &sections {
        out.push(String::new());
        out.push(format!("===== {title} ====="));
        for _ in jobs {
            out.extend(results.next().unwrap_or_default());
        }
    }
    out.join("\n") + "\n"
}

pub fn build_package_list() -> Option<String> {
    which("dpkg-query")?;
    let out = run(
        &["dpkg-query", "-W", "-f", "${binary:Package}\t${Version}\n", "libvirt*", "qemu-system-*", "numad", "swtpm*", "inotify-tools"],
        None,
    );
    Some(format!("{}{}", out.stdout, out.stderr))
}

// --------------------------------------------------------------------------
// Export libvirt
// --------------------------------------------------------------------------

fn unique_names(names: &[String]) -> Vec<(String, String)> {
    let mut used = std::collections::HashSet::new();
    let mut mapping = Vec::new();
    for name in names {
        let base = safe_name(name);
        let mut candidate = base.clone();
        let mut counter = 1;
        while used.contains(&candidate) {
            counter += 1;
            candidate = format!("{base}-{counter}");
        }
        used.insert(candidate.clone());
        mapping.push((name.clone(), candidate));
    }
    mapping
}

/// Une définition à exporter : arguments de `virsh`, dossier et nom du fichier de destination.
struct DumpJob {
    args: Vec<String>,
    folder: PathBuf,
    filename: String,
}

fn dump(virsh: &Virsh, job: &DumpJob) {
    let args: Vec<&str> = job.args.iter().map(String::as_str).collect();
    let (code, out) = virsh.call(&args);
    if code == 0 {
        let _ = fs::write(job.folder.join(&job.filename), out);
    }
}

/// Chaque appel à `virsh` coûte surtout un démarrage de processus et une connexion à libvirtd : avec
/// quelques machines virtuelles, la file d'appels en série prenait plusieurs secondes. Les appels
/// indépendants sont donc menés de front.
fn export_virsh(virsh: &Virsh, stage: &Path, domains: Option<&[String]>) {
    let base = stage.join("virsh-exports");
    for sub in ["domains", "networks", "pools"] {
        let _ = fs::create_dir_all(base.join(sub));
    }
    if !virsh.available() {
        virsh.log(tr!("Domain export unavailable: virsh not found."));
        return;
    }
    let mut jobs: Vec<DumpJob> = Vec::new();
    match domains {
        None => virsh.log(tr!("Domain export unavailable: virsh or libvirtd not reachable.")),
        Some(list) => {
            let _ = fs::write(base.join("domains.list"), list.iter().map(|d| format!("{d}\n")).collect::<String>());
            for (name, safe) in unique_names(list) {
                for (extra, suffix) in [(Some("--inactive"), "persistent"), (None, "current")] {
                    let mut args = vec!["dumpxml".to_string()];
                    args.extend(extra.map(String::from));
                    args.push(name.clone());
                    jobs.push(DumpJob { args, folder: base.join("domains"), filename: format!("{safe}.{suffix}.xml") });
                }
            }
        }
    }
    let kinds = [("net-list", "net-dumpxml", "networks", "networks.list"), ("pool-list", "pool-dumpxml", "pools", "pools.list")];
    let listings = par_map(kinds.to_vec(), 2, |(listing, _, _, _)| virsh.names(&[listing, "--all", "--name"]));
    for ((_, dump_cmd, folder, list_file), names) in kinds.iter().zip(listings) {
        let Some(names) = names else { continue };
        let _ = fs::write(base.join(list_file), names.iter().map(|n| format!("{n}\n")).collect::<String>());
        for (name, safe) in unique_names(&names) {
            jobs.push(DumpJob {
                args: vec![dump_cmd.to_string(), name.clone()],
                folder: base.join(folder),
                filename: format!("{safe}.xml"),
            });
        }
    }
    par_map(jobs, WORKERS, |job| dump(virsh, &job));
}

// --------------------------------------------------------------------------
// Écriture de l'archive
// --------------------------------------------------------------------------

/// Taille d'un enregistrement de tar (20 blocs de 512 octets) : unité de ses points de contrôle.
const TAR_RECORD: u64 = 10240;
/// Préfixe des lignes de progression que tar écrit sur stderr (`--checkpoint-action=echo`).
const PROGRESS_PREFIX: &str = "tar: LBPROGRESS";

/// Crée `dest_tmp` (0600, jamais en suivant un lien) via GNU tar ; renvoie les avertissements.
pub fn write_archive(dest_tmp: &Path, tar_args: &[String], owner: (u32, u32)) -> Result<Vec<String>> {
    write_archive_with(dest_tmp, tar_args, owner, None)
}

/// Comme [`write_archive`] ; `progress` reçoit le nombre d'octets du flux tar déjà produits (avant
/// compression), environ tous les 200 Kio, d'après les points de contrôle de tar.
pub fn write_archive_with(
    dest_tmp: &Path,
    tar_args: &[String],
    owner: (u32, u32),
    mut progress: Option<&mut dyn FnMut(u64)>,
) -> Result<Vec<String>> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dest_tmp)
        .ctx(tr!("creating {}", dest_tmp.display()))?;
    let stdout = Stdio::from(file.try_clone()?);
    let mut command = Command::new("tar");
    command.args(["--acls", "--xattrs", "--numeric-owner", "--ignore-failed-read"]);
    if progress.is_some() {
        command.args(["--checkpoint=20", "--checkpoint-action=echo=LBPROGRESS%u"]);
    }
    command.args(["-czpf", "-"]).args(tar_args).stdin(Stdio::null()).stdout(stdout).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("tar (GNU tar) not found."),
        Err(e) => bail!("tar could not start: {}", e),
    };
    // tar écrit l'archive directement dans le fichier : seul stderr passe par un tube, lu ligne à ligne.
    let mut lines: Vec<String> = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        for raw in std::io::BufRead::split(std::io::BufReader::new(stderr), b'\n').map_while(std::result::Result::ok) {
            let line = String::from_utf8_lossy(&raw).into_owned();
            if let Some(records) = line.strip_prefix(PROGRESS_PREFIX) {
                if let (Some(callback), Ok(records)) = (progress.as_mut(), records.trim().parse::<u64>()) {
                    callback(records.saturating_mul(TAR_RECORD));
                }
            } else if !line.trim().is_empty() {
                lines.push(line);
            }
        }
    }
    let status = child.wait()?;
    let code = status.code().unwrap_or(2);
    if code >= 2 {
        let tail: Vec<&str> = lines.iter().rev().take(5).map(String::as_str).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("tar failed (code {1}): {0}", tail.join(" | "), code);
    }
    file.sync_all().ctx(tr!("syncing the archive"))?;
    let mut warnings = lines;
    if let Err(e) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
        warnings.push(tr!("0600 permissions not applied (file system without permissions?): {}", e));
    }
    if is_root() {
        if let Err(e) = std::os::unix::fs::fchown(&file, Some(owner.0), Some(owner.1)) {
            warnings.push(tr!("owner not applied (file system without owners?): {}", e));
        }
    }
    Ok(warnings)
}

/// Taille cumulée des fichiers d'une arborescence, plus 512 octets par entrée (en-tête tar).
fn tree_bytes(path: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(meta) = fs::symlink_metadata(&current) else { continue };
        total += 512;
        if meta.is_file() {
            total += meta.len().div_ceil(512) * 512;
        } else if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&current) {
                pending.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }
    total
}

pub fn rotate(ctx: &Context, kind: Kind, keep: u32) -> usize {
    if keep == 0 {
        return 0; // 0 = tout conserver
    }
    let mut removed = 0;
    for archive in list_archives(&ctx.backup_dir()).into_iter().filter(|a| a.kind == kind).skip(keep as usize) {
        if fs::remove_file(&archive.path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Crée le dossier des archives (0700, propriétaire = utilisateur choisi).
///
/// Avec `create_parents` à faux, le dossier parent doit exister : une clé USB ou un disque réseau
/// non monté ne doit pas faire écrire silencieusement les archives sur le disque système.
pub fn ensure_backup_dir(ctx: &Context, create_parents: bool) -> Result<(u32, u32)> {
    let (uid, gid) = ctx.owner_ids()?;
    let dir = ctx.backup_dir();
    if !dir.is_dir() {
        let parent_ok = dir.parent().map(Path::is_dir).unwrap_or(false);
        if !create_parents && !parent_ok {
            bail!("the archive folder {} was not found and its parent does not exist (device not mounted?).", dir.display());
        }
        let mut missing = Vec::new();
        let mut cur = dir.clone();
        while !cur.exists() {
            missing.push(cur.clone());
            match cur.parent() {
                Some(p) => cur = p.to_path_buf(),
                None => break,
            }
        }
        for d in missing.iter().rev() {
            fs::create_dir(d).ctx(tr!("creating {}", d.display()))?;
            if is_root() {
                let _ = std::os::unix::fs::chown(d, Some(uid), Some(gid));
            }
            let _ = fs::set_permissions(d, fs::Permissions::from_mode(if *d == dir { 0o700 } else { 0o755 }));
        }
    }
    if let Err(e) = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)) {
        ctx.rep.warn(&tr!("0700 permissions not applied to {0}: {1}", dir.display(), e));
    }
    if is_root() {
        if let Err(e) = std::os::unix::fs::chown(&dir, Some(uid), Some(gid)) {
            ctx.rep.warn(&tr!("owner not applied to {0}: {1}", dir.display(), e));
        }
    }
    Ok((uid, gid))
}

pub fn stamp_now() -> String {
    now_local().stamp()
}

// --------------------------------------------------------------------------
// Mesure des phases
// --------------------------------------------------------------------------

/// Durée de chaque phase : sans elle, une sauvegarde « lente » ne dit pas où elle perd son temps.
struct Phases {
    list: Vec<(&'static str, Duration)>,
    started: Instant,
}

impl Phases {
    fn new() -> Self {
        Phases { list: Vec::new(), started: Instant::now() }
    }

    fn time<T>(&mut self, name: &'static str, f: impl FnOnce() -> T) -> T {
        let begin = Instant::now();
        let value = f();
        self.list.push((name, begin.elapsed()));
        value
    }

    fn summary(&self) -> String {
        let total = human_duration(self.started.elapsed());
        let parts: Vec<String> = self
            .list
            .iter()
            .filter(|(_, d)| d.as_millis() >= 50)
            .map(|(n, d)| format!("{} {}", crate::i18n::tr_dynamic(n).unwrap_or(n), human_duration(*d)))
            .collect();
        if parts.is_empty() {
            tr!("total {}", total)
        } else {
            tr!("{0} — total {1}", parts.join(" · "), total)
        }
    }

    fn json(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (name, duration) in &self.list {
            map.insert((*name).to_string(), json!((duration.as_secs_f64() * 1000.0).round() / 1000.0));
        }
        map.insert("total".into(), json!((self.started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0));
        Value::Object(map)
    }
}

// --------------------------------------------------------------------------
// Plan de sauvegarde : ce qui sera archivé
// --------------------------------------------------------------------------

struct Plan {
    domains: Option<Vec<String>>,
    states: BTreeMap<String, String>,
    include_state: bool,
    note: &'static str,
    include: Vec<String>,
    selection: Selection,
}

fn read_domains(virsh: &Virsh) -> (Option<Vec<String>>, BTreeMap<String, String>) {
    let domains: Option<Vec<String>> = if virsh.available() { virsh.names(&["list", "--all", "--name"]) } else { None };
    let mut states = BTreeMap::new();
    if let Some(list) = &domains {
        for (domain, state) in par_map(list.clone(), WORKERS, |d| {
            let state = virsh.domstate(&d);
            (d, state)
        }) {
            states.insert(domain, state);
        }
    }
    (domains, states)
}

/// NVRAM/TPM ne sont copiés que si tout est réellement éteint.
fn vm_state_policy(domains: &Option<Vec<String>>, states: &BTreeMap<String, String>) -> (bool, &'static str) {
    match domains {
        None => (false, tr!("Cannot check the state of the VMs: UEFI/NVRAM and TPM states omitted.")),
        Some(_) if states.values().all(|s| s == "shut off") => {
            (true, tr!("All VMs are shut off: persistent UEFI/NVRAM and TPM states added if they exist."))
        }
        Some(_) => {
            (false, tr!("At least one VM is running or paused: UEFI/NVRAM and TPM states omitted to avoid an inconsistent live copy."))
        }
    }
}

fn build_plan(ctx: &Context, virsh: &Virsh, layout: selfinstall::Layout, phases: &mut Phases) -> Plan {
    let (domains, states) = phases.time(tr_noop!("VM states"), || read_domains(virsh));
    let (include_state, note) = vm_state_policy(&domains, &states);
    let (include, selection) = phases.time(tr_noop!("content selection"), || {
        let mut include: Vec<String> = Vec::new();
        for group in GROUPS {
            if group.id == "nvram" && !include_state {
                continue;
            }
            for rel in group.paths {
                let path = ctx.root.join(rel);
                if layout == selfinstall::Layout::Package && selfinstall::is_program_path(rel) {
                    continue; // ancienne copie locale éventuelle : la copie de l'archive vient du paquet
                }
                if (path.exists() || fs::symlink_metadata(&path).is_ok()) && !include.iter().any(|i| i == rel) {
                    include.push((*rel).to_string());
                }
            }
        }
        let selection = Selection::scan(&ctx.root, &include, ctx.config.lean_backup);
        (include, selection)
    });
    Plan { domains, states, include_state, note, include, selection }
}

fn skipped_line(item: &crate::selection::Skipped) -> String {
    format!("/{} ({}, {})", item.rel, item.why.label(), human_size(item.size))
}

/// Ce que contient (et ne contient pas) l'archive, en quelques lignes.
fn describe_selection(selection: &Selection, lean: bool) -> Vec<String> {
    let mut lines =
        vec![tr!("Content retained: {0} file(s), {1} before compression.", selection.file_count(), human_size(selection.total_bytes()))];
    if !selection.skipped.is_empty() {
        lines.push(tr!("Skipped (lean backup): {0} file(s), {1}.", selection.skipped.len(), human_size(selection.skipped_bytes())));
        for item in selection.skipped.iter().take(8) {
            lines.push(format!("  - {}", skipped_line(item)));
        }
        if selection.skipped.len() > 8 {
            lines.push(tr!("  - … and {} more", selection.skipped.len() - 8));
        }
        lines.push(tr!("  To archive everything: \"libvirt-backup backup --full\" (or the setting lean_backup=false).").into());
    } else if !lean {
        lines.push(tr!("Full backup: no file is left out.").into());
    }
    lines
}

fn lean_readme(selection: &Selection, lean: bool) -> String {
    if !lean {
        return tr!("Full mode: the whole content of the backed-up paths is archived.\n").into();
    }
    let rules = tr!(
        "compiled executables and files larger than {0} KiB in /usr/local/sbin and /usr/local/bin, files larger than {1} MiB elsewhere",
        SCRIPT_MAX_BYTES >> 10,
        MAX_FILE_BYTES >> 20
    );
    if selection.skipped.is_empty() {
        tr!("Lean mode ({}): no file was left out.\n", rules)
    } else {
        tr!(
            "Lean mode ({2}): {0} file(s), {1}, left out; list in host-inventory/manifest.json (key \"skipped\").\n",
            selection.skipped.len(),
            human_size(selection.skipped_bytes()),
            rules
        )
    }
}

/// Simulation : établit et décrit ce qu'une sauvegarde archiverait, sans rien écrire.
pub fn preview_backup(ctx: &Context) -> Result<Value> {
    let rep = ctx.rep.clone();
    let mut phases = Phases::new();
    let virsh = Virsh::new(None);
    let layout = selfinstall::Layout::detect();
    let plan = build_plan(ctx, &virsh, layout, &mut phases);
    let selection = &plan.selection;
    rep.info(&tr!("Backup simulation — {} mode; nothing is written.", if ctx.config.lean_backup { tr!("lean") } else { tr!("full") }));
    rep.info(&format!("  {:<9} {:>9} {:>11}   {}", tr!("Group"), tr!("Files"), tr!("Size"), tr!("Skipped")));
    for total in selection.by_group() {
        if total.files == 0 && total.skipped == 0 {
            continue;
        }
        let ignored =
            if total.skipped == 0 { "—".to_string() } else { format!("{} ({})", total.skipped, human_size(total.skipped_bytes)) };
        rep.info(&format!("  {:<9} {:>9} {:>11}   {ignored}", total.id, total.files, human_size(total.bytes)));
    }
    rep.info("");
    for line in describe_selection(selection, ctx.config.lean_backup) {
        rep.info(&line);
    }
    let largest = selection.largest(5);
    if !largest.is_empty() {
        rep.info(tr!("Largest files retained:"));
        for item in largest {
            rep.info(&format!("  - /{} ({})", item.rel, human_size(item.size)));
        }
    }
    rep.info(plan.note);
    rep.info(&tr!("Durations: {}", phases.summary()));
    let groups: Vec<Value> = selection
        .by_group()
        .iter()
        .map(|t| json!({"id": t.id, "files": t.files, "bytes": t.bytes, "skipped": t.skipped, "skipped_bytes": t.skipped_bytes}))
        .collect();
    let skipped: Vec<Value> =
        selection.skipped.iter().map(|s| json!({"path": format!("/{}", s.rel), "size": s.size, "why": s.why.json()})).collect();
    let data = json!({
        "dry_run": true, "lean_backup": ctx.config.lean_backup, "files": selection.file_count(), "bytes": selection.total_bytes(),
        "groups": groups, "skipped": skipped, "vm_state_included": plan.include_state, "durations": phases.json(),
    });
    rep.result(data.clone());
    Ok(data)
}

/// Point (ou repointe) le lien « latest » sur `name`, de façon atomique. Message d'avertissement en cas d'échec.
pub fn point_latest_link(ctx: &Context, name: &str, owner: (u32, u32)) -> std::result::Result<(), String> {
    let backup_dir = ctx.backup_dir();
    let latest = backup_dir.join(LATEST_LINK);
    let link_tmp = backup_dir.join(format!(".{LATEST_LINK}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&link_tmp);
    let result = std::os::unix::fs::symlink(name, &link_tmp).and_then(|_| {
        if is_root() {
            let _ = std::os::unix::fs::lchown(&link_tmp, Some(owner.0), Some(owner.1));
        }
        fs::rename(&link_tmp, &latest)
    });
    if let Err(e) = result {
        let _ = fs::remove_file(&link_tmp);
        return Err(tr!("link \"{0}\" not created (file system without symbolic links?): {1}", LATEST_LINK, e));
    }
    Ok(())
}

// --------------------------------------------------------------------------
// Sauvegarde
// --------------------------------------------------------------------------

pub struct UmaskGuard(libc::mode_t);
impl UmaskGuard {
    pub fn set(mask: libc::mode_t) -> Self {
        // SAFETY: appel sans effet de bord mémoire.
        UmaskGuard(unsafe { libc::umask(mask) })
    }
}
impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: idem.
        unsafe { libc::umask(self.0) };
    }
}

struct Cleanup {
    stage: Option<PathBuf>,
    tmp_archive: Option<PathBuf>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(t) = &self.tmp_archive {
            let _ = fs::remove_file(t);
        }
        if let Some(s) = &self.stage {
            let _ = fs::remove_dir_all(s);
        }
    }
}

pub fn create_backup(ctx: &Context, if_changed: bool) -> Result<BackupResult> {
    let rep = ctx.rep.clone();
    let mut phases = Phases::new();
    let (uid, gid) = ensure_backup_dir(ctx, false)?;
    let _umask = UmaskGuard::set(0o077);
    let mut lock = FileLock::new(&ctx.lock_path());
    let mut cleanup = Cleanup { stage: None, tmp_archive: None };
    let mut gauge = Gauge::new(rep.as_ref());

    rep.step(1, 5, tr!("Preparing and locking..."));
    crate::sys::check_interrupted()?;
    if !lock.try_acquire()? {
        log(Tag::Backup, tr!("Backup skipped: a backup is already in progress."));
        rep.warn(tr!("a backup is already in progress; this one is skipped."));
        return Ok(BackupResult { skipped: Some("busy"), ..Default::default() });
    }

    fs::create_dir_all(&ctx.run_dir).ctx(tr!("creating {}", ctx.run_dir.display()))?;
    let stage = mkdtemp(&ctx.run_dir, "libvirt-config-backup.")?;
    cleanup.stage = Some(stage.clone());
    fs::create_dir(stage.join("host-inventory"))?;
    fs::create_dir(stage.join("virsh-exports"))?;
    let virsh = Virsh::new(Some(&stage.join("virsh-exports/virsh-errors.log")));
    gauge.set(0.02);

    // --- état des VM et contenu : NVRAM/TPM copiés seulement si tout est réellement éteint ---
    rep.step(2, 5, tr!("Analysing the state of the virtual machines and the content to archive..."));
    crate::sys::check_interrupted()?;
    let layout = selfinstall::Layout::detect();
    let plan = build_plan(ctx, &virsh, layout, &mut phases);
    let Plan { domains, states, include_state, note, include, selection } = plan;
    for line in describe_selection(&selection, ctx.config.lean_backup) {
        rep.info(&line);
    }
    gauge.set(0.08);

    // Installé par paquet, l'outil vit dans /usr/bin (propriété de dpkg, hors des chemins sauvegardés) :
    // une copie en disposition locale est ajoutée à l'archive pour qu'une machine vierge puisse la restaurer.
    let mut tool_entries: Vec<String> = Vec::new();
    let tool_root = stage.join("tool-root");
    if layout == selfinstall::Layout::Package {
        match selfinstall::own_binaries().and_then(|bins| selfinstall::stage_tool_payload(&tool_root, &bins)) {
            Ok(entries) => tool_entries = entries,
            Err(e) => rep.warn(&tr!("the tool could not be added to the archive: {}", e)),
        }
    }

    let fp = phases.time(tr_noop!("fingerprint"), || selection.fingerprint(&ctx.root));
    gauge.set(0.10);
    if if_changed {
        let last = list_archives(&ctx.backup_dir()).into_iter().find(|a| a.kind == Kind::Auto);
        if let Some(last) = last {
            if read_manifest(&last.path).get("fingerprint").and_then(Value::as_str) == Some(fp.as_str()) {
                rep.info(tr!("Configuration unchanged since the last backup: nothing to do."));
                return Ok(BackupResult { skipped: Some("unchanged"), ..Default::default() });
            }
        }
    }

    // --- inventaire et exports libvirt : trois travaux indépendants, menés de front ---
    phases.time(tr_noop!("libvirt inventory and export"), || {
        busy(rep.as_ref(), tr!("Inventory of the host and export of the libvirt definitions..."), || -> Result<()> {
            let (report, packages) = thread::scope(|scope| {
                let report = scope.spawn(|| build_host_report(ctx));
                let packages = scope.spawn(build_package_list);
                export_virsh(&virsh, &stage, domains.as_deref());
                gauge.set(0.30);
                (report.join().unwrap_or_default(), packages.join().unwrap_or(None))
            });
            fs::write(stage.join("host-inventory/host-report.txt"), report)?;
            if let Some(packages) = packages {
                fs::write(stage.join("host-inventory/virtualization-packages.txt"), packages)?;
            }
            Ok(())
        })
    })?;
    gauge.set(0.45);
    rep.step(3, 5, tr!("libvirt definitions exported."));
    crate::sys::check_interrupted()?;

    let created = now_local();
    let lean = ctx.config.lean_backup;
    let readme =
        readme_text().replace("@VERSION@", VERSION).replace("@DATE@", &created.iso()).replace("@LEAN@", &lean_readme(&selection, lean))
            + "\n"
            + note
            + "\n";
    fs::write(stage.join("README.txt"), readme)?;
    let skipped_json: Vec<Value> =
        selection.skipped.iter().take(200).map(|s| json!({"path": format!("/{}", s.rel), "size": s.size, "why": s.why.json()})).collect();
    let manifest = json!({
        "tool": APP_NAME, "version": VERSION, "created": created.iso(),
        "hostname": hostname(), "kernel": kernel_release(),
        "backup_user": ctx.config.backup_user, "fingerprint": fp, "paths": include, "tool_layout": layout.id(),
        "vm_state_included": include_state, "vm_state_note": note, "domains": states,
        "lean_backup": lean, "files": selection.file_count(), "bytes": selection.total_bytes(),
        "skipped_count": selection.skipped.len(), "skipped_bytes": selection.skipped_bytes(), "skipped": skipped_json,
    });
    // Le manifeste est écrit en tête d'archive : le lire (liste, comparaison d'empreinte) ne demande plus
    // de décompresser toute l'archive. Son nom dans l'archive reste host-inventory/manifest.json.
    let manifest_root = stage.join("manifest-root");
    fs::create_dir_all(manifest_root.join("host-inventory"))?;
    fs::write(manifest_root.join(MANIFEST_MEMBER), serde_json::to_string_pretty(&manifest).unwrap() + "\n")?;
    let list_file = stage.join("tar-files.list");
    fs::write(&list_file, selection.tar_list())?;

    // --- archive : écrite en temporaire, vérifiée, puis renommée atomiquement ---
    rep.step(4, 5, tr!("Creating and verifying the archive..."));
    crate::sys::check_interrupted()?;
    let backup_dir = ctx.backup_dir();
    let name = format!("{ARCHIVE_PREFIX}{}.tar.gz", stamp_now());
    let final_path = backup_dir.join(&name);
    let tmp_archive = backup_dir.join(format!(".{name}.tmp"));
    cleanup.tmp_archive = Some(tmp_archive.clone());
    let abs = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned();
    let mut tar_args: Vec<String> = vec!["-C".into(), abs(&manifest_root), MANIFEST_MEMBER.into()];
    tar_args.extend([
        "-C".into(),
        abs(&ctx.root),
        "--no-recursion".into(),
        "--null".into(),
        "-T".into(),
        abs(&list_file),
        "--recursion".into(),
    ]);
    if !tool_entries.is_empty() {
        tar_args.extend(["-C".into(), abs(&tool_root)]);
        tar_args.extend(tool_entries.iter().cloned());
    }
    tar_args.extend(["-C".into(), abs(&stage), "README.txt".into(), "virsh-exports".into(), "host-inventory".into()]);
    let mut warnings = Vec::new();
    // Taille du flux tar (avant compression) : base de la jauge pendant l'écriture de l'archive.
    let estimate = (selection.total_bytes()
        + 1024 * selection.file_count()
        + tree_bytes(&manifest_root)
        + tree_bytes(&stage.join("virsh-exports"))
        + tree_bytes(&stage.join("host-inventory"))
        + tree_bytes(&stage.join("README.txt"))
        + if tool_entries.is_empty() { 0 } else { tree_bytes(&tool_root) }
        + TAR_RECORD)
        .max(1);
    let written = phases.time(tr_noop!("compression"), || {
        busy(rep.as_ref(), &tr!("Compressing {} of configuration...", human_size(selection.total_bytes())), || {
            let mut on_progress = |done: u64| gauge.set_in(0.45, 0.90, (done as f64 / estimate as f64).min(0.99));
            write_archive_with(&tmp_archive, &tar_args, (uid, gid), Some(&mut on_progress))
        })
    })?;
    warnings.extend(written);
    gauge.set(0.90);
    let report = phases.time(tr_noop!("verification"), || {
        inspect_archive_with(&tmp_archive, &mut |read, total| gauge.set_in(0.90, 0.98, read as f64 / total.max(1) as f64))
    })?;
    gauge.set(0.98);
    // tar ignore (sans le dire) un fichier disparu ou illisible entre le parcours et la lecture : on
    // compare l'archive à la sélection pour le signaler.
    let archived: std::collections::HashSet<&str> = report.system_members.iter().map(|(n, _)| crate::archive::strip_name(n)).collect();
    let missing: Vec<&str> = selection.items.iter().map(|i| i.rel.as_str()).filter(|rel| !archived.contains(rel)).collect();
    if !missing.is_empty() {
        let shown: Vec<String> = missing.iter().take(5).map(|rel| format!("/{rel}")).collect();
        let more = if missing.len() > 5 { tr!(" … and {} more", missing.len() - 5) } else { String::new() };
        warnings.push(tr!(
            "{0} file(s) vanished or became unreadable during the backup and are missing from the archive: {1}{2}",
            missing.len(),
            shown.join(", "),
            more
        ));
    }
    for line in &warnings {
        rep.warn(line);
    }
    if !report.valid() {
        let bad: Vec<&String> = report.unknown.iter().chain(report.dangerous.iter()).take(5).collect();
        rep.warn(&tr!(
            "the archive contains entries that the restore will refuse: {}",
            bad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    fs::rename(&tmp_archive, &final_path).ctx(tr!("renaming to {}", final_path.display()))?;
    cleanup.tmp_archive = None;

    if let Err(msg) = point_latest_link(ctx, &name, (uid, gid)) {
        rep.warn(&msg);
        warnings.push(msg);
    }

    rep.step(5, 5, tr!("Rotating old archives..."));
    crate::sys::check_interrupted()?;
    let removed = rotate(ctx, Kind::Auto, ctx.config.retention);
    gauge.set(1.0);

    let size = fs::metadata(&final_path)?.len();
    let summary = phases.summary();
    log(Tag::Backup, &tr!("Backup created: {}", final_path.display()));
    rep.info(&tr!("Backup created: {0} ({1})", final_path.display(), human_size(size)));
    rep.info(&tr!("Durations: {}", summary));
    rep.result(json!({
        "path": final_path.to_string_lossy(), "size": size, "removed": removed,
        "files": selection.file_count(), "bytes": selection.total_bytes(), "ignored": selection.skipped.len(),
        "durations": phases.json(),
    }));
    drop(lock);
    Ok(BackupResult {
        path: Some(final_path),
        skipped: None,
        size,
        removed,
        warnings,
        files: selection.file_count(),
        ignored: selection.skipped.len(),
    })
}
