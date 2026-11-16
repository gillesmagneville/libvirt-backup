# libvirt Backup — Contexte du projet

🇬🇧 [English version](PROJECT_CONTEXT.md)

Ce fichier donne à un développeur (ou à un assistant IA) le contexte nécessaire pour travailler sur le projet : à quoi il sert, comment il est construit, les conventions et les décisions déjà prises. La documentation utilisateur est dans le [README](README.fr.md).

## 1. Objet du logiciel

libvirt Backup sauvegarde et restaure la **configuration persistante d'un hôte de virtualisation libvirt/QEMU** : les définitions que libvirt conserve, les réseaux, les pools, les secrets, les états UEFI NVRAM et TPM, les profils AppArmor, les unités systemd, les réglages VFIO/GRUB/initramfs, et les scripts et hooks locaux. Ce **n'est pas** une sauvegarde des disques virtuels.

Il remplace deux scripts shell (`install-libvirt-autobackup.sh` et `restore-libvirt-backup.sh`) et doit **reproduire très exactement leurs actions** : mêmes options de `tar`, mêmes étapes dans le même ordre, même service, mêmes noms et même format d'archives (les archives des anciens scripts restent restaurables), propriétaires, modes et ACL d'origine, archive de sécurité avant une restauration, verrou partagé, demande de mot de passe pour l'élévation de privilèges. Les différences voulues sont listées dans le README (*Fidélité aux scripts d'origine*).

Deux exigences fondatrices :

- **L'outil fait partie de chaque archive**, pour qu'une machine vierge se restaure à partir d'une seule archive, avec un exécutable natif (ni Python, ni GTK, ni chaîne de compilation).
- **L'utilisateur choisit où vont les archives** (disque interne, clé USB, partage réseau).

## 2. Architecture

### 2.1 Principe général

Un seul paquet Rust, édition 2021, trois couches :

- le **moteur** (la bibliothèque `libvirt_backup`) : archives, sélection du contenu, sauvegarde, restauration, suppression, service de surveillance, auto-installation, configuration, traductions ;
- la **ligne de commande** (`libvirt-backup`, `src/cli.rs`) : une fine couche au-dessus du moteur, avec `clap` ;
- l'**interface graphique** (`libvirt-backup-gui`, `src/gui.rs`, fonctionnalité `gui`, GTK 4 via `gtk4-rs`) : elle **ne s'exécute jamais en root**. Elle lit (liste des archives, détails, état du service) avec le moteur, dans son propre processus, et lance chaque opération qui modifie le système comme sous-processus `pkexec libvirt-backup … --json`, dont les événements JSON alimentent la jauge et le journal.

### 2.2 Le moteur

| Module | Rôle |
|---|---|
| `config` | réglages (`/etc/libvirt-backup/config.json`), groupes restaurables et leurs chemins, constantes, mots de confirmation |
| `selection` | décide du contenu d'une archive (parcours des métadonnées, filtre allégé) avant le lancement de `tar` |
| `backup` | `create_backup` : verrou, état des VM, liste explicite de fichiers, exports `virsh`, inventaire, compression avec progression, vérification, rotation |
| `archive` | lister, inspecter et vérifier les archives (manifeste, CRC gzip complet, refus des entrées dangereuses) |
| `restore` | restauration par groupe, archive de sécurité, ordre des étapes système (AppArmor, systemd, libvirt, `virsh define`, numad) |
| `delete` | suppression d'archives (avertissements, uniquement dans le dossier de sauvegarde, lien `latest`) |
| `service` | la surveillance (inotify, délai de regroupement, `Type=notify`) et `systemctl` |
| `selfinstall` | installe l'outil lui-même, en disposition *locale* (`/usr/local/lib/libvirt-backup`) ou *paquet* (`/usr/bin`) |
| `report` | le trait `Reporter` (sorties texte, silencieuse et JSON), la `Gauge` de progression |
| `i18n`, `locales/` | traductions (voir 2.4) |
| `guiprefs` | préférences de la fenêtre pour chaque utilisateur (`gui.json`) |
| `sys`, `context`, `virsh` | utilitaires système, contexte d'exécution (fausse racine pour les tests), appels à `virsh` |

Les archives sont produites par **GNU tar en sous-processus** (`--acls --xattrs --numeric-owner`, liste explicite de fichiers), car la fidélité aux scripts d'origine (ACL, attributs étendus, propriétaires) compte plus que d'éviter un sous-processus. La restauration utilise `tar --no-recursion` avec une liste explicite (voir le README pour la raison).

### 2.3 Protocole JSON entre la ligne de commande et la fenêtre

Avec `--json`, la ligne de commande écrit **un objet JSON par ligne** sur la sortie standard : `step` (`index`, `total`, `message`), `progress` (`fraction` de 0 à 1), `info`, `warn`, `error`, `result`, `busy` (`state` : `start`/`stop`) et enfin `end` (`code`). Les écritures ignorent un tube fermé : fermer la fenêtre ne tue jamais une opération en cours.

Pendant une sauvegarde, la fenêtre suit les événements `progress` (pourcentage réel) ; pour les autres opérations, elle suit les événements `step` et la barre ne recule jamais. Le pourcentage de la compression vient de `tar --checkpoint=20 --checkpoint-action=echo=LBPROGRESS%u`, lu sur l'erreur standard de tar, rapporté à une estimation de la taille du flux tar.

### 2.4 Traductions

La ligne de commande et la fenêtre parlent anglais, français, allemand et espagnol. **Le texte anglais est la clé.** Dans le code, `tr!("Back up now")` renvoie le texte de la langue active (un `&'static str` ; avec des arguments `{0}`, `{1}`, un `String`) ; `tr_noop!` marque un texte dans un `static`. Les tables `src/locales/{fr,de,es}.rs` associent à chaque clé sa traduction ; l'anglais n'a pas de table et sert de repli. La langue est résolue au démarrage : `--language`, puis `LIBVIRT_BACKUP_LANG`, puis le réglage `language` (la fenêtre : `gui.json`), puis la langue du système, puis l'anglais. L'aide de `clap` est traduite à l'exécution en parcourant l'arbre des commandes (`localize` dans `cli.rs`). Les mots de confirmation sont localisés, et les mots français sont toujours acceptés. La fenêtre définit `LANGUAGE` pour les textes propres à GTK et transmet `--language` à la ligne de commande qu'elle lance (`pkexec` nettoie l'environnement).

`tests/i18n.rs` transforme une traduction oubliée ou inutile en échec de test.

### 2.5 Privilèges

- Ligne de commande : les commandes qui exigent root se relancent par `sudo` depuis un terminal (jamais en mode `--json`).
- Fenêtre : `pkexec` sur le lanceur `/usr/local/bin/libvirt-backup` (disposition locale) ou `/usr/bin/libvirt-backup` (paquet), autorisé par une action polkit dédiée (`data/app.policy`).
- Tests : tout l'outil s'exécute sur une **fausse racine** (`--root`, option cachée) avec de faux `virsh`, `systemctl` et `inotifywait` en tête du `PATH` : aucun test ne touche au vrai système.

## 3. Arborescence

```
libvirt-backup/
├── Cargo.toml, Cargo.lock, build.rs    paquet ; build.rs enregistre la date de compilation (LB_BUILD_DATE)
├── src/
│   ├── lib.rs, bin/                    la bibliothèque et les deux exécutables (3 lignes chacun)
│   ├── cli.rs, gui.rs                  ligne de commande, fenêtre
│   ├── config.rs, selection.rs, backup.rs, archive.rs, restore.rs, delete.rs,
│   │   service.rs, selfinstall.rs, report.rs, context.rs, virsh.rs, sys.rs
│   ├── i18n.rs, locales/{fr,de,es}.rs  traductions
│   └── guiprefs.rs                     préférences de la fenêtre
├── data/                               entrée de menu, règle polkit, icônes (sources SVG, jeu PNG)
├── metainfo/                           métadonnées AppStream
├── packaging/                          constructeur de .deb, scripts de maintenance, pages de manuel (en, fr), générateur d'icônes
├── screenshots/                        captures d'écran du README (en/, fr/), faites par `make screenshots`
├── tests/                              tests d'intégration (fausse racine) et test graphique
└── README.md, CHANGELOG.md, CONTRIBUTING.md, SECURITY.md, CODE_OF_CONDUCT.md, PROJECT_CONTEXT.md  (+ .fr.md)
```

## 4. Dépendances

- Compilation : Rust ≥ 1.87 ; pour la fenêtre, `pkg-config`, `build-essential`, `libgtk-4-dev`.
- Crates : `clap`, `serde`/`serde_json`, `flate2`, `tar` (lecture des archives), `sha2`, `libc` ; avec la fonctionnalité `gui`, `gtk4` (API ≥ 4.10) et `async-channel`. Dev : `tempfile`.
- Exécution : GNU `tar`, `inotify-tools`, `sudo` ; pour la fenêtre, `libgtk-4-1` et `pkexec` ; `virsh` (`libvirt-clients`) sur le vrai hôte.

## 5. Conventions de code

- Format avec `cargo fmt` (`rustfmt.toml`), aucun avertissement `clippy` (`make check`).
- Tout texte montré à une personne passe par `tr!` (ou `bail!`, qui traduit). Ne jamais construire une phrase par concaténation : une seule clé avec des repères, pour qu'une traduction puisse réordonner les mots.
- Erreurs : `bail!("message {}", x)` renvoie une `AppError` dont le message est traduit ; `.ctx(…)` ajoute un contexte lisible aux erreurs d'entrée/sortie.
- Tout ce qui écrit dans le système doit être atteignable sur la fausse racine, pour pouvoir être testé.
- Tests : `make test` (intégration, fausse racine), `make test-gui` (fenêtre sous Xvfb), `cargo test --test i18n` après toute modification de texte (il échoue aussi sur un libellé français écrit en dur hors de `tr!`), `make screenshots` pour régénérer `screenshots/{en,fr}/` après un changement visuel.
- La documentation est bilingue : `X.md` en anglais, `X.fr.md` en français, mis à jour ensemble. Les commentaires du code source sont en français.
- Les versions suivent SemVer ; le journal suit Keep a Changelog. La version n'est que dans `Cargo.toml` (les paquets et la fenêtre « À propos » la lisent).

## 6. Décisions techniques prises

- **Rust plutôt que Python ou Go** : un outil Python dépend de l'interpréteur et des modules installés, ce qui est fragile sur la machine même qu'on restaure ; la ligne de commande Rust est un seul exécutable natif. L'outil est copié dans chaque archive.
- **GNU tar en sous-processus** pour les archives, par fidélité aux scripts d'origine (voir 2.2).
- **Le contenu d'une archive est décidé d'abord** (métadonnées seulement), puis donné à `tar` comme liste explicite : la sauvegarde est bien plus rapide, et `--dry-run` peut la décrire. Le filtre *allégé* écarte les exécutables compilés et les fichiers très volumineux ; `--full` / `lean_backup=false` rétablit le comportement d'origine.
- **Deux dispositions de l'outil** : *locale* (installation compilée, fichiers sous `/usr/local`, tous dans des chemins sauvegardés) et *paquet* (dpkg possède `/usr/bin` ; l'outil n'y écrit jamais). Les archives portent toujours la copie de la disposition locale.
- **La fenêtre ne s'exécute jamais en root** et pilote la ligne de commande par JSON : une seule implémentation de chaque opération.
- **L'anglais comme clé de traduction** : une traduction absente se replie sur un texte anglais lisible plutôt que sur un identifiant, et le code reste lisible.
- **Le nom du produit « libvirt Backup » n'est pas traduit**, comme « Planche-Contact ».
- **GPL-3.0-only**, comme les autres projets de l'auteur.

## 7. Bogues connus / limites

- Seul le vrai hôte peut exercer systemd, polkit, libvirt et `pkexec` : les tests utilisent des bouchons. Essayez l'installation sur un hôte non critique d'abord.
- Les messages que `clap` écrit lui-même restent en anglais.
- Les textes allemands et espagnols n'ont pas été relus par des locuteurs natifs.
- Les paquets sont construits pour l'architecture et la version d'Ubuntu/Debian de la machine qui les construit (les dépendances de bibliothèques sont calculées avec `dpkg-shlibdeps`) et ne sont pas signés par une clé GPG.
- Un changement de langue dans la fenêtre s'applique au prochain démarrage.

## 8. Feuille de route

Des idées, aucune n'est engagée :

- relecture des textes allemands et espagnols ; autres langues ;
- pages de manuel en allemand et en espagnol ;
- paquets signés et dépôt `apt` ;
- restauration d'une seule définition de VM depuis une archive.
