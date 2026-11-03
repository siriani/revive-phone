#!/bin/sh
# Logs que sobrevivem a travamento: kernel (incremental) + saúde a cada minuto.
KLOG=/var/log/kernel-persist.log
HLOG=/var/log/revive-health.log
echo "=== boot $(date '+%F %T') kernel $(uname -v)" >> $KLOG
echo "=== boot $(date '+%F %T')" >> $HLOG
dmesg >> $KLOG
dmesg -C
n=0
while true; do
	dmesg -c >> $KLOG 2>/dev/null
	if [ $((n % 2)) -eq 0 ]; then
		up=$(cut -d. -f1 /proc/uptime)
		st=$(wpa_cli -p /run/revive-wpa -i wlan0 status 2>/dev/null | grep -E "^(ssid|wpa_state)=" | cut -d= -f2 | tr "\n" " ")
		ip=$(ip -4 -o addr show wlan0 2>/dev/null | awk '{print $4}')
		sig=$(iw dev wlan0 link 2>/dev/null | awk '/signal/{print $2}')
		t=$(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null)
		mem=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
		gw=$(ip route | awk '/^default/{print $3; exit}')
		p=$(ping -c 1 -W 2 "$gw" >/dev/null 2>&1 && echo ok || echo FALHOU)
		rp=$(cat /sys/class/remoteproc/remoteproc0/state 2>/dev/null)
		echo "$(date '+%F %T') up=${up}s wifi=[$st] ip=$ip sinal=${sig}dBm cpu=$((t/1000))C memlivre=${mem}MB ping_gw=$p wcnss=$rp" >> $HLOG
	fi
	# não deixa os logs crescerem sem limite (2 MB cada)
	for f in $KLOG $HLOG; do
		[ "$(stat -c %s $f)" -gt 2097152 ] && tail -c 1048576 $f > $f.tmp && mv $f.tmp $f
	done
	sync
	n=$((n + 1))
	sleep 30
done
