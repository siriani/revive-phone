#!/usr/bin/env python3
"""Revive: ponte Meshtastic por Bluetooth LE -> TCP 127.0.0.1:4403.

O app (ui-slint) fala o protocolo de stream do Meshtastic na porta 4403; aqui a
ponte conecta no nó por BLE e repassa os pacotes. Lê /etc/revive/mesh.conf:
    ble=AA:BB:CC:DD:EE:FF   (sem ela, procura por 15 s e grava o endereço)
    pin=123456              (opcional; o agente do revive-bt usa no pareamento)
O app lê host= no mesmo arquivo; na ponte use host=127.0.0.1.

Cada quadro do protocolo é 0x94 0xC3 + tamanho u16 big-endian + protobuf.
Logs curtos em stdout (o OpenRC manda para /var/log/revive-mesh-ble.log).
"""
import asyncio
import os
import struct
import subprocess
import sys
import time

try:
    from bleak import BleakClient, BleakError, BleakScanner
except ImportError:
    print("revive-mesh-ble: instale o pacote py3-bleak", file=sys.stderr)
    sys.exit(1)

CONF = "/etc/revive/mesh.conf"
HOST = "127.0.0.1"
PORTA = 4403
MAGIC = b"\x94\xc3"

# UUIDs do meshtastic/python (meshtastic/ble_interface.py)
SERVICO = "6ba1b218-15a8-461f-9fa8-5dcae273eafd"
TORADIO = "f75c76d2-129e-4dad-a1dd-7866124401e7"    # write
FROMRADIO = "2c55e69e-4993-11ed-b878-0242ac120002"  # read
FROMNUM = "ed9da18c-a800-4f66-a670-aa7547e34453"    # notify

SCAN = 15
ESPERA_MIN, ESPERA_MAX = 5, 60


def log(msg):
    print(time.strftime("%F %T"), msg, flush=True)


def ler_conf(caminho=CONF):
    """Devolve (ble, pin, linhas). host= e porta= são do app, não daqui."""
    ble = pin = None
    linhas = []
    try:
        with open(caminho, encoding="utf-8") as f:
            linhas = f.read().splitlines()
    except OSError:
        pass
    for linha in linhas:
        chave, sep, valor = linha.partition("=")
        if not sep:
            continue
        chave, valor = chave.strip().lower(), valor.strip()
        if chave == "ble" and valor:
            ble = valor.upper()
        elif chave == "pin" and valor:
            pin = valor
    return ble, pin, linhas


def gravar_ble(mac, linhas, caminho=CONF):
    """Grava ble=MAC mantendo as outras linhas (host= etc.)."""
    novas, achou = [], False
    for linha in linhas:
        chave, sep, _ = linha.partition("=")
        if sep and chave.strip().lower() == "ble":
            if not achou:
                novas.append(f"ble={mac.upper()}")
                achou = True
            continue
        novas.append(linha)
    if not achou:
        novas.append(f"ble={mac.upper()}")
    pasta = os.path.dirname(caminho)
    if pasta:
        os.makedirs(pasta, exist_ok=True)
    tmp = caminho + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write("\n".join(novas).rstrip("\n") + "\n")
    os.chmod(tmp, 0o600)  # o arquivo pode ter pin=
    os.replace(tmp, caminho)


def quadro(payload):
    return MAGIC + struct.pack(">H", len(payload)) + payload


def pede_pareamento(erro):
    texto = str(erro).lower()
    return any(t in texto for t in ("auth", "encryp", "pair", "insufficient"))


async def procurar():
    """Procura por 15 s um nó que anuncie o serviço GATT do Meshtastic."""
    achados = []

    def viu(dispositivo, adv):
        uuids = {str(u).lower() for u in (adv.service_uuids or [])}
        if SERVICO in uuids:
            achados.append(dispositivo.address)

    log(f"procurando nó Meshtastic por {SCAN} s...")
    scanner = BleakScanner(detection_callback=viu)
    await scanner.start()
    try:
        await asyncio.sleep(SCAN)
    finally:
        await scanner.stop()
    return achados[0] if achados else None


def mtu_de(cliente):
    """MTU negociado, com 23 (o mínimo BLE) como reserva."""
    for caminho in ("mtu_size", "_mtu_size"):
        valor = getattr(cliente, caminho, None)
        if isinstance(valor, int) and valor >= 23:
            return valor
    backend = getattr(cliente, "_backend", None)
    valor = getattr(backend, "_mtu_size", None)
    if isinstance(valor, int) and valor >= 23:
        return valor
    return 23


async def pedir_mtu(cliente):
    """Pede MTU 512 quando o backend deixa; o BlueZ costuma negociar sozinho."""
    backend = getattr(cliente, "_backend", None)
    for alvo in (cliente, backend):
        pedir = getattr(alvo, "request_mtu", None)
        if callable(pedir):
            try:
                await asyncio.wait_for(pedir(512), 5)
                break
            except Exception:
                pass
    if backend is not None and hasattr(backend, "_acquire_mtu"):
        try:
            await asyncio.wait_for(backend._acquire_mtu(), 5)
        except Exception:
            pass
    return mtu_de(cliente)


class Ponte:
    def __init__(self, mac, pin=None):
        self.mac = mac
        self.pin = pin  # usado pelo agente do revive-bt, não pelo bleak
        self.ble = None
        self.mtu = 23
        self.tcp = None
        self.trava_tcp = asyncio.Lock()
        self.trava_ble = asyncio.Lock()
        self.parar = asyncio.Event()
        self.tarefas = set()
        # pacotes do app que chegaram com o BLE fora (ex.: want_config logo ao abrir):
        # vão para o nó assim que ele conectar, senão o app esperaria para sempre
        self.pendentes = []

    # ---------------------------------------------------------- TCP
    async def para_cliente(self, payload):
        async with self.trava_tcp:
            escritor = self.tcp
            if escritor is None:
                return
            try:
                escritor.write(quadro(payload))
                await escritor.drain()
            except (ConnectionError, OSError, RuntimeError):
                self.tcp = None

    async def para_radio(self, payload):
        cliente = self.ble
        if cliente is None or not cliente.is_connected:
            self.pendentes = (self.pendentes + [payload])[-10:]
            log("sem BLE: pacote do app guardado para quando o nó conectar")
            return
        parar = self.parar
        passo = max(1, self.mtu - 3)
        try:
            async with self.trava_ble:
                for i in range(0, len(payload), passo):
                    await cliente.write_gatt_char(TORADIO, payload[i:i + passo], response=True)
        except Exception as e:
            log(f"ToRadio: {e}")
            parar.set()
            return
        # como o cliente BLE oficial (meshtastic-python): depois de escrever, lê o
        # FromRadio até esvaziar — não depende do aviso do FromNum chegar
        await self.drenar()

    async def tcp_cliente(self, leitor, escritor):
        async with self.trava_tcp:
            antigo, self.tcp = self.tcp, escritor
        if antigo is not None:
            log("novo cliente TCP: fechando o anterior")
            antigo.close()
            try:
                await antigo.wait_closed()
            except Exception:
                pass
        log("cliente TCP conectado")

        buf = bytearray()
        try:
            while True:
                dados = await leitor.read(4096)
                if not dados:
                    break
                buf.extend(dados)
                while len(buf) >= 4:
                    if buf[0] != MAGIC[0] or buf[1] != MAGIC[1]:
                        log("quadro inválido do app; fechando")
                        return
                    n = struct.unpack(">H", buf[2:4])[0]
                    if len(buf) < 4 + n:
                        break
                    payload = bytes(buf[4:4 + n])
                    del buf[:4 + n]
                    await self.para_radio(payload)
        except (ConnectionError, OSError) as e:
            log(f"TCP: {e}")
        finally:
            async with self.trava_tcp:
                if self.tcp is escritor:
                    self.tcp = None
            escritor.close()
            try:
                await escritor.wait_closed()
            except Exception:
                pass
            log("cliente TCP saiu")

    # ---------------------------------------------------------- BLE
    async def drenar(self):
        """Lê FromRadio até vir vazio (cada leitura é um pacote)."""
        cliente = self.ble
        if cliente is None or not cliente.is_connected:
            return
        parar = self.parar
        try:
            async with self.trava_ble:
                while True:
                    dados = await cliente.read_gatt_char(FROMRADIO)
                    if not dados:
                        break
                    await self.para_cliente(bytes(dados))
        except Exception as e:
            log(f"FromRadio: {e}")
            parar.set()

    def from_num(self, _caracteristica, _dados):
        tarefa = asyncio.create_task(self.drenar())
        self.tarefas.add(tarefa)
        tarefa.add_done_callback(self.tarefas.discard)

    async def sessao(self):
        parar = asyncio.Event()
        self.parar = parar

        def caiu(_cliente):
            log("BLE caiu")
            parar.set()

        # pareado e confiável, o BlueZ pode manter a ligação aberta depois que esta ponte
        # caiu; o nó então para de se anunciar e a busca do bleak não o acha
        try:
            info = subprocess.run(["bluetoothctl", "info", self.mac], capture_output=True,
                                  text=True, timeout=5).stdout
            if "Connected: yes" in info:
                log("ligação antiga ainda aberta no BlueZ: desconectando")
                subprocess.run(["bluetoothctl", "disconnect", self.mac], capture_output=True, timeout=10)
                await asyncio.sleep(2)
        except (OSError, subprocess.SubprocessError) as e:
            log(f"bluetoothctl: {e}")

        cliente = BleakClient(self.mac, disconnected_callback=caiu)
        try:
            try:
                await cliente.connect()
            except BleakError as e:
                if not pede_pareamento(e):
                    raise
                log("nó pediu pareamento; chamando pair()")
                try:
                    await cliente.pair()
                except Exception as e2:
                    log(f"pair: {e2}")
                await cliente.connect()

            self.ble = cliente
            self.mtu = await pedir_mtu(cliente)
            log(f"MTU {self.mtu}")

            # o nó pode exigir criptografia; pair() é inofensivo se já pareado
            try:
                await asyncio.wait_for(cliente.pair(), 30)
                log("pareamento ok")
            except Exception as e:
                log(f"pair: {e}")

            notifica = True
            try:
                await cliente.start_notify(FROMNUM, self.from_num)
            except Exception as e:
                notifica = False
                log(f"FromNum sem notify ({e}); lendo a cada 2 s")

            await self.drenar()  # drena uma vez depois de conectar
            log(f"conectado ao nó {self.mac}")
            pendentes, self.pendentes = self.pendentes, []
            for payload in pendentes:
                await self.para_radio(payload)

            # com ou sem aviso do FromNum, lê a cada 2 s: o aviso às vezes não chega
            # (CCCD sem efeito depois do pareamento) e aí nada passaria para o app
            if True:
                while not parar.is_set():
                    try:
                        await asyncio.wait_for(parar.wait(), 2)
                    except asyncio.TimeoutError:
                        await self.drenar()
        finally:
            self.ble = None
            try:
                if cliente.is_connected:
                    await cliente.disconnect()
            except (BleakError, OSError):
                pass

    async def ble_loop(self):
        espera = ESPERA_MIN
        while True:
            inicio = time.monotonic()
            try:
                await self.sessao()
            except BleakError as e:
                log(f"BLE: {e}")
            except Exception as e:
                log(f"BLE: {type(e).__name__}: {e}")
            if time.monotonic() - inicio >= 30:
                espera = ESPERA_MIN  # conexão durou: recomeça a espera curta
            log(f"reconectando em {espera} s")
            await asyncio.sleep(espera)
            espera = min(espera * 2, ESPERA_MAX)


async def main():
    mac, pin, linhas = ler_conf()
    if not mac and os.environ.get("REVIVE_MESH_PROCURAR") != "1":
        # o nó é escolhido na tela (Meshtastic > engrenagem > Procurar): pegar o primeiro
        # que aparecer poderia conectar no nó do vizinho
        log("sem ble= no mesh.conf: escolha um nó no app Meshtastic; tento de novo em 30 s")
        return 0
    if not mac:
        try:
            mac = await procurar()
        except Exception as e:
            log(f"procura: {e}")
            mac = None
        if not mac:
            log("sem ble= no mesh.conf e nenhum nó Meshtastic encontrado; tento de novo em 60 s")
            return 0
        gravar_ble(mac, linhas)
        log(f"nó encontrado: {mac} (gravado em {CONF})")
    if pin:
        log("há pin= no mesh.conf: o agente do revive-bt usa no pareamento")

    ponte = Ponte(mac, pin)
    try:
        servidor = await asyncio.start_server(ponte.tcp_cliente, HOST, PORTA)
    except OSError as e:
        log(f"TCP {HOST}:{PORTA}: {e}")
        return 1
    log(f"TCP em {HOST}:{PORTA} -> BLE {mac}")
    async with servidor:
        await ponte.ble_loop()


if __name__ == "__main__":
    try:
        sys.exit(asyncio.run(main()))
    except KeyboardInterrupt:
        sys.exit(0)
