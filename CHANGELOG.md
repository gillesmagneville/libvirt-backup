# Changelog

🇫🇷 [Version française](CHANGELOG.fr.md)

All notable changes to **libvirt Backup** will be documented in this file.

The project follows the recommendations of **Keep a Changelog** and **Semantic Versioning (SemVer)** as closely as possible.

## [Unreleased]

## [2.3.0] - 2026-11-16

First release published on GitHub. The program becomes multilingual (English, French, German, Spanish), gets a progress gauge for the backup, a menu with Preferences, About and Quit, and is now distributed under the GNU GPL v3 licence.

### Added

- **The command line and the graphical interface speak four languages**: English, French, German and Spanish. The language is chosen, in this order, by `--language`, then the `LIBVIRT_BACKUP_LANG` environment variable, then the `language` setting (`config set language=de`; in the window, the Preferences), then the system language (`LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE`, like gettext). When the system language is not translated, English is used, and the window says so once at startup. The `--help` texts, the messages, the log, the README stored in each archive and the confirmation words follow the language (see *Changed*).
- **A progress gauge for the backup.** In a terminal, a bar with the percentage and the elapsed time (`[████░░░░] 42 %`); in the `--json` stream, `progress` events (`{"event":"progress","fraction":0.42}`, from 0 to 1, never going back); in the window, a gauge with the percentage under the activity line. The percentage is real: the compression is measured from tar's own checkpoints, then the verification from the bytes read, not an estimate by steps.
- **A menu in the window** (the hamburger button) with **Preferences** (Ctrl+,), **About** and **Quit** (Ctrl+Q).
  - *Preferences*: the language of the window (system language, English, Français, Deutsch, Español), saved for each user in `~/.config/libvirt-backup/gui.json`; the change applies at the next start, and the command-line operations launched from the window use it immediately.
  - *About*: version and build date, developer (Gilles MAGNEVILLE), the mention that the program was designed with the help of [Claude.ai](https://claude.ai) (Anthropic), the GNU GPL v3 licence and the link to this repository — the same notes as [Planche-Contact](https://github.com/gillesmagneville/planche-contact).
  - *Quit*: if an operation is running, the window asks for a confirmation (button *Cancel* by default); the same question is asked when the window is closed. The operation, run by a separate process, would otherwise finish without being watched.
- A **French man page** (`man -L fr libvirt-backup`), and the `--language`, `LIBVIRT_BACKUP_LANG` and `language` options in the English one.
- **AppStream metadata** (`io.github.gillesmagneville.LibvirtBackup.metainfo.xml`), installed by the `libvirt-backup-gui` package, so that software centers show the application correctly; the `.desktop` file is translated (name, comment, keywords).
- **The documentation of the project**, in English and in French: `README`, `CHANGELOG`, `CONTRIBUTING`, `SECURITY` (private vulnerability reporting), `CODE_OF_CONDUCT` (Contributor Covenant 3.0) and `PROJECT_CONTEXT`. The packages ship the README, the changelog, the security policy and the code of conduct under `/usr/share/doc/libvirt-backup/`.
- **Tests**: `tests/i18n.rs` (every text of the code and of `--help` is translated in the three tables, the placeholders match, no unused translation, language order, confirmation words, README of the archive, no French label hard-coded outside `tr!`), `tests/progress.rs` (the gauge) and `tests/guiprefs.rs` (the preferences of the window); the end-to-end test of the window now covers the menu, About, Preferences, the language passed to the command line, the gauge and the Quit confirmation.
- `make screenshots` (`packaging/make-screenshots.sh`, `tests/gui_shots.rs`) regenerates the screenshots of the README, in English and in French, from the real window under Xvfb.

### Changed

- **Licence: GNU GPL v3** (`GPL-3.0-only`), with a `LICENSE` file; the Debian `copyright` file no longer says "all rights reserved".
- **The confirmation words follow the language**: `RESTORE` / `DELETE` in English, `RESTAURER` / `SUPPRIMER` in French, `WIEDERHERSTELLEN` / `LÖSCHEN` in German, `RESTAURAR` / `ELIMINAR` in Spanish. The French words are **always accepted**, so scripts and habits keep working.
- **The name of the application is "libvirt Backup" in every language** (the window and the menu said "Sauvegarde libvirt"); the generic name and the comment of the menu entry are translated.
- The README stored in each archive is written in the language of the run (a restore does not depend on it).
- In the window, the activity line and the gauge are on two lines, the gauge taking the whole width; during an operation without a real percentage (restore, install) the bar never goes back.
- The Debian changelog is written in English.
- The version number of `Cargo.toml` moves to 2.3.0; the program also records its build date (`SOURCE_DATE_EPOCH` is honoured, for reproducible builds).

### Known limitations

- The error messages that clap writes by itself (an unknown option, a missing argument) and the words `[possible values: …]` stay in English.
- The log lines sent to syslog and the journal follow the language of the process that writes them (the watcher service follows the `language` setting).
- The German and Spanish texts were written by the maintainer with the help of Claude.ai and have not been proofread by native speakers yet.
- The Debian packages are not signed with a GPG key; check the `.sha256` file of the release.

## Before the public repository (2.0.0 – 2.2.0)

The versions 2.0.0 to 2.2.0 were built privately before this repository existed:

- **2.2.0**: much faster backups (the content is decided once and handed to tar as an explicit list; compiled executables and very large files of `/usr/local/{sbin,bin}` are skipped, `lean_backup` setting and `--full`), `backup --dry-run`, the duration of each phase, the `delete` command and the *Delete…* button (with a warning), a complete icon set.
- **2.1.0**: the Debian packages `libvirt-backup` and `libvirt-backup-gui`; every archive carries a copy of the tool in the `/usr/local` layout.
- **2.0.0**: the tool rewritten in Rust (one native command line, optional GTK4 interface, no Python), replacing the two original shell scripts `install-libvirt-autobackup.sh` and `restore-libvirt-backup.sh`, with the choice of the backup location.
