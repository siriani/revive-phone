//! Telegram (MTProto em Rust puro, crate grammers): login, conversas, mensagens e
//! novidades em tempo real. Roda numa thread própria com tokio; a interface manda
//! comandos (`Cmd`) e recebe eventos (`Evento`).
//!
//! api_id/api_hash ficam em /etc/revive/telegram.conf; a sessão (login) em
//! /var/lib/revive/telegram.session. Os dois só o root lê.

use std::sync::Arc;

use grammers_client::{Client, SignInError};
use grammers_client::client::LoginToken;
use grammers_client::client::PasswordToken;
use grammers_client::update::Update;
use grammers_client::session::types::PeerRef;
use grammers_mtsender::SenderPool;
use grammers_session::storages::SqliteSession;
use tokio::sync::mpsc;

const CONF: &str = "/etc/revive/telegram.conf";
const SESSAO: &str = "/var/lib/revive/telegram.session";

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
    pub nao_lidas: i32,
}

#[derive(Clone)]
pub struct Msg {
    pub texto: String,
    pub minha: bool,
    pub hora: String,
}

pub enum Evento {
    /// "sem-conf", "telefone", "codigo", "senha", "pronto" ou "erro: ..."
    Estado(String),
    Conversas(Vec<Conversa>),
    /// conversa aberta: nome e últimas mensagens (mais antigas primeiro)
    Chat(String, Vec<Msg>),
    /// mensagem nova: (é da conversa aberta?, nome de quem mandou/conversa, mensagem)
    Nova(bool, String, Msg),
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

fn hora(m: &grammers_client::message::Message) -> String {
    let ts = m.date().as_second();
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

fn msg(m: &grammers_client::message::Message) -> Msg {
    let t = m.text();
    Msg {
        texto: if t.is_empty() { "[mídia]".into() } else { t.to_string() },
        minha: m.outgoing(),
        hora: hora(m),
    }
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
                        let nome = m.sender().and_then(|p| p.name()).unwrap_or("?").to_string();
                        let da_aberta = aberta
                            .and_then(|i| conversas.get(i))
                            .is_some_and(|(p, _)| m.peer().is_some_and(|mp| mp.id() == p.id));
                        avisar(Evento::Nova(da_aberta, nome, msg(&m)));
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
                let mut v = vec![];
                while let Ok(Some(m)) = it.next().await {
                    v.push(msg(&m));
                }
                v.reverse();
                avisar(Evento::Chat(nome, v));
            }
            Cmd::Enviar(texto) => {
                let Some((peer, _)) = aberta.and_then(|i| conversas.get(i)).map(|x| x.clone()) else { continue };
                match client.send_message(peer, texto.as_str()).await {
                    Ok(m) => avisar(Evento::Nova(true, "eu".into(), msg(&m))),
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
        let nao_lidas = match &d.raw {
            grammers_client::tl::enums::Dialog::Dialog(x) => x.unread_count,
            _ => 0,
        };
        refs.push((d.peer_ref(), nome.clone()));
        lista.push(Conversa { nome, previa, nao_lidas });
    }
    avisar(Evento::Conversas(lista));
    refs
}
