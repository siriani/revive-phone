# Roadmap

What is planned, roughly in order. Device-specific items refer to the Motorola Moto G
2013 (falcon); see [devices/motorola-falcon](devices/motorola-falcon/README.md).

## Done

- **Display (DRM/KMS)** — the CMI 720p panel driver and the PM8226 WLED backlight work on
  the Moto G through the `msm` driver, loaded after boot by `revive-video`. Brightness
  is controllable through the WLED backlight.
- **GPU** — Adreno 305 with Mesa freedreno (FD305, OpenGL 3.1), using the linux-firmware
  `a300` firmware.
- **ADSP** — the QDSP6 remoteproc boots with the firmware taken from the phone.
- **Cameras** — front MT9M114 and rear AR0543 (5 MP, RAW10 debayered in the app) through
  CAMSS, with the no-IOMMU fixes (contiguous buffers, RDI write limit). Camera app with
  a front/rear switch. The AR0543 driver stays private (tables from a proprietary library).
- **Audio (loudspeaker)** — the stock ADSP firmware speaks the mainline APR/QDSP6
  protocol; machine driver `qcom,msm8226-sndcard` + a new WCD9306/9302 codec driver.
  Music plays through the speaker; the output switches to a Bluetooth speaker when one
  connects. Hardware volume keys.
- **CPU at 1.19 GHz** — APC regulator driver and fixed OPP voltages; stable under load
  (capped at 998 MHz by default).
- **UI** — Rust + Slint, three languages, app store, Meshtastic, DOOM II, About screen.

## Big projects (kernel / hardware)

- **Audio, part 2** — headphone jack (MBHC), earpiece and the microphones (capture
  path), then a voice recorder app. Codec reset when the ADSP restarts.
- **Boot reliability** — one boot with the audio DTB hung before userspace once (not
  reproduced); find the cause.
- **CPR** — real voltage scaling instead of fixed OPP voltages.
- **Upstream** — the panel driver, MSM8226 CAMSS, the CAMSS no-IOMMU fixes, the SLIMbus
  NGD PIO mode, the audio stack and the falcon DTS, to the kernel mailing lists.

## Apps

- **Voice recorder** — waits for the microphone (above), or a USB/Bluetooth one.
- **Music** — offline audio cache (play without network, size-limited) and a queue screen.
- **YouTube** — search through an API, play with mpv + yt-dlp (audio first; low-resolution
  video now that the display driver works).
- **Reader** — fetch a web page and show its main text and images (Wikipedia, news).
  A full browser is on hold: modern engines need a GPU or a window system, and the
  packaged NetSurf/Links builds cannot draw on the framebuffer.
- **Notifications** — ntfy / Home Assistant / MQTT panel.

## Fixes and packaging

- Publish a release with the prebuilt `revive-ui` binary so the installer can download it.
