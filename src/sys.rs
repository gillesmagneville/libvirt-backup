//! Utilitaires bas niveau : erreurs, sous-processus, verrou, syslog, sd_notify, dates, comptes.

use crate::tr;
use std::ffi::{CStr, CString, OsStr};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

// --------------------------------------------------------------------------
// Erreurs
// --------------------------------------------------------------------------

/// Erreur attendue : message lisible, sans trace d'appels.
#[derive(Debug, Clone, PartialEq)]
pub struct AppError(pub String);

impl AppError {
    pub fn new(message: impl Into<String>) -> Self {
        AppError(message.into())
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AppError {}

impl From<io::Error> for AppError {
    fn from(e: io::Error) -> Self {
        AppError(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

/// `bail!("message {}", x)` : retourne immédiatement une `AppError`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::sys::AppError($crate::tr!($($arg)*).to_string()))
    };
}

/// Ajoute un contexte lisible (chemin, action) à une erreur d'entrée/sortie.
pub trait Context<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T>;
}

impl<T> Context<T> for io::Result<T> {
    fn ctx(self, what: impl fmt::Display) -> Result<T> {
        self.map_err(|e| AppError(tr!("{0}: {1}", what, e)))
    }
}

// --------------------------------------------------------------------------
// Sous-processus
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Cherche un exécutable dans le PATH courant.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Exécute une commande sans jamais lever d'erreur.
///
/// Le code 127 signale une commande introuvable, 124 un dépassement de délai (comme `timeout(1)`).
pub fn run<S: AsRef<OsStr>>(argv: &[S], timeout: Option<Duration>) -> Output {
    run_env(argv, timeout, &[])
}

pub fn run_env<S: AsRef<OsStr>>(argv: &[S], timeout: Option<Duration>, env: &[(&str, &str)]) -> Output {
    let name = argv[0].as_ref().to_string_lossy().into_owned();
    let mut cmd = Command::new(argv[0].as_ref());
    cmd.args(argv[1..].iter().map(|a| a.as_ref())).env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Output { code: 127, stdout: String::new(), stderr: tr!("{}: command not found", name) }
        }
        Err(e) => return Output { code: 126, stdout: String::new(), stderr: format!("{name}: {e}") },
    };
    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if let Some(limit) = timeout {
            if started.elapsed() >= limit {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break None;
            }
        }
        thread::sleep(Duration::from_millis(15));
    };
    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
    let mut stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
    let code = if timed_out {
        stderr = tr!("{1}: timed out ({0}s)", timeout.map(|t| t.as_secs()).unwrap_or(0), name);
        124
    } else {
        use std::os::unix::process::ExitStatusExt;
        status.map(|s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))).unwrap_or(1)
    };
    Output { code, stdout, stderr }
}

/// Sortie standard + erreur d'une commande, sans la dernière fin de ligne ; jamais d'erreur.
pub fn capture<S: AsRef<OsStr>>(argv: &[S], timeout_secs: u64) -> String {
    let out = run(argv, Some(Duration::from_secs(timeout_secs)));
    format!("{}{}", out.stdout, out.stderr).trim_end_matches('\n').to_string()
}

// --------------------------------------------------------------------------
// Journal système
// --------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum Tag {
    Backup,
    Watch,
    Restore,
}

impl Tag {
    fn cstr(self) -> &'static CStr {
        match self {
            Tag::Backup => c"libvirt-config-backup",
            Tag::Watch => c"libvirt-config-watch",
            Tag::Restore => c"libvirt-config-restore",
        }
    }
}

/// Écrit dans le journal (équivalent de `logger -t TAG`) ; sans effet si indisponible.
pub fn log(tag: Tag, message: &str) {
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    // SAFETY: les pointeurs sont valides pendant l'appel ; le format est constant.
    unsafe {
        libc::openlog(tag.cstr().as_ptr(), 0, libc::LOG_DAEMON);
        libc::syslog(libc::LOG_INFO, c"%s".as_ptr(), text.as_ptr());
    }
}

// --------------------------------------------------------------------------
// Verrou et notification systemd
// --------------------------------------------------------------------------

/// Verrou exclusif non bloquant (flock), partagé avec les anciens scripts.
pub struct FileLock {
    path: PathBuf,
    file: Option<File>,
}

impl FileLock {
    pub fn new(path: &Path) -> Self {
        FileLock { path: path.to_path_buf(), file: None }
    }

    pub fn try_acquire(&mut self) -> Result<bool> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).ctx(tr!("creating {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.path)
            .ctx(tr!("opening lock {}", self.path.display()))?;
        use std::os::fd::AsRawFd;
        // SAFETY: descripteur valide pour la durée de l'appel.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Ok(false);
        }
        self.file = Some(file);
        Ok(true)
    }

    pub fn release(&mut self) {
        self.file = None; // la fermeture libère le verrou
    }
}

/// Envoie un message à systemd (READY=1, STATUS=...) ; `false` hors service systemd.
pub fn sd_notify(state: &str) -> bool {
    let Some(addr) = std::env::var_os("NOTIFY_SOCKET") else { return false };
    let addr = addr.to_string_lossy().into_owned();
    if addr.is_empty() {
        return false;
    }
    let Ok(sock) = UnixDatagram::unbound() else { return false };
    let sent = if let Some(name) = addr.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        match std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()) {
            Ok(a) => sock.send_to_addr(state.as_bytes(), &a).is_ok(),
            Err(_) => false,
        }
    } else {
        sock.send_to(state.as_bytes(), &addr).is_ok()
    };
    sent
}

// --------------------------------------------------------------------------
// Fichiers
// --------------------------------------------------------------------------

pub fn is_root() -> bool {
    // SAFETY: appel sans effet de bord.
    unsafe { libc::geteuid() == 0 }
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Écrit un fichier via un temporaire du même dossier puis `rename` atomique.
///
/// Le propriétaire n'est modifié que si le processus est root. Un lien symbolique existant à la
/// destination est remplacé, jamais suivi.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32, owner: Option<(u32, u32)>) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).ctx(tr!("creating {}", parent.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), TMP_COUNTER.fetch_add(1, Ordering::Relaxed)));
    let write = || -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).ctx(tr!("creating {}", tmp.display()))?;
        file.write_all(data).ctx(tr!("writing {}", tmp.display()))?;
        file.sync_all().ctx(tr!("syncing {}", tmp.display()))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).ctx(tr!("file permissions"))?;
        if let (Some((uid, gid)), true) = (owner, is_root()) {
            std::os::unix::fs::chown(&tmp, Some(uid), Some(gid)).ctx(tr!("file owner"))?;
        }
        fs::rename(&tmp, path).ctx(tr!("replacing {}", path.display()))?;
        Ok(())
    };
    let result = write();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Crée un dossier temporaire unique (`mkdtemp`) sous `dir`.
pub fn mkdtemp(dir: &Path, prefix: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir).ctx(tr!("creating {}", dir.display()))?;
    let template = dir.join(format!("{prefix}XXXXXX"));
    let c = CString::new(template.as_os_str().as_encoded_bytes().to_vec()).map_err(|_| AppError::new(tr!("invalid path")))?;
    let raw = c.into_raw();
    // SAFETY: `raw` est un tampon modifiable terminé par NUL, repris ensuite par CString::from_raw.
    let res = unsafe { libc::mkdtemp(raw) };
    let owned = unsafe { CString::from_raw(raw) };
    if res.is_null() {
        return Err(AppError(tr!("creating a temporary folder in {0}: {1}", dir.display(), io::Error::last_os_error())));
    }
    use std::os::unix::ffi::OsStrExt;
    Ok(PathBuf::from(OsStr::from_bytes(owned.to_bytes())))
}

pub fn human_size(size: u64) -> String {
    let mut value = size as f64;
    // Le français garde « o, Ko, Mo, Go » (octets) ; les autres langues, « B, KB, MB, GB ».
    let units = if crate::i18n::current() == crate::i18n::Lang::Fr { ["o", "Ko", "Mo", "Go"] } else { ["B", "KB", "MB", "GB"] };
    for unit in units {
        if value < 1024.0 || unit == units[3] {
            return if unit == units[0] { format!("{value:.0} {unit}") } else { format!("{value:.1} {unit}") };
        }
        value /= 1024.0;
    }
    unreachable!()
}

/// Durée lisible : « 340 ms », « 2,4 s », « 1 min 05 s ».
pub fn human_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{} ms", d.as_millis())
    } else if secs < 60.0 {
        let text = format!("{secs:.1} s");
        // Virgule décimale en français, allemand et espagnol ; point en anglais.
        if crate::i18n::current() == crate::i18n::Lang::En {
            text
        } else {
            text.replace('.', ",")
        }
    } else {
        format!("{} min {:02} s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

/// Applique `f` à chaque élément avec au plus `workers` fils d'exécution ; l'ordre des résultats est
/// celui des éléments. Sert aux appels indépendants à `virsh` et aux commandes d'inventaire, dont la
/// durée est presque entièrement de l'attente (démarrage du processus, connexion à libvirtd).
pub fn par_map<T: Send, R: Send>(items: Vec<T>, workers: usize, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let count = items.len();
    if count == 0 {
        return Vec::new();
    }
    let workers = workers.clamp(1, count);
    if workers == 1 {
        return items.into_iter().map(f).collect();
    }
    let queue = std::sync::Mutex::new(items.into_iter().enumerate());
    let results: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..count).map(|_| None).collect());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let next = queue.lock().unwrap().next();
                let Some((index, item)) = next else { break };
                let value = f(item);
                results.lock().unwrap()[index] = Some(value);
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("result produced")).collect()
}

/// Nom de fichier sûr dérivé d'un nom de VM/réseau/pool (comme le `sed` d'origine).
pub fn safe_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' { c } else { '_' }).collect();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Même disque (même périphérique) que `/` ?
pub fn same_device_as_root(path: &Path) -> bool {
    let mut probe = path;
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p,
            None => return false,
        }
    }
    match (fs::metadata(probe), fs::metadata("/")) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

// --------------------------------------------------------------------------
// Comptes
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UserEntry {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

fn passwd_to_entry(pw: &libc::passwd) -> UserEntry {
    // SAFETY: les pointeurs de `passwd` renvoyés par getpw*_r sont valides tant que le tampon vit.
    let (name, home) =
        unsafe { (CStr::from_ptr(pw.pw_name).to_string_lossy().into_owned(), CStr::from_ptr(pw.pw_dir).to_string_lossy().into_owned()) };
    UserEntry { name, uid: pw.pw_uid, gid: pw.pw_gid, home: PathBuf::from(home) }
}

pub fn user_by_name(name: &str) -> Option<UserEntry> {
    let c = CString::new(name).ok()?;
    let mut buf = vec![0u8; 16384];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: tampons valides et correctement dimensionnés.
    let rc = unsafe { libc::getpwnam_r(c.as_ptr(), &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| passwd_to_entry(&pwd))
}

pub fn user_by_uid(uid: u32) -> Option<UserEntry> {
    let mut buf = vec![0u8; 16384];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: idem.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then(|| passwd_to_entry(&pwd))
}

pub fn current_uid() -> u32 {
    // SAFETY: appel sans effet de bord.
    unsafe { libc::getuid() }
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: tampon valide ; gethostname termine par NUL si la taille suffit.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

pub fn kernel_release() -> String {
    // SAFETY: structure remplie par uname.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return String::new();
        }
        CStr::from_ptr(u.release.as_ptr()).to_string_lossy().into_owned()
    }
}

// --------------------------------------------------------------------------
// Dates
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub nanos: u32,
    /// Décalage par rapport à UTC en secondes (None pour une date lue dans un nom de fichier).
    pub offset: Option<i32>,
}

impl DateTime {
    pub fn from_tm(tm: &libc::tm, nanos: u32, with_offset: bool) -> Self {
        DateTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
            nanos,
            offset: with_offset.then_some(tm.tm_gmtoff as i32),
        }
    }

    /// `2026-11-16T16:42:11+01:00` (sans décalage si inconnu).
    pub fn iso(&self) -> String {
        let base = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second);
        match self.offset {
            Some(off) => {
                let sign = if off < 0 { '-' } else { '+' };
                let a = off.abs();
                format!("{base}{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60)
            }
            None => base,
        }
    }

    /// `AAAAmmjj-HHMMSS-NNNNNNNNN` : horodatage des noms d'archives.
    pub fn stamp(&self) -> String {
        format!("{:04}{:02}{:02}-{:02}{:02}{:02}-{:09}", self.year, self.month, self.day, self.hour, self.minute, self.second, self.nanos)
    }

    /// `2026-11-16 16:42:11`
    pub fn display(&self) -> String {
        format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second)
    }

    /// Date et heure dans la langue active : `16/11/2026 à 16:42:11` (fr), `2026-11-16 16:42:11` (en),
    /// `16.11.2026 um 16:42:11` (de), `16/11/2026 a las 16:42:11` (es).
    pub fn display_local(&self) -> String {
        use crate::i18n::Lang;
        let (d, m, y, hh, mm, ss) = (self.day, self.month, self.year, self.hour, self.minute, self.second);
        match crate::i18n::current() {
            Lang::Fr => format!("{d:02}/{m:02}/{y:04} à {hh:02}:{mm:02}:{ss:02}"),
            Lang::En => self.display(),
            Lang::De => format!("{d:02}.{m:02}.{y:04} um {hh:02}:{mm:02}:{ss:02}"),
            Lang::Es => format!("{d:02}/{m:02}/{y:04} a las {hh:02}:{mm:02}:{ss:02}"),
        }
    }
}

pub fn local_from_epoch(secs: i64, nanos: u32) -> DateTime {
    // SAFETY: localtime_r écrit dans la structure fournie.
    unsafe {
        let t: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        DateTime::from_tm(&tm, nanos, true)
    }
}

pub fn now_local() -> DateTime {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    local_from_epoch(now.as_secs() as i64, now.subsec_nanos())
}

pub fn system_time_to_local(t: std::time::SystemTime) -> DateTime {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    local_from_epoch(d.as_secs() as i64, d.subsec_nanos())
}

// --------------------------------------------------------------------------
// Interruption (Ctrl+C, SIGTERM, SIGHUP) pendant une opération longue
// --------------------------------------------------------------------------

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// Message de l'erreur produite par une interruption (comparé pour choisir le code de sortie 130).
pub fn interrupted_message() -> &'static str {
    tr!("operation interrupted.")
}

extern "C" fn on_interrupt(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Pendant sa vie, SIGINT/SIGTERM/SIGHUP lèvent un drapeau au lieu de tuer le processus : l'opération
/// s'arrête au prochain point de contrôle et les nettoyages (verrou, temporaires, service de
/// surveillance) s'exécutent, comme avec le `trap` des anciens scripts.
pub struct InterruptGuard {
    old: Vec<(libc::c_int, libc::sigaction)>,
}

impl InterruptGuard {
    pub fn new() -> Self {
        INTERRUPTED.store(false, Ordering::SeqCst);
        let mut old = Vec::new();
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: structures initialisées à zéro puis remplies par sigaction ; le gestionnaire ne fait
            // qu'écrire un booléen atomique.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = on_interrupt as extern "C" fn(libc::c_int) as usize;
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, &action, &mut previous) == 0 {
                    old.push((sig, previous));
                }
            }
        }
        InterruptGuard { old }
    }
}

impl Default for InterruptGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        for (sig, previous) in &self.old {
            // SAFETY: restauration de l'action précédemment lue.
            unsafe { libc::sigaction(*sig, previous, std::ptr::null_mut()) };
        }
    }
}

/// Erreur si une interruption a été demandée depuis la création de l'[`InterruptGuard`].
pub fn check_interrupted() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err(AppError::new(interrupted_message()));
    }
    Ok(())
}
