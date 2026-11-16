//! Rapports de progression : texte (terminal), JSON (interface graphique), collecteur (tests).
//!
//! Les opérations longues s'appuient toutes sur la même interface [`Reporter`] ; la CLI et
//! l'interface GTK4 voient donc exactement les mêmes étapes.

use crate::tr;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub trait Reporter: Send + Sync {
    fn step(&self, _index: u32, _total: u32, _message: &str) {}
    fn info(&self, _message: &str) {}
    fn warn(&self, _message: &str) {}
    fn error(&self, _message: &str) {}
    fn result(&self, _data: Value) {}
    fn busy_start(&self, message: &str) {
        self.info(message);
    }
    fn busy_end(&self, _message: &str, _ok: bool) {}
    /// Événement de fin (protocole JSON uniquement).
    fn end(&self, _code: i32) {}
    /// Progression globale de l'opération en cours, de 0.0 à 1.0 (jauge). Les opérations dont la durée
    /// n'est pas prévisible n'en émettent pas : l'interface affiche alors une barre qui pulse.
    fn progress(&self, _fraction: f64) {}
}

/// Jauge monotone : n'émet que si la progression gagne au moins 0,5 % (au plus 200 événements).
pub struct Gauge<'a> {
    rep: &'a dyn Reporter,
    last: f64,
}

impl<'a> Gauge<'a> {
    /// Démarre la jauge à 0 %.
    pub fn new(rep: &'a dyn Reporter) -> Self {
        rep.progress(0.0);
        Gauge { rep, last: 0.0 }
    }

    /// Fixe la progression globale (jamais en arrière).
    pub fn set(&mut self, fraction: f64) {
        let f = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { return };
        if f >= self.last + 0.005 || (f >= 1.0 && self.last < 1.0) {
            self.last = f;
            self.rep.progress(f);
        }
    }

    /// Fixe la progression à l'intérieur d'une plage : `phase` (0..1) est l'avancement de la phase qui
    /// occupe la tranche `from..to` de la jauge globale.
    pub fn set_in(&mut self, from: f64, to: f64, phase: f64) {
        self.set(from + (to - from) * phase.clamp(0.0, 1.0));
    }
}

/// Barre de texte `[████░░░░]` (ou `[##--]` si le terminal n'est pas en UTF-8).
pub fn gauge_bar(fraction: f64, width: usize) -> String {
    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|v| !v.is_empty())
        .map(|v| {
            let v = v.to_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
        .unwrap_or(false);
    let (full, empty) = if utf8 { ("█", "░") } else { ("#", "-") };
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!("[{}{}]", full.repeat(filled), empty.repeat(width - filled))
}

/// Exécute `f` entouré d'un indicateur d'activité.
pub fn busy<T, E>(rep: &dyn Reporter, message: &str, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    rep.busy_start(message);
    let result = f();
    rep.busy_end(message, result.is_ok());
    result
}

/// Ne produit aucune sortie (démon de surveillance).
pub struct QuietReporter;
impl Reporter for QuietReporter {}

/// Conserve les événements en mémoire (tests).
#[derive(Default)]
pub struct Collector {
    events: Mutex<Vec<(String, String)>>,
}

impl Collector {
    pub fn new() -> Self {
        Self::default()
    }
    fn add(&self, kind: &str, text: String) {
        self.events.lock().unwrap().push((kind.to_string(), text));
    }
    pub fn messages(&self, kind: &str) -> Vec<String> {
        self.events.lock().unwrap().iter().filter(|(k, _)| k == kind).map(|(_, m)| m.clone()).collect()
    }
}

impl Reporter for Collector {
    fn step(&self, index: u32, total: u32, message: &str) {
        self.add("step", format!("[{index}/{total}] {message}"));
    }
    fn info(&self, message: &str) {
        self.add("info", message.to_string());
    }
    fn warn(&self, message: &str) {
        self.add("warn", message.to_string());
    }
    fn error(&self, message: &str) {
        self.add("error", message.to_string());
    }
    fn result(&self, data: Value) {
        self.add("result", data.to_string());
    }
    fn progress(&self, fraction: f64) {
        self.add("progress", format!("{fraction:.4}"));
    }
}

// --------------------------------------------------------------------------
// Texte
// --------------------------------------------------------------------------

struct Spinner {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    started: Instant,
}

/// Sortie lisible dans un terminal, avec indicateur d'activité pour les tâches longues.
#[derive(Default)]
pub struct TextReporter {
    lock: Arc<Mutex<()>>,
    spinner: Mutex<Option<Spinner>>,
    /// Dernière progression reçue, en millionièmes + 1 (0 = aucune : l'indicateur d'activité tourne).
    gauge: Arc<AtomicU64>,
}

impl TextReporter {
    pub fn new() -> Self {
        Self::default()
    }
    fn print_out(&self, text: &str) {
        let _g = self.lock.lock().unwrap();
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
    fn print_err(&self, text: &str) {
        let _g = self.lock.lock().unwrap();
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{text}");
        let _ = err.flush();
    }
}

impl Reporter for TextReporter {
    fn step(&self, index: u32, total: u32, message: &str) {
        self.print_out(&format!("[{index}/{total}] {message}"));
    }
    fn info(&self, message: &str) {
        self.print_out(message);
    }
    fn warn(&self, message: &str) {
        self.print_err(&tr!("WARNING: {}", message));
    }
    fn error(&self, message: &str) {
        self.print_err(&tr!("ERROR: {}", message));
    }

    fn busy_start(&self, message: &str) {
        self.print_out(message);
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let lock = self.lock.clone();
        let gauge = self.gauge.clone();
        let message = message.to_string();
        let started = Instant::now();
        let interactive = std::io::stdout().is_terminal();
        let handle = thread::spawn(move || {
            let frames = ['|', '/', '-', '\\'];
            let mut frame = 0usize;
            let mut last_report = started;
            let tick = if interactive { Duration::from_millis(200) } else { Duration::from_secs(1) };
            while !stop2.load(Ordering::Relaxed) {
                // park_timeout (et non sleep) : busy_end réveille le fil aussitôt, sans attendre la fin
                // du délai (jusqu'à 1 s perdue à chaque étape en sortie non interactive).
                thread::park_timeout(tick);
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                let elapsed = started.elapsed().as_secs();
                let raw = gauge.load(Ordering::Relaxed);
                let percent = (raw > 0).then(|| (((raw - 1) as f64 / 1_000_000.0) * 100.0).floor() as u32);
                let _g = lock.lock().unwrap();
                let mut out = std::io::stdout().lock();
                if interactive {
                    let line = match percent {
                        Some(p) => tr!("{0} {1}%   Elapsed time: {2}s", gauge_bar(p as f64 / 100.0, 24), p, elapsed),
                        None => tr!("In progress {0}  Elapsed time: {1}s", frames[frame % 4], elapsed),
                    };
                    let _ = write!(out, "\r\x1b[K  {line}");
                    let _ = out.flush();
                    frame += 1;
                } else if last_report.elapsed() >= Duration::from_secs(5) {
                    let line = match percent {
                        Some(p) => tr!("Still in progress: {0} ({1}s elapsed, {2}%).", message, elapsed, p),
                        None => tr!("Still in progress: {0} ({1}s elapsed).", message, elapsed),
                    };
                    let _ = writeln!(out, "  {line}");
                    let _ = out.flush();
                    last_report = Instant::now();
                }
            }
        });
        *self.spinner.lock().unwrap() = Some(Spinner { stop, handle, started });
    }

    fn progress(&self, fraction: f64) {
        if fraction.is_finite() {
            self.gauge.store((fraction.clamp(0.0, 1.0) * 1_000_000.0) as u64 + 1, Ordering::Relaxed);
        }
    }

    fn busy_end(&self, _message: &str, ok: bool) {
        let Some(spinner) = self.spinner.lock().unwrap().take() else { return };
        spinner.stop.store(true, Ordering::Relaxed);
        spinner.handle.thread().unpark();
        let _ = spinner.handle.join();
        if std::io::stdout().is_terminal() {
            let _g = self.lock.lock().unwrap();
            let mut out = std::io::stdout().lock();
            let _ = write!(out, "\r\x1b[K");
            let _ = out.flush();
        }
        let elapsed = crate::sys::human_duration(spinner.started.elapsed());
        if ok {
            self.print_out(&tr!("  Done in {}.", elapsed));
        } else {
            self.print_err(tr!("  ERROR: operation interrupted or failed."));
        }
    }
}

// --------------------------------------------------------------------------
// JSON
// --------------------------------------------------------------------------

/// Un événement JSON par ligne sur stdout : protocole lu par l'interface graphique.
#[derive(Default)]
pub struct JsonReporter {
    lock: Mutex<()>,
}

impl JsonReporter {
    pub fn new() -> Self {
        Self::default()
    }
    fn emit(&self, value: Value) {
        let _g = self.lock.lock().unwrap();
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    }
}

impl Reporter for JsonReporter {
    fn step(&self, index: u32, total: u32, message: &str) {
        self.emit(json!({"event": "step", "index": index, "total": total, "message": message}));
    }
    fn info(&self, message: &str) {
        self.emit(json!({"event": "info", "message": message}));
    }
    fn warn(&self, message: &str) {
        self.emit(json!({"event": "warn", "message": message}));
    }
    fn error(&self, message: &str) {
        self.emit(json!({"event": "error", "message": message}));
    }
    fn result(&self, data: Value) {
        self.emit(json!({"event": "result", "data": data}));
    }
    fn busy_start(&self, message: &str) {
        self.emit(json!({"event": "busy", "message": message, "state": "start"}));
    }
    fn busy_end(&self, message: &str, _ok: bool) {
        self.emit(json!({"event": "busy", "message": message, "state": "end"}));
    }
    fn end(&self, code: i32) {
        self.emit(json!({"event": "end", "code": code}));
    }
    fn progress(&self, fraction: f64) {
        self.emit(json!({"event": "progress", "fraction": (fraction.clamp(0.0, 1.0) * 10000.0).round() / 10000.0}));
    }
}
