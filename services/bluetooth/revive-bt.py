#!/usr/bin/env python3
"""Revive: Bluetooth para a interface na tela.

Fala com o BlueZ pelo D-Bus: liga o adaptador, procura aparelhos, pareia,
confia e conecta (mouse, teclado, caixa de som). Registra o agente de
pareamento: quando um teclado pede um código, o código vai para a tela.

A interface (revive-ui) lê o estado em /run/revive-bt/state.json e manda
comandos de uma linha pelo FIFO /run/revive-bt/cmd:
    scan | pair MAC | connect MAC | disconnect MAC | remove MAC | som MAC
O áudio das caixas sai pelo bluez-alsa (perfil A2DP).
"""
import json
import math
import os
import random
import socket
import struct
import subprocess
import time
import wave

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

RUN = "/run/revive-bt"
STATE = f"{RUN}/state.json"
CMD = f"{RUN}/cmd"
LOG = "/var/log/revive-bt.log"
SOM = "/usr/share/revive/test.wav"
ASOUND = "/etc/asound.conf"
MESH = "/etc/revive/mesh.conf"
MPV_SOCK = "/run/revive-mpv.sock"
# placa de som do alto-falante interno (msm8226 "falcon"); só existe com o DTB de áudio
CARTAO_INTERNO = "/proc/asound/falcon"
ASOUND_INTERNO = ("# escrito pelo revive-bt: alto-falante interno (nenhuma caixa conectada)\n"
                  'pcm.!default { type plug slave.pcm "hw:falcon,0" }\n'
                  'ctl.!default { type hw card falcon }\n')

BLUEZ = "org.bluez"
ADAPTER = "org.bluez.Adapter1"
DEVICE = "org.bluez.Device1"
PROPS = "org.freedesktop.DBus.Properties"
AGENT_PATH = "/revive/agent"
SCAN_SEGUNDOS = 30


def log(msg):
    try:
        with open(LOG, "a") as f:
            f.write(time.strftime("%F %T ") + msg + "\n")
        if os.path.getsize(LOG) > 1_000_000:
            os.replace(LOG, LOG + ".1")
    except OSError:
        pass


def mac_do_path(path):
    """Caminho D-Bus do BlueZ (dev_AA_BB...) -> MAC."""
    nome = str(path).rsplit("/", 1)[-1]
    return nome[4:].replace("_", ":") if nome.startswith("dev_") else ""


def pin_mesh(mac):
    """PIN fixo do nó Meshtastic (mesh.conf: ble=MAC / pin=...)."""
    ble = pin = None
    try:
        with open(MESH) as f:
            for linha in f:
                chave, sep, valor = linha.partition("=")
                if not sep:
                    continue
                chave, valor = chave.strip().lower(), valor.strip()
                if chave == "ble":
                    ble = valor.upper()
                elif chave == "pin":
                    pin = valor
    except OSError:
        return None
    if ble == mac.upper():
        return pin or "123456"  # PIN padrão do Meshtastic
    return None


def tipo(icon):
    """Ícone do BlueZ -> tipo mostrado na tela."""
    if icon.startswith("input-keyboard"):
        return "teclado"
    if icon.startswith("input-mouse") or icon.startswith("input-tablet"):
        return "mouse"
    if icon.startswith("input-gaming"):
        return "controle"
    if icon.startswith("audio"):
        return "som"
    if icon.startswith("phone"):
        return "celular"
    if icon.startswith("computer"):
        return "computador"
    return ""


def recarregar_mpv():
    """O mpv só relê o asound.conf ao reabrir a saída de áudio."""
    try:
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(2)
        s.connect(MPV_SOCK)
        s.sendall(b'{"command":["set_property","audio-device","auto"]}\n{"command":["ao-reload"]}\n')
        s.close()
    except OSError as e:
        log(f"mpv: {e}")


def gerar_som():
    """Dois bipes (dó-sol) para testar a caixa de som."""
    if os.path.exists(SOM) and os.path.getsize(SOM) > 200_000:  # versão com silêncio no início
        return
    os.makedirs(os.path.dirname(SOM), exist_ok=True)
    rate = 44100
    # 0,6 s de silêncio: a caixa engole o começo enquanto o canal A2DP abre
    frames = bytearray(int(rate * 0.6) * 4)
    for freq in (523.25, 783.99):
        n = int(rate * 0.45)
        for i in range(n):
            env = min(1.0, i / 800, (n - i) / 4000)
            v = int(12000 * env * math.sin(2 * math.pi * freq * i / rate))
            frames += struct.pack("<hh", v, v)
    with wave.open(SOM, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(bytes(frames))


class Agente(dbus.service.Object):
    """Agente de pareamento. Aceita o que o próprio usuário pediu na tela e
    mostra os códigos que precisam ser digitados no aparelho."""

    def __init__(self, bus, bt):
        super().__init__(bus, AGENT_PATH)
        self.bt = bt

    def nome(self, path):
        return self.bt.nome_de(path)

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Release(self):
        pass

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="s")
    def RequestPinCode(self, path):
        mac = mac_do_path(path)
        fixo = pin_mesh(mac)
        if fixo:
            log(f"agente: PIN do mesh.conf para {mac}")
            return fixo
        pin = f"{random.randint(0, 999999):06d}"
        self.bt.prompt = {"texto": f"Digite {pin} no {self.nome(path)} e aperte Enter", "codigo": pin}
        log(f"agente: PIN {pin} para {path}")
        self.bt.salvar()
        return pin

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def DisplayPinCode(self, path, pin):
        self.bt.prompt = {"texto": f"Digite {pin} no {self.nome(path)} e aperte Enter", "codigo": str(pin)}
        self.bt.salvar()

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="u")
    def RequestPasskey(self, path):
        mac = mac_do_path(path)
        fixo = pin_mesh(mac)
        if fixo and fixo.isdigit():
            log(f"agente: passkey do mesh.conf para {mac}")
            return dbus.UInt32(int(fixo))
        chave = random.randint(0, 999999)
        self.bt.prompt = {"texto": f"Digite {chave:06d} no {self.nome(path)} e aperte Enter",
                          "codigo": f"{chave:06d}"}
        self.bt.salvar()
        return dbus.UInt32(chave)

    @dbus.service.method("org.bluez.Agent1", in_signature="ouq", out_signature="")
    def DisplayPasskey(self, path, chave, digitados):
        self.bt.prompt = {"texto": f"Digite {int(chave):06d} no {self.nome(path)} e aperte Enter",
                          "codigo": f"{int(chave):06d}", "digitados": int(digitados)}
        self.bt.salvar()

    @dbus.service.method("org.bluez.Agent1", in_signature="ou", out_signature="")
    def RequestConfirmation(self, path, chave):
        # o pareamento foi pedido na tela; confirma (o código aparece por 1 instante)
        log(f"agente: confirmação {int(chave):06d} para {path}")

    @dbus.service.method("org.bluez.Agent1", in_signature="o", out_signature="")
    def RequestAuthorization(self, path):
        pass

    @dbus.service.method("org.bluez.Agent1", in_signature="os", out_signature="")
    def AuthorizeService(self, path, uuid):
        pass

    @dbus.service.method("org.bluez.Agent1", in_signature="", out_signature="")
    def Cancel(self):
        self.bt.prompt = None
        self.bt.salvar()


class Bluetooth:
    def __init__(self):
        dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
        self.bus = dbus.SystemBus()
        self.om = dbus.Interface(self.bus.get_object(BLUEZ, "/"), "org.freedesktop.DBus.ObjectManager")
        self.adapter_path = None
        self.scanning = False
        self.scan_fim = 0
        self.prompt = None
        self.msg = ""
        self.ocupado = set()  # MACs com pareamento/conexão em andamento
        self.ultimo_estado = ""
        self.tentativas = {}  # MAC -> hora da última reconexão automática
        self.agente = Agente(self.bus, self)

    # ---------------------------------------------------------- utilidades
    def objetos(self):
        return self.om.GetManagedObjects()

    def achar_adaptador(self):
        for path, ifaces in self.objetos().items():
            if ADAPTER in ifaces:
                return path
        return None

    def preparar(self):
        """Liga o adaptador e registra o agente (repete até o hci0 existir)."""
        path = self.achar_adaptador()
        if not path:
            log("sem adaptador ainda")
            return True  # tenta de novo
        self.adapter_path = path
        props = dbus.Interface(self.bus.get_object(BLUEZ, path), PROPS)
        for nome, valor in (("Powered", True), ("Pairable", True)):
            try:
                props.Set(ADAPTER, nome, dbus.Boolean(valor))
            except dbus.DBusException as e:
                log(f"adaptador {nome}: {e}")
        try:
            mgr = dbus.Interface(self.bus.get_object(BLUEZ, "/org/bluez"), "org.bluez.AgentManager1")
            mgr.RegisterAgent(AGENT_PATH, "KeyboardDisplay")
            mgr.RequestDefaultAgent(AGENT_PATH)
        except dbus.DBusException as e:
            if "AlreadyExists" not in str(e):
                log(f"agente: {e}")
        endereco = props.Get(ADAPTER, "Address")
        log(f"adaptador {path} {endereco} pronto")
        return False  # pronto, para de repetir

    def dev(self, mac):
        return f"{self.adapter_path}/dev_{mac.replace(':', '_')}"

    def nome_de(self, path):
        try:
            props = dbus.Interface(self.bus.get_object(BLUEZ, path), PROPS)
            return str(props.Get(DEVICE, "Alias"))
        except dbus.DBusException:
            return "aparelho"

    def iface(self, mac):
        return dbus.Interface(self.bus.get_object(BLUEZ, self.dev(mac)), DEVICE)

    # ---------------------------------------------------------- estado
    def aparelhos(self):
        lista = []
        for path, ifaces in self.objetos().items():
            d = ifaces.get(DEVICE)
            if not d or not str(path).startswith(str(self.adapter_path)):
                continue
            nome = str(d.get("Name", ""))
            pareado = bool(d.get("Paired", False))
            if not nome and not pareado:
                continue  # sem nome não dá para o usuário reconhecer
            lista.append({
                "mac": str(d.get("Address", "")),
                "nome": str(d.get("Alias", nome)) or str(d.get("Address", "")),
                "tipo": tipo(str(d.get("Icon", ""))),
                "pareado": pareado,
                "conectado": bool(d.get("Connected", False)),
                "rssi": int(d["RSSI"]) if "RSSI" in d else None,
                "ocupado": str(d.get("Address", "")) in self.ocupado,
            })
        lista.sort(key=lambda a: (not a["conectado"], not a["pareado"], -(a["rssi"] or -999)))
        return lista

    def salvar(self):
        try:
            estado = {"pronto": self.adapter_path is not None, "procurando": self.scanning,
                      "aparelhos": self.aparelhos() if self.adapter_path else [],
                      "prompt": self.prompt, "msg": self.msg}
        except dbus.DBusException as e:
            log(f"estado: {e}")
            return True
        texto = json.dumps(estado, ensure_ascii=False)
        if texto != self.ultimo_estado:
            self.ultimo_estado = texto
            tmp = STATE + ".tmp"
            with open(tmp, "w") as f:
                f.write(texto)
            os.replace(tmp, STATE)
        return True

    def tick(self):
        if self.adapter_path is None:
            return True
        if self.scanning and time.time() > self.scan_fim:
            self.parar_scan()
        self.reconectar_som()
        self.saida_de_som()
        self.salvar()
        return True

    # ---------------------------------------------------------- ações
    def scan(self):
        a = dbus.Interface(self.bus.get_object(BLUEZ, self.adapter_path), ADAPTER)
        try:
            a.SetDiscoveryFilter({"Transport": "auto"})
        except dbus.DBusException:
            pass
        try:
            a.StartDiscovery()
        except dbus.DBusException as e:
            if "InProgress" not in str(e):
                log(f"scan: {e}")
                self.msg = "erro ao procurar"
                return
        self.scanning, self.scan_fim, self.msg = True, time.time() + SCAN_SEGUNDOS, ""
        log("procurando aparelhos")

    def parar_scan(self):
        self.scanning = False
        try:
            dbus.Interface(self.bus.get_object(BLUEZ, self.adapter_path), ADAPTER).StopDiscovery()
        except dbus.DBusException:
            pass

    def pair(self, mac):
        self.parar_scan()  # o rádio é um só: procurar atrapalha o pareamento
        self.ocupado.add(mac)
        self.msg = "pareando..."
        nome = self.nome_de(self.dev(mac))
        log(f"parear {mac} {nome}")

        def ok():
            log(f"pareado {mac}")
            self.prompt = None
            props = dbus.Interface(self.bus.get_object(BLUEZ, self.dev(mac)), PROPS)
            props.Set(DEVICE, "Trusted", dbus.Boolean(True))
            self.connect(mac)

        def erro(e):
            log(f"parear {mac}: {e}")
            self.ocupado.discard(mac)
            self.prompt = None
            if "AlreadyExists" in str(e):
                self.connect(mac)
            else:
                self.msg = f"não pareou: {e.get_dbus_name().split('.')[-1]}"
            self.salvar()

        self.iface(mac).Pair(reply_handler=ok, error_handler=erro, timeout=90)

    def connect(self, mac, auto=False):
        self.ocupado.add(mac)
        if not auto:
            self.msg = "conectando..."
        try:
            dbus.Interface(self.bus.get_object(BLUEZ, self.dev(mac)), PROPS).Set(
                DEVICE, "Trusted", dbus.Boolean(True))
        except dbus.DBusException:
            pass

        def ok():
            log(f"conectado {mac}")
            self.ocupado.discard(mac)
            if not auto:
                self.msg = ""
            self.depois_de_conectar(mac)
            self.salvar()

        def erro(e):
            log(f"conectar {mac}: {e}")
            self.ocupado.discard(mac)
            if not auto:
                self.msg = f"não conectou: {e.get_dbus_name().split('.')[-1]}"
            self.salvar()

        self.iface(mac).Connect(reply_handler=ok, error_handler=erro, timeout=40)

    def disconnect(self, mac):
        try:
            self.iface(mac).Disconnect()
        except dbus.DBusException as e:
            log(f"desconectar {mac}: {e}")

    def remove(self, mac):
        try:
            a = dbus.Interface(self.bus.get_object(BLUEZ, self.adapter_path), ADAPTER)
            a.RemoveDevice(self.dev(mac))
            log(f"esquecido {mac}")
        except dbus.DBusException as e:
            log(f"esquecer {mac}: {e}")

    def depois_de_conectar(self, mac):
        """Caixa de som conectada vira a saída padrão do ALSA."""
        aparelho = next((a for a in self.aparelhos() if a["mac"] == mac), None)
        if aparelho and aparelho["tipo"] == "som":
            with open(ASOUND, "w") as f:
                f.write("# escrito pelo revive-bt: última caixa de som conectada\n"
                        f'defaults.bluealsa.device "{mac}"\n'
                        'defaults.bluealsa.profile "a2dp"\n'
                        'pcm.!default { type plug slave.pcm "bluealsa" }\n'
                        'ctl.!default { type bluealsa }\n')
            recarregar_mpv()
            self.som(mac)

    def saida_de_som(self):
        """Sem caixa conectada, a saída padrão volta para o alto-falante interno."""
        if any(a["tipo"] == "som" and a["conectado"] for a in self.aparelhos()):
            return
        if not os.path.exists(CARTAO_INTERNO):
            return
        try:
            with open(ASOUND) as f:
                if f.read() == ASOUND_INTERNO:
                    return
        except OSError:
            pass
        with open(ASOUND, "w") as f:
            f.write(ASOUND_INTERNO)
        log("saída de som: alto-falante interno")
        recarregar_mpv()

    def som(self, mac):
        gerar_som()
        subprocess.Popen(["aplay", "-q", "-D", f"bluealsa:DEV={mac},PROFILE=a2dp", SOM],
                         stdout=subprocess.DEVNULL, stderr=open(LOG, "a"))

    def reconectar_som(self):
        """Caixas de som não se reconectam sozinhas: tenta a cada 2 min."""
        agora = time.time()
        for a in self.aparelhos():
            if a["tipo"] == "som" and a["pareado"] and not a["conectado"] and not a["ocupado"]:
                if agora - self.tentativas.get(a["mac"], 0) > 120:
                    self.tentativas[a["mac"]] = agora
                    self.connect(a["mac"], auto=True)

    def comando(self, linha):
        partes = linha.split()
        if not partes or self.adapter_path is None:
            return
        acao, mac = partes[0], (partes[1].upper() if len(partes) > 1 else "")
        log(f"comando: {linha}")
        try:
            if acao == "scan":
                self.scan()
            elif acao == "pair" and mac:
                self.pair(mac)
            elif acao == "connect" and mac:
                self.connect(mac)
            elif acao == "disconnect" and mac:
                self.disconnect(mac)
            elif acao == "remove" and mac:
                self.remove(mac)
            elif acao == "som" and mac:
                self.som(mac)
            elif acao == "cancelar":
                self.prompt, self.msg = None, ""
        except dbus.DBusException as e:
            log(f"{linha}: {e}")
            self.msg = "erro no Bluetooth"
        self.salvar()

    # ---------------------------------------------------------- laço
    def run(self):
        os.makedirs(RUN, exist_ok=True)
        if not os.path.exists(CMD):
            os.mkfifo(CMD, 0o622)
        os.chmod(CMD, 0o622)
        fd = os.open(CMD, os.O_RDWR | os.O_NONBLOCK)  # RDWR: nunca recebe EOF
        buf = bytearray()

        def ler(_fd, _cond):
            try:
                buf.extend(os.read(fd, 4096))
            except BlockingIOError:
                pass
            while b"\n" in buf:
                linha, _, resto = bytes(buf).partition(b"\n")
                buf[:] = resto
                self.comando(linha.decode(errors="replace").strip())
            return True

        GLib.io_add_watch(fd, GLib.IO_IN, ler)
        if self.preparar():
            GLib.timeout_add_seconds(3, self.preparar)
        GLib.timeout_add(1500, self.tick)
        self.salvar()
        log("iniciado")
        GLib.MainLoop().run()


if __name__ == "__main__":
    gerar_som()
    Bluetooth().run()
