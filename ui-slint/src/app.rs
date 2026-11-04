//! Estado da interface e ligação das telas (Slint) com os dados.
//!
//! O estado mora numa thread-local da thread da interface; buscas de rede rodam em
//! outras threads e voltam por `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde_json::Value;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::dados;
use crate::manifesto;
use crate::musica::{self, Subsonic};
use crate::{Album, AppInfo, AppWindow, Aparelho, Bolha, Faixa, Item, Moeda, Rede, Tecla, Tema, TgConversa, Vela};
use crate::telegram;
use std::rc::Rc;
use crate::cripto;

const POR_PAGINA: usize = 8;

/// Apps nativos do lançador: (id da tela, nome, ícone, cor de fundo).
/// Para criar um app: uma tela em ui/app.slint (`if root.tela == "<id>"`) + uma linha aqui.
/// O ícone é um caractere da fonte embutida (listado em `caracteres-embutidos`).
const APPS: &[(&str, &str, &str, (u8, u8, u8))] = &[
    ("painel", "Painel", "◷", (70, 55, 30)),
    ("cripto", "Cripto", "★", (58, 45, 90)),
    ("telegram", "Telegram", "✉", (32, 90, 140)),
    ("musica", "Música", "♫", (40, 60, 90)),
    ("wifi", "Wi-Fi", "≋", (30, 70, 55)),
    ("bt", "Bluetooth", "✱", (45, 58, 90)),
];

#[derive(Clone)]
enum Acao {
    Tocar(Arc<Vec<Value>>, usize),
    Aleatorio(Arc<Vec<Value>>),
    Album(Value),
    Artista(Value),
    Playlist(Value),
    /// item só informativo (apps por manifesto)
    Nada,
    /// abre a conversa i do Telegram
    TgAbrir(usize),
}

#[derive(Clone)]
struct ItemDados {
    titulo: String,
    sub: String,
    verde: bool,
    acao: Acao,
}

#[derive(Clone, Default)]
struct Lista {
    titulo: String,
    itens: Vec<ItemDados>,
    pagina: usize,
    vazio: String,
}

enum Modo {
    Senha(String),
    Busca,
    /// campo da configuração do Navidrome: "url", "usuário" ou "senha"
    Campo(String),
    /// Telegram: "telefone", "codigo", "senha" ou "mensagem"
    Tg(String),
}

struct Estado {
    lista: Lista,
    pilha: Vec<Lista>,
    origem: String,
    geracao: u64,
    artistas: Option<Vec<Value>>,
    texto: String,
    shift: bool,
    sym: bool,
    ocultar: bool,
    modo: Modo,
    ultimo_id: String,
    scrobbled: String,
    capa: String,
    bt_ultimo: String,
    conf_url: String,
    conf_user: String,
    conf_pass: String,
}

thread_local! {
    /// fileiras da tela de música: nome -> (modelo na tela, álbuns do Navidrome)
    static FILEIRAS: RefCell<std::collections::HashMap<String, (Rc<VecModel<Album>>, Vec<Value>)>> = RefCell::default();
    /// faixas do álbum aberto
    static FAIXAS_ALBUM: RefCell<Vec<Value>> = RefCell::default();
    /// ícones das moedas já baixados (url -> imagem)
    static ICONES: RefCell<std::collections::HashMap<String, Image>> = RefCell::default();
    static EST: RefCell<Estado> = RefCell::new(Estado {
        lista: Lista::default(),
        pilha: vec![],
        origem: "musica".into(),
        geracao: 0,
        artistas: None,
        texto: String::new(),
        shift: false,
        sym: false,
        ocultar: true,
        modo: Modo::Busca,
        ultimo_id: String::new(),
        scrobbled: String::new(),
        capa: String::new(),
        bt_ultimo: String::new(),
        conf_url: String::new(),
        conf_user: String::new(),
        conf_pass: String::new(),
    });
}

/// Cliente do Navidrome; trocado quando a tela de configuração salva dados novos.
fn api() -> Arc<Subsonic> {
    API.get_or_init(|| RwLock::new(Arc::new(Subsonic::novo()))).read().unwrap().clone()
}
static API: OnceLock<RwLock<Arc<Subsonic>>> = OnceLock::new();

fn trocar_api(nova: Subsonic) {
    let _ = api();
    *API.get().unwrap().write().unwrap() = Arc::new(nova);
}

fn ss(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

fn curto(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

/// Embaralha sem depender de crate de números aleatórios (xorshift com o relógio).
fn embaralhar(v: &mut [Value]) {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

// --------------------------------------------------------------------- listas

fn mostrar_lista(ui: &AppWindow) {
    EST.with_borrow(|e| {
        let l = &e.lista;
        let paginas = l.itens.len().div_ceil(POR_PAGINA).max(1);
        let pag = l.pagina.min(paginas - 1);
        let itens: Vec<Item> = l
            .itens
            .iter()
            .skip(pag * POR_PAGINA)
            .take(POR_PAGINA)
            .map(|i| Item { titulo: ss(curto(&i.titulo, 28)), sub: ss(curto(&i.sub, 38)), verde: i.verde })
            .collect();
        ui.set_lista_titulo(ss(curto(&l.titulo, 40)));
        ui.set_itens(ModelRc::new(VecModel::from(itens)));
        ui.set_lista_vazio(ss(l.vazio.clone()));
        ui.set_pagina(pag as i32);
        ui.set_paginas(paginas as i32);
    });
}

/// Abre uma lista carregada do Navidrome em segundo plano.
fn abrir_lista<F>(ui: &AppWindow, titulo: &str, carregar: F)
where
    F: FnOnce(&Subsonic) -> Result<Vec<ItemDados>, String> + Send + 'static,
{
    let geracao = EST.with_borrow_mut(|e| {
        if ui.get_tela() == "lista" {
            let atual = std::mem::take(&mut e.lista);
            e.pilha.push(atual);
        }
        e.lista = Lista { titulo: titulo.into(), vazio: "carregando...".into(), ..Default::default() };
        e.geracao += 1;
        e.geracao
    });
    ui.set_tela(ss("lista"));
    mostrar_lista(ui);
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = carregar(&api());
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            let atual = EST.with_borrow_mut(|e| {
                if e.geracao != geracao {
                    return false; // outra lista já foi pedida depois desta
                }
                match r {
                    Ok(itens) => {
                        e.lista.vazio = "nada aqui".into();
                        e.lista.itens = itens;
                    }
                    Err(erro) => e.lista.vazio = format!("não carregou: {}", curto(&erro, 40)),
                }
                true
            });
            if atual {
                mostrar_lista(&ui);
            }
        });
    });
}

fn itens_faixas(faixas: Vec<Value>, com_album: bool) -> Vec<ItemDados> {
    let f = Arc::new(faixas);
    let mut v = vec![
        ItemDados { titulo: "▶ Tocar tudo".into(), sub: String::new(), verde: true, acao: Acao::Tocar(f.clone(), 0) },
        ItemDados { titulo: "Aleatório".into(), sub: String::new(), verde: false, acao: Acao::Aleatorio(f.clone()) },
    ];
    for (i, x) in f.iter().enumerate() {
        let titulo = x["title"].as_str().unwrap_or("?");
        let (titulo, sub) = if com_album {
            (titulo.to_string(), format!("{} · {}", x["artist"].as_str().unwrap_or(""), x["album"].as_str().unwrap_or("")))
        } else {
            let n = x["track"].as_i64().unwrap_or(i as i64 + 1);
            (format!("{n}. {titulo}"), musica::mmss(x["duration"].as_f64().unwrap_or(0.0)))
        };
        v.push(ItemDados { titulo, sub, verde: false, acao: Acao::Tocar(f.clone(), i) });
    }
    v
}

fn itens_albuns(albuns: Vec<Value>) -> Vec<ItemDados> {
    albuns
        .into_iter()
        .map(|a| {
            let ano = a["year"].as_i64().map(|y| format!(" · {y}")).unwrap_or_default();
            ItemDados {
                titulo: a["name"].as_str().unwrap_or("?").into(),
                sub: format!("{}{ano}", a["artist"].as_str().unwrap_or("")),
                verde: false,
                acao: Acao::Album(a),
            }
        })
        .collect()
}

fn itens_artistas(artistas: Vec<Value>) -> Vec<ItemDados> {
    artistas
        .into_iter()
        .map(|a| ItemDados {
            titulo: a["name"].as_str().unwrap_or("?").into(),
            sub: format!("{} álbuns", a["albumCount"].as_i64().unwrap_or(0)),
            verde: false,
            acao: Acao::Artista(a),
        })
        .collect()
}

fn tocar(ui: &AppWindow, faixas: Vec<Value>, inicio: usize) {
    ui.set_tela(ss("tocando"));
    ui.set_aviso(ss("abrindo..."));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = api().tocar(&faixas, inicio);
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            ui.set_aviso(ss(match r {
                Ok(()) => String::new(),
                Err(e) => format!("não tocou: {}", curto(&e, 40)),
            }))
        });
    });
}

fn executar(ui: &AppWindow, acao: Acao) {
    match acao {
        Acao::Tocar(f, i) => tocar(ui, f.to_vec(), i),
        Acao::Aleatorio(f) => {
            let mut v = f.to_vec();
            embaralhar(&mut v);
            tocar(ui, v, 0);
        }
        Acao::Album(a) => {
            let id = a["id"].as_str().unwrap_or("").to_string();
            abrir_lista(ui, a["name"].as_str().unwrap_or("Álbum"), move |s| Ok(itens_faixas(s.album(&id)?, false)));
        }
        Acao::Artista(a) => {
            let id = a["id"].as_str().unwrap_or("").to_string();
            abrir_lista(ui, a["name"].as_str().unwrap_or(""), move |s| Ok(itens_albuns(s.albuns_do_artista(&id)?)));
        }
        Acao::Nada => {}
        Acao::TgAbrir(i) => tg_cmd(telegram::Cmd::Abrir(i)),
        Acao::Playlist(p) => {
            let id = p["id"].as_str().unwrap_or("").to_string();
            abrir_lista(ui, p["name"].as_str().unwrap_or(""), move |s| Ok(itens_faixas(s.playlist(&id)?, true)));
        }
    }
}

fn inicial(nome: &str) -> String {
    let c = nome.trim().chars().next().unwrap_or('#').to_uppercase().next().unwrap_or('#');
    let sem_acento = match c {
        'Á' | 'À' | 'Â' | 'Ã' | 'Ä' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => 'O',
        'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        c => c,
    };
    if sem_acento.is_ascii_uppercase() { sem_acento.to_string() } else { "#".into() }
}

fn artistas_letra(ui: &AppWindow, letra: String) {
    EST.with_borrow_mut(|e| {
        e.pilha.clear();
        e.origem = "letras".into();
    });
    let cache = EST.with_borrow(|e| e.artistas.clone());
    abrir_lista(ui, &format!("Artistas · {letra}"), move |s| {
        let todos = match cache {
            Some(a) => a,
            None => s.artistas()?,
        };
        Ok(itens_artistas(todos.into_iter().filter(|a| inicial(a["name"].as_str().unwrap_or("")) == letra).collect()))
    });
}

// -------------------------------------------------------------------- teclado

fn teclas(shift: bool, sym: bool, confirmar: &str) -> Vec<Tecla> {
    let linhas: [&str; 4] = if sym {
        ["1234567890", "@#$%&*-+()", "!\"':;/?_=", ".,<>[]{}"]
    } else {
        ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm"]
    };
    let (top, kh, gap) = (120.0, 104.0, 8.0);
    let kw = (1280.0 - 20.0 - gap * 9.0) / 10.0;
    let tecla_cor = Color::from_rgb_u8(45, 52, 64);
    let mut v = vec![];
    for (r, linha) in linhas.iter().enumerate() {
        let n = linha.chars().count() as f32;
        let x0 = (1280.0 - (kw * n + gap * (n - 1.0))) / 2.0;
        for (i, ch) in linha.chars().enumerate() {
            let rotulo = if shift && !sym { ch.to_uppercase().to_string() } else { ch.to_string() };
            v.push(Tecla {
                rotulo: ss(rotulo),
                x: x0 + i as f32 * (kw + gap),
                y: top + r as f32 * (kh + gap),
                w: kw,
                h: kh,
                cor: tecla_cor,
                fonte: 44.0,
            });
        }
    }
    let y = top + 4.0 * (kh + gap);
    let especial = |rotulo: &str, x0: f32, x1: f32, fonte: f32, cor: Color| Tecla {
        rotulo: ss(rotulo),
        x: x0,
        y,
        w: x1 - x0,
        h: kh,
        cor,
        fonte,
    };
    v.push(especial(if sym { "abc" } else { "⇧" }, 10.0, 170.0, 44.0, tecla_cor));
    v.push(especial(if sym { "ABC" } else { "?123" }, 180.0, 340.0, 26.0, tecla_cor));
    v.push(especial("espaço", 350.0, 780.0, 26.0, tecla_cor));
    v.push(especial("⌫", 790.0, 950.0, 44.0, tecla_cor));
    v.push(especial(confirmar, 960.0, 1270.0, 34.0, Color::from_rgb_u8(40, 110, 70)));
    v
}

fn atualizar_teclado(ui: &AppWindow) {
    EST.with_borrow(|e| {
        let senha = matches!(&e.modo, Modo::Senha(_))
            || matches!(&e.modo, Modo::Campo(c) if c == "senha")
            || matches!(&e.modo, Modo::Tg(c) if c == "senha");
        let confirmar = match &e.modo {
            Modo::Senha(_) => "Conectar",
            Modo::Busca => "Buscar",
            Modo::Campo(_) => "OK",
            Modo::Tg(_) => "Enviar",
        };
        let mostrado = if senha && e.ocultar { "•".repeat(e.texto.chars().count()) } else { e.texto.clone() };
        let ini = mostrado.chars().count().saturating_sub(30);
        ui.set_teclado_texto(ss(mostrado.chars().skip(ini).collect::<String>()));
        ui.set_teclado_vazio(ss(match &e.modo {
            Modo::Senha(s) => format!("senha de {}", curto(s, 18)),
            Modo::Busca => "buscar música".into(),
            Modo::Campo(c) => format!("{c} do Navidrome"),
            Modo::Tg(c) => match c.as_str() {
                "telefone" => "telefone com DDI, ex.: +5511999999999".into(),
                "codigo" => "código que chegou no Telegram".into(),
                "senha" => "senha de duas etapas".into(),
                _ => "mensagem".into(),
            },
        }));
        ui.set_teclado_senha(senha);
        ui.set_teclado_ocultar(e.ocultar);
        ui.set_teclado_confirmar(ss(confirmar));
        ui.set_teclas(ModelRc::new(VecModel::from(teclas(e.shift, e.sym, confirmar))));
    });
}

fn abrir_teclado(ui: &AppWindow, modo: Modo) {
    EST.with_borrow_mut(|e| {
        e.modo = modo;
        e.texto.clear();
        e.shift = false;
        e.sym = false;
        e.ocultar = true;
    });
    atualizar_teclado(ui);
    ui.set_tela(ss("teclado"));
}

fn confirmar(ui: &AppWindow) {
    let (modo, texto) = EST.with_borrow(|e| {
        let m = match &e.modo {
            Modo::Senha(s) => Modo::Senha(s.clone()),
            Modo::Busca => Modo::Busca,
            Modo::Campo(c) => Modo::Campo(c.clone()),
            Modo::Tg(c) => Modo::Tg(c.clone()),
        };
        (m, e.texto.clone())
    });
    match modo {
        Modo::Senha(ssid) => conectar_wifi(ui, ssid, texto),
        Modo::Busca => buscar(ui, texto.trim().to_string()),
        Modo::Tg(c) => {
            let v = texto.trim().to_string();
            if v.is_empty() {
                return;
            }
            match c.as_str() {
                "mensagem" => {
                    ui.set_tela(ss("tg-chat"));
                    tg_cmd(telegram::Cmd::Enviar(v));
                }
                passo => {
                    ui.set_tg_mensagem(ss("aguarde..."));
                    ui.set_tela(ss("tg-login"));
                    tg_cmd(match passo {
                        "telefone" => telegram::Cmd::Telefone(v),
                        "codigo" => telegram::Cmd::Codigo(v),
                        _ => telegram::Cmd::Senha(v),
                    });
                }
            }
        }
        Modo::Campo(c) => {
            let v = texto.trim().to_string();
            EST.with_borrow_mut(|e| match c.as_str() {
                "url" => e.conf_url = v,
                "usuário" => e.conf_user = v,
                _ => e.conf_pass = v,
            });
            ui.set_tela(ss("musica-conf"));
            atualizar_conf(ui);
        }
    }
}

fn cancelar_teclado(ui: &AppWindow) {
    match EST.with_borrow(|e| match &e.modo {
        Modo::Senha(_) => 0,
        Modo::Busca => 1,
        Modo::Campo(_) => 2,
        Modo::Tg(c) if c == "mensagem" => 3,
        Modo::Tg(_) => 4,
    }) {
        0 => ir(ui, "wifi"),
        1 => ui.set_tela(ss("musica")),
        3 => ui.set_tela(ss("tg-chat")),
        4 => ui.set_tela(ss("tg-login")),
        _ => ui.set_tela(ss("musica-conf")),
    }
}

fn tecla(ui: &AppWindow, t: &str) {
    match t {
        "[mostrar]" => EST.with_borrow_mut(|e| e.ocultar = !e.ocultar),
        "[cancelar]" => return cancelar_teclado(ui),
        "⇧" => EST.with_borrow_mut(|e| e.shift = !e.shift),
        "abc" | "ABC" => EST.with_borrow_mut(|e| {
            e.sym = false;
            e.shift = t == "ABC";
        }),
        "?123" => EST.with_borrow_mut(|e| {
            e.sym = true;
            e.shift = false;
        }),
        "espaço" => EST.with_borrow_mut(|e| e.texto.push(' ')),
        "⌫" => EST.with_borrow_mut(|e| {
            e.texto.pop();
        }),
        "Conectar" | "Buscar" | "OK" | "Enviar" => return confirmar(ui),
        letra => EST.with_borrow_mut(|e| {
            e.texto.push_str(letra);
            e.shift = false;
        }),
    }
    atualizar_teclado(ui);
}

fn tecla_fisica(ui: &AppWindow, t: &str) {
    let tela = ui.get_tela();
    if tela == "teclado" {
        match t {
            "[enter]" => confirmar(ui),
            "[esc]" => cancelar_teclado(ui),
            "[apagar]" => {
                EST.with_borrow_mut(|e| {
                    e.texto.pop();
                });
                atualizar_teclado(ui);
            }
            s if s.chars().count() == 1 && !s.chars().any(char::is_control) => {
                EST.with_borrow_mut(|e| e.texto.push_str(s));
                atualizar_teclado(ui);
            }
            _ => {}
        }
    } else if t == "[esc]" {
        ui.set_tela(ss("inicio"));
    } else if t == " " && tela == "tocando" {
        std::thread::spawn(|| musica::controle("pausar"));
    }
}

// ---------------------------------------------------------------------- Wi-Fi

fn ir(ui: &AppWindow, tela: &str) {
    ui.set_aviso(ss(""));
    if let Some(id) = tela.strip_prefix("m:") {
        return abrir_manifesto(ui, id);
    }
    if tela == "cripto" {
        carregar_cripto(ui);
    }
    if tela == "telegram" {
        return abrir_telegram(ui);
    }
    if tela == "musica-conf" {
        abrir_conf(ui);
    }
    if tela == "musica" && FILEIRAS.with_borrow(|f| f.is_empty()) {
        carregar_musica(ui);
    }
    ui.set_tela(ss(tela));
    match tela {
        "wifi" => procurar_redes(ui),
        "bt" => {
            if !dados::bt_cmd("scan") {
                ui.set_aviso(ss("serviço revive-bt parado"));
            }
        }
        _ => {}
    }
}

// --------------------------------------------- música no estilo Spotify

const FILAS: [(&str, &str); 4] =
    [("recentes", "recent"), ("frequentes", "frequent"), ("novidades", "newest"), ("aleatorios", "random")];

fn album_slint(a: &Value) -> Album {
    Album {
        id: ss(a["id"].as_str().unwrap_or("")),
        nome: ss(a["name"].as_str().unwrap_or("?")),
        artista: ss(a["artist"].as_str().unwrap_or("")),
        capa: Image::default(),
    }
}

fn mostrar_fileira(ui: &AppWindow, fila: &str, m: Rc<VecModel<Album>>) {
    let r = ModelRc::from(m);
    match fila {
        "recentes" => ui.set_mus_recentes(r),
        "frequentes" => ui.set_mus_frequentes(r),
        "novidades" => ui.set_mus_novidades(r),
        _ => ui.set_mus_aleatorios(r),
    }
}

/// Início da música: 4 fileiras de álbuns; as capas chegam uma a uma (cache em disco).
fn carregar_musica(ui: &AppWindow) {
    if !api().configurado() {
        ui.set_mus_mensagem(ss("Configure o Navidrome em ⚙ Ajustes"));
        return;
    }
    ui.set_mus_mensagem(ss(""));
    FILEIRAS.with_borrow_mut(|f| f.clear());
    for (fila, tipo) in FILAS {
        let fraca = ui.as_weak();
        std::thread::spawn(move || {
            let Ok(albuns) = api().lista_albuns(tipo) else { return };
            let capas: Vec<String> = albuns.iter().map(|a| a["coverArt"].as_str().unwrap_or("").to_string()).collect();
            let fraca2 = fraca.clone();
            let _ = fraca.upgrade_in_event_loop(move |ui| {
                let m = Rc::new(VecModel::from(albuns.iter().map(album_slint).collect::<Vec<_>>()));
                FILEIRAS.with_borrow_mut(|f| f.insert(fila.to_string(), (m.clone(), albuns)));
                mostrar_fileira(&ui, fila, m);
            });
            // capas em sequência, nesta mesma thread
            for (i, id) in capas.into_iter().enumerate() {
                if id.is_empty() {
                    continue;
                }
                let Some(buf) = api().capa_cache(&id, 180) else { continue };
                let _ = fraca2.upgrade_in_event_loop(move |_| {
                    FILEIRAS.with_borrow(|f| {
                        if let Some((m, _)) = f.get(fila) {
                            if let Some(mut a) = m.row_data(i) {
                                a.capa = Image::from_rgba8(buf);
                                m.set_row_data(i, a);
                            }
                        }
                    })
                });
            }
        });
    }
}

fn abrir_album_pagina(ui: &AppWindow, album: Value) {
    let ano = album["year"].as_i64().map(|y| y.to_string()).unwrap_or_default();
    ui.set_album_titulo(ss(album["name"].as_str().unwrap_or("?")));
    ui.set_album_artista(ss(album["artist"].as_str().unwrap_or("")));
    ui.set_album_info(ss(ano));
    ui.set_album_capa(Image::default());
    ui.set_album_faixas(ModelRc::new(VecModel::from(Vec::<Faixa>::new())));
    FAIXAS_ALBUM.with_borrow_mut(|f| f.clear());
    ui.set_tela(ss("album"));
    let id = album["id"].as_str().unwrap_or("").to_string();
    let capa = album["coverArt"].as_str().unwrap_or("").to_string();
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let faixas = api().album(&id).unwrap_or_default();
        let buf = if capa.is_empty() { None } else { api().capa_cache(&capa, 300) };
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            if ui.get_tela() != "album" {
                return;
            }
            let v: Vec<Faixa> = faixas
                .iter()
                .enumerate()
                .map(|(i, f)| Faixa {
                    titulo: ss(format!("{}. {}", f["track"].as_i64().unwrap_or(i as i64 + 1), f["title"].as_str().unwrap_or("?"))),
                    sub: ss(format!("{} · {}", f["artist"].as_str().unwrap_or(""), musica::mmss(f["duration"].as_f64().unwrap_or(0.0)))),
                })
                .collect();
            let n = v.len();
            ui.set_album_faixas(ModelRc::new(VecModel::from(v)));
            let info = ui.get_album_info();
            ui.set_album_info(ss(if info.is_empty() { format!("{n} faixas") } else { format!("{info} · {n} faixas") }));
            if let Some(b) = buf {
                ui.set_album_capa(Image::from_rgba8(b));
            }
            FAIXAS_ALBUM.with_borrow_mut(|f| *f = faixas);
        });
    });
}

fn mus_navegar(ui: &AppWindow, k: &str) {
    match k {
        "home" => carregar_musica(ui),
        "buscar" => musica_abrir(ui, "buscar"),
        "artistas" => musica_abrir(ui, "artistas"),
        "playlists" => musica_abrir(ui, "playlists"),
        "aleatorias" => musica_abrir(ui, "aleatorias"),
        "ajustes" => ir(ui, "musica-conf"),
        _ => ir(ui, "inicio"),
    }
}

// ---------------------------------------------------------------- telegram

thread_local! {
    static TG: RefCell<Option<tokio::sync::mpsc::UnboundedSender<telegram::Cmd>>> = RefCell::default();
    static TG_ESTADO: RefCell<String> = RefCell::default();
    static TG_BOLHAS: RefCell<Option<Rc<VecModel<Bolha>>>> = RefCell::default();
}

/// Mostra uma lista já pronta (sem buscar nada), na tela de lista paginada.
fn mostrar_direto(ui: &AppWindow, titulo: &str, itens: Vec<ItemDados>) {
    EST.with_borrow_mut(|e| {
        let pagina = if e.lista.titulo == titulo { e.lista.pagina } else { 0 };
        e.lista = Lista { titulo: titulo.into(), itens, pagina, vazio: "nada aqui".into() };
    });
    ui.set_tela(ss("lista"));
    mostrar_lista(ui);
}

fn tg_cmd(c: telegram::Cmd) {
    TG.with_borrow(|t| {
        if let Some(t) = t {
            let _ = t.send(c);
        }
    });
}

fn bolha(m: &telegram::Msg) -> Bolha {
    Bolha {
        texto: ss(m.texto.clone()),
        minha: m.minha,
        hora: ss(m.hora.clone()),
        midia: m.midia,
        video: m.video,
        foto: Image::default(),
    }
}

/// Mostra o passo de login (ou a lista, se já está logado).
fn tg_mostrar_estado(ui: &AppWindow) {
    let estado = TG_ESTADO.with_borrow(|e| e.clone());
    let (msg, botao) = match estado.as_str() {
        "" => ("conectando ao Telegram...".to_string(), ""),
        "sem-conf" => ("Falta /etc/revive/telegram.conf (api_id e api_hash do my.telegram.org).".into(), ""),
        "telefone" => ("Entre com a sua conta: toque em Digitar e escreva o telefone com DDI.".into(), "Digitar"),
        "codigo" => ("O Telegram mandou um código para a sua conta. Toque em Digitar.".into(), "Digitar"),
        "senha" => ("A conta tem verificação em duas etapas. Digite a senha.".into(), "Digitar"),
        "pronto" => ("carregando conversas...".into(), ""),
        erro => (erro.to_string(), "Digitar"),
    };
    ui.set_tg_mensagem(ss(msg));
    ui.set_tg_botao(ss(botao));
}

fn abrir_telegram(ui: &AppWindow) {
    if TG.with_borrow(|t| t.is_none()) {
        let fraca = ui.as_weak();
        let tx = telegram::iniciar(move |ev| {
            let _ = fraca.upgrade_in_event_loop(move |ui| tg_evento(&ui, ev));
        });
        TG.with_borrow_mut(|t| *t = Some(tx));
    }
    EST.with_borrow_mut(|e| {
        e.pilha.clear();
        e.origem = "inicio".into();
    });
    if TG_ESTADO.with_borrow(|e| e == "pronto") {
        ui.set_tg_mensagem(ss("carregando conversas..."));
        ui.set_tela(ss("tg-conversas"));
        tg_cmd(telegram::Cmd::Conversas);
    } else {
        tg_mostrar_estado(ui);
        ui.set_tela(ss("tg-login"));
    }
}

fn tg_evento(ui: &AppWindow, ev: telegram::Evento) {
    let tela = ui.get_tela();
    match ev {
        telegram::Evento::Estado(s) => {
            let pronto = s == "pronto";
            TG_ESTADO.with_borrow_mut(|e| *e = s);
            if tela == "tg-login" {
                tg_mostrar_estado(ui);
                if pronto {
                    abrir_telegram(ui);
                }
            }
        }
        telegram::Evento::Conversas(v) => {
            if tela != "tg-login" && tela != "tg-conversas" {
                return;
            }
            // avatar: iniciais e uma cor fixa por nome (paleta de tons do Material)
            const CORES: [(u8, u8, u8); 8] = [
                (0x5c, 0x6b, 0xc0), (0x26, 0xa6, 0x9a), (0xef, 0x6c, 0x00), (0xab, 0x47, 0xbc),
                (0x42, 0x85, 0xf4), (0xd8, 0x1b, 0x60), (0x7c, 0xb3, 0x42), (0x8d, 0x6e, 0x63),
            ];
            let modelo: Vec<TgConversa> = v
                .iter()
                .map(|c| {
                    let iniciais: String = c
                        .nome
                        .split_whitespace()
                        .filter_map(|p| p.chars().find(|ch| ch.is_alphanumeric()))
                        .take(2)
                        .collect::<String>()
                        .to_uppercase();
                    let h = c.nome.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
                    let (r, g, b) = CORES[h % CORES.len()];
                    TgConversa {
                        nome: ss(c.nome.clone()),
                        previa: ss(c.previa.replace('\n', " ")),
                        hora: ss(c.hora.clone()),
                        nao_lidas: c.nao_lidas,
                        iniciais: ss(if iniciais.is_empty() { "?".into() } else { iniciais }),
                        cor: Color::from_rgb_u8(r, g, b),
                    }
                })
                .collect();
            ui.set_tg_mensagem(ss("nenhuma conversa"));
            ui.set_tg_conversas(ModelRc::new(VecModel::from(modelo)));
            ui.set_tela(ss("tg-conversas"));
        }
        telegram::Evento::Chat(nome, msgs) => {
            let m = Rc::new(VecModel::from(msgs.iter().map(bolha).collect::<Vec<_>>()));
            TG_BOLHAS.with_borrow_mut(|b| *b = Some(m.clone()));
            ui.set_tg_nome(ss(curto(&nome, 40)));
            ui.set_tg_bolhas(ModelRc::from(m));
            ui.set_tela(ss("tg-chat"));
        }
        telegram::Evento::Foto(i, buf) => {
            TG_BOLHAS.with_borrow(|b| {
                if let Some(b) = b {
                    if let Some(mut x) = b.row_data(i) {
                        x.foto = Image::from_rgba8(buf);
                        b.set_row_data(i, x);
                    }
                }
            });
        }
        telegram::Evento::Nova(da_aberta, nome, m, foto) => {
            if da_aberta && (tela == "tg-chat" || tela == "teclado") {
                TG_BOLHAS.with_borrow(|b| {
                    if let Some(b) = b {
                        let mut x = bolha(&m);
                        if let Some(f) = foto {
                            x.foto = Image::from_rgba8(f);
                        }
                        b.push(x);
                    }
                });
            } else if !m.minha {
                ui.set_aviso(ss(curto(&format!("✉ {nome}: {}", m.texto.replace('\n', " ")), 60)));
            }
        }
    }
}

// ------------------------------------------------- configuração do Navidrome

fn abrir_conf(ui: &AppWindow) {
    let (url, user, pass) = api().dados();
    EST.with_borrow_mut(|e| {
        e.conf_url = url;
        e.conf_user = user;
        e.conf_pass = pass;
    });
    ui.set_conf_mensagem(ss(""));
    atualizar_conf(ui);
}

fn atualizar_conf(ui: &AppWindow) {
    EST.with_borrow(|e| {
        ui.set_conf_url(ss(curto(&e.conf_url, 60)));
        ui.set_conf_usuario(ss(e.conf_user.clone()));
        ui.set_conf_senha(ss("•".repeat(e.conf_pass.chars().count())));
    });
}

fn editar_campo(ui: &AppWindow, campo: String) {
    let atual = EST.with_borrow(|e| match campo.as_str() {
        "url" => e.conf_url.clone(),
        "usuário" => e.conf_user.clone(),
        _ => e.conf_pass.clone(),
    });
    abrir_teclado(ui, Modo::Campo(campo));
    EST.with_borrow_mut(|e| e.texto = atual);
    atualizar_teclado(ui);
}

/// Testa o login com os dados digitados e, se der certo, salva e passa a usá-los.
fn salvar_conf(ui: &AppWindow) {
    let (url, user, pass) = EST.with_borrow(|e| (e.conf_url.clone(), e.conf_user.clone(), e.conf_pass.clone()));
    if url.is_empty() || user.is_empty() {
        ui.set_conf_mensagem(ss("preencha a URL e o usuário"));
        return;
    }
    ui.set_conf_mensagem(ss("testando..."));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let novo = Subsonic::com(&url, &user, &pass);
        let r = novo.testar().and_then(|_| novo.salvar()).map(|_| novo);
        let _ = fraca.upgrade_in_event_loop(move |ui| match r {
            Ok(novo) => {
                trocar_api(novo);
                ui.set_musica_configurada(true);
                ui.set_conf_mensagem(ss("conectado e salvo ✓"));
            }
            Err(e) => ui.set_conf_mensagem(ss(format!("não conectou: {}", curto(&e, 40)))),
        });
    });
}

// ------------------------------------------------------------------- cripto

/// Preço no formato brasileiro (milhar com ponto, centavos com vírgula), em R$ ou US$.
fn preco(v: f64, moeda: &str) -> String {
    let simbolo = if moeda == "usd" { "US$" } else { "R$" };
    if v >= 100.0 {
        format!("{simbolo} {}", dados::milhar(v))
    } else {
        format!("{simbolo} {v:.2}").replace('.', ",")
    }
}

fn carregar_cripto(ui: &AppWindow) {
    let moeda = cripto::moeda();
    ui.set_cripto_moeda(ss(moeda.clone()));
    ui.set_cripto_mensagem(ss("carregando..."));
    let ja_tem: Vec<String> = ICONES.with_borrow(|i| i.keys().cloned().collect());
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = cripto::mercado(&moeda).map(|moedas| {
            let icones: Vec<_> = moedas
                .iter()
                .map(|m| if ja_tem.contains(&m.icone_url) { None } else { cripto::icone(&m.icone_url) })
                .collect();
            (moedas, icones)
        });
        let _ = fraca.upgrade_in_event_loop(move |ui| match r {
            Ok((moedas, icones)) => {
                let v: Vec<Moeda> = moedas
                    .iter()
                    .zip(icones)
                    .map(|(m, buf)| {
                        let icone = ICONES.with_borrow_mut(|c| {
                            if let Some(b) = buf {
                                c.insert(m.icone_url.clone(), Image::from_rgba8(b));
                            }
                            c.get(&m.icone_url).cloned().unwrap_or_default()
                        });
                        Moeda {
                            id: ss(m.id.clone()),
                            simbolo: ss(m.simbolo.clone()),
                            nome: ss(m.nome.clone()),
                            preco: ss(preco(m.preco, &moeda)),
                            variacao: ss(format!("{:+.2}%", m.variacao).replace('.', ",")),
                            subiu: m.variacao >= 0.0,
                            icone,
                        }
                    })
                    .collect();
                ui.set_moedas(ModelRc::new(VecModel::from(v)));
            }
            Err(e) => {
                ui.set_moedas(ModelRc::new(VecModel::from(Vec::<Moeda>::new())));
                ui.set_cripto_mensagem(ss(format!("não carregou: {}", curto(&e, 40))));
            }
        });
    });
}

fn abrir_grafico(ui: &AppWindow, id: String, nome: String) {
    ui.set_grafico_titulo(ss(format!("{nome} · últimas 24 h")));
    ui.set_grafico_info(ss("carregando..."));
    ui.set_velas(ModelRc::new(VecModel::from(Vec::<Vela>::new())));
    ui.set_tela(ss("grafico"));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let moeda = cripto::moeda();
        let r = cripto::grafico(&id, &moeda);
        let _ = fraca.upgrade_in_event_loop(move |ui| match r {
            Ok(g) => {
                let velas: Vec<Vela> = g
                    .velas
                    .iter()
                    .map(|v| Vela {
                        x: v.x,
                        topo: v.topo,
                        base: v.base,
                        corpo_topo: v.corpo_topo,
                        corpo_base: v.corpo_base,
                        subiu: v.subiu,
                    })
                    .collect();
                let var = (g.fechamento / g.abertura - 1.0) * 100.0;
                ui.set_grafico_maximo(ss(format!("máx {}", preco(g.maximo, &moeda))));
                ui.set_grafico_minimo(ss(format!("mín {}", preco(g.minimo, &moeda))));
                let pct = format!("{var:+.2}%").replace('.', ",");
                ui.set_grafico_info(ss(format!(
                    "{} → {}  ({pct})",
                    preco(g.abertura, &moeda),
                    preco(g.fechamento, &moeda)
                )));
                ui.set_grafico_subiu(var >= 0.0);
                ui.set_velas(ModelRc::new(VecModel::from(velas)));
            }
            Err(e) => ui.set_grafico_info(ss(format!("não carregou: {}", curto(&e, 40)))),
        });
    });
}

/// App por manifesto: busca a URL dele e mostra na lista; "Voltar" volta ao lançador.
fn abrir_manifesto(ui: &AppWindow, id: &str) {
    let Some(m) = manifesto::achar(id) else { return };
    EST.with_borrow_mut(|e| {
        e.pilha.clear();
        e.origem = "inicio".into();
    });
    ui.set_tela(ss("inicio")); // a lista nova não deve empilhar a tela anterior
    abrir_lista(ui, &m.nome, move |_| {
        Ok(m.buscar()?
            .into_iter()
            .map(|(titulo, sub, verde)| ItemDados { titulo, sub, verde, acao: Acao::Nada })
            .collect())
    });
}

fn procurar_redes(ui: &AppWindow) {
    ui.set_redes(ModelRc::new(VecModel::from(Vec::<Rede>::new())));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let redes = dados::wifi_procurar();
        let atual = dados::wifi().ssid;
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            let v: Vec<Rede> = redes
                .iter()
                .map(|r| Rede {
                    ssid: ss(curto(&r.ssid, 22)),
                    barras: dados::barras(r.sinal),
                    segura: r.segura,
                    conectada: r.ssid == atual,
                })
                .collect();
            ui.set_redes(ModelRc::new(VecModel::from(v)));
        });
    });
}

fn conectar_wifi(ui: &AppWindow, ssid: String, senha: String) {
    ui.set_tela(ss("inicio"));
    ui.set_aviso(ss(format!("conectando a {ssid}...")));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        dados::wifi_conectar(&ssid, &senha);
        let _ = fraca.upgrade_in_event_loop(|ui| ui.set_aviso(ss("")));
    });
}

// --------------------------------------------------------------------- música

fn buscar(ui: &AppWindow, texto: String) {
    if texto.is_empty() {
        return;
    }
    EST.with_borrow_mut(|e| {
        e.pilha.clear();
        e.origem = "musica".into();
    });
    let titulo = format!("“{}”", curto(&texto, 16));
    abrir_lista(ui, &titulo, move |s| {
        let (artistas, albuns, faixas) = s.buscar(&texto)?;
        let mut itens: Vec<ItemDados> = itens_artistas(artistas);
        for i in &mut itens {
            i.sub = "artista".into();
        }
        itens.extend(itens_albuns(albuns));
        let f = Arc::new(faixas);
        itens.extend(f.iter().enumerate().map(|(i, x)| ItemDados {
            titulo: x["title"].as_str().unwrap_or("?").into(),
            sub: format!("{} · música", x["artist"].as_str().unwrap_or("")),
            verde: false,
            acao: Acao::Tocar(f.clone(), i),
        }));
        Ok(itens)
    });
}

fn musica_abrir(ui: &AppWindow, chave: &str) {
    EST.with_borrow_mut(|e| {
        e.pilha.clear();
        e.origem = "musica".into();
    });
    match chave {
        "tocando" => ui.set_tela(ss("tocando")),
        "artistas" => {
            ui.set_tela(ss("letras"));
            if EST.with_borrow(|e| e.artistas.is_none()) {
                let fraca = ui.as_weak();
                std::thread::spawn(move || {
                    if let Ok(a) = api().artistas() {
                        let _ = fraca.upgrade_in_event_loop(move |_| EST.with_borrow_mut(|e| e.artistas = Some(a)));
                    }
                });
            }
        }
        "newest" | "frequent" | "random" => {
            let titulo = match chave {
                "newest" => "Álbuns recentes",
                "frequent" => "Mais tocados",
                _ => "Álbuns aleatórios",
            };
            let tipo = chave.to_string();
            abrir_lista(ui, titulo, move |s| Ok(itens_albuns(s.lista_albuns(&tipo)?)));
        }
        "aleatorias" => abrir_lista(ui, "Músicas aleatórias", |s| Ok(itens_faixas(s.aleatorias()?, true))),
        "playlists" => abrir_lista(ui, "Playlists", |s| {
            Ok(s.playlists()?
                .into_iter()
                .map(|p| ItemDados {
                    titulo: p["name"].as_str().unwrap_or("?").into(),
                    sub: format!("{} músicas", p["songCount"].as_i64().unwrap_or(0)),
                    verde: false,
                    acao: Acao::Playlist(p),
                })
                .collect())
        }),
        "buscar" => abrir_teclado(ui, Modo::Busca),
        "config" => ir(ui, "musica-conf"),
        _ => {}
    }
}

/// Capa da faixa: baixa, decodifica (JPEG/PNG) e manda para a tela.
fn carregar_capa(ui: &AppWindow, id: String) {
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let buf = api().capa(&id, 660).ok().and_then(|bytes| {
            let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let (w, h) = img.dimensions();
            Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(img.as_raw(), w, h))
        });
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            if EST.with_borrow(|e| e.capa == id) {
                ui.set_toc_capa(buf.map(Image::from_rgba8).unwrap_or_default());
            }
        });
    });
}

fn aplicar_player(ui: &AppWindow, st: Option<musica::Estado>) {
    let Some(st) = st else {
        ui.set_toc_ativo(false);
        ui.set_tocando(ss(""));
        return;
    };
    ui.set_toc_ativo(true);
    ui.set_toc_titulo(ss(st.titulo.clone()));
    ui.set_toc_artista(ss(st.artista.clone()));
    ui.set_toc_album(ss(st.album.clone()));
    ui.set_toc_progresso(if st.dur > 0.0 { (st.t / st.dur).clamp(0.0, 1.0) as f32 } else { 0.0 });
    ui.set_toc_tempo(ss(musica::mmss(st.t)));
    ui.set_toc_duracao(ss(musica::mmss(st.dur)));
    ui.set_toc_posicao(ss(format!("{} de {}", st.pos + 1, st.total)));
    ui.set_toc_pausa(st.pausa);
    ui.set_toc_volume(ss(format!("volume {:.0}%", st.volume)));
    ui.set_tocando(ss(curto(
        &format!("{} {} — {}", if st.pausa { "▶" } else { "❚❚" }, st.titulo, st.artista),
        34,
    )));
    // faixa nova: "tocando agora" no Navidrome e capa; passou da metade: scrobble
    let (nova, capa_nova, metade) = EST.with_borrow_mut(|e| {
        let nova = e.ultimo_id != st.id;
        e.ultimo_id = st.id.clone();
        let capa_nova = e.capa != st.capa;
        e.capa = st.capa.clone();
        let metade = st.dur > 0.0 && st.t > st.dur / 2.0 && e.scrobbled != st.id;
        if metade {
            e.scrobbled = st.id.clone();
        }
        (nova, capa_nova, metade)
    });
    if nova || metade {
        let id = st.id.clone();
        std::thread::spawn(move || api().scrobble(&id, metade));
    }
    if capa_nova {
        ui.set_toc_capa(Image::default());
        if !st.capa.is_empty() {
            carregar_capa(ui, st.capa.clone());
        }
    }
}

// ------------------------------------------------------------------- Bluetooth

fn aplicar_bt(ui: &AppWindow, st: Value) {
    let conectados = dados::bt_conectados(&st);
    ui.set_bt_conectados(ss(if conectados.is_empty() {
        String::new()
    } else {
        format!("Bluetooth: {}", conectados.join(", "))
    }));
    ui.set_bt_pronto(st["pronto"].as_bool().unwrap_or(false));
    ui.set_bt_procurando(st["procurando"].as_bool().unwrap_or(false));
    ui.set_bt_mensagem(ss(st["msg"].as_str().unwrap_or("")));
    ui.set_prompt_codigo(ss(st["prompt"]["codigo"].as_str().unwrap_or("")));
    ui.set_prompt_texto(ss(st["prompt"]["texto"].as_str().unwrap_or("")));
    let lista = st["aparelhos"].to_string();
    if EST.with_borrow(|e| e.bt_ultimo == lista) {
        return; // nada mudou: não refaz a lista (evita redesenho)
    }
    EST.with_borrow_mut(|e| e.bt_ultimo = lista);
    let v: Vec<Aparelho> = st["aparelhos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .take(8)
        .map(|a| {
            let b = |k: &str| a[k].as_bool().unwrap_or(false);
            let estado = if b("ocupado") {
                "aguarde..."
            } else if b("conectado") {
                "conectado"
            } else if b("pareado") {
                "pareado"
            } else {
                "toque para parear"
            };
            let tipo = a["tipo"].as_str().unwrap_or("");
            Aparelho {
                mac: ss(a["mac"].as_str().unwrap_or("")),
                nome: ss(curto(a["nome"].as_str().unwrap_or("?"), 18)),
                sub: ss(if tipo.is_empty() { estado.to_string() } else { format!("{tipo} · {estado}") }),
                conectado: b("conectado"),
                pareado: b("pareado"),
                som: tipo == "som",
                ocupado: b("ocupado"),
            }
        })
        .collect();
    ui.set_aparelhos(ModelRc::new(VecModel::from(v)));
}

fn bt_acao(ui: &AppWindow, acao: &str, mac: &str) {
    let linha = if acao == "cancelar" { "cancelar".to_string() } else { format!("{acao} {mac}") };
    let aviso = match acao {
        "pair" => "pareando...",
        "connect" => "conectando...",
        "disconnect" => "desconectando...",
        "remove" => "esquecido",
        "som" => "tocando...",
        _ => "",
    };
    ui.set_aviso(ss(if dados::bt_cmd(&linha) { aviso } else { "serviço revive-bt parado" }));
}

// ------------------------------------------------------------------- ligação

/// Tema salvo (/var/lib/revive/tema: "escuro" ou "claro").
const TEMA_FILE: &str = "/var/lib/revive/tema";

pub fn aplicar_tema(ui: &AppWindow, escuro: bool) {
    ui.global::<Tema>().set_escuro(escuro);
    ui.invoke_aplicar_tema();
}

pub fn ligar(ui: &AppWindow) {
    let escuro = std::fs::read_to_string(TEMA_FILE).map(|s| s.trim() != "claro").unwrap_or(true);
    aplicar_tema(ui, escuro);
    ui.set_musica_configurada(api().configurado());
    let apps: Vec<AppInfo> = APPS
        .iter()
        .map(|(id, nome, icone, (r, g, b))| AppInfo {
            id: ss(*id),
            nome: ss(*nome),
            icone: ss(*icone),
            cor: Color::from_rgb_u8(*r, *g, *b),
        })
        .chain(manifesto::todos().iter().map(|m| AppInfo {
            id: ss(format!("m:{}", m.id)),
            nome: ss(m.nome.clone()),
            icone: ss(m.icone.clone()),
            cor: Color::from_rgb_u8(m.cor.0, m.cor.1, m.cor.2),
        }))
        .collect();
    ui.set_apps(ModelRc::new(VecModel::from(apps)));
    let f = ui.as_weak();
    ui.on_cripto_abrir(move |id, nome| {
        if let Some(ui) = f.upgrade() {
            abrir_grafico(&ui, id.to_string(), nome.to_string());
        }
    });
    let f = ui.as_weak();
    ui.on_mus_abrir_album(move |fila, i| {
        if let Some(ui) = f.upgrade() {
            let a = FILEIRAS.with_borrow(|m| m.get(fila.as_str()).and_then(|(_, v)| v.get(i as usize).cloned()));
            if let Some(a) = a {
                abrir_album_pagina(&ui, a);
            }
        }
    });
    let f = ui.as_weak();
    ui.on_mus_navegar(move |k| f.upgrade().iter().for_each(|ui| mus_navegar(ui, &k)));
    let f = ui.as_weak();
    ui.on_album_tocar(move |i| {
        if let Some(ui) = f.upgrade() {
            let faixas = FAIXAS_ALBUM.with_borrow(|v| v.clone());
            tocar(&ui, faixas, i.max(0) as usize);
        }
    });
    let f = ui.as_weak();
    ui.on_album_aleatorio(move || {
        if let Some(ui) = f.upgrade() {
            let mut faixas = FAIXAS_ALBUM.with_borrow(|v| v.clone());
            embaralhar(&mut faixas);
            tocar(&ui, faixas, 0);
        }
    });
    let f = ui.as_weak();
    ui.on_tg_digitar(move || {
        if let Some(ui) = f.upgrade() {
            let passo = TG_ESTADO.with_borrow(|e| match e.as_str() {
                "codigo" => "codigo",
                "senha" => "senha",
                _ => "telefone",
            });
            abrir_teclado(&ui, Modo::Tg(passo.into()));
        }
    });
    ui.on_tg_abrir(|i| tg_cmd(telegram::Cmd::Abrir(i.max(0) as usize)));
    ui.on_tg_atualizar(|| tg_cmd(telegram::Cmd::Conversas));
    let f = ui.as_weak();
    ui.on_tg_escrever(move || f.upgrade().iter().for_each(|ui| abrir_teclado(ui, Modo::Tg("mensagem".into()))));
    let f = ui.as_weak();
    ui.on_tg_voltar(move || f.upgrade().iter().for_each(abrir_telegram));
    let f = ui.as_weak();
    ui.on_conf_campo(move |c| f.upgrade().iter().for_each(|ui| editar_campo(ui, c.to_string())));
    let f = ui.as_weak();
    ui.on_conf_salvar(move || f.upgrade().iter().for_each(salvar_conf));
    let f = ui.as_weak();
    ui.on_cripto_atualizar(move || f.upgrade().iter().for_each(carregar_cripto));
    let f = ui.as_weak();
    ui.on_cripto_trocar_moeda(move || {
        if let Some(ui) = f.upgrade() {
            cripto::trocar_moeda();
            carregar_cripto(&ui);
        }
    });

    // relógio
    let fraca = ui.as_weak();
    let tique = move || {
        if let Some(ui) = fraca.upgrade() {
            let agora = chrono::Local::now();
            ui.set_hora(ss(agora.format("%H:%M").to_string()));
            ui.set_data(ss(agora.format("%d/%m/%Y").to_string()));
        }
    };
    tique();
    let relogio = slint::Timer::default();
    relogio.start(slint::TimerMode::Repeated, Duration::from_secs(1), tique);
    std::mem::forget(relogio);

    // Bitcoin: a cada minuto e no botão "Atualizar"
    let atualizar_btc = {
        let fraca = ui.as_weak();
        move || {
            let fraca = fraca.clone();
            std::thread::spawn(move || {
                let r = dados::btc();
                let _ = fraca.upgrade_in_event_loop(move |ui| match r {
                    Ok(b) => {
                        ui.set_btc_brl(ss(format!("R$ {}", dados::milhar(b.brl))));
                        ui.set_btc_usd(ss(format!("US$ {}", dados::milhar(b.usd))));
                        if let Some(v) = b.variacao {
                            ui.set_btc_variacao(ss(format!("{v:+.2}% em 24 h").replace('.', ",")));
                            ui.set_btc_subiu(v >= 0.0);
                        }
                        ui.set_btc_info(ss(format!("atualizado às {}", chrono::Local::now().format("%H:%M"))));
                    }
                    Err(e) => ui.set_btc_info(ss(format!("erro: {}", curto(&e, 40)))),
                });
            });
        }
    };
    atualizar_btc();
    let a = atualizar_btc.clone();
    ui.on_atualizar_btc(move || a());
    let t = slint::Timer::default();
    // a API gratuita da CoinGecko limita as chamadas por minuto (o app Cripto também usa)
    t.start(slint::TimerMode::Repeated, Duration::from_secs(300), atualizar_btc);
    std::mem::forget(t);

    // telas
    let f = ui.as_weak();
    ui.on_ir(move |t| f.upgrade().iter().for_each(|ui| ir(ui, &t)));
    let f = ui.as_weak();
    ui.on_wifi_procurar(move || f.upgrade().iter().for_each(procurar_redes));
    let f = ui.as_weak();
    ui.on_wifi_escolher(move |ssid, segura| {
        if let Some(ui) = f.upgrade() {
            if segura {
                abrir_teclado(&ui, Modo::Senha(ssid.to_string()));
            } else {
                conectar_wifi(&ui, ssid.to_string(), String::new());
            }
        }
    });
    let f = ui.as_weak();
    ui.on_tecla(move |t| f.upgrade().iter().for_each(|ui| tecla(ui, &t)));
    let f = ui.as_weak();
    ui.on_tecla_fisica(move |t| f.upgrade().iter().for_each(|ui| tecla_fisica(ui, &t)));
    let f = ui.as_weak();
    ui.on_bt_acao(move |a, m| f.upgrade().iter().for_each(|ui| bt_acao(ui, &a, &m)));
    let f = ui.as_weak();
    ui.on_bt_procurar(move || f.upgrade().iter().for_each(|ui| ir(ui, "bt")));
    let f = ui.as_weak();
    ui.on_musica_abrir(move |c| f.upgrade().iter().for_each(|ui| musica_abrir(ui, &c)));
    let f = ui.as_weak();
    ui.on_letra(move |l| f.upgrade().iter().for_each(|ui| artistas_letra(ui, l.to_string())));
    let f = ui.as_weak();
    ui.on_item(move |i| {
        if let Some(ui) = f.upgrade() {
            let acao = EST.with_borrow(|e| {
                e.lista.itens.get(e.lista.pagina * POR_PAGINA + i as usize).map(|x| x.acao.clone())
            });
            if let Some(a) = acao {
                executar(&ui, a);
            }
        }
    });
    let f = ui.as_weak();
    ui.on_pagina_mudar(move |d| {
        if let Some(ui) = f.upgrade() {
            EST.with_borrow_mut(|e| {
                let paginas = e.lista.itens.len().div_ceil(POR_PAGINA).max(1);
                e.lista.pagina = (e.lista.pagina as i32 + d).clamp(0, paginas as i32 - 1) as usize;
            });
            mostrar_lista(&ui);
        }
    });
    let f = ui.as_weak();
    ui.on_lista_voltar(move || {
        if let Some(ui) = f.upgrade() {
            let anterior = EST.with_borrow_mut(|e| e.pilha.pop().map(|l| e.lista = l).is_some());
            if anterior {
                mostrar_lista(&ui);
            } else {
                let origem = EST.with_borrow(|e| e.origem.clone());
                ui.set_tela(ss(origem));
            }
        }
    });
    ui.on_controle(|c| {
        let c = c.to_string();
        std::thread::spawn(move || musica::controle(&c));
    });
    ui.on_pular(|fr| {
        std::thread::spawn(move || musica::pular(fr));
    });

    // Wi-Fi, Bluetooth e player em segundo plano (uma vez por segundo)
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let mut n = 0u64;
        let mut wifi = dados::Wifi::default();
        loop {
            if n % 5 == 0 {
                wifi = dados::wifi();
            }
            let bt = dados::bt_estado();
            let player = musica::estado();
            let w = wifi.clone();
            let _ = fraca.upgrade_in_event_loop(move |ui| {
                let ok = w.estado == "COMPLETED";
                ui.set_wifi_ok(ok);
                ui.set_wifi_ssid(ss(if w.ssid.is_empty() { "desconectado".into() } else { w.ssid.clone() }));
                ui.set_wifi_ip(ss(if w.ip.is_empty() { w.estado.to_lowercase() } else { w.ip.clone() }));
                aplicar_bt(&ui, bt);
                aplicar_player(&ui, player);
            });
            n += 1;
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}
