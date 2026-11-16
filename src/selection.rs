//! Sélection explicite du contenu d'une archive.
//!
//! L'ancien script confiait à `tar` le parcours récursif des chemins : tout ce qui s'y trouvait était
//! lu, comprimé puis relu pour contrôle. Un dossier de scripts qui contient quelques binaires
//! compilés suffisait à faire passer la sauvegarde de quelques secondes à plusieurs minutes.
//!
//! Ici, le contenu est établi une fois pour toutes (un simple parcours des métadonnées), puis :
//!   - remis tel quel à `tar` sous forme de liste (`--files-from`), donc rien d'autre n'est lu ;
//!   - utilisé pour l'empreinte qui évite les archives identiques ;
//!   - décrit à l'utilisateur (nombre de fichiers, taille, fichiers ignorés).
//!
//! En mode allégé (réglage `lean_backup`, actif par défaut) deux familles de fichiers sont ignorées :
//!   - dans `/usr/local/sbin` et `/usr/local/bin`, les exécutables compilés (ELF) et tout fichier
//!     dépassant [`SCRIPT_MAX_BYTES`] : ce sont des programmes installés à la main, pas des scripts ;
//!   - partout ailleurs, tout fichier dépassant [`MAX_FILE_BYTES`].
//!
//! Le groupe « outil » (le programme lui-même) n'est jamais filtré.

use crate::tr;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::config::{group_of, GROUPS};

/// Au-delà, un fichier n'est plus de la configuration (mode allégé).
pub const MAX_FILE_BYTES: u64 = 16 << 20;
/// Au-delà, un fichier de `/usr/local/{sbin,bin}` n'est plus un script (mode allégé).
pub const SCRIPT_MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub rel: String,
    pub group: &'static str,
    pub kind: Kind,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Exécutable compilé dans un dossier de scripts.
    Binary,
    /// Fichier trop volumineux pour de la configuration.
    Large,
}

impl Why {
    pub fn label(self) -> &'static str {
        match self {
            Why::Binary => tr!("compiled executable"),
            Why::Large => tr!("large file"),
        }
    }
    pub fn json(self) -> &'static str {
        match self {
            Why::Binary => "binary",
            Why::Large => "large",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub rel: String,
    pub group: &'static str,
    pub size: u64,
    pub why: Why,
}

#[derive(Debug, Default)]
pub struct Selection {
    pub items: Vec<Item>,
    pub skipped: Vec<Skipped>,
}

/// Total par groupe (clé : identifiant de groupe).
#[derive(Debug, Default, Clone)]
pub struct GroupTotal {
    pub id: &'static str,
    pub files: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub skipped_bytes: u64,
}

fn is_elf(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).map(|_| magic == [0x7f, b'E', b'L', b'F']).unwrap_or(false)
}

fn in_script_dir(rel: &str) -> bool {
    ["usr/local/sbin", "usr/local/bin"].iter().any(|d| rel == *d || rel.strip_prefix(d).is_some_and(|rest| rest.starts_with('/')))
}

impl Selection {
    /// Parcourt `include` (chemins relatifs à `root`) sans suivre les liens symboliques.
    pub fn scan(root: &Path, include: &[String], lean: bool) -> Selection {
        let mut selection = Selection::default();
        for rel in include {
            selection.walk(root, rel, lean);
        }
        selection
    }

    fn walk(&mut self, root: &Path, rel: &str, lean: bool) {
        let path = root.join(rel);
        let Ok(meta) = fs::symlink_metadata(&path) else { return };
        let group = group_of(rel).unwrap_or("");
        let file_type = meta.file_type();
        let kind = if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_symlink() {
            Kind::Symlink
        } else if file_type.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        if kind == Kind::File && lean && group != "tool" {
            let why = if meta.len() > MAX_FILE_BYTES || (in_script_dir(rel) && meta.len() > SCRIPT_MAX_BYTES) {
                Some(Why::Large)
            } else if in_script_dir(rel) && is_elf(&path) {
                Some(Why::Binary)
            } else {
                None
            };
            if let Some(why) = why {
                self.skipped.push(Skipped { rel: rel.to_string(), group, size: meta.len(), why });
                return;
            }
        }
        self.items.push(Item { rel: rel.to_string(), group, kind, size: meta.len(), mode: meta.mode(), uid: meta.uid(), gid: meta.gid() });
        if kind == Kind::Dir {
            let Ok(entries) = fs::read_dir(&path) else { return };
            let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            for name in names {
                self.walk(root, &format!("{rel}/{name}"), lean);
            }
        }
    }

    pub fn file_count(&self) -> u64 {
        self.items.iter().filter(|i| i.kind == Kind::File).count() as u64
    }

    /// Octets de données avant compression.
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().filter(|i| i.kind == Kind::File).map(|i| i.size).sum()
    }

    pub fn skipped_bytes(&self) -> u64 {
        self.skipped.iter().map(|s| s.size).sum()
    }

    /// Totaux par groupe, dans l'ordre des groupes.
    pub fn by_group(&self) -> Vec<GroupTotal> {
        let mut totals: Vec<GroupTotal> = GROUPS.iter().map(|g| GroupTotal { id: g.id, ..Default::default() }).collect();
        for item in self.items.iter().filter(|i| i.kind == Kind::File) {
            if let Some(t) = totals.iter_mut().find(|t| t.id == item.group) {
                t.files += 1;
                t.bytes += item.size;
            }
        }
        for skipped in &self.skipped {
            if let Some(t) = totals.iter_mut().find(|t| t.id == skipped.group) {
                t.skipped += 1;
                t.skipped_bytes += skipped.size;
            }
        }
        totals
    }

    /// Les `count` plus gros fichiers retenus.
    pub fn largest(&self, count: usize) -> Vec<&Item> {
        let mut files: Vec<&Item> = self.items.iter().filter(|i| i.kind == Kind::File).collect();
        files.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.rel.cmp(&b.rel)));
        files.truncate(count);
        files
    }

    /// Liste pour `tar --null --files-from` (noms relatifs, séparés par NUL).
    pub fn tar_list(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for item in &self.items {
            out.extend_from_slice(item.rel.as_bytes());
            out.push(0);
        }
        out
    }

    /// Hash du contenu, des droits et des propriétaires (pas des dates) de ce qui sera archivé.
    pub fn fingerprint(&self, root: &Path) -> String {
        let mut digest = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        for item in &self.items {
            digest.update(format!("{}\0{:o}\0{}\0{}\0", item.rel, item.mode, item.uid, item.gid).as_bytes());
            let path = root.join(&item.rel);
            match item.kind {
                Kind::Symlink => {
                    if let Ok(target) = fs::read_link(&path) {
                        digest.update(target.as_os_str().as_encoded_bytes());
                    }
                }
                Kind::File => match File::open(&path) {
                    Ok(mut file) => {
                        while let Ok(n) = file.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            digest.update(&buf[..n]);
                        }
                    }
                    Err(_) => digest.update(b"<illisible>"),
                },
                Kind::Dir | Kind::Other => {}
            }
            digest.update(b"\n");
        }
        digest.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}
