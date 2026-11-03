# revive-ui (Slint)

The on-screen interface, written in Rust with [Slint](https://slint.dev). One static
binary draws straight into `/dev/fb0` with Slint's software renderer — rotated to
landscape and only where something changed — and reads touch, keyboards and mice
through evdev. No display server, no GPU, about 9 MB of RAM.

On a Moto G 2013 a button press repaints in ~3 ms, versus 50–135 ms for the previous
Python/Pillow interface (still in `ui/` as a fallback).

## Apps

The home screen is a launcher. Each native app is a screen in `ui/app.slint`
(`if root.tela == "<id>"`) plus one line in `APPS` in `src/app.rs` (id, name, icon,
colour). Current apps: Painel (clock + Bitcoin), Música (Navidrome), Wi-Fi, Bluetooth.

Next step: manifest apps — a JSON file in `/etc/revive/apps/` describing a card/list app
(Home Assistant sensors, ntfy notifications, Uptime Kuma status) rendered by generic
templates, so new apps need no rebuild.

## Build (cross-compiling for armv7)

```sh
rustup target add armv7-unknown-linux-musleabihf
cargo install cargo-zigbuild        # needs zig in PATH
cargo zigbuild --release --target armv7-unknown-linux-musleabihf
# result: target/armv7-unknown-linux-musleabihf/release/revive-ui -> /opt/revive/revive-ui
```

Fonts are pre-rendered into the binary (`ui/fontes`, DejaVu). Characters that only
show up at runtime (network names, song titles) must be listed in
`caracteres-embutidos` in `ui/app.slint`.

| File | What |
|---|---|
| `ui/app.slint` | all screens |
| `src/tela.rs` | framebuffer pixel formats, rotation, touch/mouse/keyboard |
| `src/main.rs` | Slint platform and event loop (input boost, auto-rotation) |
| `src/app.rs` | screen state and navigation |
| `src/dados.rs`, `src/musica.rs` | Wi-Fi, Bitcoin, Bluetooth, Navidrome and mpv |
