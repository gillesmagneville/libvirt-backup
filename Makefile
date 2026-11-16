CARGO ?= cargo
REL   := target/release

.PHONY: help build gui test test-gui screenshots check install uninstall status static deb clean zip

help:
	@echo "make test       lance les tests (sans toucher au système : fausse racine + faux virsh/systemctl)"
	@echo "make build      compile la ligne de commande seule (aucune dépendance GTK)"
	@echo "make gui        compile la ligne de commande + l'interface GTK4 (libgtk-4-dev requis)"
	@echo "make test-gui   test graphique de bout en bout sous Xvfb (xvfb-run requis)"
	@echo "make screenshots  régénère screenshots/{en,fr}/ (xvfb-run et ImageMagick requis)"
	@echo "make install    installe l'outil + le service de surveillance (sudo ; compile d'abord avec 'make gui')"
	@echo "make uninstall  retire le service (--purge à la main pour retirer aussi le programme)"
	@echo "make status     état de l'installation"
	@echo "make static     ligne de commande statique (musl), optionnelle"
	@echo "make deb        construit dist/libvirt-backup_*.deb et dist/libvirt-backup-gui_*.deb"
	@echo "make zip        construit ../libvirt-backup.zip"

build:
	$(CARGO) build --release --locked

gui:
	$(CARGO) build --release --locked --features gui

test:
	$(CARGO) test --locked

test-gui:
	xvfb-run -a -s "-screen 0 1280x900x24" $(CARGO) test --locked --features gui --test gui_smoke

screenshots:
	packaging/make-screenshots.sh

check:
	$(CARGO) clippy --locked --all-targets
	$(CARGO) fmt --check

install:
	@test -x $(REL)/libvirt-backup || { echo "Compile d'abord : make gui (ou make build sans interface graphique)"; exit 1; }
	$(REL)/libvirt-backup install

uninstall:
	$(REL)/libvirt-backup uninstall

status:
	$(REL)/libvirt-backup status

static:
	rustup target add x86_64-unknown-linux-musl
	$(CARGO) build --release --locked --target x86_64-unknown-linux-musl

deb: gui
	packaging/build-deb.sh dist

clean:
	$(CARGO) clean

zip:
	cd .. && rm -f libvirt-backup.zip && zip -qr libvirt-backup.zip libvirt-backup -x 'libvirt-backup/target/*' 'libvirt-backup/dist/*' 'libvirt-backup/.git/*'
