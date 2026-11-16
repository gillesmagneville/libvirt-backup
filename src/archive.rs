//! Lecture, inspection et validation des archives (anciennes et nouvelles).

use crate::tr;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde_json::Value;
use tar::EntryType;

use crate::bail;
use crate::config::{group_of, ARCHIVE_PREFIX, LATEST_LINK, MANIFEST_MEMBER, META_DIRS, META_FILES, SAFETY_PREFIX};
use crate::sys::{system_time_to_local, AppError, DateTime, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Auto,
    Safety,
    External,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Auto => tr!("automatic"),
            Kind::Safety => tr!("pre-restore safety"),
            Kind::External => tr!("external"),
        }
    }
    pub fn json(self) -> &'static str {
        match self {
            Kind::Auto => "auto",
            Kind::Safety => "safety",
            Kind::External => "external",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveInfo {
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub created: DateTime,
    pub sort_key: String,
}

impl ArchiveInfo {
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Décompose `libvirt-config-AAAAMMJJ-HHMMSS[-NNN].tar.gz` (ou `pre-restore-safety-...`).
fn parse_name(name: &str) -> Option<(DateTime, String)> {
    let rest = name.strip_prefix(ARCHIVE_PREFIX).or_else(|| name.strip_prefix(SAFETY_PREFIX))?;
    let rest = rest.strip_suffix(".tar.gz")?;
    let mut parts = rest.split('-');
    let date = parts.next()?;
    let time = parts.next()?;
    let frac = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(date, 8) || !digits(time, 6) {
        return None;
    }
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    let n = |s: &str| s.parse::<u32>().unwrap_or(0);
    let created = DateTime {
        year: date[0..4].parse().unwrap_or(1970),
        month: n(&date[4..6]),
        day: n(&date[6..8]),
        hour: n(&time[0..2]),
        minute: n(&time[2..4]),
        second: n(&time[4..6]),
        nanos: 0,
        offset: None,
    };
    let key = format!("{date}{time}{:0>9}", frac.unwrap_or(""));
    Some((created, key))
}

/// Archives du dossier, de la plus récente à la plus ancienne (liens symboliques ignorés).
pub fn list_archives(dir: &Path) -> Vec<ArchiveInfo> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == LATEST_LINK {
            continue;
        }
        let Some((created, key)) = parse_name(&name) else { continue };
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        if !meta.is_file() {
            continue;
        }
        let kind = if name.starts_with(SAFETY_PREFIX) { Kind::Safety } else { Kind::Auto };
        found.push(ArchiveInfo { path: entry.path(), kind, size: meta.len(), created, sort_key: key });
    }
    found.sort_by(|a, b| (&b.sort_key, b.name()).cmp(&(&a.sort_key, a.name())));
    found
}

/// Désigne une archive : rien/`latest` (la plus récente automatique), un numéro de `list`, un nom de
/// fichier du dossier ou un chemin quelconque (clé USB, copie distante...).
pub fn resolve_archive(dir: &Path, spec: Option<&str>) -> Result<ArchiveInfo> {
    let archives = list_archives(dir);
    let spec = spec.unwrap_or("");
    if spec.is_empty() || spec == "latest" {
        return archives
            .into_iter()
            .find(|a| a.kind == Kind::Auto)
            .ok_or_else(|| AppError(tr!("No {1}*.tar.gz archive found in {0}.", dir.display(), ARCHIVE_PREFIX)));
    }
    if spec.bytes().all(|b| b.is_ascii_digit()) {
        let number: usize = spec.parse().unwrap_or(0);
        if number < 1 || number > archives.len() {
            bail!("Invalid backup number: {1} (1 to {0}).", archives.len(), spec);
        }
        return Ok(archives.into_iter().nth(number - 1).unwrap());
    }
    if let Some(found) = archives.iter().find(|a| a.name() == spec) {
        return Ok(found.clone());
    }
    let candidate = expand_home(spec);
    if candidate.is_file() {
        let meta = fs::metadata(&candidate)?;
        let real = fs::canonicalize(&candidate).unwrap_or(candidate);
        let created = meta.modified().map(system_time_to_local).unwrap_or_else(|_| crate::sys::now_local());
        return Ok(ArchiveInfo { path: real, kind: Kind::External, size: meta.len(), created, sort_key: String::new() });
    }
    bail!("Archive not found: {}", spec)
}

/// Développe un `~/` initial.
pub fn expand_home(spec: &str) -> PathBuf {
    if let Some(rest) = spec.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(spec)
}

#[derive(Debug, Default)]
pub struct ArchiveReport {
    pub path: PathBuf,
    pub entries: u64,
    /// (nom stocké, groupe)
    pub system_members: Vec<(String, &'static str)>,
    pub meta_members: Vec<String>,
    pub unknown: Vec<String>,
    pub dangerous: Vec<String>,
    pub readme: String,
    pub manifest: Value,
}

impl ArchiveReport {
    pub fn valid(&self) -> bool {
        self.unknown.is_empty() && self.dangerous.is_empty()
    }

    pub fn group_count(&self, id: &str) -> usize {
        self.system_members.iter().filter(|(_, g)| *g == id).count()
    }

    pub fn manifest_str(&self, key: &str) -> Option<String> {
        self.manifest.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Class {
    System,
    Meta,
    Ignore,
    Unknown,
    Dangerous,
}

pub fn strip_name(name: &str) -> &str {
    let mut n = name;
    while let Some(rest) = n.strip_prefix("./") {
        n = rest;
    }
    n.trim_end_matches('/')
}

/// Classe un nom de membre d'archive.
pub fn classify(name: &str) -> Class {
    let normalized = strip_name(name);
    if normalized.is_empty() || normalized == "." {
        return Class::Ignore;
    }
    if name.starts_with('/') || normalized.split('/').any(|c| c == "..") || name.contains('\0') {
        return Class::Dangerous;
    }
    if group_of(normalized).is_some() {
        return Class::System;
    }
    if META_FILES.contains(&normalized)
        || META_DIRS.contains(&normalized)
        || META_DIRS.iter().any(|d| normalized.starts_with(&format!("{d}/")))
    {
        return Class::Meta;
    }
    Class::Unknown
}

fn corrupt(path: &Path, why: impl std::fmt::Display) -> AppError {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    AppError(tr!("Archive unreadable or damaged ({0}): {1}", name, why))
}

fn read_limited(entry: &mut impl Read) -> String {
    let mut buf = Vec::new();
    let _ = entry.take(2_000_000).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Parcourt toute l'archive (ce qui contrôle aussi l'intégrité gzip) et classe ses entrées.
pub fn inspect_archive(path: &Path) -> Result<ArchiveReport> {
    inspect_archive_with(path, &mut |_, _| {})
}

/// Lecteur qui compte les octets (compressés) lus dans le fichier : base de la jauge de vérification.
struct CountingReader<R> {
    inner: R,
    count: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<R: io::Read> io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

/// Comme [`inspect_archive`] ; `progress(lus, total)` donne les octets compressés lus et la taille du fichier.
pub fn inspect_archive_with(path: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<ArchiveReport> {
    let file = File::open(path).map_err(|e| corrupt(path, e))?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let count = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let mut report = ArchiveReport { path: path.to_path_buf(), manifest: Value::Object(Default::default()), ..Default::default() };
    let reader = CountingReader { inner: BufReader::new(file), count: count.clone() };
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    {
        let entries = archive.entries().map_err(|e| corrupt(path, e))?;
        for entry in entries {
            let mut entry = entry.map_err(|e| corrupt(path, e))?;
            report.entries += 1;
            progress(count.get(), total);
            let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            let kind_type = entry.header().entry_type();
            let mut class = classify(&name);
            if class == Class::System && matches!(kind_type, EntryType::Char | EntryType::Block | EntryType::Fifo) {
                class = Class::Dangerous;
            }
            if matches!(class, Class::System | Class::Meta) && kind_type == EntryType::Link {
                let target = entry.link_name_bytes().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                if !matches!(classify(&target), Class::System | Class::Meta) {
                    class = Class::Dangerous;
                }
            }
            match class {
                Class::System => {
                    let group = group_of(strip_name(&name)).expect(tr!("classified as system"));
                    report.system_members.push((name, group));
                }
                Class::Meta => {
                    let stripped = strip_name(&name).to_string();
                    if kind_type.is_file() && (stripped == "README.txt" || stripped == MANIFEST_MEMBER) {
                        let data = read_limited(&mut entry);
                        if stripped == "README.txt" {
                            report.readme = data;
                        } else if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&data) {
                            report.manifest = v;
                        }
                    }
                    report.meta_members.push(name);
                }
                Class::Unknown => report.unknown.push(name),
                Class::Dangerous => report.dangerous.push(name),
                Class::Ignore => {}
            }
        }
    }
    // tar s'arrête au marqueur de fin d'archive : lire jusqu'à la fin du flux gzip pour que la somme
    // de contrôle (CRC) soit réellement vérifiée.
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink()).map_err(|e| corrupt(path, e))?;
    Ok(report)
}

/// Manifeste d'une archive, ou `Null` s'il est absent (ancien format) ou illisible.
pub fn read_manifest(path: &Path) -> Value {
    let Ok(file) = File::open(path) else { return Value::Null };
    let mut archive = tar::Archive::new(GzDecoder::new(BufReader::new(file)));
    let Ok(entries) = archive.entries() else { return Value::Null };
    for entry in entries {
        let Ok(mut entry) = entry else { return Value::Null };
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        if strip_name(&name) == MANIFEST_MEMBER && entry.header().entry_type().is_file() {
            return serde_json::from_str(&read_limited(&mut entry)).unwrap_or(Value::Null);
        }
    }
    Value::Null
}
