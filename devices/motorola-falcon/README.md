# Motorola Moto G 2013 (falcon)

Tested on the XT1033 (Brazilian dual SIM), Snapdragon 400 (MSM8226, 4× Cortex-A7),
1 GB RAM, postmarketOS edge, mainline Linux 6.16 (msm8226-mainline fork).

## Hardware status

| Part | Status | Notes |
|---|---|---|
| Display (DRM/KMS, `msm`) | ✅ | CMI 720p panel driver ([patch 0002](patches/0002-drm-panel-falcon-cmi-reset-energia-clock.patch)); loaded 15 s after boot by `revive-video`. simplefb stays enabled as the fallback |
| Touch (Synaptics S2316T, RMI4) | ✅ | needs **l28 at 3.1 V** (see below) |
| Wi-Fi (WCN3620, 2.4 GHz) | ✅ | needs the wcn36xx patch; 802.11n disabled (up to 54 Mbps) |
| Bluetooth | ✅ | fixed address via `local-bd-address` (`revive-set-bt-address`) |
| Accelerometer (LIS3DH), compass (AK8963) | ✅ | the accelerometer sometimes needs a re-probe at boot |
| Modem | ✅ | ModemManager sees it and reads the IMEI; calls/SMS untested |
| GPS | ✅ | sees satellites (fix needs open sky) |
| Vibration, temperatures | ✅ | |
| Notification LED | ⚠️ | LPG + MPP6 driver loads, not visually confirmed |
| CPU frequency | ⚠️ | 300–787 MHz; 1.19 GHz needs CPR/voltage scaling (not done) |
| Backlight (WLED, PM8226) | ✅ | `qcom_wled`, loaded with the display driver |
| GPU (Adreno 305) | ✅ | Mesa freedreno (FD305, OpenGL 3.1); `a300_pm4.fw` and `a300_pfp.fw` from linux-firmware |
| ADSP (LPASS) | ✅ | remoteproc with the firmware extracted from the stock Android (not redistributed) |
| Camera (front MT9M114) | ⚠️ | sensor detected over CCI; no frames yet. Rear AR0543 not started |
| Audio (speaker/jack) | ⚠️ | WIP: the ADSP is up; the WCD9306 codec driver is in progress |

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
5. **Framebuffer clocks** — the simplefb node keeps the DSI byte and pixel clocks
   (`MDSS_BYTE0_CLK`, `MDSS_PCLK0_CLK`) that the bootloader leaves on. Without simplefb,
   the `msm` driver hangs reading the DSI PLL, so simplefb stays enabled and `msm` takes
   over after boot (see [Testing safely](#testing-safely)).
6. **lk2nd cannot find the falcon DTB** — `boot/revive-fixboot.sh` copies
   `/etc/revive/device.dtb` to `/boot` and writes an explicit `fdt` line into
   `extlinux.conf`; an apk hook runs it after every package update.
7. **Touch/accelerometer probe sometimes fails at boot** — `revive-i2c-retry` re-binds
   the drivers until the devices appear.

## Testing safely

The display, backlight and GPU drivers are loaded after boot, not during it: a bad DSI
initialisation used to freeze the phone while it started.

- **Boot protection.** `install.sh` adds `modprobe.blacklist=msm,qcom_wled` to the
  `append` line of `/boot/extlinux/extlinux.conf`. The kernel then does not auto-load
  `msm` or `qcom_wled`, so the boot finishes on simplefb even if the driver would hang.
  `revive-fixboot` does not remove that parameter.
- **Late loading.** The `revive-video` service waits 15 s after boot, loads `msm` and
  `qcom_wled` and restarts `revive-ui`. Its log is `/var/log/revive-video.log`. To change
  the delay, set `REVIVE_VIDEO_DELAY` in `/etc/conf.d/revive-video`.
- **Check that the late load worked.** The log should say `msm e qcom_wled carregados`.
  If it says `falhou`, the blacklist may also refuse explicit loads. Then remove the
  parameter from the `append` line by hand, for the test only, and reboot.
- **Loading by hand** (`modprobe msm`): keep an SSH session open first. If the DSI hangs,
  the phone needs a power cycle.
- **Never use a DTB without the eMMC mask.** `&sdhc_1` must keep
  `sdhci-caps-mask = <0x7 0x0>` (no HS200/SDR104/DDR50). Without it, HS200 tuning
  corrupted writes on this eMMC until ext4 went read-only. Before copying a DTB to
  `/etc/revive/device.dtb`, check that the mask is there:
  ```sh
  dtc -I dtb -O dts /etc/revive/device.dtb | grep sdhci-caps-mask   # expect <0x07 0x00>
  ```
- **Trying a new DTB.** Keep the working `device.dtb` in `/etc/revive/backup/`, copy the
  new one, run `revive-fixboot`, reboot once, then check `dmesg` for the
  `motorola,falcon-panel-cmi` probe (no regulator or reset errors) and `ls /dev/dri`.
  To go back, restore the backup, run `revive-fixboot` and reboot.
- **Local Bluetooth address.** The DTS has only a commented placeholder for
  `local-bd-address`. Set the real address on the phone with `revive-set-bt-address`,
  never in the repository.

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
   Android `modem`/`persist` partitions to `/lib/firmware` (not redistributable). The
   ADSP (`adsp.mdt` and `adsp.b*`) goes from the same stock install to
   `/lib/firmware/qcom/msm8226/motorola/falcon/`. `install.sh` downloads the GPU firmware
   (`a300_pm4.fw`, `a300_pfp.fw`) from linux-firmware when it is missing.
6. **Revive**: copy the compiled DTB to `/etc/revive/device.dtb`, then
   `./install.sh --device motorola-falcon`, put your network in
   `/etc/revive/wpa_supplicant.conf`, optionally write the stock Wi-Fi MAC to
   `/etc/revive/wifi-mac`, run `revive-set-bt-address` and reboot.

## Updating

`apk upgrade` is safe: the apk hook restores the DTB and the `fdt` line. But if the
**kernel package** is upgraded, it replaces the patched kernel and Wi-Fi starts
freezing again — pin it or rebuild it with the patch.
