//! libvirt-backup : sauvegarde et restauration de la configuration libvirt/QEMU de l'hôte.
//!
//! Le moteur (cette bibliothèque) est partagé par la ligne de commande (`libvirt-backup`) et par
//! l'interface GTK4 (`libvirt-backup-gui`, fonctionnalité `gui`), qui pilote la ligne de commande.

pub mod archive;
pub mod backup;
pub mod cli;
pub mod config;
pub mod context;
pub mod delete;
#[cfg(feature = "gui")]
pub mod gui;
pub mod guiprefs;
pub mod i18n;
pub mod locales;
pub mod report;
pub mod restore;
pub mod selection;
pub mod selfinstall;
pub mod service;
pub mod sys;
pub mod virsh;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
