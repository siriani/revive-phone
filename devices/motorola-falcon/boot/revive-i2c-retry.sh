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

# O firmware do touch sobe em modo lento (o driver do Android mudava isto no boot):
# F11 em "reporte reduzido" + filtro de posição e F01 na taxa de reporte alternativa.
# Resultado: ~11 leituras/s e o toque chegava até 0,5 s atrasado. Passa para modo
# contínuo, sem filtro e taxa normal (~92 leituras/s). Endereços lidos da tabela PDT.
touch_rapido() {
	command -v i2ctransfer >/dev/null || return 1
	i2ctransfer -f -y 2 w2@0x20 0xff 0x00 || return 1
	addr=0xe9; f01=; f11=
	while :; do
		set -- $(i2ctransfer -f -y 2 w1@0x20 $addr r6@0x20) || return 1
		case $6 in
			0x00) break ;;
			0x01) f01=$3 ;;
			0x11) f11=$3 ;;
		esac
		addr=$(printf "0x%02x" $((addr - 6)))
	done
	[ -n "$f01" ] && [ -n "$f11" ] || return 1
	v=$(i2ctransfer -f -y 2 w1@0x20 $f11 r1@0x20)
	i2ctransfer -f -y 2 w2@0x20 $f11 $(printf "0x%02x" $((v & ~0x0f)))
	v=$(i2ctransfer -f -y 2 w1@0x20 $f01 r1@0x20)
	i2ctransfer -f -y 2 w2@0x20 $f01 $(printf "0x%02x" $((v & ~0x40)))
	echo "$(date '+%F %T') touch em modo rápido (F01 $f01, F11 $f11)" >> "$LOG"
}
touch_rapido || echo "$(date '+%F %T') touch: não deu para pôr em modo rápido" >> "$LOG"
religar acelerometro 0-0019 st-accel-i2c 'ls /sys/bus/iio/devices/*/in_accel_y_raw >/dev/null 2>&1'
