"""Música do Navidrome (API Subsonic) tocada pelo mpv do serviço revive-mpv.

O mpv fica sempre rodando (socket JSON em /run/revive-mpv.sock), então a
música continua se a interface reiniciar. A fila com os dados das faixas
fica salva em /run/revive-queue.json pelo mesmo motivo.
"""
import configparser
import hashlib
import json
import os
import random
import socket
import string
import threading
import time
import urllib.parse
import urllib.request

CONF = "/etc/revive/navidrome.conf"
SOCK = "/run/revive-mpv.sock"
FILA = "/run/revive-queue.json"
CAPAS = "/tmp/revive-covers"


def _log(msg):
    try:
        with open("/var/log/revive-ui.log", "a") as f:
            f.write(time.strftime("%F %T ") + "musica: " + msg + "\n")
    except OSError:
        pass


class Subsonic:
    def __init__(self):
        c = configparser.ConfigParser()
        c.read(CONF)
        s = c["navidrome"] if c.has_section("navidrome") else {}
        self.urls = s.get("urls", "").split()
        self.user = s.get("user", "")
        self.pw = s.get("pass", "")
        self.base = None

    def ok(self):
        return bool(self.urls and self.user)

    def _auth(self):
        salt = "".join(random.choices(string.ascii_lowercase + string.digits, k=10))
        token = hashlib.md5((self.pw + salt).encode()).hexdigest()
        return {"u": self.user, "t": token, "s": salt, "v": "1.16.1", "c": "revive", "f": "json"}

    def _escolher(self):
        """Em casa usa o endereço da rede local; fora, o público."""
        for url in self.urls:
            try:
                q = urllib.parse.urlencode(self._auth())
                with urllib.request.urlopen(f"{url}/rest/ping.view?{q}", timeout=3) as r:
                    if json.load(r)["subsonic-response"]["status"] == "ok":
                        self.base = url
                        _log(f"servidor {url}")
                        return
            except Exception as e:
                _log(f"{url}: {e}")
        raise OSError("Navidrome fora do ar")

    def url(self, metodo, **params):
        if not self.base:
            self._escolher()
        q = urllib.parse.urlencode({**self._auth(), **params})
        return f"{self.base}/rest/{metodo}.view?{q}"

    def get(self, metodo, **params):
        for tentativa in (0, 1):
            try:
                with urllib.request.urlopen(self.url(metodo, **params), timeout=15) as r:
                    d = json.load(r)["subsonic-response"]
                if d.get("status") != "ok":
                    raise OSError(d.get("error", {}).get("message", "erro"))
                return d
            except (OSError, ValueError) as e:
                if tentativa:
                    raise
                _log(f"{metodo}: {e}; trocando de servidor")
                self.base = None  # mudou de rede: escolhe o endereço de novo
        raise OSError("sem resposta do Navidrome")

    # ---------------------------------------------------------- consultas
    def artistas(self):
        d = self.get("getArtists")
        return [a for idx in d["artists"].get("index", []) for a in idx.get("artist", [])]

    def albuns_do_artista(self, artist_id):
        return self.get("getArtist", id=artist_id)["artist"].get("album", [])

    def album(self, album_id):
        return self.get("getAlbum", id=album_id)["album"].get("song", [])

    def lista_albuns(self, tipo, n=40):
        return self.get("getAlbumList2", type=tipo, size=n)["albumList2"].get("album", [])

    def aleatorias(self, n=50):
        return self.get("getRandomSongs", size=n)["randomSongs"].get("song", [])

    def playlists(self):
        return self.get("getPlaylists")["playlists"].get("playlist", [])

    def playlist(self, pid):
        return self.get("getPlaylist", id=pid)["playlist"].get("entry", [])

    def buscar(self, texto):
        r = self.get("search3", query=texto, artistCount=10, albumCount=10, songCount=30)["searchResult3"]
        return r.get("artist", []), r.get("album", []), r.get("song", [])

    def stream(self, song_id):
        return self.url("stream", id=song_id)

    def capa(self, cover_id, tamanho=500):
        """Baixa a capa uma vez e guarda em /tmp."""
        if not cover_id:
            return None
        os.makedirs(CAPAS, exist_ok=True)
        path = f"{CAPAS}/{cover_id}-{tamanho}.jpg"
        if not os.path.exists(path):
            try:
                with urllib.request.urlopen(self.url("getCoverArt", id=cover_id, size=tamanho), timeout=15) as r:
                    data = r.read()
                with open(path + ".tmp", "wb") as f:
                    f.write(data)
                os.replace(path + ".tmp", path)
            except Exception as e:
                _log(f"capa {cover_id}: {e}")
                return None
        return path

    def scrobble(self, song_id, terminou):
        try:
            self.get("scrobble", id=song_id, submission="true" if terminou else "false")
        except Exception as e:
            _log(f"scrobble: {e}")


class Mpv:
    """Cliente do socket JSON do mpv."""

    def __init__(self):
        self.lock = threading.Lock()

    def cmd(self, *args):
        with self.lock:
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.settimeout(2)
                s.connect(SOCK)
                s.sendall((json.dumps({"command": list(args)}) + "\n").encode())
                buf = b""
                while True:  # o mpv também manda eventos; procura a resposta
                    chunk = s.recv(65536)
                    if not chunk:
                        return None
                    buf += chunk
                    for linha in buf.split(b"\n")[:-1]:
                        msg = json.loads(linha)
                        if "error" in msg:
                            s.close()
                            return msg.get("data") if msg["error"] == "success" else None
                    buf = buf.split(b"\n")[-1]
            except (OSError, ValueError):
                return None

    def prop(self, nome):
        return self.cmd("get_property", nome)


class Player:
    """Fila de faixas + mpv + scrobble."""

    def __init__(self):
        self.api = Subsonic()
        self.mpv = Mpv()
        self.fila = []
        self.pos = -1
        self.scrobbled = None
        try:
            with open(FILA) as f:
                self.fila = json.load(f)
        except (OSError, ValueError):
            pass

    def tocar(self, faixas, inicio=0):
        if not faixas:
            return
        self.mpv.cmd("playlist-clear")
        self.mpv.cmd("loadfile", self.api.stream(faixas[inicio]["id"]), "replace")
        for s in faixas[inicio + 1:]:
            self.mpv.cmd("loadfile", self.api.stream(s["id"]), "append")
        for s in faixas[:inicio]:  # o começo do álbum vai para o fim da fila
            self.mpv.cmd("loadfile", self.api.stream(s["id"]), "append")
        self.fila = faixas[inicio:] + faixas[:inicio]
        with open(FILA, "w") as f:
            json.dump(self.fila, f)
        self.mpv.cmd("set_property", "pause", False)
        _log(f"tocando {len(faixas)} faixas")

    def estado(self):
        """Faixa atual, tempo, duração e pausa (None se nada tocando)."""
        pos = self.mpv.prop("playlist-pos")
        if pos is None or pos < 0 or pos >= len(self.fila):
            return None
        faixa = self.fila[pos]
        t = self.mpv.prop("time-pos") or 0
        dur = self.mpv.prop("duration") or faixa.get("duration") or 0
        pausa = bool(self.mpv.prop("pause"))
        if pos != self.pos:
            self.pos = pos
            threading.Thread(target=self.api.scrobble, args=(faixa["id"], False), daemon=True).start()
        if dur and t > dur / 2 and self.scrobbled != (pos, faixa["id"]):
            self.scrobbled = (pos, faixa["id"])
            threading.Thread(target=self.api.scrobble, args=(faixa["id"], True), daemon=True).start()
        return {"faixa": faixa, "t": t, "dur": dur, "pausa": pausa, "pos": pos, "total": len(self.fila)}

    def pausar(self):
        self.mpv.cmd("cycle", "pause")

    def proxima(self):
        self.mpv.cmd("playlist-next", "force")

    def anterior(self):
        t = self.mpv.prop("time-pos") or 0
        if t > 5:
            self.mpv.cmd("seek", 0, "absolute")
        else:
            self.mpv.cmd("playlist-prev", "force")

    def volume(self, delta):
        v = self.mpv.prop("volume") or 80
        v = max(0, min(130, v + delta))
        self.mpv.cmd("set_property", "volume", v)
        return v

    def pular_para(self, fracao):
        dur = self.mpv.prop("duration")
        if dur:
            self.mpv.cmd("seek", dur * fracao, "absolute")
