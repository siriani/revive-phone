//! Telegram (MTProto em Rust puro, crate grammers): login, conversas, mensagens,
//! fotos e novidades em tempo real. Roda numa thread própria com tokio; a interface
//! manda comandos (`Cmd`) e recebe eventos (`Evento`).
//!
//! api_id/api_hash ficam em /etc/revive/telegram.conf; a sessão (login) em
//! /var/lib/revive/telegram.session. Os dois só o root lê. Fotos baixadas ficam em
//! /var/cache/revive/tg.

use std::sync::Arc;

use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::media::Media;
use grammers_client::message::Message;
use grammers_client::session::types::PeerRef;
use grammers_client::update::Update;
use grammers_client::{Client, SignInError};
use grammers_mtsender::SenderPool;
use grammers_session::storages::SqliteSession;
use tokio::sync::mpsc;

const CONF: &str = "/etc/revive/telegram.conf";
const SESSAO: &str = "/var/lib/revive/telegram.session";
const CACHE: &str = "/var/cache/revive/tg";

pub enum Cmd {
    Telefone(String),
    Codigo(String),
    Senha(String),
    Conversas,
    Abrir(usize),
    Enviar(String),
}

#[derive(Clone)]
pub struct Conversa {
    pub nome: String,
    pub previa: String,
    pub hora: String,
    pub nao_lidas: i32,
}

#[derive(Clone)]
pub struct Msg {
    pub texto: String,
    pub minha: bool,
    pub hora: String,
    /// tem foto ou vídeo (a imagem chega depois, num Evento::Foto)
    pub midia: bool,
    pub video: bool,
}

/// Imagem já decodificada e reduzida (pronta para a tela).
pub type Imagem = slint::SharedPixelBuffer<slint::Rgba8Pixel>;

pub enum Evento {
    /// "sem-conf", "telefone", "codigo", "senha", "pronto" ou "erro: ..."
    Estado(String),
    Conversas(Vec<Conversa>),
    /// conversa aberta: nome e últimas mensagens (mais antigas primeiro)
    Chat(String, Vec<Msg>),
    /// mensagem nova: (é da conversa aberta?, nome de quem mandou, mensagem, foto)
    Nova(bool, String, Msg, Option<Imagem>),
    /// foto/miniatura da mensagem `i` da conversa aberta
    Foto(usize, Imagem),
}

fn ler_conf() -> Option<(i32, String)> {
    let mut id = None;
    let mut hash = None;
    for linha in std::fs::read_to_string(CONF).ok()?.lines() {
        let Some((k, v)) = linha.split_once('=') else { continue };
        match k.trim() {
            "api_id" => id = v.trim().parse().ok(),
            "api_hash" => hash = Some(v.trim().to_string()),
            _ => {}
        }
    }
    Some((id?, hash?))
}

fn hora(m: &Message) -> String {
    let ts = m.date().as_second();
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

fn msg(m: &Message) -> Msg {
    let t = m.text().to_string();
    let (midia, video, rotulo): (bool, bool, String) = match m.media() {
        Some(Media::Photo(_)) => (true, false, String::new()),
        Some(Media::Document(d)) => match d.mime_type() {
            Some(t) if t.starts_with("video/") => (true, true, String::new()),
            Some(t) if t.starts_with("audio/") => (false, false, "♪ áudio".into()),
            _ => (false, false, format!("arquivo: {}", d.name().unwrap_or("?"))),
        },
        Some(Media::Sticker(_)) => (false, false, "figurinha".into()),
        Some(_) => (false, false, "[mídia]".into()),
        None => (false, false, String::new()),
    };
    let texto = if t.is_empty() { rotulo } else { t };
    Msg { texto, minha: m.outgoing(), hora: hora(m), midia, video }
}

/// Foto (versão média, ~800 px) ou miniatura do vídeo, com cache em disco, reduzida
/// para caber no balão.
async fn imagem(client: &Client, m: &Message) -> Option<Imagem> {
    let arq = format!("{CACHE}/{}_{}.jpg", m.peer_id().bot_api_dialog_id().unwrap_or(0), m.id());
    let bytes = match std::fs::read(&arq) {
        Ok(b) => b,
        Err(_) => {
            let thumbs = match m.media()? {
                Media::Photo(p) => p.thumbs(),
                Media::Document(d) => d.thumbs(),
                _ => return None,
            };
            // a maior versão de até ~200 KB (fotos: ~800 px; vídeos: a miniatura)
            let alvo = thumbs
                .iter()
                .filter(|t| t.size() > 0 && t.size() < 200_000)
                .max_by_key(|t| t.size())
                .or_else(|| thumbs.iter().max_by_key(|t| t.size()))?
                .clone();
            let mut it = client.iter_download(&alvo);
            let mut b = vec![];
            while let Ok(Some(parte)) = it.next().await {
                b.extend(parte);
            }
            if b.is_empty() {
                return None;
            }
            let _ = std::fs::create_dir_all(CACHE);
            let _ = std::fs::write(&arq, &b);
            b
        }
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let img = img.resize(560, 420, image::imageops::FilterType::Triangle).to_rgba8();
    let (w, h) = img.dimensions();
    Some(slint::SharedPixelBuffer::clone_from_slice(img.as_raw(), w, h))
}

/// Liga o Telegram numa thread nova. `avisar` recebe os eventos (de outra thread).
pub fn iniciar<F>(avisar: F) -> mpsc::UnboundedSender<Cmd>
where
    F: Fn(Evento) + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(rodar(rx, avisar));
    });
    tx
}

async fn rodar<F: Fn(Evento)>(mut rx: mpsc::UnboundedReceiver<Cmd>, avisar: F) {
    let Some((api_id, api_hash)) = ler_conf() else {
        avisar(Evento::Estado("sem-conf".into()));
        return;
    };
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let sessao = match SqliteSession::open(SESSAO).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            avisar(Evento::Estado(format!("erro: sessão: {e}")));
            return;
        }
    };
    let pool = SenderPool::new(Arc::clone(&sessao), api_id);
    let client = Client::new(pool.handle);
    tokio::spawn(pool.runner.run());
    let mut updates_rx = Some(pool.updates);

    let mut token: Option<LoginToken> = None;
    let mut senha_token: Option<PasswordToken> = None;
    let mut conversas: Vec<(PeerRef, String)> = vec![];
    let mut aberta: Option<usize> = None;
    let mut stream = None;

    let autorizado = client.is_authorized().await.unwrap_or(false);
    avisar(Evento::Estado(if autorizado { "pronto" } else { "telefone" }.into()));

    loop {
        // depois do login, as novidades chegam pelo stream
        if stream.is_none() && client.is_authorized().await.unwrap_or(false) {
            // a lista de conversas guarda os peers na sessão (o stream precisa deles)
            conversas = carregar_conversas(&client, &avisar).await;
            if let Some(u) = updates_rx.take() {
                stream = client.stream_updates(u, Default::default()).await.ok();
            }
        }
        let cmd = match stream.as_mut() {
            Some(s) => tokio::select! {
                c = rx.recv() => c,
                u = s.next() => {
                    if let Ok(Update::NewMessage(m)) = u {
                        // canais não têm remetente pessoal: usa o nome do canal/grupo
                        let nome = m
                            .sender()
                            .and_then(|p| p.name())
                            .or_else(|| m.peer().and_then(|p| p.name()))
                            .unwrap_or("Telegram")
                            .to_string();
                        let da_aberta = aberta
                            .and_then(|i| conversas.get(i))
                            .is_some_and(|(p, _)| m.peer().is_some_and(|mp| mp.id() == p.id));
                        let dados = msg(&m);
                        let foto = if da_aberta && dados.midia { imagem(&client, &m).await } else { None };
                        avisar(Evento::Nova(da_aberta, nome, dados, foto));
                    }
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let Some(cmd) = cmd else { return };
        match cmd {
            Cmd::Telefone(t) => match client.request_login_code(&t, &api_hash).await {
                Ok(tk) => {
                    token = Some(tk);
                    avisar(Evento::Estado("codigo".into()));
                }
                Err(e) => avisar(Evento::Estado(format!("erro: {e}"))),
            },
            Cmd::Codigo(c) => {
                let Some(tk) = token.as_ref() else { continue };
                match client.sign_in(tk, &c).await {
                    Ok(_) => avisar(Evento::Estado("pronto".into())),
                    Err(SignInError::PasswordRequired(pt)) => {
                        senha_token = Some(pt);
                        avisar(Evento::Estado("senha".into()));
                    }
                    Err(SignInError::InvalidCode) => avisar(Evento::Estado("erro: código errado".into())),
                    Err(e) => avisar(Evento::Estado(format!("erro: {e}"))),
                }
            }
            Cmd::Senha(s) => {
                let Some(pt) = senha_token.take() else { continue };
                match client.check_password(pt, s.as_bytes()).await {
                    Ok(_) => avisar(Evento::Estado("pronto".into())),
                    Err(e) => avisar(Evento::Estado(format!("erro: {e}"))),
                }
            }
            Cmd::Conversas => {
                aberta = None;
                conversas = carregar_conversas(&client, &avisar).await;
            }
            Cmd::Abrir(i) => {
                let Some((peer, nome)) = conversas.get(i).cloned() else { continue };
                aberta = Some(i);
                let mut it = client.iter_messages(peer).limit(40);
                let mut mensagens = vec![];
                while let Ok(Some(m)) = it.next().await {
                    mensagens.push(m);
                }
                mensagens.reverse();
                avisar(Evento::Chat(nome, mensagens.iter().map(msg).collect()));
                // fotos uma a uma, da mais nova para a mais antiga; para se chegar outro comando
                for (i, m) in mensagens.iter().enumerate().rev() {
                    if !rx.is_empty() {
                        break;
                    }
                    if msg(m).midia {
                        if let Some(img) = imagem(&client, m).await {
                            avisar(Evento::Foto(i, img));
                        }
                    }
                }
            }
            Cmd::Enviar(texto) => {
                let Some((peer, _)) = aberta.and_then(|i| conversas.get(i)).map(|x| x.clone()) else { continue };
                match client.send_message(peer, texto.as_str()).await {
                    Ok(m) => avisar(Evento::Nova(true, "eu".into(), msg(&m), None)),
                    Err(e) => avisar(Evento::Estado(format!("erro: {e}"))),
                }
            }
        }
    }
}

async fn carregar_conversas<F: Fn(Evento)>(client: &Client, avisar: &F) -> Vec<(PeerRef, String)> {
    let mut it = client.iter_dialogs().limit(40);
    let mut refs = vec![];
    let mut lista = vec![];
    while let Ok(Some(d)) = it.next().await {
        let nome = d.peer().name().unwrap_or("?").to_string();
        let previa = d.last_message.as_ref().map(|m| msg(m).texto).unwrap_or_default();
        let hora = d.last_message.as_ref().map(hora).unwrap_or_default();
        let nao_lidas = match &d.raw {
            grammers_client::tl::enums::Dialog::Dialog(x) => x.unread_count,
            _ => 0,
        };
        refs.push((d.peer_ref(), nome.clone()));
        lista.push(Conversa { nome, previa, hora, nao_lidas });
    }
    avisar(Evento::Conversas(lista));
    refs
}
