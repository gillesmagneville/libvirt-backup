# Politique de sécurité

🇬🇧 [English version](SECURITY.md)

libvirt Backup est maintenu par une seule personne, à titre bénévole. Cette page explique comment signaler un problème de sécurité et à quoi vous pouvez vous attendre en retour.

## Versions prises en charge

Seule la **dernière version publiée** reçoit les corrections de sécurité. Si vous utilisez une version plus ancienne, mettez-la d'abord à jour : les versions sont sur la [page Releases](https://github.com/gillesmagneville/libvirt-backup/releases) et les changements sont listés dans le [CHANGELOG](CHANGELOG.fr.md).

## Signaler une vulnérabilité

**Merci de ne pas signaler une vulnérabilité dans une Issue, une Pull Request ou une discussion publique** : tout le monde pourrait la lire avant qu'un correctif existe.

Signalez-la en privé, de l'une de ces deux façons :

1. **Sur GitHub** : ouvrez l'[onglet Security](https://github.com/gillesmagneville/libvirt-backup/security) du dépôt et cliquez sur **Report a vulnerability**. Seuls vous et le mainteneur voyez le signalement.
2. **Par courriel** : écrivez à [gilles@magneville.fr](mailto:gilles@magneville.fr) avec l'objet « libvirt Backup security ».

Indiquez, autant que possible :

- la version de libvirt Backup (`libvirt-backup --version`, ou la fenêtre « À propos ») et la façon dont vous l'avez installé (paquets `.deb`, ou compilé avec `make`) ;
- votre distribution et sa version ;
- une description du problème et les étapes pour le reproduire ;
- l'impact que vous lui voyez ;
- si le problème vient d'une archive précise, l'archive elle-même — seulement si vous avez le droit de la partager et si elle ne contient rien de privé.

## À quoi s'attendre

Ce sont des objectifs, pas des garanties, puisqu'il n'y a qu'un mainteneur :

- un accusé de réception sous **7 jours** ;
- une réponse indiquant si le cas est considéré comme une vulnérabilité, puis des nouvelles au moins toutes les quelques semaines jusqu'à son règlement ;
- un correctif publié dans une **nouvelle version**, mentionné dans le CHANGELOG ; pour un problème grave, aussi un avis de sécurité GitHub ;
- vous êtes cité dans les notes de version, sauf si vous préférez rester anonyme.

Gardez les détails privés jusqu'à la publication d'une version corrigée, pour laisser aux utilisateurs le temps de mettre à jour.

## Ce qui est couvert

libvirt Backup s'exécute avec les droits d'administrateur (via `sudo` ou `pkexec`) et réécrit des fichiers système quand il restaure une archive : les endroits où l'on pourrait en abuser comptent. Un signalement est pertinent, par exemple, si :

- une archive fait écrire, remplacer ou supprimer par `restore` quelque chose **en dehors** des chemins que l'outil doit traiter (traversée de chemin, chemins absolus, `..`, liens symboliques ou physiques, fichiers de périphérique), ou extraire quelque chose avant la fin des vérifications ;
- l'interface graphique, la règle `pkexec`/polkit, les lanceurs ou la relance par `sudo` permettent à un utilisateur sans privilège d'exécuter en root ce qu'il ne pourrait pas exécuter autrement ;
- le service de surveillance, son fichier de verrou, ses fichiers temporaires ou le dossier des archives peuvent être détournés par un autre utilisateur local (mauvais propriétaire ou droits, noms de fichiers prévisibles, liens suivis) ;
- une sauvegarde, un journal ou un message divulgue un secret qu'il ne devrait pas contenir ;
- un paquet `.deb`, ses scripts de maintenance ou les fichiers d'une version peuvent être détournés, ou un fichier d'une version ne correspond pas à son empreinte `.sha256` publiée.

## Ce qui n'est pas couvert

- **Restaurer une archive à laquelle vous ne faites pas confiance.** Une configuration libvirt contient des scripts et des hooks exécutés en root : restaurer une archive, c'est exécuter ce qu'elle contient. Ne restaurez que des archives que vous avez faites vous-même ou qui viennent de quelqu'un de confiance. L'outil refuse les entrées dangereuses, mais il ne peut pas décider si le contenu *légitime* d'une archive est inoffensif.
- Une vulnérabilité dans un **composant développé par quelqu'un d'autre** (GNU tar, libvirt, GTK, polkit, systemd, la bibliothèque standard de Rust et les crates utilisées…) : signalez-la à ce projet. Dites-moi si la version livrée avec libvirt Backup est concernée, pour que je la mette à jour.
- Les problèmes qui supposent un ordinateur déjà compromis, un accès physique à celui-ci, ou un utilisateur déjà root.
- Les bogues ordinaires sans conséquence pour la sécurité : ouvrez plutôt une [Issue](https://github.com/gillesmagneville/libvirt-backup/issues).

## Vérifier ce que vous téléchargez

Téléchargez libvirt Backup uniquement depuis la [page Releases](https://github.com/gillesmagneville/libvirt-backup/releases) de ce dépôt. Chaque fichier est publié avec son empreinte `.sha256`, aussi donnée dans les notes de version et dans le [README](README.fr.md) :

```bash
sha256sum -c libvirt-backup_<version>-1_amd64.deb.sha256
```

Les paquets ne sont pas signés par une clé GPG : cette vérification n'en est que plus utile.
