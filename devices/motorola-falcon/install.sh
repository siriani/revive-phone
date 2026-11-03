#!/bin/sh
# Partes do Revive específicas do Motorola Moto G 2013 (falcon). Chamado pelo install.sh.
set -e
cd "$(dirname "$0")"
apk add -q dtc

# boot: lk2nd precisa do dtb do falcon e da linha "fdt" no extlinux.conf após cada atualização
install -m755 boot/revive-fixboot.sh /usr/local/sbin/revive-fixboot
install -m755 boot/revive-fixboot.post-commit /etc/apk/commit_hooks.d/revive-fixboot.post-commit
# touch e acelerômetro às vezes não respondem no primeiro probe
install -m755 boot/revive-i2c-retry.sh /usr/local/sbin/revive-i2c-retry
install -m755 boot/revive-i2c-retry.initd /etc/init.d/revive-i2c-retry
rc-update add revive-i2c-retry default

# Wi-Fi sem 802.11n, fora do NetworkManager
install -m755 wifi/revive-wifi.start /etc/local.d/revive-wifi.start
mkdir -p /etc/NetworkManager/conf.d
install -m644 wifi/networkmanager-wlan0-unmanaged.conf /etc/NetworkManager/conf.d/revive-wlan0-unmanaged.conf
[ -f /etc/revive/wpa_supplicant.conf ] || install -m600 wifi/wpa_supplicant.conf.example /etc/revive/wpa_supplicant.conf
touch /etc/revive/wifi-no-ht
rc-update add local default

install -m755 set-bt-address.sh /usr/local/sbin/revive-set-bt-address
if [ -f /etc/revive/device.dtb ]; then
	/usr/local/sbin/revive-fixboot
else
	echo "AVISO: falta /etc/revive/device.dtb (dtb compilado do falcon, ver devices/motorola-falcon/README.md)"
fi
