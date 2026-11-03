#!/bin/sh
# Grava um endereço Bluetooth fixo no dtb (local-bd-address). Sem isso o WCN3620 usa
# um endereço provisório (02:00:...) e os pareamentos se perdem a cada boot.
# Uso: set-bt-address.sh AA:BB:CC:DD:EE:FF   (padrão: MAC do Wi-Fi + 1)
set -e
DTB=/etc/revive/device.dtb
NODE=/soc/remoteproc@fb204000/smd-edge/wcnss/bluetooth
mac=${1:-}
if [ -z "$mac" ]; then
	wifi=$(cat /etc/revive/wifi-mac 2>/dev/null || cat /sys/class/net/wlan0/address)
	last=$(printf '%02x' $(( (0x${wifi##*:} + 1) & 0xff )))
	mac="${wifi%:*}:$last"
fi
set -- $(echo "$mac" | tr ':' ' ')
fdtput -t bx "$DTB" "$NODE" local-bd-address "$6" "$5" "$4" "$3" "$2" "$1"  # ordem invertida
echo "endereço Bluetooth $mac gravado em $DTB (vale no próximo boot)"
/usr/local/sbin/revive-fixboot
