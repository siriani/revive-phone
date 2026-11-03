#!/usr/bin/env python3
"""Revive: interface na tela para celulares antigos com Linux (postmarketOS).

Desenha direto no framebuffer (/dev/fb0) com Pillow, em paisagem, e lê touch,
teclado e mouse (inclusive Bluetooth) via evdev. Telas: início (relógio, preço
do BTC, Wi-Fi), redes Wi-Fi, teclado na tela, Bluetooth (serviço revive-bt) e
música do Navidrome (musica.py + serviço revive-mpv).

O Wi-Fi é controlado por um wpa_supplicant próprio (wpa_cli, ctrl_interface
/run/revive-wpa) em vez do NetworkManager, porque alguns chips antigos precisam
de opções que o NetworkManager não expõe (ex.: WCN36xx sem 802.11n).
"""
import fcntl
import json
import mmap
import os
import select
import subprocess
import threading
import random
import time
import unicodedata
import urllib.request

from PIL import Image, ImageDraw, ImageFont

try:
    import musica
except ImportError:  # sem o módulo de música a interface segue sem a tela de música
    musica = None

try:
    import evdev
    from evdev import ecodes
except ImportError:  # sem touch: a interface só mostra informações
    evdev = None

FB = "/dev/fb0"
WPA_CTRL = "/run/revive-wpa"
IFACE = "wlan0"
FONT = "/usr/share/fonts/dejavu/DejaVuSans.ttf"
FONT_BOLD = "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf"
LOG = "/var/log/revive-ui.log"
NO_HT_FLAG = "/etc/revive/wifi-no-ht"  # existe nos aparelhos cujo Wi-Fi trava com 802.11n
BT_STATE = "/run/revive-bt/state.json"
BT_CMD = "/run/revive-bt/cmd"

BG = (14, 17, 22)
CARD = (28, 33, 42)
FG = (235, 238, 242)
MUTED = (140, 150, 165)
ACCENT = (247, 147, 26)  # laranja do bitcoin
GOOD = (80, 200, 120)
BAD = (235, 90, 90)
KEY = (45, 52, 64)
KEY_DOWN = (80, 92, 110)


def log(msg):
    try:
        with open(LOG, "a") as f:
            f.write(time.strftime("%F %T ") + msg + "\n")
    except OSError:
        pass


def font(size, bold=False):
    try:
        return ImageFont.truetype(FONT_BOLD if bold else FONT, size)
    except OSError:
        return ImageFont.load_default()


# ---------------------------------------------------------------- framebuffer

class Framebuffer:
    def __init__(self, path=FB):
        name = os.path.basename(path)
        sysfs = f"/sys/class/graphics/{name}"
        self.pw, self.ph = (int(x) for x in open(f"{sysfs}/virtual_size").read().split(","))
        # a tela é retrato (720x1280), a interface é paisagem: desenha 1280x720 e gira
        self.w, self.h = self.ph, self.pw
        self.rot = 270  # 90 = topo da interface no lado direito do aparelho; 270 = no esquerdo
        self.bpp = int(open(f"{sysfs}/bits_per_pixel").read())
        self.stride = int(open(f"{sysfs}/stride").read())
        self.fd = os.open(path, os.O_RDWR)
        self.mem = mmap.mmap(self.fd, self.stride * self.ph)
        log(f"fb {self.pw}x{self.ph} bpp={self.bpp} stride={self.stride} (paisagem {self.w}x{self.h})")
        # tira o console de texto (fbcon) da tela: terminal em modo gráfico
        try:
            self.tty = os.open("/dev/tty1", os.O_RDWR)
            fcntl.ioctl(self.tty, 0x4B3A, 1)  # KDSETMODE, KD_GRAPHICS
        except OSError as e:
            log(f"tty1 modo gráfico: {e}")

    def girar(self, img):
        return img.transpose(Image.Transpose.ROTATE_90 if self.rot == 90 else Image.Transpose.ROTATE_270)

    def to_log(self, px, py):
        """Ponto da tela física (retrato) -> ponto da interface (paisagem)."""
        if self.rot == 90:
            return self.w - 1 - py, px
        return py, self.h - 1 - px

    def show(self, img):
        if img.size != (self.w, self.h):
            img = img.resize((self.w, self.h))
        img = self.girar(img)
        if self.bpp == 32:
            raw = img.convert("RGB").tobytes("raw", "BGRX")
            row = self.pw * 4
        elif self.bpp == 24:
            raw = img.convert("RGB").tobytes("raw", "BGR")
            row = self.pw * 3
        else:  # 16 bpp RGB565
            raw = img.convert("RGB").tobytes("raw", "BGR;16")
            row = self.pw * 2
        if row == self.stride:
            self.mem.seek(0)
            self.mem.write(raw)
        else:
            for y in range(self.ph):
                self.mem.seek(y * self.stride)
                self.mem.write(raw[y * row:(y + 1) * row])

    def patch(self, img, x0, y0):
        """Escreve só um pedaço da tela (cursor do mouse) sem redesenhar tudo.
        x0, y0 e img estão na interface (paisagem)."""
        w = min(img.width, self.w - x0)
        h = min(img.height, self.h - y0)
        if w <= 0 or h <= 0:
            return
        img = self.girar(img.crop((0, 0, w, h)))
        if self.rot == 90:
            x0, y0 = y0, self.w - x0 - w
        else:
            x0, y0 = self.h - y0 - h, x0
        w, h = img.width, img.height
        mode = {32: "BGRX", 24: "BGR"}.get(self.bpp, "BGR;16")
        bpp = self.bpp // 8
        raw = img.crop((0, 0, w, h)).convert("RGB").tobytes("raw", mode)
        row = w * bpp
        for i in range(h):
            self.mem.seek((y0 + i) * self.stride + x0 * bpp)
            self.mem.write(raw[i * row:(i + 1) * row])


# ------------------------------------------------------------------- entrada

def _keymap():
    """Teclado físico (padrão US): código -> (normal, com shift)."""
    m = {}
    for ch in "abcdefghijklmnopqrstuvwxyz":
        m[getattr(ecodes, f"KEY_{ch.upper()}")] = (ch, ch.upper())
    for d, s in zip("1234567890", "!@#$%^&*()"):
        m[getattr(ecodes, f"KEY_{d}")] = (d, s)
    for nome, n, s in (("MINUS", "-", "_"), ("EQUAL", "=", "+"), ("LEFTBRACE", "[", "{"),
                       ("RIGHTBRACE", "]", "}"), ("SEMICOLON", ";", ":"), ("APOSTROPHE", "'", '"'),
                       ("GRAVE", "`", "~"), ("BACKSLASH", "\\", "|"), ("COMMA", ",", "<"),
                       ("DOT", ".", ">"), ("SLASH", "/", "?"), ("SPACE", " ", " "),
                       ("102ND", "\\", "|"), ("RO", "/", "?")):
        m[getattr(ecodes, f"KEY_{nome}")] = (n, s)
    return m


KEYMAP = _keymap() if evdev else {}
SPECIAL = {"KEY_ENTER": "ENTER", "KEY_KPENTER": "ENTER", "KEY_BACKSPACE": "BACKSPACE",
           "KEY_ESC": "ESC", "KEY_TAB": "TAB"}


class Inputs:
    """Touch, teclados e mouses. Os Bluetooth aparecem e somem a qualquer
    hora, então a lista de dispositivos é refeita a cada poucos segundos."""

    def __init__(self, width, height, fisico=None, conv=None):
        self.w, self.h = width, height
        self.fw, self.fh = fisico or (width, height)  # tamanho do painel (o touch fala nele)
        self.conv = conv or (lambda x, y: (x, y))
        self.devs = {}  # caminho -> (InputDevice, tipo)
        self.meta = {}  # caminho -> (max_x, max_y) do touch
        self.x = self.y = 0
        self.shift = 0
        self.caps = False
        self.mx, self.my = width // 2, height // 2
        self.mouse_on = False
        self.scan()

    def scan(self):
        if evdev is None:
            return
        for p in evdev.list_devices():
            if p in self.devs:
                continue
            try:
                d = evdev.InputDevice(p)
                caps = d.capabilities()
            except OSError:
                continue
            absc = [c[0] if isinstance(c, tuple) else c for c in caps.get(ecodes.EV_ABS, [])]
            keys = caps.get(ecodes.EV_KEY, [])
            rel = caps.get(ecodes.EV_REL, [])
            if ecodes.ABS_MT_POSITION_X in absc:
                kind = "touch"
                ax = d.absinfo(ecodes.ABS_MT_POSITION_X)
                ay = d.absinfo(ecodes.ABS_MT_POSITION_Y)
                self.meta[p] = (ax.max or self.w, ay.max or self.h)
            elif ecodes.REL_X in rel and ecodes.BTN_LEFT in keys:
                kind = "mouse"
            elif ecodes.KEY_A in keys and ecodes.KEY_Z in keys:
                kind = "teclado"
            else:
                d.close()
                continue
            self.devs[p] = (d, kind)
            log(f"entrada: {kind} {d.name} {p}")
        if not any(k == "touch" for _, k in self.devs.values()) and not getattr(self, "avisou", False):
            log("touch: nenhum dispositivo encontrado (procurando a cada 5 s)")
            self.avisou = True

    def tem(self, kind):
        return any(k == kind for _, k in self.devs.values())

    def fds(self):
        return [d.fd for d, _ in self.devs.values()]

    def read(self, fd):
        """Lê um dispositivo pronto. Devolve ações: ("tap", x, y),
        ("key", texto ou ENTER/BACKSPACE/ESC/TAB) e ("move",)."""
        path = next(p for p, (d, _) in self.devs.items() if d.fd == fd)
        d, kind = self.devs[path]
        try:
            events = list(d.read())
        except OSError:  # aparelho Bluetooth desligou
            log(f"entrada: {kind} {d.name} saiu")
            del self.devs[path]
            return []
        if kind == "touch":
            return self._touch(path, events)
        if kind == "mouse":
            return self._mouse(events)
        return self._teclado(events)

    def _touch(self, path, events):
        max_x, max_y = self.meta[path]
        taps = []
        for ev in events:
            if ev.code == ecodes.ABS_MT_POSITION_X and ev.type == ecodes.EV_ABS:
                self.x = ev.value * self.fw // max(max_x, 1)
            elif ev.code == ecodes.ABS_MT_POSITION_Y and ev.type == ecodes.EV_ABS:
                self.y = ev.value * self.fh // max(max_y, 1)
            elif ev.type == ecodes.EV_ABS and ev.code == ecodes.ABS_MT_TRACKING_ID and ev.value == -1:
                taps.append(("tap", *self.conv(self.x, self.y)))
            elif ev.type == ecodes.EV_KEY and ev.code == ecodes.BTN_TOUCH and ev.value == 0:
                taps.append(("tap", *self.conv(self.x, self.y)))
        # BTN_TOUCH e TRACKING_ID=-1 podem chegar juntos: um toque só
        uniq = []
        for t in taps:
            if t not in uniq:
                uniq.append(t)
        return uniq

    def _mouse(self, events):
        out = []
        for ev in events:
            if ev.type == ecodes.EV_REL and ev.code == ecodes.REL_X:
                self.mx = min(max(self.mx + ev.value * 2, 0), self.w - 1)
                self.mouse_on = True
                out.append(("move",))
            elif ev.type == ecodes.EV_REL and ev.code == ecodes.REL_Y:
                self.my = min(max(self.my + ev.value * 2, 0), self.h - 1)
                self.mouse_on = True
                out.append(("move",))
            elif ev.type == ecodes.EV_KEY and ev.code == ecodes.BTN_LEFT and ev.value == 0:
                self.mouse_on = True
                out.append(("tap", self.mx, self.my))
        return out

    def _teclado(self, events):
        out = []
        for ev in events:
            if ev.type != ecodes.EV_KEY:
                continue
            if ev.code in (ecodes.KEY_LEFTSHIFT, ecodes.KEY_RIGHTSHIFT):
                self.shift = max(0, self.shift + (1 if ev.value == 1 else -1 if ev.value == 0 else 0))
                continue
            if ev.value == 0:  # só pressionar (1) e repetição (2)
                continue
            if ev.code == ecodes.KEY_CAPSLOCK:
                self.caps = not self.caps
            elif ev.code in KEYMAP:
                n, s = KEYMAP[ev.code]
                upper = bool(self.shift) != (self.caps and n.isalpha())
                out.append(("key", s if upper else n))
            else:
                nome = ecodes.KEY.get(ev.code)
                nome = nome[0] if isinstance(nome, list) else nome
                if nome in SPECIAL:
                    out.append(("key", SPECIAL[nome]))
        return out


# ------------------------------------------------------------------ dados

class Btc:
    URLS = [
        "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin"
        "&vs_currencies=brl,usd&include_24hr_change=true",
    ]

    def __init__(self):
        self.brl = self.usd = self.change = None
        self.updated = 0
        self.error = ""

    def refresh(self):
        try:
            req = urllib.request.Request(self.URLS[0], headers={"User-Agent": "revive-ui"})
            with urllib.request.urlopen(req, timeout=10) as r:
                d = json.load(r)["bitcoin"]
            self.brl, self.usd = d["brl"], d["usd"]
            self.change = d.get("brl_24h_change") or d.get("usd_24h_change")
            self.updated = time.time()
            self.error = ""
        except Exception as e:  # rede caiu, limite da API etc.
            self.error = str(e)[:60]
            log(f"btc: {e}")


def wpa(*args):
    cmd = ["wpa_cli", "-p", WPA_CTRL, "-i", IFACE, *args]
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=10).stdout.strip()
    except Exception as e:
        log(f"wpa_cli {args}: {e}")
        return ""


def wifi_status():
    st = dict(line.split("=", 1) for line in wpa("status").splitlines() if "=" in line)
    ip = ""
    try:
        out = subprocess.run(["ip", "-4", "-o", "addr", "show", IFACE],
                             capture_output=True, text=True, timeout=5).stdout
        if " inet " in out:
            ip = out.split(" inet ")[1].split("/")[0]
    except Exception:
        pass
    return st.get("ssid", ""), st.get("wpa_state", "?"), ip


def wifi_scan():
    wpa("scan")
    time.sleep(4)
    nets = {}
    for line in wpa("scan_results").splitlines()[1:]:
        parts = line.split("\t")
        if len(parts) < 5 or not parts[4]:
            continue
        ssid, sig, flags = parts[4], int(parts[2]), parts[3]
        if ssid not in nets or nets[ssid][0] < sig:
            nets[ssid] = (sig, "WPA" in flags or "WEP" in flags)
    return sorted(((s, *v) for s, v in nets.items()), key=lambda n: -n[1])


def wifi_connect(ssid, psk):
    """Cria (ou substitui) a rede no wpa_supplicant e salva a configuração."""
    for line in wpa("list_networks").splitlines()[1:]:
        parts = line.split("\t")
        if len(parts) > 1 and parts[1] == ssid:
            wpa("remove_network", parts[0])
    nid = wpa("add_network").splitlines()[-1]
    wpa("set_network", nid, "ssid", json.dumps(ssid))
    if psk:
        wpa("set_network", nid, "psk", json.dumps(psk))
    else:
        wpa("set_network", nid, "key_mgmt", "NONE")
    if os.path.exists(NO_HT_FLAG):  # ex.: firmware do WCN36xx trava com agregação 802.11n
        wpa("set_network", nid, "disable_ht", "1")
    wpa("select_network", nid)
    wpa("save_config")
    subprocess.run(["udhcpc", "-i", IFACE, "-n", "-q", "-t", "6"],
                   capture_output=True, timeout=40)
    log(f"wifi: conectar {ssid} id={nid}")


ROT_FILE = "/var/lib/revive/rotation"


def ler_rotacao():
    try:
        r = int(open(ROT_FILE).read())
        return r if r in (90, 270) else 270
    except (OSError, ValueError):
        return 270


def achar_acelerometro():
    import glob
    for d in glob.glob("/sys/bus/iio/devices/iio:device*"):
        try:
            if os.path.exists(f"{d}/in_accel_y_raw"):
                return d
        except OSError:
            pass
    return None


def bt_state():
    """Estado publicado pelo serviço revive-bt (aparelhos, código de pareamento)."""
    try:
        with open(BT_STATE) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def bt_cmd(linha):
    """Manda um comando para o serviço revive-bt. Devolve False se ele não está rodando."""
    try:
        fd = os.open(BT_CMD, os.O_WRONLY | os.O_NONBLOCK)
    except OSError:
        return False
    try:
        os.write(fd, (linha + "\n").encode())
    finally:
        os.close(fd)
    return True


# --------------------------------------------------------------- interface

class Button:
    def __init__(self, box, label, action, color=KEY):
        self.box, self.label, self.action, self.color = box, label, action, color

    def hit(self, x, y):
        x0, y0, x1, y1 = self.box
        return x0 <= x <= x1 and y0 <= y <= y1


# seta do mouse (polígono dentro de uma caixa CUR_W x CUR_H)
CURSOR = [(0, 0), (0, 36), (10, 27), (17, 42), (23, 39), (16, 25), (28, 25)]
CUR_W, CUR_H = 32, 46

KEYS_LOWER = ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"]
KEYS_SYM = ["1234567890", "@#$%&*-+()", "!\"':;/?_=", ".,<>[]{}"]


class App:
    def __init__(self):
        self.fb = Framebuffer()
        self.W, self.H = self.fb.w, self.fb.h
        self.inp = Inputs(self.W, self.H, (self.fb.pw, self.fb.ph), self.fb.to_log)
        self.fb.rot = ler_rotacao()
        self.acel = None  # sysfs do acelerômetro (procurado de tempos em tempos)
        self.giro_votos = 0
        self.btc = Btc()
        self.screen = "home"
        self.nets = []
        self.ssid = ""
        self.text = ""
        self.shift = False
        self.sym = False
        self.show_pw = False
        self.msg = ""
        self.buttons = []
        self.lock = threading.Lock()
        self.dirty = True
        self.bt = bt_state()
        self.bt_mtime = 0
        self.frame = None  # última tela desenhada (para apagar o cursor)
        self.cur_pos = None
        self.player = musica.Player() if musica else None
        self.lista = {"titulo": "", "itens": [], "voltar": None}
        self.pagina = 0
        self.teclado = "wifi"  # o teclado na tela serve para a senha e para a busca
        self.artistas = []
        self.capas = {}  # cover id -> Image já redimensionada
        self.toc = None  # estado do player na última tela desenhada
        self.ultimo_x = 0
        self.wifi_info = ("", "?", "")
        self.player_st = None
        self.barra_geo = None
        self.progresso = False  # o status mudou só o tempo da faixa: atualiza só a barra
        self.boost_ate = 0
        self.f_big = font(96, True)
        self.f_mid = font(44, True)
        self.f_txt = font(34)
        self.f_small = font(26)

    # ---------- desenho
    def btn(self, d, box, label, action, color=KEY, f=None):
        d.rounded_rectangle(box, radius=18, fill=color)
        f = f or self.f_txt
        cx, cy = (box[0] + box[2]) / 2, (box[1] + box[3]) / 2
        d.text((cx, cy), label, font=f, fill=FG, anchor="mm")
        self.buttons.append(Button(box, label, action, color))

    def draw(self):
        self.buttons = []
        img = Image.new("RGB", (self.W, self.H), BG)
        d = ImageDraw.Draw(img)
        getattr(self, f"draw_{self.screen}")(d)
        if self.msg:
            d.text((self.W / 2, self.H - 30), self.msg, font=self.f_small, fill=MUTED, anchor="mm")
        if self.bt.get("prompt"):
            self.draw_prompt(d)
        self.fb.show(img)
        self.frame, self.cur_pos = img, None
        self.cursor()

    def cursor(self):
        """Seta do mouse desenhada por cima da última tela, só no pedaço que muda."""
        if self.frame is None:
            return
        if self.cur_pos:  # apaga a seta antiga
            x, y = self.cur_pos
            self.fb.patch(self.frame.crop((x, y, x + CUR_W, y + CUR_H)), x, y)
            self.cur_pos = None
        if not self.inp.mouse_on:
            return
        x, y = self.inp.mx, self.inp.my
        pedaco = self.frame.crop((x, y, x + CUR_W, y + CUR_H))
        ImageDraw.Draw(pedaco).polygon(CURSOR, fill=FG, outline=(0, 0, 0), width=2)
        self.fb.patch(pedaco, x, y)
        self.cur_pos = (x, y)

    def draw_prompt(self, d):
        """Código de pareamento: o teclado Bluetooth pede para digitar nele."""
        W, H, p = self.W, self.H, self.bt["prompt"]
        d.rectangle((0, 0, W, H), fill=(0, 0, 0))
        d.rounded_rectangle((240, 90, W - 240, H - 90), radius=28, fill=CARD)
        d.text((W / 2, 150), "Parear teclado", font=self.f_mid, fill=FG, anchor="mm")
        d.text((W / 2, 290), p.get("codigo", ""), font=font(120, True), fill=ACCENT, anchor="mm")
        texto = p.get("texto", "")
        d.text((W / 2, 410), texto[:52], font=self.f_small, fill=MUTED, anchor="mm")
        self.buttons = []  # só o cancelar vale enquanto o código está na tela
        self.btn(d, (W / 2 - 160, 480, W / 2 + 160, 570), "Cancelar", lambda: bt_cmd("cancelar"),
                 f=self.f_small)

    def draw_home(self, d):
        W, H = self.W, self.H
        L = 620  # coluna esquerda: relógio e bitcoin; direita: Wi-Fi e botões
        d.text((L / 2 + 15, 75), time.strftime("%H:%M"), font=font(110, True), fill=FG, anchor="mm")
        d.text((L / 2 + 15, 150), time.strftime("%d/%m/%Y"), font=self.f_txt, fill=MUTED, anchor="mm")

        d.rounded_rectangle((30, 195, L, H - 30), radius=28, fill=CARD)
        d.text((60, 235), "Bitcoin", font=self.f_mid, fill=ACCENT, anchor="lm")
        b, cx = self.btc, (30 + L) / 2
        if b.brl:
            brl = f"R$ {b.brl:,.0f}".replace(",", ".")
            usd = f"US$ {b.usd:,.0f}".replace(",", ".")
            d.text((cx, 350), brl, font=font(84, True), fill=FG, anchor="mm")
            d.text((cx, 440), usd, font=self.f_mid, fill=MUTED, anchor="mm")
            if b.change is not None:
                cor = GOOD if b.change >= 0 else BAD
                d.text((cx, 515), f"{b.change:+.2f}% em 24 h", font=self.f_txt, fill=cor, anchor="mm")
            idade = int(time.time() - b.updated)
            d.text((cx, 600), f"atualizado há {idade // 60} min" if idade >= 60 else "atualizado agora",
                   font=self.f_small, fill=MUTED, anchor="mm")
        else:
            d.text((cx, 400), "sem dados" if b.updated == 0 else "erro", font=self.f_mid, fill=MUTED, anchor="mm")
            if b.error:
                d.text((cx, 480), b.error[:34], font=self.f_small, fill=BAD, anchor="mm")

        R0, R1 = L + 30, W - 30
        ssid, state, ip = self.wifi_info
        d.rounded_rectangle((R0, 30, R1, 210), radius=28, fill=CARD)
        d.text((R0 + 30, 70), "Wi-Fi", font=self.f_mid, fill=FG, anchor="lm")
        ok = state == "COMPLETED"
        d.ellipse((R1 - 60, 55, R1 - 30, 85), fill=GOOD if ok else BAD)
        d.text((R0 + 30, 130), ssid or "desconectado", font=self.f_txt, fill=FG if ok else MUTED, anchor="lm")
        d.text((R0 + 30, 178), ip or state.lower(), font=self.f_small, fill=MUTED, anchor="lm")

        bw = (R1 - R0 - 30) / 3
        self.btn(d, (R0, 230, R0 + bw, 330), "Wi-Fi", self.go_wifi)
        self.btn(d, (R0 + bw + 15, 230, R0 + 2 * bw + 15, 330), "Bluetooth", self.go_bt, f=self.f_small)
        self.btn(d, (R0 + 2 * bw + 30, 230, R1, 330), "Atualizar", self.refresh_btc, color=(70, 55, 30),
                 f=self.f_small)
        if self.player:
            st = self.player_st
            if st:
                f = st["faixa"]
                rot = ("❚❚ " if not st["pausa"] else "▶ ") + f"{f.get('title', '')} — {f.get('artist', '')}"
                self.btn(d, (R0, 350, R1, 470), rot[:34], self.go_tocando, color=(40, 60, 90), f=self.f_small)
            else:
                self.btn(d, (R0, 350, R1, 470), "♪ Música", self.go_musica, color=(40, 60, 90))
        conectados = [a["nome"] for a in self.bt.get("aparelhos", []) if a.get("conectado")]
        if conectados:
            d.text(((R0 + R1) / 2, 510), "Bluetooth: " + ", ".join(conectados)[:30], font=self.f_small,
                   fill=MUTED, anchor="mm")

    def grade(self, i, linhas=4, top=95, alt=110, gap=14):
        """Caixa do i-ésimo item numa grade de 2 colunas (paisagem)."""
        W = self.W
        col, lin = divmod(i, linhas)
        cw = (W - 60 - gap) / 2
        x = 30 + col * (cw + gap)
        y = top + lin * (alt + gap)
        return (x, y, x + cw, y + alt)

    def rodape(self, d, esquerda, direita=None):
        """Botões da última linha: (rótulo, ação)."""
        W, H = self.W, self.H
        if direita:
            self.btn(d, (30, H - 100, W / 2 - 8, H - 20), *esquerda)
            self.btn(d, (W / 2 + 8, H - 100, W - 30, H - 20), *direita)
        else:
            self.btn(d, (30, H - 100, W - 30, H - 20), *esquerda)

    def draw_wifi(self, d):
        W = self.W
        d.text((W / 2, 45), "Redes Wi-Fi", font=self.f_mid, fill=FG, anchor="mm")
        if not self.nets:
            d.text((W / 2, 300), "procurando...", font=self.f_txt, fill=MUTED, anchor="mm")
        atual = self.wifi_info[0]
        for i, (ssid, sig, sec) in enumerate(self.nets[:8]):
            barras = 4 if sig > -55 else 3 if sig > -65 else 2 if sig > -75 else 1
            box = self.grade(i)
            x1, y = box[2], box[1]
            d.rounded_rectangle(box, radius=18, fill=CARD, outline=GOOD if ssid == atual else None, width=3)
            d.text((box[0] + 25, y + 55), ssid[:20], font=self.f_txt, fill=FG, anchor="lm")
            for k in range(4):  # barras de sinal
                h = 12 + k * 9
                x = x1 - 90 + k * 16
                d.rectangle((x, y + 72 - h, x + 10, y + 72), fill=FG if k < barras else KEY)
            if sec:  # cadeado desenhado (a fonte não tem o emoji)
                x0 = x1 - 140
                d.arc((x0 + 4, y + 34, x0 + 22, y + 56), 180, 360, fill=MUTED, width=3)
                d.rectangle((x0, y + 46, x0 + 26, y + 70), fill=MUTED)
            self.buttons.append(Button(box, ssid, lambda s=ssid, p=sec: self.pick(s, p)))
        self.rodape(d, ("Voltar", self.go_home), ("Procurar", self.go_wifi))

    def draw_bt(self, d):
        W, st = self.W, self.bt
        d.text((W / 2, 45), "Bluetooth", font=self.f_mid, fill=FG, anchor="mm")
        aparelhos = st.get("aparelhos", [])
        if not st.get("pronto"):
            d.text((W / 2, 300), "Bluetooth iniciando...", font=self.f_txt, fill=MUTED, anchor="mm")
        elif not aparelhos:
            d.text((W / 2, 300), "procurando..." if st.get("procurando") else "toque em Procurar",
                   font=self.f_txt, fill=MUTED, anchor="mm")
        for i, a in enumerate(aparelhos[:8]):
            box = self.grade(i)
            x1, y = box[2], box[1]
            d.rounded_rectangle(box, radius=18, fill=CARD, outline=GOOD if a["conectado"] else None, width=3)
            d.text((box[0] + 25, y + 38), a["nome"][:18], font=self.f_txt, fill=FG, anchor="lm")
            if a["ocupado"]:
                estado = "aguarde..."
            elif a["conectado"]:
                estado = "conectado"
            elif a["pareado"]:
                estado = "pareado"
            else:
                estado = "toque para parear"
            sub = f"{a['tipo']} · {estado}" if a["tipo"] else estado
            limite = 34 - (9 if a["pareado"] else 0) - (8 if a["conectado"] and a["tipo"] == "som" else 0)
            d.text((box[0] + 25, y + 80), sub[:limite], font=self.f_small, fill=GOOD if a["conectado"] else MUTED,
                   anchor="lm")
            if not a["ocupado"]:
                if a["conectado"]:
                    acao = lambda m=a["mac"]: self.bt_do(f"disconnect {m}", "desconectando...")
                elif a["pareado"]:
                    acao = lambda m=a["mac"]: self.bt_do(f"connect {m}", "conectando...")
                else:
                    acao = lambda m=a["mac"]: self.bt_do(f"pair {m}", "pareando...")
                self.buttons.append(Button(box, a["nome"], acao))
            # botões pequenos à direita (registrados depois: têm prioridade no toque)
            bx = x1 - 85
            if a["pareado"]:
                self.btn(d, (bx, y + 20, bx + 65, y + 90), "✕",
                         lambda m=a["mac"]: self.bt_do(f"remove {m}", "esquecido"), f=self.f_txt)
                bx -= 80
            if a["conectado"] and a["tipo"] == "som":
                self.btn(d, (bx, y + 20, bx + 65, y + 90), "♪",
                         lambda m=a["mac"]: self.bt_do(f"som {m}", "tocando..."), color=(40, 110, 70),
                         f=self.f_txt)
        if st.get("msg") and not self.msg:
            d.text((W / 2, self.H - 125), st["msg"], font=self.f_small, fill=MUTED, anchor="mm")
        self.rodape(d, ("Voltar", self.go_home),
                    ("procurando..." if st.get("procurando") else "Procurar", self.bt_scan))

    def draw_password(self, d):
        W = self.W
        busca = self.teclado == "busca"
        d.rounded_rectangle((30, 20, 760, 100), radius=18, fill=CARD)
        shown = self.text if (self.show_pw or busca) else "•" * len(self.text)
        vazio = "buscar música" if busca else f"senha de {self.ssid[:18]}"
        d.text((55, 60), shown[-30:] or vazio, font=self.f_txt, fill=FG if self.text else MUTED, anchor="lm")
        if not busca:
            self.btn(d, (775, 20, 990, 100), "mostrar" if not self.show_pw else "ocultar", self.toggle_show,
                     f=self.f_small)
        self.btn(d, (1005, 20, W - 30, 100), "Cancelar", self.go_musica if busca else self.go_wifi,
                 f=self.f_small)

        rows = KEYS_SYM if self.sym else KEYS_LOWER
        top, kh, gap = 120, 104, 8
        kw = (W - 20 - gap * 9) / 10
        for r, row in enumerate(rows):
            n = len(row)
            x0 = (W - (kw * n + gap * (n - 1))) / 2
            for i, ch in enumerate(row):
                c = ch.upper() if (self.shift and not self.sym) else ch
                x = x0 + i * (kw + gap)
                y = top + r * (kh + gap)
                self.btn(d, (x, y, x + kw, y + kh), c, lambda c=c: self.type(c), f=self.f_mid)
        y = top + 4 * (kh + gap)
        self.btn(d, (10, y, 170, y + kh), "⇧" if not self.sym else "abc", self.toggle_shift, f=self.f_mid)
        self.btn(d, (180, y, 340, y + kh), "?123" if not self.sym else "ABC", self.toggle_sym, f=self.f_small)
        self.btn(d, (350, y, 780, y + kh), "espaço", lambda: self.type(" "), f=self.f_small)
        self.btn(d, (790, y, 950, y + kh), "⌫", self.backspace, f=self.f_mid)
        self.btn(d, (960, y, W - 10, y + kh), "Buscar" if busca else "Conectar", self.confirmar,
                 color=(40, 110, 70))

    # ---------- música
    def draw_musica(self, d):
        W = self.W
        d.text((W / 2, 45), "Música", font=self.f_mid, fill=FG, anchor="mm")
        if not self.player or not self.player.api.ok():
            d.text((W / 2, 300), "sem /etc/revive/navidrome.conf", font=self.f_small, fill=BAD, anchor="mm")
        opcoes = [("Artistas", self.go_letras), ("Álbuns recentes", lambda: self.albuns("newest")),
                  ("Álbuns mais tocados", lambda: self.albuns("frequent")),
                  ("Álbuns aleatórios", lambda: self.albuns("random")),
                  ("Músicas aleatórias", self.aleatorias), ("Playlists", self.go_playlists),
                  ("Buscar", self.go_busca)]
        if self.player_st:
            opcoes.insert(0, ("Tocando agora", self.go_tocando))
        for i, (nome, acao) in enumerate(opcoes[:8]):
            self.btn(d, self.grade(i), nome, acao, color=CARD)
        self.rodape(d, ("Voltar", self.go_home))

    def draw_letras(self, d):
        W = self.W
        d.text((W / 2, 45), "Artistas", font=self.f_mid, fill=FG, anchor="mm")
        letras = list("ABCDEFGHIJKLMNOPQRSTUVWXYZ#")
        cols, gap = 9, 12
        kw = (W - 60 - gap * (cols - 1)) / cols
        kh = 150
        for i, l in enumerate(letras):
            x = 30 + (i % cols) * (kw + gap)
            y = 95 + (i // cols) * (kh + gap)
            self.btn(d, (x, y, x + kw, y + kh), l, lambda l=l: self.artistas_letra(l), f=self.f_mid)
        self.rodape(d, ("Voltar", self.go_musica))

    def draw_lista(self, d):
        W, H, L = self.W, self.H, self.lista
        d.text((W / 2, 45), L["titulo"][:40], font=self.f_mid, fill=FG, anchor="mm")
        itens = L["itens"]
        por_pag = 8
        paginas = max(1, (len(itens) + por_pag - 1) // por_pag)
        self.pagina = min(self.pagina, paginas - 1)
        if not itens:
            d.text((W / 2, 300), L.get("vazio", "carregando..."), font=self.f_txt, fill=MUTED, anchor="mm")
        for i, it in enumerate(itens[self.pagina * por_pag:(self.pagina + 1) * por_pag]):
            box = self.grade(i)
            x0, y = box[0], box[1]
            d.rounded_rectangle(box, radius=18, fill=it.get("cor", CARD))
            if it.get("sub"):
                d.text((x0 + 25, y + 38), it["nome"][:26], font=self.f_txt, fill=FG, anchor="lm")
                d.text((x0 + 25, y + 82), it["sub"][:36], font=self.f_small, fill=MUTED, anchor="lm")
            else:
                d.text((x0 + 25, y + 55), it["nome"][:26], font=self.f_txt, fill=FG, anchor="lm")
            self.buttons.append(Button(box, it["nome"], it["acao"]))
        yb = H - 100
        self.btn(d, (30, yb, 400, yb + 80), "Voltar", L["voltar"] or self.go_musica)
        if paginas > 1:
            d.text((W / 2 + 70, yb + 40), f"{self.pagina + 1}/{paginas}", font=self.f_small, fill=MUTED,
                   anchor="mm")
            self.btn(d, (W - 470, yb, W - 260, yb + 80), "◀", lambda: self.mudar_pagina(-1), f=self.f_mid)
            self.btn(d, (W - 240, yb, W - 30, yb + 80), "▶", lambda: self.mudar_pagina(1), f=self.f_mid)

    def draw_tocando(self, d):
        W, H = self.W, self.H
        st = self.player.estado() if self.player else None  # na hora: reflete o toque em ⏯
        self.toc = self.player_st = st
        if not st:
            d.text((W / 2, 300), "nada tocando", font=self.f_txt, fill=MUTED, anchor="mm")
            self.rodape(d, ("Voltar", self.go_musica))
            return
        f = st["faixa"]
        lado = H - 60  # capa quadrada à esquerda
        capa = self.capa(f.get("coverArt"), lado)
        if capa:
            d._image.paste(capa, (30 + (lado - capa.width) // 2, 30 + (lado - capa.height) // 2))
        else:
            d.rounded_rectangle((30, 30, 30 + lado, 30 + lado), radius=20, fill=CARD)
            d.text((30 + lado / 2, 30 + lado / 2), "♪", font=font(160), fill=MUTED, anchor="mm")
        x0, x1 = lado + 70, W - 40
        cx = (x0 + x1) / 2
        d.text((cx, 70), f.get("title", "")[:24], font=self.f_mid, fill=FG, anchor="mm")
        d.text((cx, 125), f.get("artist", "")[:30], font=self.f_txt, fill=MUTED, anchor="mm")
        d.text((cx, 170), f.get("album", "")[:36], font=self.f_small, fill=MUTED, anchor="mm")
        # barra de progresso (toque nela para pular)
        yb = 235
        frac = st["t"] / st["dur"] if st["dur"] else 0
        d.rounded_rectangle((x0, yb - 6, x1, yb + 6), radius=6, fill=KEY)
        d.rounded_rectangle((x0, yb - 6, x0 + (x1 - x0) * frac, yb + 6), radius=6, fill=ACCENT)
        mmss = lambda s: f"{int(s) // 60}:{int(s) % 60:02d}"
        d.text((x0, yb + 32), mmss(st["t"]), font=self.f_small, fill=MUTED, anchor="lm")
        d.text((x1, yb + 32), mmss(st["dur"]), font=self.f_small, fill=MUTED, anchor="rm")
        d.text((cx, yb + 32), f"{st['pos'] + 1} de {st['total']}", font=self.f_small, fill=MUTED, anchor="mm")
        self.barra_geo = (x0, x1, yb, cx)
        self.buttons.append(Button((x0, yb - 40, x1, yb + 20), "barra",
                                   lambda: self.player.pular_para((self.ultimo_x - x0) / (x1 - x0))))
        # controles desenhados (a fonte não tem os símbolos de mídia)
        yc, h, bw = 310, 120, (x1 - x0 - 40) / 3
        caixas = [(x0 + k * (bw + 20), yc, x0 + k * (bw + 20) + bw, yc + h) for k in range(3)]
        for box, acao in zip(caixas, (self.player.anterior, self.player.pausar, self.player.proxima)):
            self.btn(d, box, "", acao)
        c = [(b[0] + b[2]) / 2 for b in caixas]
        cy = yc + h / 2
        d.polygon([(c[0] + 20, cy - 25), (c[0] + 20, cy + 25), (c[0] - 15, cy)], fill=FG)
        d.rectangle((c[0] - 25, cy - 25, c[0] - 17, cy + 25), fill=FG)
        if st["pausa"]:
            d.polygon([(c[1] - 18, cy - 30), (c[1] - 18, cy + 30), (c[1] + 28, cy)], fill=FG)
        else:
            d.rectangle((c[1] - 22, cy - 28, c[1] - 6, cy + 28), fill=FG)
            d.rectangle((c[1] + 6, cy - 28, c[1] + 22, cy + 28), fill=FG)
        d.polygon([(c[2] - 20, cy - 25), (c[2] - 20, cy + 25), (c[2] + 15, cy)], fill=FG)
        d.rectangle((c[2] + 17, cy - 25, c[2] + 25, cy + 25), fill=FG)
        yv = 455
        self.btn(d, (x0, yv, x0 + 150, yv + 80), "vol −", lambda: self.player.volume(-10), f=self.f_small)
        self.btn(d, (x1 - 150, yv, x1, yv + 80), "vol +", lambda: self.player.volume(10), f=self.f_small)
        vol = self.player.mpv.prop("volume")
        if vol is not None:
            d.text((cx, yv + 40), f"volume {int(vol)}%", font=self.f_small, fill=MUTED, anchor="mm")
        yb2 = H - 110
        self.btn(d, (x0, yb2, cx - 8, yb2 + 80), "Início", self.go_home, f=self.f_small)
        self.btn(d, (cx + 8, yb2, x1, yb2 + 80), "Música", self.go_musica, f=self.f_small)

    # ---------- ações
    def go_home(self):
        self.screen, self.msg = "home", ""

    def go_musica(self):
        self.screen, self.msg = "musica", ""

    def go_tocando(self):
        self.screen, self.msg = "tocando", ""

    def go_letras(self):
        self.screen, self.msg = "letras", ""
        if not self.artistas:
            self.carregar(lambda: self.player.api.artistas(), lambda r: setattr(self, "artistas", r),
                          tela=False)

    def go_busca(self):
        self.screen, self.teclado, self.text, self.msg = "password", "busca", "", ""
        self.sym = self.shift = False

    def confirmar(self):
        if self.teclado == "busca":
            self.buscar(self.text.strip())
        else:
            self.connect()

    def mudar_pagina(self, delta):
        self.pagina = max(0, self.pagina + delta)

    def capa(self, cover_id, tam=500):
        """Capa já redimensionada; baixa em segundo plano na primeira vez."""
        if not cover_id or not self.player:
            return None
        if cover_id in self.capas:
            return self.capas[cover_id]
        self.capas[cover_id] = None  # baixando

        def baixar():
            path = self.player.api.capa(cover_id, tam)
            if path:
                try:
                    img = Image.open(path).convert("RGB")
                    img.thumbnail((tam, tam))
                    self.capas[cover_id] = img
                    self.dirty = True
                    return
                except OSError:
                    pass
            time.sleep(10)  # falhou (rede oscilou): deixa tentar de novo
            self.capas.pop(cover_id, None)
        threading.Thread(target=baixar, daemon=True).start()
        return None

    def carregar(self, buscar, montar, titulo="", voltar=None, tela=True):
        """Busca no Navidrome em segundo plano e monta a lista quando chegar."""
        if tela:
            self.lista = {"titulo": titulo, "itens": [], "voltar": voltar}
            self.screen, self.pagina = "lista", 0
        self.msg = "carregando..."

        def run():
            try:
                r = buscar()
                montar(r)
                self.msg = ""
            except Exception as e:
                log(f"musica: {e!r}")
                self.msg = "Navidrome fora do ar" if "fora do ar" in str(e) else f"erro: {str(e)[:30]}"
                if tela:
                    self.lista["vazio"] = "não carregou"
            self.dirty = True
        threading.Thread(target=run, daemon=True).start()

    def mostrar(self, titulo, itens, voltar=None):
        self.lista = {"titulo": titulo, "itens": itens, "voltar": voltar, "vazio": "nada aqui"}
        self.screen, self.pagina, self.dirty = "lista", 0, True

    def tocar(self, faixas, inicio=0):
        self.msg = "abrindo..."
        self.screen = "tocando"

        def run():
            self.player.tocar(faixas, inicio)
            self.msg, self.dirty = "", True
        threading.Thread(target=run, daemon=True).start()

    def itens_faixas(self, faixas, com_album=False):
        itens = [{"nome": "▶ Tocar tudo", "acao": lambda: self.tocar(faixas), "cor": (40, 110, 70)},
                 {"nome": "Aleatório", "acao": lambda: self.tocar(random.sample(faixas, len(faixas)))}]
        for i, f in enumerate(faixas):
            dur = f.get("duration", 0)
            sub = f"{f.get('artist', '')} · {f.get('album', '')}" if com_album else f"{dur // 60}:{dur % 60:02d}"
            nome = f.get("title", "?") if com_album else f"{f.get('track', i + 1)}. {f.get('title', '?')}"
            itens.append({"nome": nome, "sub": sub, "acao": lambda i=i: self.tocar(faixas, i)})
        return itens

    def abrir_album(self, album, voltar=None):
        self.carregar(lambda: self.player.api.album(album["id"]),
                      lambda r: self.mostrar(album.get("name", "Álbum"), self.itens_faixas(r), voltar),
                      album.get("name", "Álbum"), voltar)

    def itens_albuns(self, albuns, voltar):
        return [{"nome": a.get("name", "?"), "sub": f"{a.get('artist', '')} · {a.get('year', '')}".strip(" ·"),
                 "acao": lambda a=a: self.abrir_album(a, voltar)} for a in albuns]

    def albuns(self, tipo):
        titulos = {"newest": "Recentes", "frequent": "Mais tocados", "random": "Aleatórios"}
        volta = lambda: self.albuns(tipo)
        self.carregar(lambda: self.player.api.lista_albuns(tipo),
                      lambda r: self.mostrar(titulos[tipo], self.itens_albuns(r, volta)),
                      titulos[tipo])

    def aleatorias(self):
        self.carregar(lambda: self.player.api.aleatorias(),
                      lambda r: self.mostrar("Aleatórias", self.itens_faixas(r, True)), "Aleatórias")

    def go_playlists(self):
        def montar(pls):
            self.mostrar("Playlists", [
                {"nome": p.get("name", "?"), "sub": f"{p.get('songCount', 0)} músicas",
                 "acao": lambda p=p: self.carregar(
                     lambda: self.player.api.playlist(p["id"]),
                     lambda r: self.mostrar(p.get("name", ""), self.itens_faixas(r, True), self.go_playlists),
                     p.get("name", ""), self.go_playlists)} for p in pls])
        self.carregar(self.player.api.playlists, montar, "Playlists")

    def artistas_letra(self, letra):
        def inicial(nome):
            c = unicodedata.normalize("NFD", nome.strip()[:1].upper())[:1]
            return c if "A" <= c <= "Z" else "#"
        lista = [a for a in self.artistas if inicial(a.get("name", "")) == letra]
        self.mostrar(f"Artistas · {letra}", [
            {"nome": a.get("name", "?"), "sub": f"{a.get('albumCount', 0)} álbuns",
             "acao": lambda a=a: self.abrir_artista(a, lambda: self.artistas_letra(letra))} for a in lista],
            self.go_letras)
        if not self.artistas:
            self.lista["vazio"] = "carregando artistas..."

    def abrir_artista(self, artista, voltar):
        volta = lambda: self.abrir_artista(artista, voltar)
        self.carregar(lambda: self.player.api.albuns_do_artista(artista["id"]),
                      lambda r: self.mostrar(artista.get("name", ""), self.itens_albuns(r, volta), voltar),
                      artista.get("name", ""), voltar)

    def buscar(self, texto):
        if not texto:
            return
        volta = lambda: self.buscar(texto)

        def montar(r):
            artistas, albuns, faixas = r
            itens = [{"nome": a.get("name", "?"), "sub": "artista",
                      "acao": lambda a=a: self.abrir_artista(a, volta)} for a in artistas]
            itens += self.itens_albuns(albuns, volta)
            itens += [{"nome": f.get("title", "?"), "sub": f"{f.get('artist', '')} · música",
                       "acao": lambda i=i: self.tocar(faixas, i)} for i, f in enumerate(faixas)]
            self.mostrar(f"“{texto[:16]}”", itens, self.go_busca)
        self.carregar(lambda: self.player.api.buscar(texto), montar, f"“{texto[:16]}”", self.go_busca)

    def go_wifi(self):
        self.screen, self.nets, self.msg = "wifi", [], ""
        self.draw()

        def scan():
            nets = wifi_scan()
            with self.lock:
                self.nets, self.dirty = nets, True
        threading.Thread(target=scan, daemon=True).start()

    def go_bt(self):
        self.screen, self.msg = "bt", ""
        self.bt_scan()

    def bt_scan(self):
        if not bt_cmd("scan"):
            self.msg = "serviço revive-bt parado"

    def bt_do(self, linha, aviso):
        self.msg = aviso if bt_cmd(linha) else "serviço revive-bt parado"

    def key(self, k):
        """Teclado físico (USB ou Bluetooth)."""
        if self.bt.get("prompt"):
            return  # o código é digitado no próprio teclado que está pareando
        if self.screen == "password":
            if k == "ENTER":
                self.confirmar()
            elif k == "BACKSPACE":
                self.backspace()
            elif k == "ESC":
                self.go_musica() if self.teclado == "busca" else self.go_wifi()
            elif len(k) == 1:
                self.text += k
        elif k == "ESC":
            self.go_home()
        elif k == " " and self.player and self.screen == "tocando":
            self.player.pausar()

    def pick(self, ssid, secured):
        self.ssid, self.text, self.teclado = ssid, "", "wifi"
        if secured:
            self.screen = "password"
        else:
            self.connect()

    def type(self, c):
        self.text += c
        if self.shift:
            self.shift = False

    def backspace(self):
        self.text = self.text[:-1]

    def toggle_shift(self):
        if self.sym:
            self.sym = False
        else:
            self.shift = not self.shift

    def toggle_sym(self):
        self.sym, self.shift = not self.sym, False

    def toggle_show(self):
        self.show_pw = not self.show_pw

    def connect(self):
        ssid, psk = self.ssid, self.text
        self.screen, self.msg = "home", f"conectando a {ssid}..."
        self.draw()

        def run():
            wifi_connect(ssid, psk)
            with self.lock:
                self.msg, self.dirty = "", True
            self.btc.refresh()
            self.dirty = True
        threading.Thread(target=run, daemon=True).start()

    def refresh_btc(self):
        self.msg = "atualizando..."
        self.draw()
        self.btc.refresh()
        self.msg = ""

    # ---------- laço principal
    def run(self):
        threading.Thread(target=self.btc_loop, daemon=True).start()
        threading.Thread(target=self.status_loop, daemon=True).start()
        last_min = ""
        last_scan = time.time()
        while True:
            if time.time() - last_scan > 3:  # teclado/mouse Bluetooth conectam a qualquer hora
                last_scan = time.time()
                self.inp.scan()
            r, _, _ = select.select(self.inp.fds(), [], [], 0.25)
            if r:
                self.boost()
            moveu = False
            for fd in r:
                for acao in self.inp.read(fd):
                    if acao[0] == "move":
                        moveu = True
                        continue
                    if acao[0] == "key":
                        self.key(acao[1])
                        self.dirty = True
                        continue
                    _, x, y = acao
                    self.ultimo_x = x
                    for b in reversed(self.buttons):
                        if b.hit(x, y):
                            self.flash(b)
                            b.action()
                            self.dirty = True
                            break
            self.check_bt()
            self.check_giro()
            if moveu and not self.dirty:
                self.cursor()
            minute = time.strftime("%H:%M")
            if minute != last_min and self.screen == "home":
                last_min, self.dirty = minute, True
            if self.progresso and not self.dirty:
                self.progresso = False
                self.atualizar_barra()
            if self.boost_ate and time.time() > self.boost_ate:
                self.boost_ate = 0
                self.cpu_min(False)
            if self.dirty:
                with self.lock:
                    self.dirty = False
                self.draw()

    # ---------- velocidade
    def flash(self, b):
        """Acende o botão tocado na hora (só aquele pedaço da tela), antes de redesenhar."""
        if self.frame is None:
            return
        box = tuple(int(v) for v in b.box)
        box = (max(box[0], 0), max(box[1], 0), min(box[2], self.W), min(box[3], self.H))
        if box[2] <= box[0] or box[3] <= box[1]:
            return
        pedaco = self.frame.crop(box)
        claro = Image.blend(pedaco, Image.new("RGB", pedaco.size, (255, 255, 255)), 0.3)
        self.fb.patch(claro, box[0], box[1])

    def cpu_min(self, alto):
        """Input boost: frequência mínima no máximo enquanto a pessoa mexe na tela."""
        base = "/sys/devices/system/cpu/cpu0/cpufreq"
        try:
            alvo = open(f"{base}/cpuinfo_{'max' if alto else 'min'}_freq").read().strip()
            with open(f"{base}/scaling_min_freq", "w") as f:
                f.write(alvo)
        except OSError:
            pass

    def boost(self):
        if not self.boost_ate:
            self.cpu_min(True)
        self.boost_ate = time.time() + 3

    def status_loop(self):
        """Wi-Fi e player lidos em segundo plano (o desenho não espera por eles)."""
        n = 0
        while True:
            try:
                if n % 5 == 0:
                    w = wifi_status()
                    if w != self.wifi_info:
                        self.wifi_info = w
                        if self.screen in ("home", "wifi"):
                            self.dirty = True
                st = self.player.estado() if self.player else None
                velho = self.player_st
                self.player_st = st
                chave = lambda e: e and (e["faixa"].get("id"), e["pausa"], e["pos"])
                if chave(st) != chave(velho):
                    if self.screen in ("home", "musica", "tocando"):
                        self.dirty = True
                elif self.screen == "tocando" and st:
                    self.progresso = True
            except Exception as e:
                log(f"status: {e!r}")
            n += 1
            time.sleep(1)

    def atualizar_barra(self):
        """Tela "Tocando agora": redesenha só a barra e os tempos a cada segundo."""
        st, geo = self.player_st, self.barra_geo
        if self.screen != "tocando" or not st or not geo or self.frame is None:
            return
        x0, x1, yb, cx = geo
        caixa = (int(x0) - 2, int(yb) - 10, int(x1) + 4, int(yb) + 55)
        d = ImageDraw.Draw(self.frame)
        d.rectangle(caixa, fill=BG)
        frac = st["t"] / st["dur"] if st["dur"] else 0
        d.rounded_rectangle((x0, yb - 6, x1, yb + 6), radius=6, fill=KEY)
        d.rounded_rectangle((x0, yb - 6, x0 + (x1 - x0) * frac, yb + 6), radius=6, fill=ACCENT)
        mmss = lambda s: f"{int(s) // 60}:{int(s) % 60:02d}"
        d.text((x0, yb + 32), mmss(st["t"]), font=self.f_small, fill=MUTED, anchor="lm")
        d.text((x1, yb + 32), mmss(st["dur"]), font=self.f_small, fill=MUTED, anchor="rm")
        d.text((cx, yb + 32), f"{st['pos'] + 1} de {st['total']}", font=self.f_small, fill=MUTED, anchor="mm")
        self.fb.patch(self.frame.crop(caixa), caixa[0], caixa[1])
        if self.cur_pos:  # a seta do mouse pode estar em cima da barra
            self.cur_pos = None
            self.cursor()

    def check_giro(self):
        """Gira a interface conforme o lado em que o celular está deitado (paisagem)."""
        agora = time.time()
        if agora - getattr(self, "ultimo_giro", 0) < 0.7:
            return
        self.ultimo_giro = agora
        if self.acel is None:
            if agora - getattr(self, "busca_acel", 0) > 30:
                self.busca_acel = agora
                self.acel = achar_acelerometro()
            if self.acel is None:
                return
        try:
            y = int(open(f"{self.acel}/in_accel_y_raw").read())
        except (OSError, ValueError):
            self.acel = None
            return
        # y do sensor ~ ±1000 com o aparelho de lado; deitado/em pé fica perto de 0
        alvo = 270 if y > 600 else 90 if y < -600 else self.fb.rot
        if alvo == self.fb.rot:
            self.giro_votos = 0
            return
        self.giro_votos += 1
        if self.giro_votos >= 2:  # duas leituras seguidas: não gira à toa
            self.giro_votos = 0
            self.fb.rot, self.cur_pos, self.dirty = alvo, None, True
            log(f"rotação {alvo}")
            try:
                os.makedirs(os.path.dirname(ROT_FILE), exist_ok=True)
                with open(ROT_FILE, "w") as f:
                    f.write(str(alvo))
            except OSError:
                pass

    def check_bt(self):
        """Relê o estado do Bluetooth quando o serviço revive-bt o atualiza."""
        try:
            mtime = os.stat(BT_STATE).st_mtime
        except OSError:
            return
        if mtime != self.bt_mtime:
            self.bt_mtime = mtime
            novo = bt_state()
            if novo and novo != self.bt:
                self.bt, self.dirty = novo, True
                if self.screen == "bt" and novo.get("msg"):
                    self.msg = ""  # a mensagem do serviço é mais nova que a da tela

    def btc_loop(self):
        while True:
            self.btc.refresh()
            self.dirty = True
            time.sleep(60)


if __name__ == "__main__":
    log("iniciando")
    while True:
        try:
            App().run()
        except Exception as e:  # não deixa a tela morrer; tenta de novo
            import traceback
            log(f"erro: {e!r}\n" + traceback.format_exc()[-1500:])
            time.sleep(5)
