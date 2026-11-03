#!/bin/sh
# Instala o Revive num celular com postmarketOS (Alpine + OpenRC). Rodar como root no celular.
# Uso: ./install.sh [--device <pasta em devices/>]
set -e
cd "$(dirname "$0")"
DEVICE=""
[ "$1" = "--device" ] && DEVICE=$2

apk add -q python3 py3-pillow py3-evdev py3-dbus py3-gobject3 font-dejavu \
	wpa_supplicant iw bluez bluez-alsa bluez-alsa-utils bluez-btmgmt alsa-utils mpv

mkdir -p /opt/revive /etc/revive /var/lib/revive /usr/share/revive
install -m755 ui/app.py /opt/revive/app.py
install -m644 ui/musica.py /opt/revive/musica.py
# cópia rápida para a tela em C (opcional: sem ela a interface usa o Pillow, mais lenta)
if apk add -q -t .revive-build gcc musl-dev; then
	gcc -O3 -shared -fPIC -o /opt/revive/libreviveblit.so ui/blit/revive_blit.c \
		|| echo "AVISO: não compilou ui/blit; seguindo sem o blit em C"
	apk del -q .revive-build
fi
install -m755 ui/revive-ui.initd /etc/init.d/revive-ui
install -m755 services/bluetooth/revive-bt.py /usr/local/sbin/revive-bt
install -m755 services/bluetooth/revive-bt.initd /etc/init.d/revive-bt
install -m755 services/music/revive-mpv.initd /etc/init.d/revive-mpv
install -m755 services/health/revive-health.sh /usr/local/sbin/revive-health
install -m755 services/health/revive-health.initd /etc/init.d/revive-health
[ -f /etc/revive/navidrome.conf ] || install -m600 config/navidrome.conf.example /etc/revive/navidrome.conf.example

# Bluetooth: só A2DP saindo para a caixa de som; uhid para teclado/mouse BLE; rádio ligado no boot
sed -i 's/^BLUEALSA_CONF=.*/BLUEALSA_CONF="-p a2dp-source"/' /etc/conf.d/bluealsa
grep -qx uhid /etc/modules 2>/dev/null || echo uhid >> /etc/modules
sed -i 's/^#\?AutoEnable=.*/AutoEnable=true/' /etc/bluetooth/main.conf
grep -q '^AutoEnable=true' /etc/bluetooth/main.conf || printf '\n[Policy]\nAutoEnable=true\n' >> /etc/bluetooth/main.conf
sed -i 's/^#\?FastConnectable *=.*/FastConnectable = true/' /etc/bluetooth/main.conf

for s in bluetooth bluealsa revive-bt revive-mpv revive-ui; do rc-update add $s default; done
rc-update add revive-health boot

if [ -n "$DEVICE" ]; then
	sh "devices/$DEVICE/install.sh"
fi
echo "Revive instalado. Reinicie o celular (reboot)."
