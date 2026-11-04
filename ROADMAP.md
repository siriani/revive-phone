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

## Big projects (kernel / hardware)

- **Audio** — the ADSP now runs, so the next step is the WCD9306 codec driver over
  SLIMbus (today only the chip-id probe exists). Then route the loudspeaker and the
  headphone jack, and the internal microphone (voice recorder app).
- **Cameras** — the MSM8226 camera subsystem (CAMSS) is not supported in mainline; it is
  close to the MSM8916 one, so the port starts from there, using the vendor device
  tree for clocks, regulators and GPIOs.
  - *Front (MT9M114, 1.2 MP)*: first target. The sensor is detected over CCI but has
    produced no frames yet. Next step: stream frames through CAMSS with the 24 MHz MCLK
    and one data lane; then move the sensor to the two lanes the stock firmware uses.
  - *Rear (AR0543, 5 MP)*: also needs a new sensor driver and software processing of the
    raw Bayer frames (e.g. libcamera's software ISP).
- **CPU at 1.19 GHz** — mainline stops at 787 MHz because the OPP table has no voltages;
  needs CPR/voltage scaling on the APC regulator, then a stress test.

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
