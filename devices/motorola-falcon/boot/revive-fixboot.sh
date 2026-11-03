#!/bin/sh
# Garante que o lk2nd consiga dar boot depois de qualquer atualização:
# - dtb do falcon com Wi-Fi/l8/touch/tela (/etc/revive/device.dtb)
# - extlinux.conf com "fdt" explícito (o lk2nd não conhece o dtb do falcon)
# - parâmetros extras do kernel
DTB=/etc/revive/device.dtb
CONF=/boot/extlinux/extlinux.conf
EXTRA="clk_ignore_unused pd_ignore_unused"
[ -f "$DTB" ] || exit 0
cp "$DTB" /boot/msm8226-motorola-falcon.dtb
[ -d /boot/dtbs ] && cp "$DTB" /boot/dtbs/msm8226-motorola-falcon.dtb
if [ -f "$CONF" ]; then
	sed -i 's#^\tfdtdir .*#\tfdt /msm8226-motorola-falcon.dtb#' "$CONF"
	grep -q "clk_ignore_unused" "$CONF" || sed -i "s#^\tappend #\tappend $EXTRA #" "$CONF"
fi
sync
