# Security Policy

🇫🇷 [Version française](SECURITY.fr.md)

libvirt Backup is maintained by one person, on a voluntary basis. This page explains how to report a security problem and what you can expect in return.

## Supported versions

Only the **latest release** receives security fixes. If you use an older version, please update first: the releases are on the [Releases page](https://github.com/gillesmagneville/libvirt-backup/releases) and the changes are listed in the [CHANGELOG](CHANGELOG.md).

## Reporting a vulnerability

**Please do not report a vulnerability in a public Issue, Pull Request or discussion**: anyone could read it before a fix exists.

Report it privately, in one of these two ways:

1. **On GitHub**: open the [Security tab](https://github.com/gillesmagneville/libvirt-backup/security) of the repository and click **Report a vulnerability**. Only you and the maintainer can see the report.
2. **By email**: write to [gilles@magneville.fr](mailto:gilles@magneville.fr) with the subject "libvirt Backup security".

Please include, as far as you can:

- the libvirt Backup version (`libvirt-backup --version`, or the "About" window) and how you installed it (`.deb` packages, or compiled with `make`);
- your distribution and its version;
- a description of the problem and the steps to reproduce it;
- what you think the impact is;
- if the problem comes from a particular archive, the archive itself — only if you may share it and it contains nothing private.

## What to expect

These are goals, not guarantees, since there is only one maintainer:

- an acknowledgement of your report within **7 days**;
- an answer on whether it is considered a vulnerability, then news at least every few weeks until it is settled;
- a fix published in a **new release**, mentioned in the CHANGELOG; for a serious problem, a GitHub security advisory too;
- you are credited in the release notes, unless you prefer to stay anonymous.

Please keep the details private until a fixed version is available, so that users have the time to update.

## What is covered

libvirt Backup runs with administrator rights (through `sudo` or `pkexec`) and rewrites system files when it restores an archive, so the places where it can be abused matter. A report is relevant, for example, if:

- an archive makes `restore` write, replace or delete something **outside** the paths the tool is meant to handle (path traversal, absolute paths, `..`, symbolic or hard links, device files), or extract something before the checks have completed;
- the graphical interface, the `pkexec`/polkit rule, the launchers or the `sudo` re-execution let an unprivileged user run something as root that they could not run otherwise;
- the watcher service, its lock file, its temporary files or the archive folder can be abused by another local user (wrong owner or permissions, predictable file names, links followed);
- a backup, a log or a message leaks a secret it should not contain;
- a `.deb` package, its maintainer scripts or the files of a release can be abused, or a file of a release does not match its published `.sha256` fingerprint.

## What is not covered

- **Restoring an archive that you do not trust.** A libvirt configuration contains scripts and hooks that run as root: restoring an archive means running what it contains. Only restore archives you made yourself or that come from someone you trust. The tool refuses dangerous entries, but it cannot decide whether the *legitimate* content of an archive is harmless.
- A vulnerability inside a **component developed by someone else** (GNU tar, libvirt, GTK, polkit, systemd, the Rust standard library and the crates used…): please report it to that project. Do tell me if the version shipped with libvirt Backup is affected, so that I can update it.
- Problems that need a computer which is already compromised, or physical access to it, or a user who is already root.
- Ordinary bugs without security consequences: please open an [Issue](https://github.com/gillesmagneville/libvirt-backup/issues) instead.

## Checking what you download

Download libvirt Backup only from the [Releases page](https://github.com/gillesmagneville/libvirt-backup/releases) of this repository. Each file is published with its `.sha256` fingerprint, which is also given in the release notes and in the [README](README.md):

```bash
sha256sum -c libvirt-backup_<version>-1_amd64.deb.sha256
```

The packages are not signed with a GPG key, so this check is all the more useful.
