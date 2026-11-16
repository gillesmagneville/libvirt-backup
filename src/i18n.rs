//! Internationalisation : anglais, français, allemand, espagnol.
//!
//! Choix délibéré, comme pour Planche-Contact : de simples tables Rust (`src/locales/`) plutôt que
//! gettext (`.po`/`.mo`) — aucune étape de compilation de catalogues, aucune dépendance de plus à
//! empaqueter, et le compilateur embarque les textes dans l'exécutable.
//!
//! La **clé** d'un texte est son libellé anglais, tel qu'il est écrit dans le code : l'anglais n'a donc
//! pas de table, il est toujours complet, et c'est la langue de repli quand une traduction manque. Les
//! textes à arguments utilisent des repères numérotés (`{0}`, `{1}`…, ou `{}` s'il n'y en a qu'un), de
//! sorte qu'une traduction puisse changer l'ordre des éléments.
//!
//! Servent à traduire : les messages de la ligne de commande et du moteur, l'aide (`--help`), le README
//! stocké dans chaque archive et l'interface graphique. Seuls les messages destinés au développeur
//! restent en anglais.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use crate::locales;

/// Langues disponibles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Fr,
    De,
    Es,
}

impl Lang {
    pub const ALL: [Lang; 4] = [Lang::En, Lang::Fr, Lang::De, Lang::Es];

    /// Code à deux lettres (`en`, `fr`, `de`, `es`).
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Es => "es",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        Lang::ALL.into_iter().find(|l| l.code() == code)
    }

    /// Nom de la langue dans cette langue (jamais traduit : on doit pouvoir la reconnaître).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Fr => "Français",
            Lang::De => "Deutsch",
            Lang::Es => "Español",
        }
    }

    fn index(self) -> u8 {
        match self {
            Lang::En => 0,
            Lang::Fr => 1,
            Lang::De => 2,
            Lang::Es => 3,
        }
    }

    fn from_index(index: u8) -> Lang {
        Lang::ALL.into_iter().find(|l| l.index() == index).unwrap_or(Lang::En)
    }
}

/// Valeurs possibles du réglage « langue » : `system` (défaut) ou le code d'une langue.
pub const PREFERENCES: [&str; 5] = ["system", "en", "fr", "de", "es"];

pub fn is_valid_preference(value: &str) -> bool {
    PREFERENCES.contains(&value)
}

/// Variable d'environnement qui impose la langue (prioritaire sur le réglage et sur le système).
pub const ENV_LANGUAGE: &str = "LIBVIRT_BACKUP_LANG";

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Langue active du processus.
pub fn current() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed))
}

pub fn set_current(lang: Lang) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// Code de langue (deux lettres) lu dans l'environnement : `LC_ALL`, `LC_MESSAGES`, puis `LANG`, comme
/// gettext ; `LANGUAGE` (liste de préférence) n'est consultée que si la locale n'est pas `C`/`POSIX`.
/// `None` si rien n'est défini ou si la locale est `C`/`POSIX`.
pub fn detect_system_language() -> Option<String> {
    detect_from(&|name| std::env::var(name).ok())
}

pub fn detect_from(get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().filter_map(|n| get(n)).find(|v| !v.trim().is_empty())?;
    let locale = locale.trim();
    if locale == "C" || locale == "POSIX" || locale.starts_with("C.") {
        return None;
    }
    if let Some(list) = get("LANGUAGE") {
        if let Some(first) = list.split(':').map(str::trim).find(|s| !s.is_empty() && *s != "C") {
            return Some(short_code(first));
        }
    }
    Some(short_code(locale))
}

/// `fr_FR.UTF-8` → `fr`, `de-DE` → `de`.
fn short_code(locale: &str) -> String {
    locale.split(['_', '-', '.', '@']).next().unwrap_or("").to_lowercase()
}

/// Date `AAAA-MM-JJ` écrite à la manière de la langue (fr, es : `JJ/MM/AAAA` ; de : `JJ.MM.AAAA` ;
/// en : `AAAA-MM-JJ`). Un texte qui n'a pas cette forme est renvoyé tel quel.
pub fn format_date(lang: Lang, iso: &str) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    match parts.as_slice() {
        [y, m, d] if digits(y, 4) && digits(m, 2) && digits(d, 2) => match lang {
            Lang::En => iso.to_string(),
            Lang::Fr | Lang::Es => format!("{d}/{m}/{y}"),
            Lang::De => format!("{d}.{m}.{y}"),
        },
        _ => iso.to_string(),
    }
}

/// Résultat de la résolution d'une préférence.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    pub lang: Lang,
    /// Code détecté sur le système quand il n'a pas de traduction (repli sur l'anglais).
    pub unsupported: Option<String>,
}

/// Langue effective pour une préférence (`system`, `en`, `fr`, `de`, `es`).
/// Une préférence explicite est utilisée telle quelle ; `system` suit l'environnement et se rabat
/// sur l'anglais, en signalant le code détecté, quand la langue du système n'est pas traduite.
pub fn resolve(preference: &str) -> Resolved {
    resolve_with(preference, detect_system_language())
}

pub fn resolve_with(preference: &str, detected: Option<String>) -> Resolved {
    if let Some(lang) = Lang::from_code(preference) {
        return Resolved { lang, unsupported: None };
    }
    match detected {
        Some(code) => match Lang::from_code(&code) {
            Some(lang) => Resolved { lang, unsupported: None },
            None => Resolved { lang: Lang::En, unsupported: Some(code) },
        },
        None => Resolved { lang: Lang::En, unsupported: None },
    }
}

/// Préférence retenue : la première valeur valide parmi `--language`, la variable d'environnement
/// `LIBVIRT_BACKUP_LANG` et le réglage `language` ; à défaut, `system`.
pub fn pick_preference(cli: Option<&str>, env: Option<&str>, configured: Option<&str>) -> String {
    [cli, env, configured]
        .into_iter()
        .flatten()
        .map(|v| v.trim().to_lowercase())
        .find(|v| is_valid_preference(v))
        .unwrap_or_else(|| "system".to_string())
}

/// Active la langue correspondant à la préférence et la renvoie.
pub fn set_language(preference: &str) -> Resolved {
    let resolved = resolve(preference);
    set_current(resolved.lang);
    resolved
}

type Table = HashMap<&'static str, &'static str>;

fn table(lang: Lang) -> Option<&'static Table> {
    static TABLES: [OnceLock<Table>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let (slot, strings) = match lang {
        Lang::En => return None,
        Lang::Fr => (0, locales::fr::STRINGS),
        Lang::De => (1, locales::de::STRINGS),
        Lang::Es => (2, locales::es::STRINGS),
    };
    Some(TABLES[slot].get_or_init(|| strings.iter().copied().collect()))
}

/// Traduction de `key` dans `lang`, si elle existe (l'anglais est la clé elle-même).
pub fn lookup(lang: Lang, key: &str) -> Option<&'static str> {
    table(lang).and_then(|t| t.get(key).copied())
}

/// Traduit un libellé anglais dans la langue active ; à défaut, le libellé anglais lui-même.
pub fn tr(key: &'static str) -> &'static str {
    lookup(current(), key).unwrap_or(key)
}

/// Comme [`tr`] pour un libellé qui n'est pas `'static` (aide de clap) ; `None` si inconnu.
pub fn tr_dynamic(key: &str) -> Option<&'static str> {
    lookup(current(), key)
}

/// Remplace `{}` (dans l'ordre), `{N}` (numéroté depuis 0) par les arguments ; `{{` et `}}` donnent des
/// accolades littérales. Un repère sans argument est laissé tel quel (jamais de panique).
pub fn render(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.char_indices().peekable();
    let mut next_positional = 0usize;
    while let Some((i, c)) = chars.next() {
        match c {
            '{' => {
                if matches!(chars.peek(), Some((_, '{'))) {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let rest = &template[i + 1..];
                match rest.find('}') {
                    Some(end) if rest[..end].chars().all(|ch| ch.is_ascii_digit()) => {
                        let spec = &rest[..end];
                        let index = if spec.is_empty() {
                            next_positional += 1;
                            next_positional - 1
                        } else {
                            spec.parse().unwrap_or(usize::MAX)
                        };
                        match args.get(index) {
                            Some(arg) => out.push_str(&arg.to_string()),
                            None => out.push_str(&template[i..i + end + 2]),
                        }
                        for _ in 0..end + 1 {
                            chars.next();
                        }
                    }
                    _ => out.push('{'),
                }
            }
            '}' => {
                if matches!(chars.peek(), Some((_, '}'))) {
                    chars.next();
                }
                out.push('}');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Libellé anglais → texte de la langue active. Sans argument, renvoie un `&'static str` ; avec
/// arguments (`{0}`, `{1}`…), un `String`.
#[macro_export]
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::tr($key)
    };
    ($key:literal, $($arg:expr),+ $(,)?) => {
        $crate::i18n::render($crate::i18n::tr($key), &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

/// Marque un libellé à traduire plus tard (tableaux `static`/`const`) : la valeur reste le libellé
/// anglais, à passer à [`tr`] au moment de l'afficher.
#[macro_export]
macro_rules! tr_noop {
    ($key:literal) => {
        $key
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }

    #[test]
    fn writes_dates_the_local_way() {
        assert_eq!(format_date(Lang::En, "2026-11-16"), "2026-11-16");
        assert_eq!(format_date(Lang::Fr, "2026-11-16"), "16/11/2026");
        assert_eq!(format_date(Lang::Es, "2026-11-16"), "16/11/2026");
        assert_eq!(format_date(Lang::De, "2026-11-16"), "16.11.2026");
        assert_eq!(format_date(Lang::Fr, "pas une date"), "pas une date");
        assert_eq!(format_date(Lang::De, "2026-1-6"), "2026-1-6");
    }

    #[test]
    fn detects_the_language_like_gettext() {
        assert_eq!(detect_from(&env(&[("LANG", "fr_FR.UTF-8")])), Some("fr".into()));
        assert_eq!(detect_from(&env(&[("LANG", "de-DE")])), Some("de".into()));
        assert_eq!(detect_from(&env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "es_ES.UTF-8")])), Some("es".into()));
        assert_eq!(detect_from(&env(&[("LANG", "fr_FR.UTF-8"), ("LANGUAGE", "de:fr")])), Some("de".into()));
        assert_eq!(detect_from(&env(&[("LANG", "C"), ("LANGUAGE", "de")])), None);
        assert_eq!(detect_from(&env(&[("LC_ALL", "C"), ("LANG", "fr_FR.UTF-8")])), None);
        assert_eq!(detect_from(&env(&[("LANG", "C.UTF-8")])), None);
        assert_eq!(detect_from(&env(&[])), None);
    }

    #[test]
    fn falls_back_to_english_and_reports_the_unsupported_code() {
        assert_eq!(resolve_with("fr", Some("de".into())), Resolved { lang: Lang::Fr, unsupported: None });
        assert_eq!(resolve_with("system", Some("de".into())), Resolved { lang: Lang::De, unsupported: None });
        assert_eq!(resolve_with("system", Some("ja".into())), Resolved { lang: Lang::En, unsupported: Some("ja".into()) });
        assert_eq!(resolve_with("system", None), Resolved { lang: Lang::En, unsupported: None });
        assert_eq!(resolve_with("valeur-invalide", Some("fr".into())), Resolved { lang: Lang::Fr, unsupported: None });
    }

    #[test]
    fn picks_the_first_valid_preference() {
        assert_eq!(pick_preference(Some("de"), Some("fr"), Some("es")), "de");
        assert_eq!(pick_preference(None, Some("FR"), Some("es")), "fr");
        assert_eq!(pick_preference(Some("xx"), Some(""), Some("es")), "es");
        assert_eq!(pick_preference(None, None, None), "system");
        assert_eq!(pick_preference(Some("system"), Some("fr"), None), "system");
    }

    #[test]
    fn renders_positional_and_numbered_placeholders() {
        assert_eq!(render("{} et {}", &[&"a", &2]), "a et 2");
        assert_eq!(render("{1} avant {0}", &[&"a", &"b"]), "b avant a");
        assert_eq!(render("{0}{0}", &[&"x"]), "xx");
        assert_eq!(render("{{littéral}} {}", &[&1]), "{littéral} 1");
        assert_eq!(render("manque {3}", &[&1]), "manque {3}");
        assert_eq!(render("{abc} reste", &[]), "{abc} reste");
    }
}
