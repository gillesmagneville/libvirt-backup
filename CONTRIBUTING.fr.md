# Contribuer à libvirt Backup

🇬🇧 [English version](CONTRIBUTING.md)

Merci de votre intérêt pour **libvirt Backup**.

Les contributions de toute nature sont les bienvenues : signalements de bogues, demandes de fonctionnalités, améliorations de la documentation, traductions et code.

En participant à ce projet, vous vous engagez à respecter son [Code de conduite](CODE_OF_CONDUCT.fr.md).

Pour signaler un **problème de sécurité**, n'ouvrez pas d'Issue publique : suivez la [Politique de sécurité](SECURITY.fr.md).

## Signaler un bogue

Si vous découvrez un bogue, ouvrez une Issue sur GitHub en donnant le plus d'informations possible :

- Distribution et version (Debian, Ubuntu…)
- Version de libvirt Backup (`libvirt-backup --version`) et mode d'installation (paquets `.deb` ou `make`)
- La commande ou le bouton utilisé
- Les étapes pour reproduire le problème
- Le comportement attendu
- Le comportement constaté
- Les messages d'erreur, le journal de l'opération de la fenêtre ou des captures d'écran (le cas échéant)

Ne joignez pas d'archive sauf si elle ne contient rien de privé : elle contient votre configuration libvirt.

## Proposer une amélioration

Les suggestions de nouvelles fonctionnalités ou d'améliorations de l'usage sont les bienvenues.

Avant d'ouvrir une nouvelle Issue, vérifiez que la fonctionnalité n'a pas déjà été demandée.

Si possible, expliquez :

- le problème que vous cherchez à résoudre ;
- en quoi la solution proposée serait utile ;
- les alternatives possibles.

## Contribuer au code

Si vous souhaitez contribuer au code :

1. Faites un fork du dépôt.
2. Créez une branche pour votre fonctionnalité.
3. Gardez vos changements centrés sur une seule fonctionnalité ou un seul correctif.
4. Testez vos modifications : `make test` (les tests s'exécutent sur une fausse racine, rien ne touche votre système), `make check` (clippy et rustfmt) et, pour l'interface, `make test-gui` (nécessite `xvfb-run`).
5. Soumettez une Pull Request avec une description claire de vos changements.

## Conventions de code

Essayez de garder un code :

- lisible ;
- bien documenté ;
- cohérent avec le style existant (`cargo fmt`, aucun avertissement `clippy`) ;
- compatible avec la version minimale de Rust prise en charge (`rust-version` dans `Cargo.toml`).

L'outil s'exécute en root et réécrit des fichiers système : un changement qui touche aux vérifications des archives, à l'extraction ou à la gestion des privilèges demande un test, et la façon dont il échoue compte autant que la façon dont il fonctionne.

## Documentation

Les améliorations de la documentation sont toujours appréciées.

Cela comprend :

- les améliorations du README ;
- les traductions ;
- les instructions d'installation ;
- les exemples ;
- les captures d'écran.

La documentation du projet (`README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CHANGELOG.md` et `PROJECT_CONTEXT.md`) est bilingue : la version anglaise est le fichier par défaut (`X.md`) et la version française est à côté (`X.fr.md`). Quand vous modifiez l'un, mettez aussi l'autre à jour, ou dites-le dans votre Pull Request pour que le mainteneur le fasse.

## Traductions

La ligne de commande et l'interface graphique existent en anglais, français, allemand et espagnol. **Le texte anglais est la clé** : il est écrit dans le code, par exemple `tr!("Back up now")`, et les trois tables `src/locales/fr.rs`, `src/locales/de.rs` et `src/locales/es.rs` donnent la traduction de chaque clé. L'anglais n'a pas de table : c'est la langue de repli.

Pour ajouter ou modifier un texte :

1. écrivez le texte anglais dans le code avec `tr!("…")` (ou `tr_noop!("…")` pour un `static`) ; utilisez `{}` pour une valeur et `{0}`, `{1}`… pour plusieurs ;
2. ajoutez la même clé, avec sa traduction, aux trois tables (gardez-les triées comme elles le sont) ;
3. lancez `cargo test --test i18n` : il vérifie que chaque texte du code et de l'aide `--help` est traduit, que les repères correspondent et qu'aucune table n'a une clé devenue inutile.

Les corrections des traductions existantes et les traductions vers d'autres langues sont les bienvenues et pourront être intégrées après relecture et test. Les textes allemands et espagnols ont été rédigés par le mainteneur avec l'aide de Claude.ai : une relecture par des locuteurs natifs est particulièrement bienvenue.

## Questions

Si vous avez des questions, n'hésitez pas à ouvrir une Issue GitHub.

## Licence

En contribuant à ce projet, vous acceptez que vos contributions soient distribuées sous la licence GNU GPL v3.0.

Le Code de conduite est le Contributor Covenant 3.0, adapté pour ce projet, et il est distribué sous sa propre licence, [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
