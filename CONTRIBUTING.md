# Contributing to libvirt Backup

🇫🇷 [Version française](CONTRIBUTING.fr.md)

Thank you for your interest in contributing to **libvirt Backup**.

Contributions of all kinds are welcome, including bug reports, feature requests, documentation improvements, translations and code contributions.

By taking part in this project, you agree to follow its [Code of Conduct](CODE_OF_CONDUCT.md).

To report a **security problem**, please do not open a public Issue: follow the [Security Policy](SECURITY.md).

## Reporting Bugs

If you discover a bug, please open an Issue on GitHub and provide as much information as possible:

- Distribution and version (Debian, Ubuntu…)
- libvirt Backup version (`libvirt-backup --version`) and how it was installed (`.deb` packages or `make`)
- The command or the button you used
- Steps to reproduce the problem
- Expected behaviour
- Actual behaviour
- Error messages, the operation log of the window, or screenshots (if applicable)

Do not attach an archive unless it contains nothing private: it holds your libvirt configuration.

## Suggesting Enhancements

Suggestions for new features or usability improvements are welcome.

Before opening a new Issue, please check whether the feature has already been requested.

When possible, explain:

- the problem you are trying to solve;
- why the proposed solution would be useful;
- possible alternatives.

## Contributing Code

If you would like to contribute code:

1. Fork the repository.
2. Create a feature branch.
3. Keep your changes focused on a single feature or bug fix.
4. Test your modifications: `make test` (the tests run on a fake root, nothing touches your system), `make check` (clippy and rustfmt) and, for the interface, `make test-gui` (needs `xvfb-run`).
5. Submit a Pull Request with a clear description of your changes.

## Coding Guidelines

Please try to keep the code:

- readable;
- well documented;
- consistent with the existing coding style (`cargo fmt`, no `clippy` warning);
- compatible with the minimum supported Rust version (`rust-version` in `Cargo.toml`).

The tool runs as root and rewrites system files: a change that touches the archive checks, the extraction or the privilege handling needs a test, and the way it fails matters as much as the way it works.

## Documentation

Documentation improvements are always appreciated.

This includes:

- README improvements;
- translations;
- installation instructions;
- examples;
- screenshots.

The project documentation (`README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CHANGELOG.md` and `PROJECT_CONTEXT.md`) is bilingual: the English version is the default file (`X.md`) and the French version sits next to it (`X.fr.md`). When you change one of them, please update the other too, or say so in your Pull Request so that the maintainer can update it.

## Translations

The command line and the graphical interface are available in English, French, German and Spanish. **The English text is the key**: it is written in the code, for example `tr!("Back up now")`, and the three tables `src/locales/fr.rs`, `src/locales/de.rs` and `src/locales/es.rs` give the translation of each key. English has no table: it is the fallback language.

To add or change a text:

1. write the English text in the code with `tr!("…")` (or `tr_noop!("…")` for a `static`); use `{}` for one value and `{0}`, `{1}`… for several;
2. add the same key, with its translation, to the three tables (keep them sorted as they are);
3. run `cargo test --test i18n`: it checks that every text of the code and of the `--help` is translated, that the placeholders match, and that no table has a key that is not used any more.

Corrections to existing translations and translations into other languages are welcome and may be integrated after review and testing. The German and Spanish texts were written by the maintainer with the help of Claude.ai: a review by native speakers is especially welcome.

## Questions

If you have questions, feel free to open a GitHub Issue.

## License

By contributing to this project, you agree that your contributions will be distributed under the GNU GPL v3.0 license.

The Code of Conduct is the Contributor Covenant 3.0, adapted for this project, and is distributed under its own license, [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
