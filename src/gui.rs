//! Interface graphique GTK 4 de libvirt-backup.
//!
//! L'interface ne s'exécute jamais en root : les opérations qui modifient le système sont lancées dans
//! un sous-processus `pkexec libvirt-backup ... --json` dont les événements (étapes, messages)
//! alimentent la barre de progression et le journal. La lecture (liste des archives, état du service,
//! détails) se fait directement dans le processus, avec le même moteur que la ligne de commande.

use crate::tr;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use gtk4 as gtk;
use serde_json::Value;

use crate::archive::{inspect_archive, list_archives, ArchiveInfo, ArchiveReport, Kind};
use crate::config::{
    confirm_word, is_confirm_word, Config, APP_ID, AUTHOR_NAME, BUILD_DATE, CLI_NAME, GROUPS, GUI_BIN_REL, LAUNCHER_REL, REPO_URL,
};
use crate::context::Context;
use crate::delete::warnings_for;
use crate::guiprefs;
use crate::i18n::{self, Lang};
use crate::report::QuietReporter;
use crate::service;
use crate::sys::{current_uid, human_size, system_time_to_local, user_by_uid, which};
use crate::VERSION;

const CSS: &str = "
.lb-mono { font-family: monospace; }
.lb-badge { padding: 1px 9px; border-radius: 10px; background: alpha(currentColor, 0.10); font-size: 0.85em; }
.lb-banner { background: alpha(#e5a50a, 0.22); border-radius: 6px; padding: 8px 12px; }
.lb-row { border-bottom: 1px solid alpha(currentColor, 0.08); }
progressbar.lb-gauge trough, progressbar.lb-gauge progress { min-height: 10px; }
progressbar.lb-gauge text { font-size: 0.85em; margin-bottom: 3px; }
";

// --------------------------------------------------------------------------
// Aides
// --------------------------------------------------------------------------

/// Commande qui lance le programme en ligne de commande (avec ses éventuels arguments de tête).
fn cli_command() -> Option<Vec<String>> {
    if let Some(forced) = std::env::var_os("LIBVIRT_BACKUP_CLI") {
        return Some(vec![forced.to_string_lossy().into_owned()]);
    }
    let launcher = Path::new("/").join(LAUNCHER_REL);
    let me = std::fs::read_link("/proc/self/exe").or_else(|_| std::env::current_exe()).ok()?;
    let installed_gui = Path::new("/").join(GUI_BIN_REL);
    if me == installed_gui && launcher.exists() {
        return Some(vec![launcher.to_string_lossy().into_owned()]); // installé : règle polkit dédiée
    }
    let sibling = me.parent()?.join(CLI_NAME);
    if sibling.is_file() {
        return Some(vec![sibling.to_string_lossy().into_owned()]);
    }
    if launcher.exists() {
        return Some(vec![launcher.to_string_lossy().into_owned()]);
    }
    which(CLI_NAME).map(|p| vec![p.to_string_lossy().into_owned()])
}

fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}

fn label(text: &str, markup: bool, dim: bool, wrap: bool, css: Option<&str>) -> gtk::Label {
    let lab = gtk::Label::new(None);
    if markup {
        lab.set_markup(text);
    } else {
        lab.set_text(text);
    }
    lab.set_xalign(0.0);
    lab.set_wrap(wrap);
    lab.set_wrap_mode(pango::WrapMode::WordChar);
    if dim {
        lab.set_opacity(0.7);
    }
    if let Some(class) = css {
        lab.add_css_class(class);
    }
    lab
}

fn plain(text: &str) -> gtk::Label {
    label(text, false, false, true, None)
}

fn button(text: &str, css: Option<&str>, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    if let Some(class) = css {
        b.add_css_class(class);
    }
    if !tooltip.is_empty() {
        b.set_tooltip_text(Some(tooltip));
    }
    b
}

/// Vrai si le thème d'icônes fournit `name` : une icône absente s'afficherait comme « image manquante ».
fn has_icon(name: &str) -> bool {
    gdk::Display::default().map(|d| gtk::IconTheme::for_display(&d).has_icon(name)).unwrap_or(false)
}

/// Bouton avec une icône symbolique devant le texte (le texte seul si le thème n'a pas l'icône).
fn icon_button(icon: &str, text: &str, css: Option<&str>, tooltip: &str) -> gtk::Button {
    let b = button("", css, tooltip);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if has_icon(icon) {
        content.append(&gtk::Image::from_icon_name(icon));
    }
    content.append(&gtk::Label::new(Some(text)));
    b.set_child(Some(&content));
    b
}

fn alert(parent: &impl IsA<gtk::Window>, message: &str, detail: &str, buttons: &[&str], callback: impl Fn(i32) + 'static) {
    let dialog = gtk::AlertDialog::builder().message(message).detail(detail).modal(true).build();
    dialog.set_buttons(buttons);
    dialog.set_default_button(0);
    dialog.set_cancel_button(0);
    dialog.choose(Some(parent), gio::Cancellable::NONE, move |result| callback(result.unwrap_or(0)));
}

fn info_box(parent: &impl IsA<gtk::Window>, message: &str, detail: &str) {
    alert(parent, message, detail, &[tr!("Close")], |_| {});
}

fn format_created(info: &ArchiveInfo) -> String {
    info.created.display_local()
}

fn escape(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

// --------------------------------------------------------------------------
// État
// --------------------------------------------------------------------------

struct Snapshot {
    config: Config,
    status: Value,
    archives: Vec<ArchiveInfo>,
}

struct Operation {
    title: String,
    errors: Vec<String>,
    end_code: Option<i64>,
    /// Opération qui envoie des événements `progress` (la sauvegarde) : la jauge suit alors ce
    /// pourcentage réel au lieu de l'avancement par étapes.
    gauge: bool,
}

#[derive(Default)]
struct State {
    root: Option<PathBuf>,
    /// Dossier affiché à la place du dossier enregistré (choix temporaire).
    view_dir: Option<PathBuf>,
    cli: Option<Vec<String>>,
    operation: Option<Operation>,
    archives: Vec<ArchiveInfo>,
    status: Value,
    last_fraction: f64,
    pulsing: bool,
    settings_loaded: bool,
    /// Langue enregistrée dans les préférences de la fenêtre (`system`, `en`, `fr`, `de`, `es`).
    language_pref: String,
    /// Code de langue transmis à la ligne de commande (`--language`).
    cli_lang: String,
    /// Une question « quitter quand même ? » est affichée.
    quit_asking: bool,
    /// L'utilisateur a confirmé qu'il quitte malgré l'opération en cours.
    quit_forced: bool,
    /// Nombre d'événements de progression reçus pendant l'opération en cours ou la dernière (tests).
    progress_events: u32,
}

/// Widgets de la fenêtre des préférences, gardés pour les tests.
struct Prefs {
    window: gtk::Window,
    dropdown: gtk::DropDown,
    notice: gtk::Label,
}

struct Ui {
    win: gtk::ApplicationWindow,
    switcher: gtk::StackSwitcher,
    banner: gtk::Label,
    folder_label: gtk::Label,
    listbox: gtk::ListBox,
    btn_backup: gtk::Button,
    btn_backup_to: gtk::Button,
    btn_restore: gtk::Button,
    btn_details: gtk::Button,
    btn_verify: gtk::Button,
    btn_delete: gtk::Button,
    btn_open: gtk::Button,
    btn_other_dir: gtk::Button,
    btn_saved_dir: gtk::Button,
    btn_restore_file: gtk::Button,
    service_title: gtk::Label,
    service_details: gtk::Label,
    legacy_note: gtk::Label,
    btn_install: gtk::Button,
    btn_start: gtk::Button,
    btn_stop: gtk::Button,
    btn_restart: gtk::Button,
    btn_enable: gtk::Button,
    btn_disable: gtk::Button,
    btn_uninstall: gtk::Button,
    e_user: gtk::Entry,
    e_dir: gtk::Entry,
    s_retention: gtk::SpinButton,
    s_safety: gtk::SpinButton,
    s_debounce: gtk::SpinButton,
    c_lean: gtk::CheckButton,
    btn_save: gtk::Button,
    activity_label: gtk::Label,
    progress: gtk::ProgressBar,
    expander: gtk::Expander,
    log: gtk::TextView,
    prefs: RefCell<Option<Prefs>>,
    st: RefCell<State>,
}

type Handle = Rc<Ui>;

impl Ui {
    fn new(app: &gtk::Application, root: Option<PathBuf>) -> Handle {
        let win =
            gtk::ApplicationWindow::builder().application(app).title(tr!("libvirt Backup")).default_width(980).default_height(760).build();
        let mk_spin = |lo: f64, hi: f64, step: f64| gtk::SpinButton::with_range(lo, hi, step);
        let s_debounce = mk_spin(0.2, 300.0, 0.5);
        s_debounce.set_digits(1);
        let e_dir = gtk::Entry::new();
        e_dir.set_hexpand(true);
        e_dir.set_placeholder_text(Some(tr!("empty = ~/virtualisation of the user")));
        let ui = Rc::new(Ui {
            win,
            switcher: gtk::StackSwitcher::new(),
            banner: label("", false, false, true, Some("lb-banner")),
            folder_label: label("", false, true, true, None),
            listbox: gtk::ListBox::new(),
            btn_backup: icon_button("document-save-symbolic", tr!("Back up now"), Some("suggested-action"), tr!("Creates an archive in the displayed folder")),
            btn_backup_to: icon_button("document-save-as-symbolic", tr!("Back up to…"), None, tr!("Creates an archive in another folder (disk, USB drive) without changing the setting")),
            btn_restore: icon_button("document-revert-symbolic", tr!("Restore…"), Some("destructive-action"), tr!("Restores the selected backup")),
            btn_details: icon_button("document-properties-symbolic", tr!("Details"), None, tr!("Content of the selected archive")),
            btn_verify: icon_button("object-select-symbolic", tr!("Verify"), None, tr!("Checks the integrity of the archive")),
            btn_delete: icon_button(
                "user-trash-symbolic",
                tr!("Delete…"),
                Some("destructive-action"),
                tr!("Permanently deletes the selected archive (after confirmation)"),
            ),
            btn_open: icon_button("folder-open-symbolic", tr!("Open the folder"), None, tr!("Opens the archive folder in the file manager")),
            btn_other_dir: icon_button("folder-symbolic", tr!("Show another folder…"), None, tr!("Browse the archives of another folder (USB drive, remote copy)")),
            btn_saved_dir: icon_button("go-home-symbolic", tr!("Saved folder"), None, tr!("Go back to the folder of the settings")),
            btn_restore_file: icon_button("document-open-symbolic", tr!("Restore a file…"), None, tr!("Restore a .tar.gz archive located anywhere")),
            service_title: label("", true, false, true, None),
            service_details: plain(""),
            legacy_note: label(
                tr!("A unit from the old shell scripts is in place: \"Install / update\" replaces it with this tool's unit (same service name, no duplicate)."),
                false,
                false,
                true,
                Some("lb-banner"),
            ),
            btn_install: icon_button("system-software-install-symbolic", tr!("Install / update"), Some("suggested-action"), tr!("Installs the program and the service, then makes a first backup")),
            btn_start: icon_button("media-playback-start-symbolic", tr!("Start"), None, ""),
            btn_stop: icon_button("media-playback-stop-symbolic", tr!("Stop"), None, ""),
            btn_restart: icon_button("view-refresh-symbolic", tr!("Restart"), None, ""),
            btn_enable: icon_button("emblem-ok-symbolic", tr!("Enable at boot"), None, ""),
            btn_disable: icon_button("window-close-symbolic", tr!("Disable at boot"), None, ""),
            btn_uninstall: icon_button("edit-delete-symbolic", tr!("Uninstall…"), Some("destructive-action"), ""),
            e_user: gtk::Entry::new(),
            e_dir,
            s_retention: mk_spin(1.0, 10000.0, 1.0),
            s_safety: mk_spin(0.0, 10000.0, 1.0),
            s_debounce,
            c_lean: gtk::CheckButton::with_label(tr!("Lean backup: skip compiled programs and very large files")),
            btn_save: icon_button("document-save-symbolic", tr!("Save"), Some("suggested-action"), ""),
            activity_label: label(tr!("Ready."), false, false, false, None),
            progress: gtk::ProgressBar::new(),
            expander: gtk::Expander::new(Some(tr!("Operation log"))),
            log: gtk::TextView::builder().editable(false).cursor_visible(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar).build(),
            prefs: RefCell::new(None),
            st: RefCell::new(State {
                root,
                cli: cli_command(),
                status: Value::Null,
                language_pref: guiprefs::load_language().unwrap_or_else(|| "system".to_string()),
                cli_lang: i18n::current().code().to_string(),
                ..Default::default()
            }),
        });
        ui.build();
        app.set_accels_for_action("win.quit", &["<Primary>q"]);
        app.set_accels_for_action("win.preferences", &["<Primary>comma"]);
        ui
    }

    // ----------------------------------------------------------- construction
    fn build(self: &Rc<Self>) {
        self.build_header();
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.add_titled(&self.build_backups_page(), Some("backups"), tr!("Backups"));
        stack.add_titled(&self.build_service_page(), Some("service"), tr!("Watcher"));
        stack.add_titled(&self.build_settings_page(), Some("settings"), tr!("Settings"));
        self.switcher.set_stack(Some(&stack));
        outer.append(&stack);
        outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        outer.append(&self.build_activity());
        self.win.set_child(Some(&outer));

        let ui = self.clone();
        glib::timeout_add_seconds_local(15, move || {
            if ui.st.borrow().operation.is_none() {
                ui.refresh(false);
            }
            glib::ControlFlow::Continue
        });
        let ui = self.clone();
        glib::timeout_add_local(Duration::from_millis(200), move || {
            if ui.st.borrow().pulsing {
                ui.progress.pulse();
            }
            glib::ControlFlow::Continue
        });
        self.refresh(false);
    }

    fn build_header(self: &Rc<Self>) {
        let header = gtk::HeaderBar::new();
        header.set_title_widget(Some(&self.switcher));
        let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
        reload.set_tooltip_text(Some(tr!("Refresh")));
        let ui = self.clone();
        reload.connect_clicked(move |_| ui.refresh(true));
        header.pack_start(&reload);

        // menu « hamburger » : Préférences et À propos, puis Quitter dans sa propre section
        let menu = gio::Menu::new();
        let general = gio::Menu::new();
        general.append(Some(tr!("Preferences")), Some("win.preferences"));
        general.append(Some(tr!("About")), Some("win.about"));
        menu.append_section(None, &general);
        let leave = gio::Menu::new();
        leave.append(Some(tr!("Quit")), Some("win.quit"));
        menu.append_section(None, &leave);
        let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).build();
        header.pack_end(&menu_btn);

        let preferences = gio::SimpleAction::new("preferences", None);
        let ui = self.clone();
        preferences.connect_activate(move |_, _| {
            ui.open_preferences();
        });
        self.win.add_action(&preferences);
        let about = gio::SimpleAction::new("about", None);
        let ui = self.clone();
        about.connect_activate(move |_, _| {
            ui.open_about();
        });
        self.win.add_action(&about);
        let quit = gio::SimpleAction::new("quit", None);
        let ui = self.clone();
        quit.connect_activate(move |_, _| ui.quit());
        self.win.add_action(&quit);
        self.win.set_titlebar(Some(&header));

        // la croix de la fenêtre passe par la même confirmation que « Quitter »
        let ui = self.clone();
        self.win.connect_close_request(move |_| {
            if ui.can_quit_now() {
                if let Some(app) = ui.win.application() {
                    app.quit();
                }
                glib::Propagation::Proceed
            } else {
                glib::Propagation::Stop
            }
        });
    }

    fn on_click(self: &Rc<Self>, b: &gtk::Button, f: impl Fn(&Rc<Ui>) + 'static) {
        let ui = self.clone();
        b.connect_clicked(move |_| f(&ui));
    }

    fn build_backups_page(self: &Rc<Self>) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        margins(&page, 14);
        self.banner.set_visible(false);
        page.append(&self.banner);

        let folder_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        self.folder_label.set_hexpand(true);
        folder_row.append(&self.folder_label);
        folder_row.append(&self.btn_other_dir);
        folder_row.append(&self.btn_saved_dir);
        page.append(&folder_row);

        self.listbox.set_selection_mode(gtk::SelectionMode::Single);
        let ui = self.clone();
        self.listbox.connect_row_selected(move |_, _| ui.update_sensitivity());
        let ui = self.clone();
        self.listbox.connect_row_activated(move |_, _| ui.show_details());
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_child(Some(&self.listbox));
        scrolled.set_vexpand(true);
        let frame = gtk::Frame::new(None);
        frame.set_child(Some(&scrolled));
        frame.set_vexpand(true);
        page.append(&frame);

        // Les boutons passent à la ligne quand la fenêtre est étroite.
        let bar = gtk::FlowBox::new();
        bar.set_selection_mode(gtk::SelectionMode::None);
        bar.set_activate_on_single_click(false);
        bar.set_max_children_per_line(10);
        bar.set_column_spacing(8);
        bar.set_row_spacing(8);
        for b in [
            &self.btn_backup,
            &self.btn_backup_to,
            &self.btn_restore,
            &self.btn_restore_file,
            &self.btn_details,
            &self.btn_verify,
            &self.btn_delete,
            &self.btn_open,
        ] {
            bar.append(b);
        }
        page.append(&bar);

        self.on_click(&self.btn_backup.clone(), |ui| ui.backup_now());
        self.on_click(&self.btn_backup_to.clone(), |ui| ui.backup_to());
        self.on_click(&self.btn_restore.clone(), |ui| ui.open_restore());
        self.on_click(&self.btn_restore_file.clone(), |ui| ui.restore_file());
        self.on_click(&self.btn_details.clone(), |ui| ui.show_details());
        self.on_click(&self.btn_verify.clone(), |ui| ui.verify_selected());
        self.on_click(&self.btn_delete.clone(), |ui| ui.delete_selected());
        self.on_click(&self.btn_open.clone(), |ui| ui.open_folder());
        self.on_click(&self.btn_other_dir.clone(), |ui| ui.choose_view_dir());
        self.on_click(&self.btn_saved_dir.clone(), |ui| {
            ui.st.borrow_mut().view_dir = None;
            ui.refresh(false);
        });
        page
    }

    fn build_service_page(self: &Rc<Self>) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        margins(&page, 14);
        page.append(&self.service_title);
        page.append(&self.service_details);
        self.legacy_note.set_visible(false);
        page.append(&self.legacy_note);
        page.append(&label(
            tr!("The watcher observes /etc/libvirt, AppArmor, systemd, VFIO, GRUB, network, ZFS and the local scripts; each change triggers a new archive."),
            false,
            true,
            true,
            None,
        ));
        let grid = gtk::FlowBox::new();
        grid.set_selection_mode(gtk::SelectionMode::None);
        grid.set_max_children_per_line(4);
        for b in [
            &self.btn_install,
            &self.btn_start,
            &self.btn_stop,
            &self.btn_restart,
            &self.btn_enable,
            &self.btn_disable,
            &self.btn_uninstall,
        ] {
            grid.append(b);
        }
        page.append(&grid);
        self.on_click(&self.btn_install.clone(), |ui| ui.install_service());
        for (b, action) in [
            (&self.btn_start, "start"),
            (&self.btn_stop, "stop"),
            (&self.btn_restart, "restart"),
            (&self.btn_enable, "enable"),
            (&self.btn_disable, "disable"),
        ] {
            self.on_click(&b.clone(), move |ui| ui.service_action(action));
        }
        self.on_click(&self.btn_uninstall.clone(), |ui| ui.uninstall_service());
        page
    }

    fn build_settings_page(self: &Rc<Self>) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        margins(&page, 14);
        let grid = gtk::Grid::new();
        grid.set_column_spacing(12);
        grid.set_row_spacing(10);
        let browse = button(tr!("Browse…"), None, tr!("Choose the folder where the archives are stored"));
        self.on_click(&browse, |ui| ui.choose_folder());
        let rows: [(&str, &gtk::Widget, Option<&gtk::Button>); 5] = [
            (tr!("User who owns the archives"), self.e_user.upcast_ref(), None),
            (tr!("Archive folder"), self.e_dir.upcast_ref(), Some(&browse)),
            (tr!("Automatic archives kept"), self.s_retention.upcast_ref(), None),
            (tr!("Safety archives kept (0 = all)"), self.s_safety.upcast_ref(), None),
            (tr!("Debounce delay of the watcher (s)"), self.s_debounce.upcast_ref(), None),
        ];
        for (row, (text, widget, extra)) in rows.iter().enumerate() {
            let lab = label(text, false, false, false, None);
            lab.set_halign(gtk::Align::Start);
            grid.attach(&lab, 0, row as i32, 1, 1);
            grid.attach(*widget, 1, row as i32, 1, 1);
            if let Some(b) = extra {
                grid.attach(*b, 2, row as i32, 1, 1);
            }
        }
        page.append(&grid);
        self.c_lean.set_tooltip_text(Some(
            tr!("In /usr/local/sbin and /usr/local/bin: compiled executables and files larger than 1 MiB are skipped; elsewhere: files larger than 16 MiB. Unticked, the whole content of the backed-up paths is archived (slower if these folders contain large programs)."),
        ));
        page.append(&self.c_lean);
        page.append(&label(
            tr!("Preferably choose a disk or USB drive separate from the system: otherwise a failure of the system disk would also take the backups with it. These settings are saved in /etc/libvirt-backup/config.json (administrator authentication required); the service is restarted if it is active."),
            false,
            true,
            true,
            None,
        ));
        self.btn_save.set_halign(gtk::Align::Start);
        page.append(&self.btn_save);
        self.on_click(&self.btn_save.clone(), |ui| ui.save_settings());
        page
    }

    fn build_activity(self: &Rc<Self>) -> gtk::Box {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&outer, 10);
        // l'activité sur une ligne, la jauge de progression en dessous sur toute la largeur
        self.activity_label.set_hexpand(true);
        self.activity_label.set_ellipsize(pango::EllipsizeMode::End);
        self.progress.set_hexpand(true);
        self.progress.add_css_class("lb-gauge");
        outer.append(&self.activity_label);
        outer.append(&self.progress);
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_child(Some(&self.log));
        scrolled.set_min_content_height(150);
        self.expander.set_child(Some(&scrolled));
        outer.append(&self.expander);
        outer
    }

    // ---------------------------------------------------------------- état
    fn update_sensitivity(&self) {
        let st = self.st.borrow();
        let idle = st.operation.is_none();
        let selected = self.listbox.selected_row().is_some();
        let service = &st.status["service"];
        let installed = service["installed"].as_bool().unwrap_or(false);
        let active = service["active"].as_str() == Some("active");
        let enabled = service["enabled"].as_str() == Some("enabled");
        self.btn_backup.set_sensitive(idle);
        self.btn_backup_to.set_sensitive(idle);
        self.btn_restore_file.set_sensitive(idle);
        for b in [&self.btn_restore, &self.btn_details, &self.btn_verify, &self.btn_delete] {
            b.set_sensitive(idle && selected);
        }
        self.btn_open.set_sensitive(true);
        self.btn_other_dir.set_sensitive(idle);
        self.btn_saved_dir.set_visible(st.view_dir.is_some());
        self.btn_install.set_sensitive(idle);
        self.btn_start.set_sensitive(idle && installed && !active);
        self.btn_stop.set_sensitive(idle && installed && active);
        self.btn_restart.set_sensitive(idle && installed);
        self.btn_enable.set_sensitive(idle && installed && !enabled);
        self.btn_disable.set_sensitive(idle && installed && enabled);
        self.btn_uninstall.set_sensitive(idle && (installed || st.status["installed"].as_bool().unwrap_or(false)));
        self.btn_save.set_sensitive(idle);
    }

    fn refresh(self: &Rc<Self>, reload_settings: bool) {
        if reload_settings {
            self.st.borrow_mut().settings_loaded = false;
        }
        let (root, view_dir) = {
            let st = self.st.borrow();
            (st.root.clone(), st.view_dir.clone())
        };
        let (tx, rx) = async_channel::bounded::<Result<Snapshot, String>>(1);
        std::thread::spawn(move || {
            let result = (|| {
                let ctx = Context::create_lenient(Arc::new(QuietReporter), root.as_deref(), false, view_dir.as_deref())
                    .map_err(|e| e.to_string())?;
                let status = service::status(&ctx);
                let archives = list_archives(&ctx.backup_dir());
                Ok(Snapshot { config: ctx.config.clone(), status, archives })
            })();
            let _ = tx.send_blocking(result);
        });
        let ui = self.clone();
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(snapshot)) => ui.apply(snapshot),
                Ok(Err(e)) => {
                    ui.banner.set_text(&tr!("Cannot read: {}", e));
                    ui.banner.set_visible(true);
                }
                Err(_) => {}
            }
        });
    }

    fn apply(self: &Rc<Self>, snap: Snapshot) {
        let selected_name = self.selected_archive().map(|a| a.name());
        {
            let mut st = self.st.borrow_mut();
            st.status = snap.status.clone();
            st.archives = snap.archives.clone();
        }
        while let Some(row) = self.listbox.row_at_index(0) {
            self.listbox.remove(&row);
        }
        for info in &snap.archives {
            let row = self.make_row(info);
            self.listbox.append(&row);
            if selected_name.as_deref() == Some(info.name().as_str()) {
                self.listbox.select_row(Some(&row));
            }
        }
        let status = &snap.status;
        let view = self.st.borrow().view_dir.clone();
        let shown = status["backup_dir"].as_str().unwrap_or("");
        let note = if view.is_some() { tr!("  (temporary view, ≠ saved folder)") } else { "" };
        self.folder_label.set_text(&tr!("Folder: {1}  ·  {0} archive(s){2}", status["archives"], shown, note));
        let mut problems: Vec<&str> = Vec::new();
        let installed = status["installed"].as_bool().unwrap_or(false);
        if !installed {
            problems.push(tr!("The tool is not installed on the system: automatic watching is inactive (\"Watcher\" tab)."));
        } else if !status["service"]["installed"].as_bool().unwrap_or(false) {
            problems.push(tr!("The watcher service is not installed (\"Watcher\" tab)."));
        } else if status["service"]["active"].as_str() != Some("active") {
            problems.push(tr!("The watcher service is not active."));
        }
        if snap.archives.is_empty() && installed {
            problems.push(tr!("No archive yet."));
        }
        if status["backup_dir_exists"].as_bool() == Some(false) {
            problems.push(tr!("The archive folder does not exist (drive not mounted?)."));
        }
        self.banner.set_text(&problems.join(" "));
        self.banner.set_visible(!problems.is_empty());

        let state = &status["service"];
        let active = state["active"].as_str().unwrap_or("");
        if active == "active" {
            self.service_title.set_markup(tr!("<big><b>● Watcher active</b></big>"));
        } else if state["installed"].as_bool().unwrap_or(false) {
            self.service_title.set_markup(&tr!("<big><b>○ Service installed — {}</b></big>", escape(active)));
        } else {
            self.service_title.set_markup(tr!("<big><b>○ Service not installed</b></big>"));
        }
        let latest = if status["latest"].is_null() {
            tr!("none").to_string()
        } else {
            format!(
                "{} ({})",
                status["latest"]["created"].as_str().unwrap_or("").replace('T', " "),
                status["latest"]["size_human"].as_str().unwrap_or("")
            )
        };
        self.service_details.set_text(&tr!(
            "Program installed: {0}\nsystemd service: {1}   ·   at boot: {2}\nUser: {3}\nArchive folder: {4}\nLast backup: {5}",
            match (installed, status["layout"].as_str()) {
                (true, Some("package")) => tr!("yes (dpkg package)"),
                (true, _) => tr!("yes"),
                _ => tr!("no"),
            },
            active,
            state["enabled"].as_str().unwrap_or(""),
            status["backup_user"].as_str().unwrap_or(""),
            shown,
            latest
        ));
        self.legacy_note.set_visible(status["legacy_unit"].as_bool().unwrap_or(false));

        let loaded = self.st.borrow().settings_loaded;
        if !loaded {
            // réglages enregistrés (et non l'affichage temporaire)
            let cfg = &snap.config;
            self.e_user.set_text(&cfg.backup_user);
            self.e_dir.set_text(&cfg.backup_dir);
            self.s_retention.set_value(cfg.retention as f64);
            self.s_safety.set_value(cfg.safety_retention as f64);
            self.s_debounce.set_value(cfg.debounce);
            self.c_lean.set_active(cfg.lean_backup);
            self.st.borrow_mut().settings_loaded = true;
        }
        self.update_sensitivity();
    }

    fn make_row(&self, info: &ArchiveInfo) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        margins(&line, 8);
        line.add_css_class("lb-row");
        let left = gtk::Box::new(gtk::Orientation::Vertical, 2);
        left.set_hexpand(true);
        left.append(&label(&format!("<b>{}</b>", escape(&format_created(info))), true, false, false, None));
        left.append(&label(&info.name(), false, true, false, Some("lb-mono")));
        line.append(&left);
        for text in [human_size(info.size), info.kind.label().to_string()] {
            let badge = label(&text, false, false, false, Some("lb-badge"));
            badge.set_valign(gtk::Align::Center);
            line.append(&badge);
        }
        row.set_child(Some(&line));
        row
    }

    fn selected_archive(&self) -> Option<ArchiveInfo> {
        let row = self.listbox.selected_row()?;
        let st = self.st.borrow();
        st.archives.get(usize::try_from(row.index()).ok()?).cloned()
    }

    // ------------------------------------------------------------ opérations
    fn start_operation(self: &Rc<Self>, title: &str, args: Vec<String>, privileged: bool) {
        let (cli, system_root, root) = {
            let st = self.st.borrow();
            if st.operation.is_some() {
                return;
            }
            (st.cli.clone(), st.root.is_none(), st.root.clone())
        };
        let Some(prefix) = cli else {
            info_box(&self.win, tr!("Command not found"), tr!("Cannot locate the libvirt-backup command."));
            return;
        };
        let gauge = args.first().map(String::as_str) == Some("backup");
        let language = self.st.borrow().cli_lang.clone();
        let mut cmd: Vec<String> = prefix;
        // la ligne de commande, lancée par pkexec, ne voit pas l'environnement : la langue est transmise
        cmd.push("--language".into());
        cmd.push(language);
        cmd.extend(args);
        cmd.push("--json".into());
        if let Some(r) = &root {
            cmd.push("--root".into());
            cmd.push(r.to_string_lossy().into_owned());
        }
        if privileged && current_uid() != 0 && system_root {
            match which("pkexec") {
                Some(p) => cmd.insert(0, p.to_string_lossy().into_owned()),
                None => {
                    info_box(&self.win, &tr!("{}: failed", title), tr!("pkexec cannot be found: install the polkit package (pkexec)."));
                    return;
                }
            }
        }
        {
            let mut st = self.st.borrow_mut();
            st.operation = Some(Operation { title: title.to_string(), errors: Vec::new(), end_code: None, gauge });
            st.last_fraction = 0.0;
            st.pulsing = true;
            st.progress_events = 0;
        }
        self.activity_label.set_text(&format!("{title}…"));
        self.progress.set_fraction(0.0);
        self.progress.set_show_text(false);
        self.log.buffer().set_text("");
        self.expander.set_expanded(true);
        self.append(&format!("── {title} ──"));
        self.update_sensitivity();

        let (tx, rx) = async_channel::unbounded::<(Option<String>, Option<(i32, String)>)>();
        std::thread::spawn(move || {
            let spawned =
                Command::new(&cmd[0]).args(&cmd[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn();
            match spawned {
                Err(e) => {
                    let _ = tx.send_blocking((None, Some((127, e.to_string()))));
                }
                Ok(mut child) => {
                    if let Some(out) = child.stdout.take() {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            let _ = tx.send_blocking((Some(line), None));
                        }
                    }
                    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                    let _ = tx.send_blocking((None, Some((code, String::new()))));
                }
            }
        });
        let ui = self.clone();
        glib::spawn_future_local(async move {
            while let Ok((line, finished)) = rx.recv().await {
                if let Some(line) = line {
                    ui.on_line(&line);
                }
                if let Some((code, message)) = finished {
                    ui.on_finished(code, &message);
                    break;
                }
            }
        });
    }

    fn append(&self, text: &str) {
        let buf = self.log.buffer();
        buf.insert(&mut buf.end_iter(), &format!("{text}\n"));
        let mark = buf.create_mark(None, &buf.end_iter(), false);
        self.log.scroll_to_mark(&mark, 0.0, false, 0.0, 0.0);
    }

    fn on_line(&self, line: &str) {
        let Ok(data) = serde_json::from_str::<Value>(line) else {
            self.append(line);
            return;
        };
        let Some(event) = data["event"].as_str() else {
            self.append(line);
            return;
        };
        let message = data["message"].as_str().unwrap_or("");
        match event {
            "progress" => {
                // pourcentage réel de la sauvegarde : il remplace l'avancement par étapes
                let fraction = data["fraction"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
                let gauge = {
                    let mut st = self.st.borrow_mut();
                    let gauge = st.operation.as_ref().map(|o| o.gauge).unwrap_or(false);
                    if gauge && fraction >= st.last_fraction {
                        st.pulsing = false;
                        st.last_fraction = fraction;
                        st.progress_events += 1;
                    }
                    gauge
                };
                if gauge {
                    self.show_fraction(fraction);
                }
            }
            "step" => {
                let (index, total) = (data["index"].as_f64().unwrap_or(1.0), data["total"].as_f64().unwrap_or(1.0).max(1.0));
                let (title, gauge) = {
                    let mut st = self.st.borrow_mut();
                    let gauge = st.operation.as_ref().map(|o| o.gauge).unwrap_or(false);
                    if !gauge {
                        // jamais en arrière : l'installation enchaîne une sauvegarde qui a ses propres étapes
                        st.last_fraction = st.last_fraction.max(((index - 1.0) / total).max(0.0));
                        st.pulsing = false;
                    }
                    (st.operation.as_ref().map(|o| o.title.clone()).unwrap_or_default(), gauge)
                };
                if !gauge {
                    let fraction = self.st.borrow().last_fraction;
                    self.progress.set_fraction(fraction);
                }
                self.activity_label.set_text(&tr!("{0}: {1}", title, message));
                self.append(&format!("[{index}/{total}] {message}"));
            }
            "busy" => {
                let start = data["state"].as_str() == Some("start");
                let (last, gauge) = {
                    let mut st = self.st.borrow_mut();
                    let gauge = st.operation.as_ref().map(|o| o.gauge).unwrap_or(false);
                    if !gauge {
                        st.pulsing = start;
                    }
                    (st.last_fraction, gauge)
                };
                if start {
                    self.append(message);
                } else if !gauge {
                    self.progress.set_fraction(last);
                }
            }
            "info" => self.append(message),
            "warn" => self.append(&tr!("WARNING: {}", message)),
            "error" => {
                if let Some(op) = self.st.borrow_mut().operation.as_mut() {
                    op.errors.push(message.to_string());
                }
                self.append(&tr!("ERROR: {}", message));
            }
            "end" => {
                if let Some(op) = self.st.borrow_mut().operation.as_mut() {
                    op.end_code = data["code"].as_i64();
                }
            }
            _ => {}
        }
    }

    /// Positionne la jauge et écrit le pourcentage dessus.
    fn show_fraction(&self, fraction: f64) {
        self.progress.set_fraction(fraction);
        self.progress.set_text(Some(&tr!("{}%", (fraction * 100.0).floor() as u32)));
        self.progress.set_show_text(true);
    }

    fn on_finished(self: &Rc<Self>, returncode: i32, message: &str) {
        let (op, last_fraction) = {
            let mut st = self.st.borrow_mut();
            st.pulsing = false;
            (st.operation.take(), st.last_fraction)
        };
        let Some(op) = op else { return };
        let code = op.end_code.unwrap_or(returncode as i64);
        let ok = code == 0;
        if ok {
            self.show_fraction(1.0);
        } else if op.gauge {
            self.show_fraction(last_fraction);
        } else {
            self.progress.set_fraction(last_fraction);
        }
        self.activity_label.set_text(&tr!("{0}: {1}", op.title, if ok { tr!("done") } else { tr!("failed") }));
        if !message.is_empty() {
            self.append(message);
        }
        if !ok {
            let detail = if (returncode == 126 || returncode == 127) && op.end_code.is_none() {
                tr!("Authentication cancelled, refused, or command not found.").to_string()
            } else if op.errors.is_empty() {
                tr!("Exit code {}. See the operation log.", code)
            } else {
                op.errors[op.errors.len().saturating_sub(3)..].join("\n")
            };
            info_box(&self.win, &tr!("{}: failed", op.title), &detail);
        }
        self.update_sensitivity();
        self.refresh(false);
    }

    // --------------------------------------------------------------- actions
    fn displayed_dir(&self) -> Option<PathBuf> {
        self.st.borrow().view_dir.clone()
    }

    fn backup_now(self: &Rc<Self>) {
        let mut args = vec!["backup".to_string()];
        if let Some(dir) = self.displayed_dir() {
            args.extend(["--backup-dir".into(), dir.to_string_lossy().into_owned()]);
        }
        self.start_operation(tr!("Backup"), args, true);
    }

    fn pick_folder(self: &Rc<Self>, title: &str, then: impl Fn(&Rc<Ui>, PathBuf) + 'static) {
        let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
        let ui = self.clone();
        dialog.select_folder(Some(&self.win), gio::Cancellable::NONE, move |result| {
            if let Ok(folder) = result {
                if let Some(path) = folder.path() {
                    then(&ui, path);
                }
            }
        });
    }

    fn backup_to(self: &Rc<Self>) {
        self.pick_folder(tr!("Back up to the folder…"), |ui, dir| {
            let args = vec!["backup".to_string(), "--backup-dir".into(), dir.to_string_lossy().into_owned()];
            ui.start_operation(&tr!("Backup to {}", dir.display()), args, true);
        });
    }

    fn choose_view_dir(self: &Rc<Self>) {
        self.pick_folder(tr!("Show the archives of a folder"), |ui, dir| {
            ui.st.borrow_mut().view_dir = Some(dir);
            ui.refresh(false);
        });
    }

    fn choose_folder(self: &Rc<Self>) {
        self.pick_folder(tr!("Archive folder"), |ui, dir| ui.e_dir.set_text(&dir.to_string_lossy()));
    }

    fn open_folder(self: &Rc<Self>) {
        let dir = self.st.borrow().status["backup_dir"].as_str().map(PathBuf::from);
        let Some(dir) = dir else { return };
        let uri = gio::File::for_path(&dir).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE) {
            info_box(&self.win, tr!("Cannot open the folder"), &e.to_string());
        }
    }

    /// Lit l'archive hors du thread graphique puis appelle `then`.
    fn with_report(self: &Rc<Self>, info: ArchiveInfo, then: impl Fn(&Rc<Ui>, ArchiveInfo, ArchiveReport) + 'static) {
        self.activity_label.set_text(tr!("Reading the archive…"));
        self.st.borrow_mut().pulsing = true;
        let (tx, rx) = async_channel::bounded(1);
        let path = info.path.clone();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(inspect_archive(&path).map_err(|e| e.to_string()));
        });
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let result = rx.recv().await;
            {
                let mut st = ui.st.borrow_mut();
                st.pulsing = false;
            }
            ui.progress.set_fraction(0.0);
            ui.activity_label.set_text(tr!("Ready."));
            match result {
                Ok(Ok(report)) => then(&ui, info, report),
                Ok(Err(e)) => info_box(&ui.win, tr!("Unreadable archive"), &e),
                Err(_) => {}
            }
        });
    }

    fn show_details(self: &Rc<Self>) {
        if let Some(info) = self.selected_archive() {
            self.with_report(info, |ui, info, report| details_window(ui, &info, &report));
        }
    }

    fn verify_selected(self: &Rc<Self>) {
        if let Some(info) = self.selected_archive() {
            self.with_report(info, |ui, info, report| {
                if report.valid() && !report.system_members.is_empty() {
                    info_box(
                        &ui.win,
                        tr!("Valid archive"),
                        &tr!(
                            "{0}\n{1} entries, gzip/tar integrity verified, {2} restorable item(s).",
                            info.name(),
                            report.entries,
                            report.system_members.len()
                        ),
                    );
                } else {
                    let why: Vec<&str> = report.unknown.iter().chain(report.dangerous.iter()).take(5).map(String::as_str).collect();
                    info_box(
                        &ui.win,
                        tr!("Unexpected format"),
                        &if why.is_empty() { tr!("No recognized system path.").to_string() } else { why.join(", ") },
                    );
                }
            });
        }
    }

    /// Texte de la boîte de confirmation : ce qui sera supprimé et ce que cela entraîne.
    fn delete_message(&self, info: &ArchiveInfo) -> (String, String) {
        let known = self.st.borrow().archives.clone();
        let mut detail = tr!(
            "{0}\n{1}  ·  {2}  ·  {3}\n\nDeletion is PERMANENT: the archive is not moved to the trash and cannot be recovered.",
            info.name(),
            format_created(info),
            human_size(info.size),
            info.kind.label()
        );
        for warning in warnings_for(&known, std::slice::from_ref(info)) {
            detail.push_str(&format!("\n\n⚠ {warning}"));
        }
        (tr!("Delete this archive?").to_string(), detail)
    }

    fn delete_selected(self: &Rc<Self>) {
        let Some(info) = self.selected_archive() else { return };
        let (message, detail) = self.delete_message(&info);
        let ui = self.clone();
        // Le bouton par défaut et la touche Échap sont « Annuler » : rien n'est supprimé par accident.
        alert(&self.win, &message, &detail, &[tr!("Cancel"), tr!("Delete permanently")], move |choice| {
            if choice == 1 {
                ui.delete_archive(&info);
            }
        });
    }

    fn delete_archive(self: &Rc<Self>, info: &ArchiveInfo) {
        let mut args = vec!["delete".to_string(), info.name(), "--yes".into()];
        if let Some(dir) = self.displayed_dir() {
            args.extend(["--backup-dir".into(), dir.to_string_lossy().into_owned()]);
        }
        self.start_operation(&tr!("Deleting {}", info.name()), args, true);
    }

    fn open_restore(self: &Rc<Self>) {
        if let Some(info) = self.selected_archive() {
            self.with_report(info, |ui, info, report| restore_window(ui, &info, &report));
        }
    }

    fn restore_file(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(tr!(".tar.gz archives")));
        filter.add_pattern("*.tar.gz");
        filter.add_pattern("*.tgz");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(tr!("Restore an archive")).modal(true).filters(&filters).build();
        let ui = self.clone();
        dialog.open(Some(&self.win), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let Ok(meta) = std::fs::metadata(&path) else {
                info_box(&ui.win, tr!("Unreadable file"), &path.to_string_lossy());
                return;
            };
            let created = meta.modified().map(system_time_to_local).unwrap_or_else(|_| crate::sys::now_local());
            let info = ArchiveInfo { path, kind: Kind::External, size: meta.len(), created, sort_key: String::new() };
            ui.with_report(info, |ui, info, report| restore_window(ui, &info, &report));
        });
    }

    fn install_service(self: &Rc<Self>) {
        let user = self.e_user.text().trim().to_string();
        let user = if user.is_empty() { user_by_uid(current_uid()).map(|u| u.name).unwrap_or_default() } else { user };
        let mut args = vec![
            "install".to_string(),
            "--yes".into(),
            "--user".into(),
            user,
            "--retention".into(),
            self.s_retention.value_as_int().to_string(),
        ];
        let dir = self.e_dir.text().trim().to_string();
        if !dir.is_empty() {
            args.extend(["--backup-dir".into(), dir]);
        }
        self.st.borrow_mut().settings_loaded = false;
        self.start_operation(tr!("Installation"), args, true);
    }

    fn uninstall_service(self: &Rc<Self>) {
        let ui = self.clone();
        alert(
            &self.win,
            tr!("Uninstall the watcher service?"),
            tr!("The archives already created are kept in all cases."),
            &[tr!("Cancel"), tr!("Remove the service"), tr!("Also remove the program and the settings")],
            move |index| {
                if index == 1 || index == 2 {
                    let mut args = vec!["uninstall".to_string()];
                    if index == 2 {
                        args.push("--purge".into());
                    }
                    ui.start_operation(tr!("Uninstallation"), args, true);
                }
            },
        );
    }

    fn service_action(self: &Rc<Self>, action: &str) {
        let title = match action {
            "start" => tr!("Starting"),
            "stop" => tr!("Stopping"),
            "restart" => tr!("Restarting"),
            "enable" => tr!("Enabling at boot"),
            _ => tr!("Disabling at boot"),
        };
        self.start_operation(title, vec!["service".into(), action.into()], true);
    }

    fn save_settings(self: &Rc<Self>) {
        let args = vec![
            "config".to_string(),
            "set".into(),
            format!("backup_user={}", self.e_user.text().trim()),
            format!("backup_dir={}", self.e_dir.text().trim()),
            format!("retention={}", self.s_retention.value_as_int()),
            format!("safety_retention={}", self.s_safety.value_as_int()),
            format!("debounce={:.1}", self.s_debounce.value()),
            format!("lean_backup={}", self.c_lean.is_active()),
            "--restart-service".into(),
        ];
        self.st.borrow_mut().settings_loaded = false;
        self.start_operation(tr!("Saving the settings"), args, true);
    }

    // ------------------------------------------------------------ menu : quitter
    /// Vrai si l'on peut quitter tout de suite. Si une opération est en cours, pose la question (une seule
    /// fois à la fois) : l'opération, lancée dans un sous-processus, se terminerait sans affichage.
    fn can_quit_now(self: &Rc<Self>) -> bool {
        let (title, asking, forced) = {
            let st = self.st.borrow();
            (st.operation.as_ref().map(|o| o.title.clone()), st.quit_asking, st.quit_forced)
        };
        let Some(title) = title else { return true };
        if forced {
            return true;
        }
        if !asking {
            self.st.borrow_mut().quit_asking = true;
            let ui = self.clone();
            alert(
                &self.win,
                tr!("An operation is in progress"),
                &tr!(
                    "\"{}\" is not finished. If you quit now, the operation will finish in the background, without progress display.",
                    title
                ),
                &[tr!("Cancel"), tr!("Quit anyway")],
                move |choice| {
                    ui.st.borrow_mut().quit_asking = false;
                    if choice == 1 {
                        ui.st.borrow_mut().quit_forced = true;
                        ui.quit();
                    }
                },
            );
        }
        false
    }

    /// Action « Quitter » du menu (Ctrl+Q).
    fn quit(self: &Rc<Self>) {
        if self.can_quit_now() {
            if let Some(app) = self.win.application() {
                app.quit();
            }
        }
    }

    // --------------------------------------------------------- menu : préférences
    /// Fenêtre des préférences : choix de la langue de l'interface. Le changement est enregistré tout de suite
    /// (`gui.json`) mais ne s'applique à la fenêtre qu'au prochain démarrage.
    fn open_preferences(self: &Rc<Self>) {
        if let Some(existing) = self.prefs.borrow().as_ref() {
            existing.window.present();
            return;
        }
        let window = gtk::Window::builder()
            .transient_for(&self.win)
            .modal(true)
            .title(tr!("Preferences"))
            .default_width(440)
            .resizable(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        margins(&content, 20);

        let names: Vec<String> = i18n::PREFERENCES
            .iter()
            .map(|p| match Lang::from_code(p) {
                Some(lang) => lang.native_name().to_string(),
                None => tr!("System language").to_string(),
            })
            .collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let dropdown = gtk::DropDown::from_strings(&name_refs);
        dropdown.set_hexpand(true);
        let saved = self.st.borrow().language_pref.clone();
        dropdown.set_selected(i18n::PREFERENCES.iter().position(|p| *p == saved).unwrap_or(0) as u32);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let caption = gtk::Label::new(Some(tr!("Language:")));
        caption.set_xalign(0.0);
        row.append(&caption);
        row.append(&dropdown);
        content.append(&row);

        let notice = label(tr!("Restart the application for this language change to take effect."), false, true, true, None);
        notice.set_visible(false);
        content.append(&notice);

        let close = button(tr!("Close"), None, "");
        close.set_halign(gtk::Align::End);
        content.append(&close);
        window.set_child(Some(&content));

        let ui = self.clone();
        let notice2 = notice.clone();
        dropdown.connect_selected_notify(move |dd| {
            let Some(preference) = i18n::PREFERENCES.get(dd.selected() as usize) else { return };
            if *preference == ui.st.borrow().language_pref {
                return;
            }
            ui.set_language_preference(preference);
            notice2.set_visible(true);
        });
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
        let ui = self.clone();
        window.connect_close_request(move |_| {
            ui.prefs.borrow_mut().take();
            glib::Propagation::Proceed
        });
        *self.prefs.borrow_mut() = Some(Prefs { window: window.clone(), dropdown, notice });
        window.present();
    }

    /// Enregistre la langue choisie. La ligne de commande lancée ensuite l'utilise tout de suite ; les textes
    /// de la fenêtre déjà construits changent au redémarrage.
    fn set_language_preference(&self, preference: &str) {
        if let Err(e) = guiprefs::save_language(preference) {
            self.append(&tr!("WARNING: {}", e));
        }
        let mut st = self.st.borrow_mut();
        st.language_pref = preference.to_string();
        st.cli_lang = i18n::resolve(preference).lang.code().to_string();
    }

    // ------------------------------------------------------------- menu : à propos
    /// Fenêtre « À propos », avec les mêmes mentions que Planche-Contact.
    fn open_about(self: &Rc<Self>) -> gtk::Window {
        let window = gtk::Window::builder().transient_for(&self.win).modal(true).title(tr!("About")).resizable(false).build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_top(28);
        content.set_margin_bottom(24);
        content.set_margin_start(40);
        content.set_margin_end(40);

        let centered = |markup: &str, top: i32| {
            let lab = gtk::Label::new(None);
            lab.set_markup(markup);
            lab.set_justify(gtk::Justification::Center);
            lab.set_wrap(true);
            lab.set_margin_top(top);
            lab
        };

        if has_icon(APP_ID) {
            let logo = gtk::Image::from_icon_name(APP_ID);
            logo.set_pixel_size(96);
            content.append(&logo);
        }
        content.append(&centered(&format!("<span size='x-large'><b>{}</b></span>", escape(tr!("libvirt Backup"))), 8));

        let lang = i18n::current();
        let version = tr!("Version {0}  —  Built on {1}", VERSION, i18n::format_date(lang, BUILD_DATE));
        content.append(&centered(&format!("<span size='medium'>{}</span>", escape(&version)), 6));
        content.append(&centered(
            &format!(
                "<span size='small'>{}</span>",
                escape(tr!(
                    "Backup and restore of the virtualization configuration (libvirt/QEMU, AppArmor, VFIO, systemd, network, ZFS)."
                ))
            ),
            16,
        ));
        content.append(&centered(&format!("<span size='small'>{}</span>", escape(&tr!("Developed by {}", AUTHOR_NAME))), 20));
        // mention transparente de l'aide de l'IA (le lien ouvre le navigateur)
        let ai_credit = tr!("Designed with the help of {} (Anthropic)", "<a href=\"https://claude.ai\">Claude.ai</a>");
        content.append(&centered(&format!("<span size='small'>{ai_credit}</span>"), 4));
        content.append(&centered(&format!("<span size='small'>{}</span>", escape(tr!("Distributed under the GNU GPL v3 license"))), 10));
        let host = REPO_URL.trim_start_matches("https://");
        content.append(&centered(&format!("<span size='small'><a href=\"{REPO_URL}\">{host}</a></span>"), 4));

        let close = button(tr!("Close"), None, "");
        close.set_halign(gtk::Align::Center);
        close.set_margin_top(18);
        let w = window.clone();
        close.connect_clicked(move |_| w.close());
        content.append(&close);
        window.set_child(Some(&content));
        window.present();
        window
    }
}

// --------------------------------------------------------------------------
// Fenêtres secondaires
// --------------------------------------------------------------------------

fn details_window(ui: &Rc<Ui>, info: &ArchiveInfo, report: &ArchiveReport) {
    let win = gtk::Window::builder()
        .transient_for(&ui.win)
        .modal(false)
        .title(tr!("Details — {}", info.name()))
        .default_width(760)
        .default_height(600)
        .build();
    let mut lines = vec![
        tr!("Archive: {}", info.path.display()),
        tr!(
            "Size: {0}   Entries: {1}   Valid format: {2}",
            human_size(info.size),
            report.entries,
            if report.valid() { tr!("yes") } else { tr!("NO") }
        ),
    ];
    for (key, text) in
        [("created", tr!("Created on")), ("version", tr!("Tool version")), ("hostname", tr!("Host")), ("kernel", tr!("Kernel"))]
    {
        if let Some(v) = report.manifest_str(key) {
            lines.push(tr!("{0}: {1}", text, v));
        }
    }
    lines.push(String::new());
    lines.push(tr!("Content by group:").into());
    for g in GROUPS {
        lines.push(format!("  {:9} {:5}  {}", g.id, report.group_count(g.id), g.label()));
    }
    if let Some(domains) = report.manifest.get("domains").and_then(Value::as_object).filter(|d| !d.is_empty()) {
        lines.push(String::new());
        lines.push(tr!("Virtual machines at the time of the backup:").into());
        for (name, state) in domains {
            lines.push(tr!("  {1}: {0}", state.as_str().unwrap_or("?"), name));
        }
    }
    if !report.unknown.is_empty() || !report.dangerous.is_empty() {
        let refused: Vec<&str> = report.unknown.iter().chain(report.dangerous.iter()).take(10).map(String::as_str).collect();
        lines.push(String::new());
        lines.push(tr!("Entries refused at restore: {}", refused.join(", ")));
    }
    if !report.readme.is_empty() {
        lines.push(String::new());
        lines.push("─".repeat(60));
        lines.push(report.readme.clone());
    }
    let view = gtk::TextView::builder().editable(false).cursor_visible(false).monospace(true).wrap_mode(gtk::WrapMode::WordChar).build();
    margins(&view, 10);
    view.buffer().set_text(&lines.join("\n"));
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_child(Some(&view));
    win.set_child(Some(&scrolled));
    win.present();
}

fn restore_window(ui: &Rc<Ui>, info: &ArchiveInfo, report: &ArchiveReport) {
    let win = gtk::Window::builder().transient_for(&ui.win).modal(true).title(tr!("Restore a backup")).default_width(660).build();
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 10);
    margins(&outer, 16);
    outer.append(&label(&tr!("<b>Restore the backup of {}</b>", escape(&format_created(info))), true, false, true, None));
    outer.append(&label(
        &format!("{}  ·  {}  ·  {}", info.name(), human_size(info.size), info.kind.label()),
        false,
        true,
        true,
        Some("lb-mono"),
    ));
    let manifest_present = report.manifest.as_object().map(|m| !m.is_empty()).unwrap_or(false);
    let omitted = if manifest_present {
        report.manifest.get("vm_state_included") == Some(&Value::Bool(false))
    } else {
        report.readme.contains("NVRAM et TPM omis")
    };
    if omitted {
        outer.append(&label(
            tr!("The UEFI/NVRAM and TPM states were not backed up (VMs were running at that time)."),
            false,
            false,
            true,
            Some("lb-banner"),
        ));
    }
    outer.append(&label(tr!("<b>Items to restore</b>"), true, false, true, None));

    let go = button(tr!("Restore"), Some("destructive-action"), "");
    let simulate = button(tr!("Simulate"), None, tr!("Shows what would be restored, without changing anything"));
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(confirm_word()));
    let mut checks: Vec<(&'static str, gtk::CheckButton)> = Vec::new();
    for group in GROUPS {
        let n = report.group_count(group.id);
        let mut text = if n > 0 { tr!("{0} — {1} item(s)", group.label(), n) } else { tr!("{} — not in this archive", group.label()) };
        if !group.caution().is_empty() && n > 0 {
            text.push_str(&format!("   ({})", group.caution()));
        }
        let check = gtk::CheckButton::with_label(&text);
        check.set_active(n > 0);
        check.set_sensitive(n > 0);
        outer.append(&check);
        checks.push((group.id, check));
    }
    let checks = Rc::new(checks);
    let update = {
        let (checks, entry, go, simulate) = (checks.clone(), entry.clone(), go.clone(), simulate.clone());
        Rc::new(move || {
            let chosen = checks.iter().any(|(_, c)| c.is_active());
            simulate.set_sensitive(chosen);
            go.set_sensitive(chosen && is_confirm_word(&entry.text()));
        })
    };
    for (_, check) in checks.iter() {
        let update = update.clone();
        check.connect_toggled(move |_| update());
    }
    let u = update.clone();
    entry.connect_changed(move |_| u());

    outer.append(&label(
        tr!("The restore merges: no file missing from the archive is deleted. A safety archive of the current state is created before any change. All VMs must be stopped; virtual disks are not restored."),
        false,
        true,
        true,
        None,
    ));
    let word = confirm_word();
    outer.append(&label(&tr!("To confirm, type <b>{}</b>:", word), true, false, true, None));
    outer.append(&entry);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = button(tr!("Cancel"), None, "");
    {
        let win = win.clone();
        cancel.connect_clicked(move |_| win.close());
    }
    for (b, dry_run) in [(&simulate, true), (&go, false)] {
        let (win, ui, checks, path) = (win.clone(), ui.clone(), checks.clone(), info.path.clone());
        b.connect_clicked(move |_| {
            let selected: Vec<&str> = checks.iter().filter(|(_, c)| c.is_active()).map(|(id, _)| *id).collect();
            let mut args = vec!["restore".to_string(), path.to_string_lossy().into_owned(), "--only".into(), selected.join(",")];
            args.push(if dry_run { "--dry-run".into() } else { "--yes".into() });
            win.close();
            ui.start_operation(if dry_run { tr!("Restore simulation") } else { tr!("Restoring") }, args, !dry_run);
        });
    }
    for b in [&cancel, &simulate, &go] {
        buttons.append(b);
    }
    outer.append(&buttons);
    win.set_child(Some(&outer));
    update();
    win.present();
}

// --------------------------------------------------------------------------
// Application
// --------------------------------------------------------------------------

/// Choisit la langue de la fenêtre : `--language`, puis `LIBVIRT_BACKUP_LANG`, puis les préférences de
/// l'utilisateur (`gui.json`), puis la langue du système. Quand une langue est choisie explicitement,
/// `LANGUAGE` l'impose aussi aux textes que GTK fournit lui-même (boutons des sélecteurs de fichiers…).
fn init_language(cli: Option<&str>) -> i18n::Resolved {
    let env = std::env::var(i18n::ENV_LANGUAGE).ok();
    let saved = guiprefs::load_language();
    let preference = i18n::pick_preference(cli, env.as_deref(), saved.as_deref());
    let resolved = i18n::set_language(&preference);
    if preference != "system" {
        std::env::set_var("LANGUAGE", resolved.lang.code());
    }
    resolved
}

impl Ui {
    /// Au démarrage : prévient si la langue du système n'est pas traduite (l'anglais est utilisé).
    fn show_startup_notice(&self, resolved: &i18n::Resolved) {
        if let Some(code) = &resolved.unsupported {
            info_box(
                &self.win,
                tr!("Language not available"),
                &tr!("No translation is available for your system language ({}). English is used instead.", code),
            );
        }
    }
}

/// Point d'entrée de l'interface graphique. `--root DIR` (tests) travaille sur une fausse racine.
pub fn run() -> glib::ExitCode {
    let mut root: Option<PathBuf> = None;
    let mut language: Option<String> = None;
    let (mut show_help, mut show_version, mut unknown) = (false, false, None);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => root = it.next().map(PathBuf::from),
            "--language" => language = it.next().cloned(),
            other if other.starts_with("--language=") => language = Some(other["--language=".len()..].to_string()),
            "--version" | "-V" => show_version = true,
            "--help" | "-h" => show_help = true,
            other => {
                unknown.get_or_insert_with(|| other.to_string());
            }
        }
    }
    // la langue d'abord : l'aide et les erreurs sont déjà traduites
    let resolved = init_language(language.as_deref());
    if let Some(value) = &language {
        if !i18n::is_valid_preference(&value.trim().to_lowercase()) {
            eprintln!("{}", tr!("Invalid language: {0} (choose from: {1}).", value, i18n::PREFERENCES.join(", ")));
            return glib::ExitCode::from(2);
        }
    }
    if let Some(other) = unknown {
        eprintln!("{}", tr!("Unknown argument: {}", other));
        return glib::ExitCode::from(2);
    }
    if show_help {
        println!("{}", tr!("Usage: libvirt-backup-gui [--language LANGUAGE]\nGraphical interface of libvirt-backup (command line: libvirt-backup --help)."));
        return glib::ExitCode::SUCCESS;
    }
    if show_version {
        println!("libvirt-backup-gui {VERSION}");
        return glib::ExitCode::SUCCESS;
    }
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        gtk::Window::set_default_icon_name(APP_ID);
        let provider = gtk::CssProvider::new();
        provider.load_from_data(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    let window: Rc<RefCell<Option<Handle>>> = Rc::new(RefCell::new(None));
    app.connect_activate(move |app| {
        let mut slot = window.borrow_mut();
        let created = slot.is_none();
        let ui = slot.get_or_insert_with(|| Ui::new(app, root.clone()));
        ui.win.present();
        if created {
            ui.show_startup_notice(&resolved);
        }
    });
    // les arguments ont déjà été lus : GTK ne doit pas les ré-analyser
    app.run_with_args::<&str>(&[])
}

/// Pilotage programmatique de la fenêtre (test de bout en bout sous Xvfb ; non destiné aux utilisateurs).
#[doc(hidden)]
pub mod testing {
    use super::*;

    type Script = Rc<RefCell<Option<Box<dyn FnMut(&Driver) -> bool>>>>;

    pub struct Driver(Handle, std::cell::Cell<usize>);

    impl Driver {
        pub fn idle(&self) -> bool {
            self.0.st.borrow().operation.is_none()
        }
        pub fn status(&self) -> Value {
            self.0.st.borrow().status.clone()
        }
        pub fn archive_rows(&self) -> usize {
            self.0.st.borrow().archives.len()
        }
        pub fn log_text(&self) -> String {
            let buf = self.0.log.buffer();
            buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string()
        }
        pub fn banner(&self) -> String {
            if self.0.banner.is_visible() {
                self.0.banner.text().to_string()
            } else {
                String::new()
            }
        }
        pub fn folder_label(&self) -> String {
            self.0.folder_label.text().to_string()
        }
        pub fn progress(&self) -> f64 {
            self.0.progress.fraction()
        }
        pub fn activity(&self) -> String {
            self.0.activity_label.text().to_string()
        }
        pub fn select_first(&self) {
            if let Some(row) = self.0.listbox.row_at_index(0) {
                self.0.listbox.select_row(Some(&row));
            }
        }
        pub fn selected_name(&self) -> Option<String> {
            self.0.selected_archive().map(|a| a.name())
        }
        pub fn sensitive(&self, name: &str) -> bool {
            match name {
                "backup" => self.0.btn_backup.is_sensitive(),
                "backup_to" => self.0.btn_backup_to.is_sensitive(),
                "restore" => self.0.btn_restore.is_sensitive(),
                "delete" => self.0.btn_delete.is_sensitive(),
                "saved_dir" => self.0.btn_saved_dir.is_visible(),
                _ => false,
            }
        }
        pub fn backup_now(&self) {
            self.0.backup_now();
        }
        /// Texte de la boîte de confirmation de suppression pour l'archive sélectionnée.
        pub fn delete_warning(&self) -> Option<(String, String)> {
            self.0.selected_archive().map(|info| self.0.delete_message(&info))
        }
        /// Ouvre la boîte de confirmation, comme un clic sur « Supprimer… ».
        pub fn open_delete_dialog(&self) {
            self.0.delete_selected();
        }
        /// Supprime l'archive sélectionnée comme après un clic sur « Supprimer définitivement ».
        pub fn confirm_delete(&self) {
            if let Some(info) = self.0.selected_archive() {
                self.0.delete_archive(&info);
            }
        }
        pub fn lean_checked(&self) -> bool {
            self.0.c_lean.is_active()
        }
        pub fn set_lean_checked(&self, on: bool) {
            self.0.c_lean.set_active(on);
        }
        /// Nombre de boutons de la page des sauvegardes qui affichent une icône.
        pub fn button_icons(&self) -> usize {
            let buttons = [
                &self.0.btn_backup,
                &self.0.btn_backup_to,
                &self.0.btn_restore,
                &self.0.btn_restore_file,
                &self.0.btn_details,
                &self.0.btn_verify,
                &self.0.btn_delete,
                &self.0.btn_open,
            ];
            buttons
                .iter()
                .filter(|b| {
                    b.child().and_then(|c| c.first_child()).map(|first| first.downcast_ref::<gtk::Image>().is_some()).unwrap_or(false)
                })
                .count()
        }
        pub fn backup_to(&self, dir: &Path) {
            let args = vec!["backup".to_string(), "--backup-dir".into(), dir.to_string_lossy().into_owned()];
            self.0.start_operation(&tr!("Backup to {}", dir.display()), args, true);
        }
        pub fn run_operation(&self, title: &str, args: &[&str]) {
            self.0.start_operation(title, args.iter().map(|a| a.to_string()).collect(), true);
        }
        pub fn set_view_dir(&self, dir: Option<PathBuf>) {
            self.0.st.borrow_mut().view_dir = dir;
            self.0.refresh(false);
        }
        pub fn refresh(&self) {
            self.0.refresh(true);
        }
        pub fn show_page(&self, name: &str) {
            if let Some(stack) = self.0.switcher.stack() {
                stack.set_visible_child_name(name);
            }
        }
        pub fn dir_entry(&self) -> String {
            self.0.e_dir.text().to_string()
        }
        pub fn set_dir_entry(&self, text: &str) {
            self.0.e_dir.set_text(text);
        }
        pub fn save_settings(&self) {
            self.0.save_settings();
        }
        pub fn open_restore_window(&self) {
            self.0.open_restore();
        }
        pub fn toplevels(&self) -> usize {
            gtk::Window::list_toplevels().len()
        }
        /// Mémorise le nombre de fenêtres ouvertes (à comparer ensuite avec `toplevels`).
        pub fn set_toplevels_before(&self, count: usize) {
            self.1.set(count);
        }
        pub fn toplevels_before(&self) -> usize {
            self.1.get()
        }
        pub fn quit(&self) {
            if let Some(app) = self.0.win.application() {
                app.quit();
            }
        }
        /// Texte de la jauge (« 42% »), vide quand elle n'en affiche pas.
        pub fn progress_text(&self) -> String {
            if self.0.progress.shows_text() {
                self.0.progress.text().map(|t| t.to_string()).unwrap_or_default()
            } else {
                String::new()
            }
        }
        /// Libellés du menu de la barre de titre : (Préférences, À propos, Quitter), dans l'ordre.
        pub fn menu_labels(&self) -> Vec<String> {
            let mut labels = Vec::new();
            if let Some(button) = find_menu_button(self.0.win.titlebar()) {
                if let Some(model) = button.menu_model() {
                    for section in 0..model.n_items() {
                        if let Some(sub) = model.item_link(section, "section") {
                            for item in 0..sub.n_items() {
                                if let Some(label) = sub.item_attribute_value(item, "label", None).and_then(|v| v.get::<String>()) {
                                    labels.push(label);
                                }
                            }
                        }
                    }
                }
            }
            labels
        }
        /// Active une action de fenêtre (`about`, `preferences`, `quit`) comme le ferait le menu.
        pub fn activate_action(&self, name: &str) {
            gtk::prelude::ActionGroupExt::activate_action(&self.0.win, name, None);
        }
        /// Ouvre « À propos » et renvoie tous ses textes.
        pub fn about_text(&self) -> String {
            let window = self.0.open_about();
            let text = collect_text(window.upcast_ref::<gtk::Widget>());
            window.close();
            text
        }
        /// Vrai si l'action « Quitter » peut s'exécuter tout de suite (sinon une question est posée).
        pub fn can_quit_now(&self) -> bool {
            self.0.can_quit_now()
        }
        /// Langue transmise à la ligne de commande par `--language`.
        pub fn cli_language(&self) -> String {
            self.0.st.borrow().cli_lang.clone()
        }
        /// Ouvre les préférences : (libellés de la liste de langues, indice choisi).
        pub fn open_preferences(&self) -> (Vec<String>, u32) {
            self.0.open_preferences();
            let prefs = self.0.prefs.borrow();
            let Some(p) = prefs.as_ref() else { return (Vec::new(), 0) };
            let model = p.dropdown.model();
            let names = (0..model.as_ref().map(|m| m.n_items()).unwrap_or(0))
                .filter_map(|i| model.as_ref().and_then(|m| m.item(i)).and_then(|o| o.downcast::<gtk::StringObject>().ok()))
                .map(|o| o.string().to_string())
                .collect();
            (names, p.dropdown.selected())
        }
        /// Choisit une langue dans la liste des préférences ; renvoie vrai si l'avis de redémarrage s'affiche.
        pub fn choose_language(&self, index: u32) -> bool {
            let prefs = self.0.prefs.borrow();
            let Some(p) = prefs.as_ref() else { return false };
            p.dropdown.set_selected(index);
            p.notice.is_visible()
        }
        /// Force la jauge à une valeur (captures d'écran).
        pub fn set_gauge(&self, fraction: f64) {
            self.0.show_fraction(fraction);
        }
        /// Ouvre « À propos » et le laisse affiché (captures d'écran).
        pub fn show_about(&self) {
            self.0.open_about();
        }
        /// Ferme les fenêtres et boîtes de dialogue ouvertes, sauf la fenêtre principale.
        pub fn close_extra_windows(&self) {
            let main = self.0.win.clone().upcast::<gtk::Window>();
            let others: Vec<gtk::Window> =
                gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok()).filter(|w| *w != main).collect();
            for window in others {
                window.close();
            }
        }
        /// Nombre d'événements de progression reçus par la dernière opération.
        pub fn progress_events(&self) -> u32 {
            self.0.st.borrow().progress_events
        }
        pub fn close_preferences(&self) {
            let window = self.0.prefs.borrow().as_ref().map(|p| p.window.clone());
            if let Some(window) = window {
                window.close();
            }
        }
    }

    fn find_menu_button(widget: Option<gtk::Widget>) -> Option<gtk::MenuButton> {
        let widget = widget?;
        if let Ok(button) = widget.clone().downcast::<gtk::MenuButton>() {
            return Some(button);
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = find_menu_button(Some(c.clone())) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    /// Tous les textes des étiquettes d'un widget et de ses descendants.
    fn collect_text(widget: &gtk::Widget) -> String {
        let mut out = String::new();
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            out.push_str(&label.text());
            out.push('\n');
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            out.push_str(&collect_text(&c));
            child = c.next_sibling();
        }
        out
    }

    /// Lance l'application ; `script` est appelé toutes les 250 ms jusqu'à ce qu'il renvoie `true`.
    pub fn run(root: Option<PathBuf>, script: impl FnMut(&Driver) -> bool + 'static) -> glib::ExitCode {
        let script: Script = Rc::new(RefCell::new(Some(Box::new(script))));
        let resolved = super::init_language(None);
        let app = gtk::Application::builder().application_id(APP_ID).build();
        app.connect_startup(|_| {
            gtk::Window::set_default_icon_name(APP_ID);
            let provider = gtk::CssProvider::new();
            provider.load_from_data(CSS);
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            }
        });
        app.connect_activate(move |app| {
            let Some(mut script) = script.borrow_mut().take() else { return };
            let ui = Ui::new(app, root.clone());
            ui.win.present();
            ui.show_startup_notice(&resolved);
            let driver = Driver(ui, std::cell::Cell::new(0));
            glib::timeout_add_local(Duration::from_millis(250), move || {
                if script(&driver) {
                    driver.quit();
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        });
        app.run_with_args::<&str>(&[])
    }
}
