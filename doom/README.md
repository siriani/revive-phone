# revive-doom

Doom for Revive Phone. The game is drawn straight to the Linux framebuffer
with on-screen touch controls: no SDL, no X11, no window manager and no audio.

The engine is [doomgeneric](https://github.com/ozkl/doomgeneric) (a portable
port of the original Doom source) and is only compiled, not modified.

* Engine source: doomgeneric, commit `e257a09527062bfb986b6e5e0780839b6cbd5b71`
  (2026-10-11), the `doomgeneric/` directory minus the bundled platform
  backends (`doomgeneric_sdl.c`, `_xlib.c`, `_linuxvt.c`, ...).
* License: GNU GPL v2 only. See [LICENSE](LICENSE).

## It now runs inside the interface

Doom is an app of `revive-ui` (`ui-slint`). `ui-slint/build.rs` compiles every
`.c` in `engine/` into the static library `doomengine` and `ui-slint/src/doom.rs`
is the platform backend (framebuffer, evdev, overlay, pause). Nothing is started
in a separate process any more: the game is a thread of the interface, so it
starts instantly and opening another app is not needed to leave it.

* The touch overlay has a **✕** at the top left that freezes the game exactly
  where it is and returns to the launcher; the DOOM tile resumes the same game.
* The **Quit** item in the Doom menu ends the game: the launcher comes back and
  the next time the app is opened `revive-ui` restarts itself (the engine cannot
  be reinitialised in the same process).
* Game data still lives outside the repository:

```
/var/lib/revive/doom/doom2.wad      # or freedoom2.wad
/usr/share/games/doom/freedoom2.wad # used when the first one does not exist
```

## The C backend (reference / standalone)

`revive/doomgeneric_revive.c` is the original, self-contained backend: it opens
`/dev/fb0` itself, reads `/dev/input/event*` and can be built as a stand-alone
binary (`revive-doom`). It is kept as the reference implementation of the
drawing/rotation/input code that `ui-slint/src/doom.rs` ports, and for running
the game without the interface.

The target is an armv7 Linux phone (postmarketOS, musl), so the default build
uses `zig cc` as a cross compiler:

```sh
make                    # zig cc -target arm-linux-musleabihf -O2 -mcpu=cortex_a7
make CC="clang --target=arm-linux-gnueabihf"    # another cross compiler
make nativo             # host compiler, only to check that it compiles
```

`zig` must be on `PATH` (or set `CC` explicitly). The result is
`doom/revive-doom`, started with the WAD path:

```sh
revive-doom -iwad /var/lib/revive/doom/doom2.wad
```

Notes for the stand-alone binary:

* Video: `/dev/fb0` is read with `FBIOGET_VSCREENINFO`/`FBIOGET_FSCREENINFO`
  (24 or 32 bpp), the screen is used in landscape, and the 640x400 frame is
  scaled to fit with black side bars.
* Rotation: `/var/lib/revive/rotation` must contain `90` or `270` (defaults
  to `270`).
* Touch: the backend grabs the touchscreen and keyboard/gamepad at
  `/dev/input/event*`. The D-pad in the lower left moves, the round buttons on
  the right are ATIRAR (fire), USAR (use) and ARMA (next weapon), and the top
  rectangles are ESC, MAPA and ENTER. (The version inside `revive-ui` also has
  the ✕ to go back to the launcher, and its ESC moved to the right to make room
  for it.)
* Quitting from the menu calls `exit()`; the grab is released and the screen
  is cleared to black.
