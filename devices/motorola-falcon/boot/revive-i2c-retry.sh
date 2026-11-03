#!/bin/sh
# O touch (Synaptics s2316t) e o acelerômetro (LIS3DH) às vezes não respondem no I2C
# na primeira leitura do boot, e o driver desiste. Religa o driver até o dispositivo aparecer.
LOG=/var/log/revive-i2c-retry.log

religar() {  # nome dispositivo driver teste
	nome=$1 dev=$2 drv=/sys/bus/i2c/drivers/$3 teste=$4
	for i in $(seq 1 60); do
		if sh -c "$teste"; then
			echo "$(date '+%F %T') $nome ok (tentativa $i)" >> "$LOG"
			return 0
		fi
		[ -e "$drv/$dev" ] && echo "$dev" > "$drv/unbind"
		echo "$dev" > "$drv/bind" 2>/dev/null
		sleep 3
	done
	echo "$(date '+%F %T') $nome NÃO subiu após 60 tentativas" >> "$LOG"
	return 1
}

religar touch 2-0020 rmi4_i2c 'grep -qs Synaptics /sys/class/input/*/name'
religar acelerometro 0-0019 st-accel-i2c 'ls /sys/bus/iio/devices/*/in_accel_y_raw >/dev/null 2>&1'
