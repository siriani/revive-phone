# Roadmap

What is planned, roughly in order. Device-specific items refer to the Motorola Moto G
2013 (falcon); see [devices/motorola-falcon](devices/motorola-falcon/README.md).

## Big projects (kernel / hardware)

- **Display driver (DRM/KMS)** — panel driver (CMI 720p) and WLED backlight are written
  and built; still to be tested on the device through lk2nd's fastboot (`fastboot boot`,
  RAM only). Brings brightness control, vsync and the path to GPU acceleration.
- **Audio** — the phone has no sound card under mainline yet: the ADSP (QDSP6) is not
  enabled and the WCD9306 codec has no mainline driver. Porting it would enable the
  internal microphone (voice recorder app), the loudspeaker and the headphone jack.
- **Cameras** — the MSM8226 camera subsystem (CAMSS) is not supported in mainline; it is
  close to the MSM8916 one, so the port would start from there, using the vendor device
  tree for clocks, regulators and GPIOs.
  - *Front (MT9M114, 1.2 MP)*: first target — the sensor already has a mainline driver
    and outputs processed YUV, so CAMSS is the only missing piece.
  - *Rear (AR0543, 5 MP)*: also needs a new sensor driver and software processing of the
    raw Bayer frames (e.g. libcamera's software ISP).
- **CPU at 1.19 GHz** — mainline stops at 787 MHz because the OPP table has no voltages;
  needs CPR/voltage scaling on the APC regulator, then a stress test.

## Apps

- **Voice recorder** — waits for audio (above), or for a USB/Bluetooth microphone.
- **Music** — offline audio cache (play without network, size-limited) and a queue screen.
- **YouTube** — search through an API, play with mpv + yt-dlp (audio first; low-resolution
  video once the display driver works).
- **Reader** — fetch a web page and show its main text and images (Wikipedia, news).
  A full browser is on hold: modern engines need a GPU or a window system, and the
  packaged NetSurf/Links builds cannot draw on the framebuffer.
- **Notifications** — ntfy / Home Assistant / MQTT panel.

## Fixes and packaging

- The first tap right after the UI starts is sometimes ignored.
- Publish a release with the prebuilt `revive-ui` binary so the installer can download it.
