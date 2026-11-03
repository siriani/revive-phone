# Motorola Moto G 2013 (falcon)

Tested on the XT1033 (Brazilian dual SIM), Snapdragon 400 (MSM8226, 4× Cortex-A7),
1 GB RAM, postmarketOS edge, mainline Linux 6.16 (msm8226-mainline fork).

## Hardware status

| Part | Status | Notes |
|---|---|---|
| Display | ✅ | `simple-framebuffer` left on by the bootloader; brightness is fixed |
| Touch (Synaptics S2316T, RMI4) | ✅ | needs **l28 at 3.1 V** (see below) |
| Wi-Fi (WCN3620, 2.4 GHz) | ✅ | needs the wcn36xx patch; 802.11n disabled (up to 54 Mbps) |
| Bluetooth | ✅ | fixed address via `local-bd-address` (`revive-set-bt-address`) |
| Accelerometer (LIS3DH), compass (AK8963) | ✅ | the accelerometer sometimes needs a re-probe at boot |
| Modem | ✅ | ModemManager sees it and reads the IMEI; calls/SMS untested |
| GPS | ✅ | sees satellites (fix needs open sky) |
| Vibration, temperatures | ✅ | |
| Notification LED | ⚠️ | LPG + MPP6 driver loads, not visually confirmed |
| CPU frequency | ⚠️ | 300–787 MHz; 1.19 GHz needs CPR/voltage scaling (not done) |
| Audio (speaker/jack), GPU, cameras | ❌ | cameras need CAMSS for MSM8226, which mainline lacks |

## Fixes in this folder

1. **Wi-Fi hard freeze ~3 min after connecting** — the WCN3620 only has 2.4 GHz,
   but during scans the firmware tags some frames as 5 GHz and `wcn36xx` dereferences
   a NULL band inside the IRQ handler. Fixed by
   [`patches/0001-wifi-wcn36xx-...patch`](patches/).
2. **Wi-Fi firmware crash on 802.11n aggregation (ADD_BA)** — worked around by running
   `wpa_supplicant` with `disable_ht=1` outside NetworkManager
   ([`wifi/`](wifi/)).
3. **Touch did not react to fingers** — the controller's supply (l28) was at 1.8 V; the
   stock firmware uses 3.1 V. Underpowered, it NAKed I²C reads and held its interrupt
   line low. The DTS now sets l28 to 3.1 V, uses a level-low interrupt and a pull-up.
4. **Regulator l8 must stay on** (it feeds the Wi-Fi PLL).
5. **No picture** — two DSI clocks that Linux cannot enable were removed from the
   framebuffer node (the bootloader leaves them on).
6. **lk2nd cannot find the falcon DTB** — `boot/revive-fixboot.sh` copies
   `/etc/revive/device.dtb` to `/boot` and writes an explicit `fdt` line into
   `extlinux.conf`; an apk hook runs it after every package update.
7. **Touch/accelerometer probe sometimes fails at boot** — `revive-i2c-retry` re-binds
   the drivers until the devices appear.

## Installing from scratch

> Unlocking the bootloader voids the warranty and erases the phone. Back up every
> partition first (`dd` from recovery or `adb`), so you can go back to Android.

1. **Unlock the bootloader** with Motorola's official unlock code
   (`fastboot oem get_unlock_data`, then `fastboot oem unlock <code>`).
2. **Flash [lk2nd](https://github.com/msm8916-mainline/lk2nd)** (msm8226 build) to the
   `boot` partition: `fastboot flash boot lk2nd.img`. lk2nd boots postmarketOS from
   `extlinux.conf` on the `userdata` partition.
3. **Kernel**: build the
   [msm8226-mainline](https://github.com/msm8226-mainline/linux) kernel (tag
   `v6.16.12-msm8226-u2` was used) with the patch from `patches/` and this folder's
   `msm8226-motorola-falcon.dts`. With pmbootstrap:
   `pmbootstrap build linux-postmarketos-qcom-msm8226 --src=<kernel tree> --force`.
4. **postmarketOS**: `pmbootstrap init` with the generic `qcom-msm8226` device,
   **OpenRC** (systemd does not work on armv7 here) and the `console` UI, then
   `pmbootstrap install` and flash the image to `userdata`
   (`fastboot flash userdata <image>`); grow the root partition afterwards.
5. **Firmware**: copy `wcnss.*`, `wlan/prima/*`, `mba.*` and `modem.*` from the stock
   Android `modem`/`persist` partitions to `/lib/firmware` (not redistributable).
6. **Revive**: copy the compiled DTB to `/etc/revive/device.dtb`, then
   `./install.sh --device motorola-falcon`, put your network in
   `/etc/revive/wpa_supplicant.conf`, optionally write the stock Wi-Fi MAC to
   `/etc/revive/wifi-mac`, run `revive-set-bt-address` and reboot.

## Updating

`apk upgrade` is safe: the apk hook restores the DTB and the `fdt` line. But if the
**kernel package** is upgraded, it replaces the patched kernel and Wi-Fi starts
freezing again — pin it or rebuild it with the patch.
