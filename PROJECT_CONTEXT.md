# libvirt Backup — Project Context

🇫🇷 [Version française](PROJECT_CONTEXT.fr.md)

This file gives a developer (or an AI assistant) the context needed to work on the project: what it is for, how it is built, the conventions and the decisions already taken. The user documentation is in the [README](README.md).

## 1. Purpose of the software

libvirt Backup backs up and restores the **persistent configuration of a libvirt/QEMU virtualization host**: the definitions libvirt stores, networks, pools, secrets, UEFI NVRAM and TPM state, AppArmor profiles, systemd units, VFIO/GRUB/initramfs settings and the local scripts and hooks. It is **not** a backup of the virtual disks.

It replaces two shell scripts (`install-libvirt-autobackup.sh` and `restore-libvirt-backup.sh`) and must **reproduce their actions exactly**: same `tar` flags, same steps in the same order, same service, same archive names and format (archives made by the old scripts stay restorable), original owners, modes and ACLs, a safety archive before a restore, a shared lock, a password request for privilege elevation. The deliberate differences are listed in the README (*Fidelity to the original scripts*).

Two founding requirements:

- **The tool is part of every archive**, so that a bare machine can be restored from a single archive, with a native executable (no Python, no GTK, no toolchain).
- **The user chooses where the archives go** (internal disk, USB drive, network share).

## 2. Architecture

### 2.1 General principle

One Rust package, edition 2021, three layers:

- the **engine** (the `libvirt_backup` library): archives, selection of the content, backup, restore, delete, watcher service, self-installation, configuration, translations;
- the **command line** (`libvirt-backup`, `src/cli.rs`): a thin layer over the engine, with `clap`;
- the **graphical interface** (`libvirt-backup-gui`, `src/gui.rs`, `gui` feature, GTK 4 through `gtk4-rs`): it **never runs as root**. It reads (list of archives, details, state of the service) with the engine, in its own process, and runs every operation that changes the system as a subprocess `pkexec libvirt-backup … --json`, whose JSON events feed the gauge and the log.

### 2.2 The engine

| Module | Role |
|---|---|
| `config` | settings (`/etc/libvirt-backup/config.json`), restorable groups and their paths, constants, confirmation words |
| `selection` | decides the content of an archive (metadata walk, lean filter) before `tar` is started |
| `backup` | `create_backup`: lock, state of the VMs, explicit file list, `virsh` exports, inventory, compression with progress, verification, rotation |
| `archive` | list, inspect and verify archives (manifest, full gzip CRC, refusal of dangerous entries) |
| `restore` | restore by group, safety archive, ordering of the system steps (AppArmor, systemd, libvirt, `virsh define`, numad) |
| `delete` | delete archives (warnings, only in the backup folder, `latest` link) |
| `service` | the watcher (inotify, debounce, `Type=notify`) and `systemctl` |
| `selfinstall` | installs the tool itself, in the *local* layout (`/usr/local/lib/libvirt-backup`) or the *package* layout (`/usr/bin`) |
| `report` | the `Reporter` trait (text, quiet and JSON outputs), the progress `Gauge` |
| `i18n`, `locales/` | translations (see 2.4) |
| `guiprefs` | per-user preferences of the window (`gui.json`) |
| `sys`, `context`, `virsh` | system helpers, run context (fake root for the tests), `virsh` calls |

Archives are produced by **GNU tar as a subprocess** (`--acls --xattrs --numeric-owner`, an explicit file list), because the fidelity to the original scripts (ACLs, extended attributes, owners) matters more than avoiding a subprocess. Restoration uses `tar --no-recursion` with an explicit list (see the README for the reason).

### 2.3 JSON protocol between the command line and the window

With `--json`, the command line writes **one JSON object per line** on standard output: `step` (`index`, `total`, `message`), `progress` (`fraction` from 0 to 1), `info`, `warn`, `error`, `result`, `busy` (`state`: `start`/`stop`) and finally `end` (`code`). Writes ignore a closed pipe: closing the window never kills an operation that is running.

During a backup the window follows the `progress` events (real percentage); for the other operations it follows the `step` events and the bar never goes back. The compression percentage comes from `tar --checkpoint=20 --checkpoint-action=echo=LBPROGRESS%u`, read on tar's standard error, against an estimate of the size of the tar stream.

### 2.4 Translations

The command line and the window speak English, French, German and Spanish. **The English text is the key.** In the code, `tr!("Back up now")` returns the text of the active language (a `&'static str`; with `{0}`, `{1}` arguments, a `String`); `tr_noop!` marks a text in a `static`. The tables `src/locales/{fr,de,es}.rs` map each key to its translation; English has no table and is the fallback. The language is resolved at startup: `--language`, then `LIBVIRT_BACKUP_LANG`, then the `language` setting (the window: `gui.json`), then the system language, then English. The `clap` help is translated at run time by walking the command tree (`localize` in `cli.rs`). Confirmation words are localized, and the French ones are always accepted. The window sets `LANGUAGE` for GTK's own texts and passes `--language` to the command line it starts (`pkexec` cleans the environment).

`tests/i18n.rs` makes a forgotten or useless translation a test failure.

### 2.5 Privileges

- Command line: commands that need root re-run themselves through `sudo` from a terminal (never in `--json` mode).
- Window: `pkexec` on the launcher `/usr/local/bin/libvirt-backup` (local layout) or `/usr/bin/libvirt-backup` (package), authorised by a dedicated polkit action (`data/app.policy`).
- Tests: the whole tool runs on a **fake root** (`--root`, a hidden option) with stub `virsh`, `systemctl` and `inotifywait` at the front of `PATH`: no test touches the real system.

## 3. Directory layout

```
libvirt-backup/
├── Cargo.toml, Cargo.lock, build.rs    package; build.rs records the build date (LB_BUILD_DATE)
├── src/
│   ├── lib.rs, bin/                    the library and the two executables (3 lines each)
│   ├── cli.rs, gui.rs                  command line, window
│   ├── config.rs, selection.rs, backup.rs, archive.rs, restore.rs, delete.rs,
│   │   service.rs, selfinstall.rs, report.rs, context.rs, virsh.rs, sys.rs
│   ├── i18n.rs, locales/{fr,de,es}.rs  translations
│   └── guiprefs.rs                     preferences of the window
├── data/                               desktop entry, polkit policy, icons (SVG sources, PNG set)
├── metainfo/                           AppStream metadata
├── packaging/                          .deb builder, maintainer scripts, man pages (en, fr), icon generator
├── screenshots/                        screenshots of the README (en/, fr/), made by `make screenshots`
├── tests/                              integration tests (fake root) and the graphical test
└── README.md, CHANGELOG.md, CONTRIBUTING.md, SECURITY.md, CODE_OF_CONDUCT.md, PROJECT_CONTEXT.md  (+ .fr.md)
```

## 4. Dependencies

- Build: Rust ≥ 1.87; for the window, `pkg-config`, `build-essential`, `libgtk-4-dev`.
- Crates: `clap`, `serde`/`serde_json`, `flate2`, `tar` (reading archives), `sha2`, `libc`; with the `gui` feature, `gtk4` (≥ 4.10 API) and `async-channel`. Dev: `tempfile`.
- Run: GNU `tar`, `inotify-tools`, `sudo`; for the window, `libgtk-4-1` and `pkexec`; `virsh` (`libvirt-clients`) on the real host.

## 5. Coding conventions

- Format with `cargo fmt` (`rustfmt.toml`), no `clippy` warning (`make check`).
- Every text shown to a person goes through `tr!` (or `bail!`, which translates). Never build a sentence by concatenation: use one key with placeholders, so that a translation can reorder words.
- Errors: `bail!("message {}", x)` returns an `AppError` whose message is translated; `.ctx(…)` adds a readable context to I/O errors.
- Anything that writes to the system must be reachable on the fake root, so that it can be tested.
- Tests: `make test` (integration, fake root), `make test-gui` (window under Xvfb), `cargo test --test i18n` after any text change (it also fails on a French label hard-coded outside `tr!`), `make screenshots` to regenerate `screenshots/{en,fr}/` after a visual change.
- Documentation is bilingual: `X.md` in English, `X.fr.md` in French, updated together. The source comments are in French.
- Versions follow SemVer; the changelog follows Keep a Changelog. The version is in `Cargo.toml` only (the packages and the About window read it).

## 6. Technical decisions made

- **Rust rather than Python or Go**: a Python tool depends on the installed interpreter and modules, which is fragile on the very machine one restores; the Rust command line is one native executable. The tool is copied into every archive.
- **GNU tar as a subprocess** for the archives, for fidelity with the original scripts (see 2.2).
- **The content of an archive is decided first** (metadata only), then handed to `tar` as an explicit list: the backup is much faster, and `--dry-run` can describe it. The *lean* filter skips compiled executables and very large files; `--full` / `lean_backup=false` restores the original behaviour.
- **Two layouts for the tool**: *local* (compiled install, files under `/usr/local`, all inside backed-up paths) and *package* (dpkg owns `/usr/bin`; the tool never writes there). Archives always carry the local layout copy.
- **The window never runs as root** and drives the command line through JSON, so that there is a single implementation of every operation.
- **English as the translation key**: a missing translation falls back to a readable English text rather than an identifier, and the code stays readable.
- **The product name "libvirt Backup" is not translated**, like "Planche-Contact".
- **GPL-3.0-only**, like the author's other projects.

## 7. Known bugs / limitations

- Only the real host can exercise systemd, polkit, libvirt and `pkexec`: the tests use stubs. Try the installation on a non-critical host first.
- The messages that `clap` writes by itself stay in English.
- The German and Spanish texts have not been proofread by native speakers.
- The packages are built for the architecture and the Ubuntu/Debian release of the machine that builds them (library dependencies are computed with `dpkg-shlibdeps`) and are not signed with a GPG key.
- A change of language in the window applies at the next start.

## 8. Roadmap

Ideas, none of them committed:

- proofreading of the German and Spanish texts; other languages;
- man pages in German and Spanish;
- signed packages and a repository for `apt`;
- restoring a single VM definition from an archive.
