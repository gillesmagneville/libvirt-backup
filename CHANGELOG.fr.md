# Journal des modifications

🇬🇧 [English version](CHANGELOG.md)

Toutes les modifications notables de **libvirt Backup** sont consignées dans ce fichier.

Le projet suit autant que possible les recommandations de **Keep a Changelog** et du **versionnage sémantique (SemVer)**.

## [Non publié]

## [2.3.0] - 2026-11-16

Première version publiée sur GitHub. Le programme devient multilingue (anglais, français, allemand, espagnol), gagne une jauge de progression de la sauvegarde, un menu avec Préférences, À propos et Quitter, et est désormais distribué sous la licence GNU GPL v3.

### Ajouté

- **La ligne de commande et l'interface graphique parlent quatre langues** : anglais, français, allemand et espagnol. La langue est choisie, dans cet ordre, par `--language`, puis la variable d'environnement `LIBVIRT_BACKUP_LANG`, puis le réglage `language` (`config set language=de` ; dans la fenêtre, les Préférences), puis la langue du système (`LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE`, comme gettext). Quand la langue du système n'est pas traduite, l'anglais est utilisé et la fenêtre le signale une fois au démarrage. Les textes de `--help`, les messages, le journal, le README stocké dans chaque archive et les mots de confirmation suivent la langue (voir *Modifié*).
- **Une jauge de progression de la sauvegarde.** Dans un terminal, une barre avec le pourcentage et le temps écoulé (`[████░░░░] 42 %`) ; dans le flux `--json`, des événements `progress` (`{"event":"progress","fraction":0.42}`, de 0 à 1, sans jamais reculer) ; dans la fenêtre, une jauge avec le pourcentage sous la ligne d'activité. Le pourcentage est réel : la compression est mesurée d'après les points de contrôle de tar lui-même, puis la vérification d'après les octets lus, et non estimée par étapes.
- **Un menu dans la fenêtre** (le bouton « hamburger ») avec **Préférences** (Ctrl+,), **À propos** et **Quitter** (Ctrl+Q).
  - *Préférences* : la langue de la fenêtre (langue du système, English, Français, Deutsch, Español), enregistrée pour chaque utilisateur dans `~/.config/libvirt-backup/gui.json` ; le changement s'applique au prochain démarrage, et les opérations en ligne de commande lancées depuis la fenêtre l'utilisent tout de suite.
  - *À propos* : version et date de compilation, développeur (Gilles MAGNEVILLE), mention que le programme a été conçu avec l'aide de [Claude.ai](https://claude.ai) (Anthropic), licence GNU GPL v3 et lien vers ce dépôt — les mêmes mentions que [Planche-Contact](https://github.com/gillesmagneville/planche-contact).
  - *Quitter* : si une opération est en cours, la fenêtre demande une confirmation (bouton *Annuler* par défaut) ; la même question est posée à la fermeture de la fenêtre. L'opération, exécutée par un processus distinct, se terminerait sinon sans être surveillée.
- Une **page de manuel en français** (`man -L fr libvirt-backup`), et les options `--language`, `LIBVIRT_BACKUP_LANG` et `language` dans la page anglaise.
- Les **métadonnées AppStream** (`io.github.gillesmagneville.LibvirtBackup.metainfo.xml`), installées par le paquet `libvirt-backup-gui`, pour que les centres de logiciels affichent correctement l'application ; le fichier `.desktop` est traduit (nom, commentaire, mots-clés).
- **La documentation du projet**, en anglais et en français : `README`, `CHANGELOG`, `CONTRIBUTING`, `SECURITY` (signalement privé des vulnérabilités), `CODE_OF_CONDUCT` (Contributor Covenant 3.0) et `PROJECT_CONTEXT`. Les paquets livrent le README, le journal des modifications, la politique de sécurité et le code de conduite dans `/usr/share/doc/libvirt-backup/`.
- **Tests** : `tests/i18n.rs` (chaque texte du code et de `--help` est traduit dans les trois tables, les repères correspondent, aucune traduction inutile, ordre des langues, mots de confirmation, README de l'archive, aucun libellé français écrit en dur hors de `tr!`), `tests/progress.rs` (la jauge) et `tests/guiprefs.rs` (les préférences de la fenêtre) ; le test de bout en bout de la fenêtre couvre désormais le menu, À propos, les Préférences, la langue transmise à la ligne de commande, la jauge et la confirmation de sortie.
- `make screenshots` (`packaging/make-screenshots.sh`, `tests/gui_shots.rs`) régénère les captures d'écran du README, en anglais et en français, à partir de la vraie fenêtre sous Xvfb.

### Modifié

- **Licence : GNU GPL v3** (`GPL-3.0-only`), avec un fichier `LICENSE` ; le fichier Debian `copyright` ne dit plus « tous droits réservés ».
- **Les mots de confirmation suivent la langue** : `RESTORE` / `DELETE` en anglais, `RESTAURER` / `SUPPRIMER` en français, `WIEDERHERSTELLEN` / `LÖSCHEN` en allemand, `RESTAURAR` / `ELIMINAR` en espagnol. Les mots français sont **toujours acceptés** : scripts et habitudes continuent de fonctionner.
- **Le nom de l'application est « libvirt Backup » dans toutes les langues** (la fenêtre et le menu disaient « Sauvegarde libvirt ») ; le nom générique et le commentaire de l'entrée de menu sont traduits.
- Le README stocké dans chaque archive est écrit dans la langue de l'exécution (une restauration n'en dépend pas).
- Dans la fenêtre, la ligne d'activité et la jauge sont sur deux lignes, la jauge occupant toute la largeur ; pendant une opération sans pourcentage réel (restauration, installation), la barre ne recule jamais.
- Le journal des modifications Debian est rédigé en anglais.
- Le numéro de version de `Cargo.toml` passe à 2.3.0 ; le programme enregistre aussi sa date de compilation (`SOURCE_DATE_EPOCH` est respecté, pour des compilations reproductibles).

### Limites connues

- Les messages d'erreur que clap écrit lui-même (option inconnue, argument manquant) et les mots `[possible values: …]` restent en anglais.
- Les lignes envoyées à syslog et au journal suivent la langue du processus qui les écrit (le service de surveillance suit le réglage `language`).
- Les textes allemands et espagnols ont été rédigés par le mainteneur avec l'aide de Claude.ai et n'ont pas encore été relus par des locuteurs natifs.
- Les paquets Debian ne sont pas signés par une clé GPG ; vérifiez le fichier `.sha256` de la version.

## Avant le dépôt public (2.0.0 – 2.2.0)

Les versions 2.0.0 à 2.2.0 ont été construites en privé, avant l'existence de ce dépôt :

- **2.2.0** : sauvegardes beaucoup plus rapides (le contenu est décidé une fois puis donné à tar comme liste explicite ; les exécutables compilés et les fichiers très volumineux de `/usr/local/{sbin,bin}` sont écartés, réglage `lean_backup` et option `--full`), `backup --dry-run`, la durée de chaque phase, la commande `delete` et le bouton *Supprimer…* (avec avertissement), un jeu d'icônes complet.
- **2.1.0** : les paquets Debian `libvirt-backup` et `libvirt-backup-gui` ; chaque archive embarque une copie de l'outil dans la disposition `/usr/local`.
- **2.0.0** : l'outil réécrit en Rust (une ligne de commande native, une interface GTK4 facultative, sans Python), qui remplace les deux scripts shell d'origine `install-libvirt-autobackup.sh` et `restore-libvirt-backup.sh`, avec le choix de l'emplacement de la sauvegarde.
