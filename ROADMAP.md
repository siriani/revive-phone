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
- **Front camera (MT9M114)** — CAMSS ported to the MSM8226; the sensor streams frames
  over CSIPHY 1 with one data lane (`data-lanes = <0>`, 19.2 MHz MCLK).

## Big projects (kernel / hardware)

- **Audio** — the SLIMbus NGD runs in PIO mode and the WCD9306 codec enumerates (chip
  id 0x0103), but there is no sound yet. Next: the codec register map and DAPM
  routes, the SLIMbus data channels and the ADSP (APR/Q6) DAIs; then the loudspeaker,
  the headphone jack and the internal microphone (voice recorder app).
- **Cameras**
  - *Front (MT9M114)*: frames work. Next: test the MSM8226 clock rates and VFE 4.0
    patches (`patches/camera/experimental/`), then libcamera / an app; two data lanes
    like the stock firmware.
  - *Rear (AR0543, 5 MP)*: needs a new sensor driver written from public documentation,
    plus software processing of the raw Bayer frames (e.g. libcamera's software ISP).
- **CPU at 1.19 GHz** — an APC regulator driver and fixed OPP voltages (the stock CPR
  ceilings) are written but **untested** (`patches/cpu/`). Next: test 998 MHz first,
  then 1.19 GHz under a stress test; real CPR later.
- **Upstream** — send the panel driver, the MSM8226 CAMSS support, the SLIMbus NGD PIO
  mode and the falcon DTS changes to the kernel mailing lists.

## Apps

- **Voice recorder** — waits for audio (above), or for a USB/Bluetooth microphone.
- **Music** — offline audio cache (play without network, size-limited) and a queue screen.
- **YouTube** — search through an API, play with mpv + yt-dlp (audio first; low-resolution
  video now that the display driver works).
- **Reader** — fetch a web page and show its main text and images (Wikipedia, news).
  A full browser is on hold: modern engines need a GPU or a window system, and the
  packaged NetSurf/Links builds cannot draw on the framebuffer.
- **Notifications** — ntfy / Home Assistant / MQTT panel.

## Fixes and packaging

- The first tap right after the UI starts is sometimes ignored.
- Publish a release with the prebuilt `revive-ui` binary so the installer can download it.
