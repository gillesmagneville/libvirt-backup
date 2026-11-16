//! Suppression d'archives choisies par l'utilisateur.
//!
//! Garde-fous : seules les archives du dossier des sauvegardes peuvent être supprimées (jamais un
//! chemin quelconque), le verrou partagé avec les sauvegardes est pris pendant l'opération, et le lien
//! « latest » est repointé sur la plus récente archive restante (ou retiré s'il n'en reste aucune).

use crate::tr;
use std::fs;

use serde_json::json;

use crate::archive::{list_archives, resolve_archive, ArchiveInfo, Kind};
use crate::backup::point_latest_link;
use crate::bail;
use crate::config::LATEST_LINK;
use crate::context::Context;
use crate::sys::{human_size, log, Context as _, FileLock, Result, Tag};

#[derive(Debug)]
pub struct DeletePlan {
    pub targets: Vec<ArchiveInfo>,
    pub total_size: u64,
    /// Conséquences à connaître avant de confirmer (à afficher telles quelles).
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct DeleteResult {
    pub removed: Vec<String>,
    pub freed: u64,
    /// `unchanged` | `relinked` | `removed`
    pub latest: &'static str,
}

/// Mises en garde propres à cette suppression, d'après les archives présentes dans le dossier.
pub fn warnings_for(known: &[ArchiveInfo], targets: &[ArchiveInfo]) -> Vec<String> {
    let is_target = |a: &ArchiveInfo| targets.iter().any(|t| t.path == a.path);
    let auto_total = known.iter().filter(|a| a.kind == Kind::Auto).count();
    let auto_left = known.iter().filter(|a| a.kind == Kind::Auto && !is_target(a)).count();
    let mut warnings = Vec::new();
    if auto_total > 0 && auto_left == 0 {
        warnings.push(
            tr!("NO automatic backup will remain: nothing will be left to restore the configuration of your virtual machines.").to_string(),
        );
    } else if known.iter().find(|a| a.kind == Kind::Auto).is_some_and(is_target) {
        warnings.push(tr!("This is the most recent backup: the \"default\" restore will use the previous one from now on.").to_string());
    }
    if targets.iter().any(|t| t.kind == Kind::Safety) {
        warnings.push(tr!("A safety archive keeps the state of the machine just before a restore: it is the way to go back.").to_string());
    }
    warnings
}

/// Désigne les archives à supprimer (numéro, nom, `latest` ou chemin d'une archive du dossier).
pub fn plan_delete(ctx: &Context, specs: &[String]) -> Result<DeletePlan> {
    if specs.is_empty() {
        bail!("Give at least one archive to delete (number, name or \"latest\").");
    }
    let dir = ctx.backup_dir();
    let known = list_archives(&dir);
    let mut targets: Vec<ArchiveInfo> = Vec::new();
    for spec in specs {
        let mut info = resolve_archive(&dir, Some(spec))?;
        if info.kind == Kind::External {
            // Un chemin explicite n'est accepté que s'il désigne une archive du dossier des sauvegardes.
            let real = fs::canonicalize(&info.path).unwrap_or_else(|_| info.path.clone());
            let inside = known.iter().find(|k| fs::canonicalize(&k.path).map(|p| p == real).unwrap_or(false));
            match inside {
                Some(found) => info = found.clone(),
                None => bail!(
                    "{0} is not in the backup folder ({1}): only archives of that folder can be deleted.",
                    info.path.display(),
                    dir.display()
                ),
            }
        }
        if !targets.iter().any(|t| t.path == info.path) {
            targets.push(info);
        }
    }
    let total_size = targets.iter().map(|t| t.size).sum();
    let warnings = warnings_for(&known, &targets);
    Ok(DeletePlan { targets, total_size, warnings })
}

pub fn run_delete(ctx: &Context, plan: &DeletePlan) -> Result<DeleteResult> {
    let rep = ctx.rep.clone();
    let dir = ctx.backup_dir();
    let mut lock = FileLock::new(&ctx.lock_path());
    if !lock.try_acquire()? {
        bail!("A backup or a restore is in progress: try again when it has finished.");
    }
    let mut result = DeleteResult { latest: "unchanged", ..Default::default() };
    let mut failed: Vec<String> = Vec::new();
    for target in &plan.targets {
        // Seules les archives du dossier, jamais un lien ni un autre fichier.
        if target.path.parent() != Some(dir.as_path()) {
            failed.push(tr!("{} (outside the backup folder)", target.name()));
            continue;
        }
        match fs::remove_file(&target.path).ctx(tr!("deleting {}", target.path.display())) {
            Ok(()) => {
                rep.info(&tr!("Archive deleted: {0} ({1})", target.name(), human_size(target.size)));
                log(Tag::Backup, &tr!("Archive deleted: {}", target.path.display()));
                result.removed.push(target.name());
                result.freed += target.size;
            }
            Err(e) => {
                rep.error(&e.to_string());
                failed.push(target.name());
            }
        }
    }

    // Le lien « latest » ne doit jamais rester pendant : repointé sur la plus récente archive
    // automatique restante, ou retiré s'il n'y en a plus.
    let link = dir.join(LATEST_LINK);
    if fs::symlink_metadata(&link).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        let newest = list_archives(&dir).into_iter().find(|a| a.kind == Kind::Auto);
        let current = fs::read_link(&link).ok().map(|t| t.to_string_lossy().into_owned());
        match newest {
            Some(n) if current.as_deref() != Some(n.name().as_str()) => match ctx.owner_ids().map(|o| point_latest_link(ctx, &n.name(), o))
            {
                Ok(Ok(())) => result.latest = "relinked",
                Ok(Err(msg)) => rep.warn(&msg),
                Err(e) => rep.warn(&e.to_string()),
            },
            Some(_) => {}
            None => {
                if fs::remove_file(&link).is_ok() {
                    result.latest = "removed";
                }
            }
        }
    }
    drop(lock);

    if !failed.is_empty() {
        bail!("{0} archive(s) could not be deleted: {1}", failed.len(), failed.join(", "));
    }
    rep.result(json!({"removed": result.removed, "freed": result.freed, "latest": result.latest}));
    Ok(result)
}
