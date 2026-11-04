#!/bin/sh
# Instala o Revive num celular com postmarketOS (Alpine + OpenRC). Rodar como root no celular.
# Uso: ./install.sh [--device <pasta em devices/>]
set -e
cd "$(dirname "$0")"
DEVICE=""
[ "$1" = "--device" ] && DEVICE=$2

apk add -q python3 py3-pillow py3-evdev py3-dbus py3-gobject3 py3-bleak font-dejavu \
	wpa_supplicant iw bluez bluez-alsa bluez-alsa-utils bluez-btmgmt alsa-utils mpv \
		mesa-dri-gallium mesa-egl mesa-gles

mkdir -p /opt/revive /etc/revive /var/lib/revive /usr/share/revive
install -m755 ui/app.py /opt/revive/app.py
install -m644 ui/musica.py /opt/revive/musica.py
# cópia rápida para a tela em C (opcional: sem ela a interface usa o Pillow, mais lenta)
if apk add -q -t .revive-build gcc musl-dev; then
	gcc -O3 -shared -fPIC -o /opt/revive/libreviveblit.so ui/blit/revive_blit.c \
		|| echo "AVISO: não compilou ui/blit; seguindo sem o blit em C"
	apk del -q .revive-build
fi
install -m755 ui-slint/revive-ui.initd /etc/init.d/revive-ui
# interface em Slint (binário estático; ver ui-slint/README.md). Sem ela, roda a de Python.
if [ -x ui-slint/revive-ui ]; then
	install -m755 ui-slint/revive-ui /opt/revive/revive-ui
elif wget -q -O /tmp/revive-ui "https://github.com/siriani/revive-phone/releases/latest/download/revive-ui-$(uname -m)"; then
	install -m755 /tmp/revive-ui /opt/revive/revive-ui
else
	echo "AVISO: sem o binário da interface em Slint para $(uname -m); usando a interface em Python"
fi
install -m755 services/bluetooth/revive-bt.py /usr/local/sbin/revive-bt
install -m755 services/bluetooth/revive-bt.initd /etc/init.d/revive-bt
install -m755 services/mesh/revive-mesh-ble.py /usr/local/sbin/revive-mesh-ble
install -m755 services/mesh/revive-mesh-ble.initd /etc/init.d/revive-mesh-ble
install -m755 services/music/revive-mpv.initd /etc/init.d/revive-mpv
install -m755 services/health/revive-health.sh /usr/local/sbin/revive-health
install -m755 services/health/revive-health.initd /etc/init.d/revive-health
install -m755 services/video/revive-video.initd /etc/init.d/revive-video
[ -f /etc/revive/navidrome.conf ] || install -m600 config/navidrome.conf.example /etc/revive/navidrome.conf.example

# GPU Adreno 3xx (a300): firmware do linux-firmware, baixado só se faltar
FWDIR=/lib/firmware/qcom
mkdir -p "$FWDIR"
for f in a300_pm4.fw a300_pfp.fw; do
	[ -f "$FWDIR/$f" ] || wget -q -O "$FWDIR/$f" "https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/plain/qcom/$f" \
		|| { rm -f "$FWDIR/$f"; echo "AVISO: não baixou $f; a GPU fica sem aceleração"; }
done

# Drivers de hardware (vídeo, CPU, câmera, áudio) ficam fora do boot (modprobe.blacklist)
# e o revive-video os carrega 15 s depois: se algum travar, o boot já terminou.
# Idempotente; só mexe no extlinux se ele existir (lk2nd, postmarketOS).
EXTLINUX=/boot/extlinux/extlinux.conf
BLACKLIST="msm qcom_wled cpufreq_dt fts2_apc_regulator i2c_qcom_cci qcom_camss mt9m114 ar0543 \
slim_qcom_ngd_ctrl snd_soc_wcd9306 apr q6core q6afe q6adm q6asm q6routing q6afe_dai q6asm_dai \
snd_q6dsp_common snd_soc_msm8226"
if [ -f "$EXTLINUX" ]; then
	grep -q 'modprobe.blacklist=' "$EXTLINUX" \
		|| sed -i 's#^\(\tappend .*\)$#\1 modprobe.blacklist=msm#' "$EXTLINUX"
	for m in $BLACKLIST; do
		grep -qE "modprobe.blacklist=([^ ]*,)?$m(,| |$)" "$EXTLINUX" \
			|| sed -i "s#modprobe.blacklist=#modprobe.blacklist=$m,#" "$EXTLINUX"
	done
	grep -q 'modprobe.blacklist=' "$EXTLINUX" \
		|| echo "AVISO: não achei a linha append em $EXTLINUX; inclua modprobe.blacklist à mão"
fi

# Bluetooth: só A2DP saindo para a caixa de som; uhid para teclado/mouse BLE; rádio ligado no boot
sed -i 's/^BLUEALSA_CONF=.*/BLUEALSA_CONF="-p a2dp-source"/' /etc/conf.d/bluealsa
grep -qx uhid /etc/modules 2>/dev/null || echo uhid >> /etc/modules
sed -i 's/^#\?AutoEnable=.*/AutoEnable=true/' /etc/bluetooth/main.conf
grep -q '^AutoEnable=true' /etc/bluetooth/main.conf || printf '\n[Policy]\nAutoEnable=true\n' >> /etc/bluetooth/main.conf
sed -i 's/^#\?FastConnectable *=.*/FastConnectable = true/' /etc/bluetooth/main.conf

for s in bluetooth bluealsa revive-bt revive-mesh-ble revive-mpv revive-ui revive-video; do rc-update add $s default; done
rc-update add revive-health boot

if [ -n "$DEVICE" ]; then
	sh "devices/$DEVICE/install.sh"
fi
echo "Revive instalado. Reinicie o celular (reboot)."
