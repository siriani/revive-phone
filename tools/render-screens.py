#!/usr/bin/env python3
"""Gera capturas das telas com dados fictícios (para documentação), sem mexer na tela real.
Uso (no celular, como root): python3 render-screens.py /tmp/telas"""
import os
import sys
import time

sys.path.insert(0, "/opt/revive")
import app  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402

saida = sys.argv[1] if len(sys.argv) > 1 else "/tmp/telas"
os.makedirs(saida, exist_ok=True)
quadros = []
app.Framebuffer.show = lambda self, img: quadros.append(img.copy())
app.Framebuffer.patch = lambda *a: None
app.App.status_loop = lambda self: None

a = app.App()
a.btc.brl, a.btc.usd, a.btc.change, a.btc.updated = 418381, 76939, 1.84, time.time()
a.wifi_info = ("HomeLab", "COMPLETED", "192.168.0.42")
a.nets = [("HomeLab", -48, True), ("HomeLab-IoT", -61, True), ("Vizinho", -79, True), ("Cafe-Aberto", -70, False)]
a.bt = {"pronto": True, "procurando": False, "prompt": None, "msg": "", "aparelhos": [
    {"mac": "00:11:22:33:44:01", "nome": "Caixa de som", "tipo": "som", "pareado": True, "conectado": True, "rssi": None, "ocupado": False},
    {"mac": "00:11:22:33:44:02", "nome": "Teclado BT", "tipo": "teclado", "pareado": True, "conectado": False, "rssi": -60, "ocupado": False},
    {"mac": "00:11:22:33:44:03", "nome": "Mouse BT", "tipo": "mouse", "pareado": False, "conectado": False, "rssi": -70, "ocupado": False}]}
faixa = {"id": "1", "title": "Open Source Blues", "artist": "The Homelabbers", "album": "Uptime 99.9%", "coverArt": "x"}
tocando = {"faixa": faixa, "t": 83, "dur": 241, "pausa": False, "pos": 2, "total": 12}
a.player.estado = lambda: tocando
a.player.mpv.prop = lambda nome: 80 if nome == "volume" else None
a.player_st = tocando
capa = Image.new("RGB", (660, 660))
d = ImageDraw.Draw(capa)
for y in range(660):  # capa fictícia em degradê
    d.line((0, y, 660, y), fill=(40 + y // 6, 60, 120 + y // 8))
d.text((330, 330), "♪", font=app.font(260), fill=(240, 240, 240), anchor="mm")
a.capas["x"] = capa
a.lista = {"titulo": "Álbuns recentes", "voltar": None, "itens": [
    {"nome": n, "sub": f"{art} · {ano}", "acao": None} for n, art, ano in [
        ("Uptime 99.9%", "The Homelabbers", 2026), ("Kernel Panic", "Null Pointer", 2024),
        ("Raspberry Jam", "GPIO Band", 2023), ("Self-Hosted", "Docker & Compose", 2025),
        ("Mesh Network", "LoRa Trio", 2022), ("Cold Storage", "ZFS Quartet", 2021),
        ("Second Life", "Old Phones", 2026), ("Low Latency", "The Pings", 2020)]]}
for tela in ["home", "wifi", "password", "bt", "musica", "lista", "tocando"]:
    a.screen = tela
    if tela == "password":
        a.ssid, a.text, a.teclado = "HomeLab", "", "wifi"
    a.draw()
    quadros[-1].save(f"{saida}/{tela}.png")
print("ok", saida)
