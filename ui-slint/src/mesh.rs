//! Meshtastic por TCP (porta 4403, "Stream API"): conecta no rádio, recebe nós e
//! mensagens de texto e manda mensagens. Roda numa thread própria; a interface manda
//! comandos (`Cmd`) e recebe eventos (`Evento`).
//!
//! O endereço fica em /etc/revive/mesh.conf (`host=IP`, `porta=4403` opcional) e as
//! últimas 200 mensagens em /var/lib/revive/mesh-msgs.json.
//!
//! O protocolo é simples: cada pacote é 0x94 0xC3 + tamanho u16 big-endian + protobuf.
//! As mensagens protobuf abaixo foram copiadas à mão do repositório oficial
//! (meshtastic/protobufs) só com os campos que usamos; sem build.rs/protoc.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use prost::Message as _;

const CONF: &str = "/etc/revive/mesh.conf";
const HISTORICO: &str = "/var/lib/revive/mesh-msgs.json";
const MAGIC: [u8; 2] = [0x94, 0xC3];
const MAX_HISTORICO: usize = 200;
/// 0xFFFFFFFF: mensagem para todos
const BROADCAST: u32 = 0xFFFF_FFFF;

pub enum Cmd {
    Conectar,
    Enviar { texto: String, para: Option<u32>, canal: u32 },
}

pub enum Evento {
    /// "conectando", "conectado" ou "erro: ..."
    Estado(String),
    Nos(Vec<No>),
    Mensagem(Msg),
    Eu(u32),
}

#[derive(Clone, Debug)]
pub struct No {
    pub num: u32,
    pub nome: String,
    pub curto: String,
    pub bateria: Option<i32>,
    pub snr: Option<f32>,
    /// last_heard (epoch)
    pub visto: Option<u32>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Msg {
    pub de: u32,
    pub de_nome: String,
    pub para: u32,
    pub canal: u32,
    pub texto: String,
    pub hora: u32,
    pub minha: bool,
}

/// Lê o endereço do rádio. Sem `host` a configuração não vale.
fn ler_conf() -> Option<(String, u16)> {
    let mut host = None;
    let mut porta = 4403;
    for linha in std::fs::read_to_string(CONF).ok()?.lines() {
        let Some((k, v)) = linha.split_once('=') else { continue };
        match k.trim() {
            "host" => {
                let v = v.trim();
                if !v.is_empty() {
                    host = Some(v.to_string());
                }
            }
            "porta" | "port" => {
                if let Ok(p) = v.trim().parse() {
                    porta = p;
                }
            }
            _ => {}
        }
    }
    Some((host?, porta))
}

pub fn configurado() -> bool {
    ler_conf().is_some()
}

pub fn iniciar(evento: impl Fn(Evento) + Send + 'static) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || rodar(rx, evento));
    tx
}

/// `historico` lê do disco, então pode ser chamado de qualquer thread.
pub fn historico() -> Vec<Msg> {
    carregar_historico()
}

fn carregar_historico() -> Vec<Msg> {
    let mut msgs = std::fs::read_to_string(HISTORICO)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Msg>>(&s).ok())
        .unwrap_or_default();
    if msgs.len() > MAX_HISTORICO {
        let corte = msgs.len() - MAX_HISTORICO;
        msgs.drain(..corte);
    }
    msgs
}

fn salvar_historico(msgs: &[Msg]) {
    if let Some(dir) = std::path::Path::new(HISTORICO).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(msgs) {
        let _ = std::fs::write(HISTORICO, s);
    }
}

fn agora() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

/// Ids de pacote/config: só precisa ser diferente a cada vez.
fn aleatorio() -> u32 {
    static SEMENTE: AtomicU32 = AtomicU32::new(0x9E37_79B9);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut x = SEMENTE.fetch_add(0x9E37_79B9 ^ t, Ordering::Relaxed) ^ t ^ std::process::id();
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    if x == 0 { 1 } else { x }
}

struct Estado {
    nos: BTreeMap<u32, No>,
    eu: Option<u32>,
    hist: Vec<Msg>,
}

enum Fim {
    /// pediram para reconectar (Cmd::Conectar)
    Reconectar,
    /// caiu ou deu erro; a mensagem vai para Evento::Estado
    Falha(String),
    /// a interface fechou o canal
    Sair,
}

fn rodar(rx: Receiver<Cmd>, evento: impl Fn(Evento)) {
    let mut est = Estado {
        nos: BTreeMap::new(),
        eu: None,
        hist: carregar_historico(),
    };
    let mut espera = 5u64;

    loop {
        let Some((host, porta)) = ler_conf() else {
            evento(Evento::Estado("erro: mesh não configurado".into()));
            match esperar_comando(&rx, Duration::from_secs(5), &evento) {
                Err(()) => return,
                Ok(_) => continue,
            }
        };

        evento(Evento::Estado("conectando".into()));
        let stream = match conectar(&host, porta) {
            Ok(s) => s,
            Err(e) => {
                evento(Evento::Estado(format!("erro: {e}")));
                match esperar_comando(&rx, Duration::from_secs(espera), &evento) {
                    Err(()) => return,
                    Ok(true) => {
                        espera = 5;
                        continue;
                    }
                    Ok(false) => {}
                }
                espera = (espera * 2).min(60);
                continue;
            }
        };

        match sessao(stream, &rx, &evento, &mut est, &mut espera) {
            Fim::Sair => return,
            Fim::Reconectar => espera = 5,
            Fim::Falha(e) => {
                evento(Evento::Estado(format!("erro: {e}")));
                match esperar_comando(&rx, Duration::from_secs(espera), &evento) {
                    Err(()) => return,
                    Ok(true) => {
                        espera = 5;
                        continue;
                    }
                    Ok(false) => {}
                }
                // espera crescente: 5 s → 60 s
                espera = (espera * 2).min(60);
            }
        }
    }
}

fn conectar(host: &str, porta: u16) -> io::Result<TcpStream> {
    let mut ultimo = None;
    for endereco in (host, porta).to_socket_addrs()? {
        match TcpStream::connect_timeout(&endereco, Duration::from_secs(10)) {
            Ok(s) => return Ok(s),
            Err(e) => ultimo = Some(e),
        }
    }
    Err(ultimo.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "endereço não encontrado")
    }))
}

/// Espera comandos sem conexão. Ok(true) = pediram para conectar já.
fn esperar_comando(
    rx: &Receiver<Cmd>,
    duracao: Duration,
    evento: &impl Fn(Evento),
) -> Result<bool, ()> {
    let fim = Instant::now() + duracao;
    loop {
        let restante = fim.saturating_duration_since(Instant::now());
        if restante.is_zero() {
            return Ok(false);
        }
        match rx.recv_timeout(restante) {
            Ok(Cmd::Conectar) => return Ok(true),
            Ok(Cmd::Enviar { .. }) => {
                evento(Evento::Estado("erro: sem conexão".into()));
            }
            Err(RecvTimeoutError::Timeout) => return Ok(false),
            Err(RecvTimeoutError::Disconnected) => return Err(()),
        }
    }
}

fn sessao(
    mut s: TcpStream,
    rx: &Receiver<Cmd>,
    evento: &impl Fn(Evento),
    est: &mut Estado,
    espera: &mut u64,
) -> Fim {
    let _ = s.set_nodelay(true);
    // timeout curto: entre um pacote e outro também atendemos a interface
    let _ = s.set_read_timeout(Some(Duration::from_millis(250)));

    let cfg = ToRadio {
        want_config_id: aleatorio(),
        ..Default::default()
    };
    if let Err(e) = mandar(&mut s, &cfg) {
        return Fim::Falha(format!("conexão: {e}"));
    }

    let mut buf = Vec::new();
    let mut ultimo_heartbeat = Instant::now();

    loop {
        if ultimo_heartbeat.elapsed() >= Duration::from_secs(300) {
            let hb = ToRadio {
                heartbeat: Some(Heartbeat { nonce: aleatorio() }),
                ..Default::default()
            };
            if let Err(e) = mandar(&mut s, &hb) {
                return Fim::Falha(format!("conexão: {e}"));
            }
            ultimo_heartbeat = Instant::now();
        }

        let mut tmp = [0u8; 4096];
        match s.read(&mut tmp) {
            Ok(0) => return Fim::Falha("conexão encerrada".into()),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => return Fim::Falha(format!("conexão: {e}")),
        }
        if buf.len() > 1_000_000 {
            return Fim::Falha("pacote grande demais".into());
        }

        // processa todos os pacotes completos que chegaram
        loop {
            let bytes = match desenquadrar(&mut buf) {
                Ok(Some(b)) => b,
                Ok(None) => break,
                Err(e) => return Fim::Falha(e),
            };
            match FromRadio::decode(bytes.as_slice()) {
                Ok(from) => {
                    if tratar_from(from, est, evento) {
                        *espera = 5;
                    }
                }
                // um pacote que não decodifica (campo novo do firmware, por exemplo) não
                // derruba a conexão: é ignorado e registrado
                Err(e) => eprintln!("mesh: FromRadio ignorado ({} bytes): {e}", bytes.len()),
            }
        }

        // comandos da interface
        loop {
            match rx.try_recv() {
                Ok(Cmd::Conectar) => return Fim::Reconectar,
                Ok(Cmd::Enviar { texto, para, canal }) => {
                    if let Err(e) = enviar_texto(&mut s, &texto, para, canal, est, evento) {
                        return Fim::Falha(format!("envio: {e}"));
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Fim::Sair,
            }
        }
    }
}

/// Devolve true quando a configuração terminou (rádio pronto).
fn tratar_from(from: FromRadio, est: &mut Estado, evento: &impl Fn(Evento)) -> bool {
    let FromRadio {
        my_info,
        node_info,
        config_complete_id,
        packet,
        ..
    } = from;

    if let Some(mi) = my_info {
        let num = mi.my_node_num;
        if num != 0 {
            if est.eu != Some(num) {
                est.eu = Some(num);
                evento(Evento::Eu(num));
            }
            est.nos.entry(num).or_insert_with(|| novo_no(num));
            evento(Evento::Nos(est.nos.values().cloned().collect()));
        }
    }

    if let Some(ni) = node_info {
        aplicar_node_info(&ni, est);
        evento(Evento::Nos(est.nos.values().cloned().collect()));
    }

    if config_complete_id.is_some() {
        evento(Evento::Estado("conectado".into()));
        evento(Evento::Nos(est.nos.values().cloned().collect()));
    }

    if let Some(p) = packet {
        tratar_pacote(p, est, evento);
    }

    config_complete_id.is_some()
}

fn novo_no(num: u32) -> No {
    No {
        num,
        nome: String::new(),
        curto: String::new(),
        bateria: None,
        snr: None,
        visto: None,
        lat: None,
        lon: None,
    }
}

fn bateria(nivel: u32) -> Option<i32> {
    // 101 (ou mais) quer dizer "ligado na energia", não uma porcentagem
    (nivel <= 100).then_some(nivel as i32)
}

fn aplicar_posicao(no: &mut No, p: &Position) {
    if p.latitude_i != 0 && p.longitude_i != 0 {
        no.lat = Some(p.latitude_i as f64 / 10_000_000.0);
        no.lon = Some(p.longitude_i as f64 / 10_000_000.0);
    }
    if p.time != 0 {
        no.visto = Some(p.time);
    }
}

fn aplicar_node_info(ni: &NodeInfo, est: &mut Estado) {
    if ni.num == 0 {
        return;
    }
    let no = est.nos.entry(ni.num).or_insert_with(|| novo_no(ni.num));
    if let Some(u) = ni.user.as_ref() {
        if !u.long_name.is_empty() {
            no.nome = u.long_name.clone();
        }
        if !u.short_name.is_empty() {
            no.curto = u.short_name.clone();
        }
    }
    if let Some(p) = ni.position.as_ref() {
        aplicar_posicao(no, p);
    }
    if ni.snr != 0.0 {
        no.snr = Some(ni.snr);
    }
    if ni.last_heard != 0 {
        no.visto = Some(ni.last_heard);
    }
    if let Some(dm) = ni.device_metrics.as_ref() {
        if let Some(b) = bateria(dm.battery_level) {
            no.bateria = Some(b);
        }
    }
}

fn tratar_pacote(p: MeshPacket, est: &mut Estado, evento: &impl Fn(Evento)) {
    let de = p.from;
    if de == 0 {
        return;
    }
    let hora = if p.rx_time != 0 { p.rx_time } else { agora() };

    {
        let no = est.nos.entry(de).or_insert_with(|| novo_no(de));
        if p.rx_snr != 0.0 {
            no.snr = Some(p.rx_snr);
        }
        no.visto = Some(hora);
        if let Some(d) = p.decoded.as_ref() {
            match d.portnum {
                3 => {
                    // POSITION_APP
                    if let Ok(pos) = Position::decode(d.payload.as_slice()) {
                        aplicar_posicao(no, &pos);
                    }
                }
                4 => {
                    // NODEINFO_APP: o User também chega por aqui
                    if let Ok(u) = User::decode(d.payload.as_slice()) {
                        if !u.long_name.is_empty() {
                            no.nome = u.long_name.clone();
                        }
                        if !u.short_name.is_empty() {
                            no.curto = u.short_name.clone();
                        }
                    }
                }
                67 => {
                    // TELEMETRY_APP
                    if let Ok(t) = Telemetry::decode(d.payload.as_slice()) {
                        if let Some(dm) = t.device_metrics.as_ref() {
                            if let Some(b) = bateria(dm.battery_level) {
                                no.bateria = Some(b);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(d) = p.decoded.as_ref() {
        if d.portnum == 1 {
            // TEXT_MESSAGE_APP
            let texto = String::from_utf8_lossy(&d.payload).to_string();
            if !texto.is_empty() {
                let de_nome = est
                    .nos
                    .get(&de)
                    .map(|n| n.nome.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| {
                        if est.eu == Some(de) {
                            "eu".into()
                        } else {
                            format!("!{de:08x}")
                        }
                    });
                let msg = Msg {
                    de,
                    de_nome,
                    para: p.to,
                    canal: p.channel,
                    texto,
                    hora,
                    minha: est.eu == Some(de),
                };
                guardar_msg(est, msg, evento);
            }
        }
    }

    evento(Evento::Nos(est.nos.values().cloned().collect()));
}

fn guardar_msg(est: &mut Estado, msg: Msg, evento: &impl Fn(Evento)) {
    est.hist.push(msg.clone());
    if est.hist.len() > MAX_HISTORICO {
        est.hist.remove(0);
    }
    salvar_historico(&est.hist);
    evento(Evento::Mensagem(msg));
}

fn enviar_texto(
    s: &mut TcpStream,
    texto: &str,
    para: Option<u32>,
    canal: u32,
    est: &mut Estado,
    evento: &impl Fn(Evento),
) -> io::Result<()> {
    let para = para.unwrap_or(BROADCAST);
    let de = est.eu.unwrap_or(0);
    let pacote = MeshPacket {
        from: de,
        to: para,
        channel: canal,
        decoded: Some(Data {
            portnum: 1,
            payload: texto.as_bytes().to_vec(),
            ..Default::default()
        }),
        id: aleatorio(),
        want_ack: true,
        hop_limit: 3,
        ..Default::default()
    };
    mandar(
        s,
        &ToRadio {
            packet: Some(pacote),
            ..Default::default()
        },
    )?;

    // mostra a mensagem na hora (o rádio não devolve o que a gente mandou)
    let de_nome = est
        .nos
        .get(&de)
        .map(|n| n.nome.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "eu".into());
    let msg = Msg {
        de,
        de_nome,
        para,
        canal,
        texto: texto.to_string(),
        hora: agora(),
        minha: true,
    };
    guardar_msg(est, msg, evento);
    Ok(())
}

fn enquadrar(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Tira um pacote inteiro do começo do buffer (None = ainda faltam bytes).
fn desenquadrar(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    if buf.len() < 4 {
        return Ok(None);
    }
    if buf[0] != MAGIC[0] || buf[1] != MAGIC[1] {
        return Err("cabeçalho inválido".into());
    }
    let n = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    if buf.len() < 4 + n {
        return Ok(None);
    }
    let payload = buf[4..4 + n].to_vec();
    buf.drain(..4 + n);
    Ok(Some(payload))
}

fn mandar(s: &mut TcpStream, m: &ToRadio) -> io::Result<()> {
    let bytes = m.encode_to_vec();
    if bytes.len() > u16::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pacote maior que 65535 bytes",
        ));
    }
    s.write_all(&enquadrar(&bytes))?;
    s.flush()
}

// ---------------------------------------------------------------------------
// Protobuf mínimo (meshtastic/mesh.proto, portnums.proto e telemetry.proto)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, prost::Message)]
struct ToRadio {
    #[prost(message, optional, tag = "1")]
    packet: Option<MeshPacket>,
    #[prost(uint32, tag = "3")]
    want_config_id: u32,
    #[prost(message, optional, tag = "7")]
    heartbeat: Option<Heartbeat>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct FromRadio {
    #[prost(message, optional, tag = "2")]
    packet: Option<MeshPacket>,
    #[prost(message, optional, tag = "3")]
    my_info: Option<MyNodeInfo>,
    #[prost(message, optional, tag = "4")]
    node_info: Option<NodeInfo>,
    /// faz parte do oneof: presente mesmo quando vale 0
    #[prost(uint32, optional, tag = "7")]
    config_complete_id: Option<u32>,
}

// Só os campos usados: o resto é ignorado pelo prost ao decodificar.
#[derive(Clone, PartialEq, prost::Message)]
struct MeshPacket {
    // números e tipos como no mesh.proto oficial (meshtastic/protobufs): nós são fixed32
    #[prost(fixed32, tag = "1")]
    from: u32,
    #[prost(fixed32, tag = "2")]
    to: u32,
    #[prost(uint32, tag = "3")]
    channel: u32,
    #[prost(message, optional, tag = "4")]
    decoded: Option<Data>,
    #[prost(fixed32, tag = "6")]
    id: u32,
    #[prost(fixed32, tag = "7")]
    rx_time: u32,
    #[prost(float, tag = "8")]
    rx_snr: f32,
    #[prost(uint32, tag = "9")]
    hop_limit: u32,
    #[prost(bool, tag = "10")]
    want_ack: bool,
    #[prost(int32, tag = "12")]
    rx_rssi: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Data {
    /// PortNum: 1 = TEXT_MESSAGE_APP, 3 = POSITION_APP, 4 = NODEINFO_APP,
    /// 67 = TELEMETRY_APP
    #[prost(int32, tag = "1")]
    portnum: i32,
    #[prost(bytes = "vec", tag = "2")]
    payload: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct User {
    #[prost(string, tag = "2")]
    long_name: String,
    #[prost(string, tag = "3")]
    short_name: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct NodeInfo {
    #[prost(uint32, tag = "1")]
    num: u32,
    #[prost(message, optional, tag = "2")]
    user: Option<User>,
    #[prost(message, optional, tag = "3")]
    position: Option<Position>,
    #[prost(float, tag = "4")]
    snr: f32,
    #[prost(fixed32, tag = "5")]
    last_heard: u32,
    #[prost(message, optional, tag = "6")]
    device_metrics: Option<DeviceMetrics>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct MyNodeInfo {
    #[prost(uint32, tag = "1")]
    my_node_num: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Position {
    #[prost(sfixed32, tag = "1")]
    latitude_i: i32,
    #[prost(sfixed32, tag = "2")]
    longitude_i: i32,
    #[prost(int32, tag = "3")]
    altitude: i32,
    // fixed32 no mesh.proto (com uint32 o prost recusa o pacote inteiro)
    #[prost(fixed32, tag = "4")]
    time: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct DeviceMetrics {
    #[prost(uint32, tag = "1")]
    battery_level: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Telemetry {
    // fixed32 no telemetry.proto
    #[prost(fixed32, tag = "1")]
    time: u32,
    #[prost(message, optional, tag = "2")]
    device_metrics: Option<DeviceMetrics>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Heartbeat {
    #[prost(uint32, tag = "1")]
    nonce: u32,
}

#[cfg(test)]
mod testes {
    use super::*;
    use prost::Message as _;

    #[test]
    fn enquadramento_ida_e_volta() {
        let dados = b"abc";
        let f = enquadrar(dados);
        assert_eq!(f[0], 0x94);
        assert_eq!(f[1], 0xC3);
        assert_eq!(u16::from_be_bytes([f[2], f[3]]), dados.len() as u16);

        let mut buf = f;
        assert_eq!(desenquadrar(&mut buf).unwrap(), Some(dados.to_vec()));
        assert!(buf.is_empty());
        assert_eq!(desenquadrar(&mut buf).unwrap(), None);
    }

    #[test]
    fn enquadramento_parcial_espera() {
        let mut buf = vec![0x94, 0xC3, 0x00];
        assert_eq!(desenquadrar(&mut buf).unwrap(), None);
        // ainda faltam o último byte do tamanho
        assert!(buf.len() == 3);
    }

    #[test]
    fn cabecalho_invalido() {
        let mut buf = vec![0x00, 0x00, 0x00, 0x01];
        assert!(desenquadrar(&mut buf).is_err());
    }

    #[test]
    fn toradio_texto_ida_e_volta() {
        let texto = "olá, mundo";
        let pacote = MeshPacket {
            to: BROADCAST,
            channel: 0,
            decoded: Some(Data {
                portnum: 1,
                payload: texto.as_bytes().to_vec(),
                ..Default::default()
            }),
            id: 42,
            want_ack: true,
            ..Default::default()
        };
        let bytes = ToRadio {
            packet: Some(pacote),
            ..Default::default()
        }
        .encode_to_vec();

        // ToRadio.packet = campo 1 (tamanho); dentro dele "to" (campo 2) é fixed32: 0x15 + 4 bytes
        assert_eq!(bytes[0], 0x0a);
        assert!(bytes.windows(5).any(|w| w == [0x15, 0xff, 0xff, 0xff, 0xff]), "to deve ser fixed32");
        let volta = ToRadio::decode(bytes.as_slice()).unwrap();
        let p = volta.packet.unwrap();
        assert_eq!(p.to, BROADCAST);
        assert_eq!(p.id, 42);
        assert!(p.want_ack);
        let d = p.decoded.unwrap();
        assert_eq!(d.portnum, 1);
        assert_eq!(String::from_utf8(d.payload).unwrap(), texto);
    }
}
