//! Traductions : tables complètes et cohérentes avec le code, langue choisie dans l'ordre annoncé.
//!
//! Le texte anglais est la clé des tables `fr`, `de` et `es` : un libellé oublié dans une table
//! s'afficherait en anglais sans aucune erreur. Ces tests l'empêchent.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use libvirt_backup::cli::untranslated_command;
use libvirt_backup::i18n::{lookup, Lang};
use libvirt_backup::locales::{de, es, fr};

type Table = &'static [(&'static str, &'static str)];

const LANGS: [(Lang, Table); 3] = [(Lang::Fr, fr::STRINGS), (Lang::De, de::STRINGS), (Lang::Es, es::STRINGS)];

// ---------------------------------------------------------------- outils

/// Repères de remplacement d'un libellé (`{}`, `{0}`, `{1}`…), triés ; `{{` et `}}` sont des accolades.
fn placeholders(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if chars.get(i + 1) == Some(&'{') {
                i += 2;
                continue;
            }
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '}') {
                let spec: String = chars[i + 1..i + 1 + end].iter().collect();
                if spec.chars().all(|c| c.is_ascii_digit()) {
                    found.push(spec);
                    i += end + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    found.sort();
    found
}

/// Littéral Rust (`"…"` avec échappements, ou chaîne brute) au début de `text` : sa valeur.
fn parse_literal(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.first() == Some(&'r') {
        let hashes = chars[1..].iter().take_while(|&&c| c == '#').count();
        if chars.get(1 + hashes) != Some(&'"') {
            return None;
        }
        let closing: String = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
        let body: String = chars[2 + hashes..].iter().collect();
        return body.find(&closing).map(|end| body[..end].to_string());
    }
    if chars.first() != Some(&'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = 1;
    while i < chars.len() {
        match chars[i] {
            '"' => return Some(out),
            '\\' => {
                i += 1;
                match chars.get(i)? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '\'' => out.push('\''),
                    'u' => {
                        let end = chars[i..].iter().position(|&c| c == '}')? + i;
                        let hex: String = chars[i + 2..end].iter().collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        i = end;
                    }
                    '\n' => {
                        // `\` en fin de ligne : la ligne suivante continue, sans son indentation
                        while chars.get(i + 1).is_some_and(|c| c.is_whitespace()) {
                            i += 1;
                        }
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "locales") {
                source_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "i18n.rs") {
            out.push(path);
        }
    }
}

/// Libellés écrits dans le code : `tr!("…")`, `tr_noop!("…")`, `i18n::tr("…")` et `bail!("…")` (qui traduit).
fn code_literals() -> BTreeMap<String, PathBuf> {
    let mut files = Vec::new();
    source_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut found = BTreeMap::new();
    for file in files {
        let src = fs::read_to_string(&file).unwrap();
        for marker in ["tr!(", "tr_noop!(", "i18n::tr(", "bail!("] {
            let mut from = 0;
            while let Some(pos) = src[from..].find(marker) {
                let start = from + pos;
                from = start + marker.len();
                if src[..start].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                // les exemples des commentaires ne sont pas des libellés
                let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
                if src[line_start..start].trim_start().starts_with("//") {
                    continue;
                }
                if let Some(text) = parse_literal(src[from..].trim_start()) {
                    found.entry(text).or_insert_with(|| file.clone());
                }
            }
        }
    }
    found
}

/// Textes d'aide de clap que le programme traduit à l'exécution (voir `localize` dans `cli.rs`).
/// `everything` : aussi les arguments cachés et les noms de valeur déduits (pour chercher les clés inutiles) ;
/// sinon seulement ce qu'une personne peut lire dans l'aide.
fn clap_texts(cmd: &clap::Command, out: &mut BTreeSet<String>, everything: bool) {
    out.extend(cmd.get_about().map(|s| s.to_string()));
    out.extend(cmd.get_subcommand_help_heading().map(str::to_string));
    out.extend(cmd.get_next_help_heading().map(str::to_string));
    for arg in cmd.get_arguments().filter(|a| everything || !a.is_hide_set()) {
        out.extend(arg.get_help().map(|s| s.to_string()));
        out.extend(arg.get_help_heading().map(str::to_string));
        // seuls les noms de valeur écrits à la main sont traduits (clap déduit les autres de l'identifiant)
        let derived = arg.get_id().as_str().to_uppercase();
        out.extend(arg.get_value_names().and_then(|names| names.first()).map(|n| n.to_string()).filter(|n| everything || *n != derived));
    }
    for sub in cmd.get_subcommands() {
        clap_texts(sub, out, everything);
    }
}

/// Texte sur une seule ligne (clap replie l'aide à la largeur du terminal).
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn has_long_about(cmd: &clap::Command) -> bool {
    cmd.get_long_about().is_some() || cmd.get_subcommands().any(has_long_about)
}

// ---------------------------------------------------------------- tables

#[test]
fn tables_have_the_same_keys_and_placeholders() {
    let reference: BTreeSet<&str> = fr::STRINGS.iter().map(|(k, _)| *k).collect();
    for (lang, table) in LANGS {
        let keys: Vec<&str> = table.iter().map(|(k, _)| *k).collect();
        let unique: BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(keys.len(), unique.len(), "{lang:?} : clé en double");
        let missing: Vec<&&str> = reference.difference(&unique).collect();
        let extra: Vec<&&str> = unique.difference(&reference).collect();
        assert!(missing.is_empty() && extra.is_empty(), "{lang:?} : manquantes {missing:?}, en trop {extra:?}");
        for (key, value) in table {
            assert!(!value.trim().is_empty(), "{lang:?} : traduction vide pour {key:?}");
            assert_eq!(placeholders(key), placeholders(value), "{lang:?} : repères différents pour {key:?} → {value:?}");
            assert_eq!(key.starts_with('\n'), value.starts_with('\n'), "{lang:?} : saut de ligne initial pour {key:?}");
            assert_eq!(key.ends_with('\n'), value.ends_with('\n'), "{lang:?} : saut de ligne final pour {key:?}");
        }
    }
}

#[test]
fn every_label_of_the_code_is_translated() {
    let literals = code_literals();
    assert!(literals.len() > 300, "le test ne trouve presque aucun libellé ({}) : l'analyse du code est cassée", literals.len());
    let mut missing = Vec::new();
    for (text, file) in &literals {
        for (lang, _) in LANGS {
            if lookup(lang, text).is_none() {
                missing.push(format!("{lang:?} · {} · {text:?}", file.file_name().unwrap().to_string_lossy()));
            }
        }
    }
    assert!(missing.is_empty(), "libellés sans traduction :\n{}", missing.join("\n"));
}

#[test]
fn no_translation_is_orphaned() {
    let mut used: BTreeSet<String> = code_literals().into_keys().collect();
    clap_texts(&untranslated_command(), &mut used, true);
    let orphans: Vec<&str> = fr::STRINGS.iter().map(|(k, _)| *k).filter(|k| !used.contains(*k)).collect();
    assert!(orphans.is_empty(), "traductions qui ne servent plus (à supprimer de src/locales) :\n{orphans:#?}");
}

#[test]
fn clap_texts_are_translated_everywhere() {
    let mut texts = BTreeSet::new();
    clap_texts(&untranslated_command(), &mut texts, false);
    assert!(texts.len() > 30, "l'aide de clap semble vide ({} textes)", texts.len());
    assert!(!has_long_about(&untranslated_command()), "une aide longue (long_about) ne serait pas traduite");
    for (lang, _) in LANGS {
        let missing: Vec<&String> = texts.iter().filter(|t| lookup(lang, t).is_none()).collect();
        assert!(missing.is_empty(), "{lang:?} : aide sans traduction : {missing:#?}");
    }
}

#[test]
fn translations_are_real_translations() {
    // Une table presque identique à l'anglais trahirait un copier-coller : on tolère les noms propres,
    // chemins, commandes et libellés identiques dans les deux langues.
    for (lang, table) in LANGS {
        let same = table.iter().filter(|(k, v)| k == v).count();
        assert!(same * 100 < table.len() * 12, "{lang:?} : {same} libellés sur {} sont identiques à l'anglais", table.len());
    }
}

// ----------------------------------------------------- aide en ligne de commande

#[test]
fn help_is_displayed_in_the_chosen_language() {
    let env = Env::new();
    let english = untranslated_command();
    let about = english.get_about().unwrap().to_string();
    let first_sub = english.get_subcommands().find(|s| s.get_name() == "backup").unwrap();
    let sub_about = first_sub.get_about().unwrap().to_string();

    let en = env.cli(&["--language", "en", "--help"]);
    assert_eq!(en.code, 0, "{}", en.all());
    assert!(flat(&en.stdout).contains(&flat(&about)) && en.stdout.contains("Usage:"), "{}", en.stdout);

    for (lang, _) in LANGS {
        let code = lang.code();
        let top = env.cli(&["--language", code, "--help"]);
        assert_eq!(top.code, 0, "{}", top.all());
        assert!(flat(&top.stdout).contains(&flat(lookup(lang, &about).unwrap())), "{code} : description du programme\n{}", top.stdout);
        assert!(top.stdout.contains(lookup(lang, "Usage:").unwrap()), "{code} : « Usage »\n{}", top.stdout);
        assert!(!flat(&top.stdout).contains(&flat(&about)), "{code} : la description anglaise ne doit plus apparaître");
        let sub = env.cli(&["--language", code, "backup", "--help"]);
        assert!(flat(&sub.stdout).contains(&flat(lookup(lang, &sub_about).unwrap())), "{code} : aide de « backup »\n{}", sub.stdout);
        assert!(sub.stdout.contains("--dry-run"), "les options elles-mêmes ne sont jamais traduites");
    }
}

// ------------------------------------------------------- choix de la langue

fn groups_in(env: &Env, vars: &[(&str, &str)], args: &[&str], unset: &[&str]) -> Run {
    let mut cmd = env.command(args);
    for name in unset {
        cmd.env_remove(name);
    }
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = cmd.stdin(std::process::Stdio::null()).output().unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn the_language_is_chosen_in_the_documented_order() {
    let env = Env::new();
    let label = |lang: Lang| {
        lookup(lang, "libvirt configuration (VMs, networks, pools, secrets)")
            .unwrap_or("libvirt configuration (VMs, networks, pools, secrets)")
    };
    let none: &[&str] = &["LIBVIRT_BACKUP_LANG", "LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"];

    // 1. --language l'emporte sur la variable d'environnement et sur le réglage
    env.set_config(&[("language", "es")]);
    let run = groups_in(&env, &[("LIBVIRT_BACKUP_LANG", "fr")], &["groups", "--language", "de"], none);
    assert!(run.stdout.contains(label(Lang::De)), "--language : {}", run.all());

    // 2. variable d'environnement, puis réglage « language » de config.json
    let run = groups_in(&env, &[("LIBVIRT_BACKUP_LANG", "fr")], &["groups"], none);
    assert!(run.stdout.contains(label(Lang::Fr)), "LIBVIRT_BACKUP_LANG : {}", run.all());
    let run = groups_in(&env, &[], &["groups"], none);
    assert!(run.stdout.contains(label(Lang::Es)), "réglage language=es : {}", run.all());

    // 3. sans réglage : la langue du système (LC_ALL, LANG, LANGUAGE), comme gettext
    env.set_config(&[]);
    let run = groups_in(&env, &[("LANG", "de_DE.UTF-8")], &["groups"], none);
    assert!(run.stdout.contains(label(Lang::De)), "LANG=de : {}", run.all());
    let run = groups_in(&env, &[("LANG", "fr_FR.UTF-8"), ("LANGUAGE", "es:fr")], &["groups"], none);
    assert!(run.stdout.contains(label(Lang::Es)), "LANGUAGE=es:fr : {}", run.all());

    // 4. langue du système non traduite ou aucune langue : anglais
    let english = "libvirt configuration (VMs, networks, pools, secrets)";
    let run = groups_in(&env, &[("LANG", "it_IT.UTF-8")], &["groups"], none);
    assert!(run.stdout.contains(english), "langue non traduite : {}", run.all());
    let run = groups_in(&env, &[], &["groups"], none);
    assert!(run.stdout.contains(english), "aucune langue : {}", run.all());

    // 5. valeur invalide : erreur claire, code 2
    let bad = env.cli(&["groups", "--language", "xx"]);
    assert_eq!(bad.code, 2, "{}", bad.all());
    assert!(bad.all().contains("xx"), "{}", bad.all());
    // `system` suit explicitement le système, même quand l'environnement impose une autre langue
    let run = groups_in(&env, &[("LANG", "de_DE.UTF-8")], &["groups", "--language", "system"], none);
    assert!(run.stdout.contains(label(Lang::De)), "--language system : {}", run.all());
}

#[test]
fn the_language_setting_is_validated_and_saved() {
    let env = Env::new();
    let saved = env.cli(&["config", "set", "language=de"]);
    assert_eq!(saved.code, 0, "{}", saved.all());
    assert!(env.read("etc/libvirt-backup/config.json").contains("\"language\": \"de\""), "{}", env.read("etc/libvirt-backup/config.json"));
    let refused = env.cli(&["config", "set", "language=klingon"]);
    assert_eq!(refused.code, 1, "{}", refused.all());
    assert!(env.read("etc/libvirt-backup/config.json").contains("\"language\": \"de\""), "une valeur refusée ne change rien");
    assert!(env.cli(&["config", "set", "language=system"]).code == 0);
    let shown = env.cli(&["config", "show"]);
    assert!(shown.stdout.contains("language=system"), "{}", shown.all());
}

// ------------------------------------------------- mots de confirmation, README

#[test]
fn confirmation_words_follow_the_language_and_the_french_word_always_works() {
    for (lang, restore_word, delete_word) in
        [(Lang::De, "WIEDERHERSTELLEN", "LÖSCHEN"), (Lang::Es, "RESTAURAR", "ELIMINAR"), (Lang::En, "RESTORE", "DELETE")]
    {
        assert_eq!(lookup(lang, "RESTORE").unwrap_or("RESTORE"), restore_word);
        assert_eq!(lookup(lang, "DELETE").unwrap_or("DELETE"), delete_word);
    }
    let env = Env::new();
    env.backup();
    env.write("etc/default/grub", "CHANGED\n");
    // en allemand, le mot allemand restaure…
    let Some(done) = env.cli_tty(&["--language", "de", "restore", "--latest", "--only", "boot"], "WIEDERHERSTELLEN\n") else { return };
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"), "{}", done.all());
    // … le mot de l'autre langue (anglais) ne restaure pas…
    env.write("etc/default/grub", "CHANGED\n");
    let refused = env.cli_tty(&["--language", "de", "restore", "--latest", "--only", "boot"], "RESTORE\n").unwrap();
    assert_eq!(refused.code, 0, "{}", refused.all());
    assert_eq!(env.read("etc/default/grub"), "CHANGED\n", "{}", refused.all());
    // … et le mot français historique reste accepté partout (scripts, habitudes).
    let french = env.cli_tty(&["--language", "de", "restore", "--latest", "--only", "boot"], "RESTAURER\n").unwrap();
    assert_eq!(french.code, 0, "{}", french.all());
    assert!(env.read("etc/default/grub").starts_with("GRUB_CMDLINE"), "{}", french.all());
}

#[test]
fn the_readme_of_an_archive_is_written_in_the_chosen_language() {
    let env = Env::new();
    let key = "Automatic backup of the host virtualization configuration.";
    let first_line = |lang: Lang| -> String {
        let (_, text) = match lang {
            Lang::De => de::STRINGS,
            Lang::Es => es::STRINGS,
            _ => fr::STRINGS,
        }
        .iter()
        .find(|(k, _)| k.starts_with(key))
        .expect("README traduit");
        text.lines().next().unwrap().to_string()
    };
    for lang in [Lang::De, Lang::Es] {
        let dir = env.tmp.path().join(format!("readme-{}", lang.code()));
        fs::create_dir(&dir).unwrap();
        let run = env.cli(&["--language", lang.code(), "backup", "--backup-dir", dir.to_str().unwrap()]);
        assert_eq!(run.code, 0, "{}", run.all());
        let readme = member(&archives(&dir)[0], "README.txt").expect("README.txt dans l'archive");
        assert!(readme.starts_with(&first_line(lang)), "{} : {readme}", lang.code());
        assert!(readme.contains(libvirt_backup::VERSION), "version dans le README");
        assert!(!readme.contains("@VERSION@") && !readme.contains("@DATE@") && !readme.contains("@LEAN@"), "repères remplacés : {readme}");
    }
}

/// Un libellé français écrit en dur dans le code passerait inaperçu des tests précédents (ils ne regardent
/// que les `tr!`) : il s'afficherait tel quel dans une interface anglaise. On cherche quelques mots français
/// dans les littéraux hors commentaires. Ne sont pas concernés : les fichiers écrits sur le système avec le
/// texte d'origine des scripts (`selfinstall.rs`) et les mots de confirmation (`config.rs`).
#[test]
fn no_french_label_is_hard_coded_in_the_code() {
    const WORDS: [&str; 9] =
        ["impossible de", "automatique", "externe", "sauvegarde", "dossier", "erreur", "fichier", "introuvable", "échec"];
    let mut files = Vec::new();
    source_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut found = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        if name == "selfinstall.rs" || name == "config.rs" {
            continue;
        }
        let src = fs::read_to_string(&file).unwrap();
        let tests_start = src.find("#[cfg(test)]").unwrap_or(src.len());
        for (number, line) in src[..tests_start].lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            // un commentaire en fin de ligne : on le retire quand les guillemets qui le précèdent sont équilibrés
            let code = match code.find(" // ") {
                Some(i) if code[..i].matches('"').count() % 2 == 0 => &code[..i],
                _ => code,
            };
            let mut in_string = false;
            let mut literal = String::new();
            let mut previous = ' ';
            for c in code.chars() {
                if c == '"' && previous != '\\' {
                    if in_string && WORDS.iter().any(|w| literal.to_lowercase().contains(w)) {
                        found.push(format!("{name}:{}: {literal}", number + 1));
                    }
                    in_string = !in_string;
                    literal.clear();
                } else if in_string {
                    literal.push(c);
                }
                previous = c;
            }
        }
    }
    assert!(found.is_empty(), "libellé français écrit en dur (utiliser tr!) :\n{}", found.join("\n"));
}
