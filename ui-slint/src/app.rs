//! Estado da interface e ligação das telas (Slint) com os dados.
//!
//! O estado mora numa thread-local da thread da interface; buscas de rede rodam em
//! outras threads e voltam por `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde_json::Value;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::{brilho, camera, clima, i18n, inicio, loja, mesh, sistema};
use crate::dados;
use crate::manifesto;
use crate::musica::{self, Subsonic};
use crate::{Album, AppInfo, AppLojaUi, FotoUi, AppOrdem, MsgMesh, NoMesh, SecaoSobre, ItemSobre, AppWindow, Aparelho, Bolha, Faixa, FilaItem, HoraClima, HoraPrevUi, DiaUi, LugarUi, IconesClima, Item, MoedaMini, Moeda, Rede, Tecla, Tema, TgConversa, Vela};
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
    ("clima", "Clima", "☀", (176, 120, 20)),
    ("camera", "Câmera", "◎", (52, 56, 66)),
    ("doom", "DOOM II", "☠", (150, 30, 20)),
    ("mesh", "Mesh", "◉", (30, 120, 70)),
    ("loja", "Loja", "↓", (40, 110, 90)),
    ("sobre", "Sobre", "ℹ", (60, 70, 110)),
    ("ajustes", "Ajustes", "⚙", (70, 74, 84)),
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
    /// nome da cidade do app Clima
    Cidade,
    /// Meshtastic: "mensagem" ou "host"
    Mesh(String),
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
        e.lista = Lista { titulo: titulo.into(), vazio: i18n::t("carregando..."), ..Default::default() };
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
                        e.lista.vazio = i18n::t("nada aqui");
                        e.lista.itens = itens;
                    }
                    Err(erro) => e.lista.vazio = i18n::tf("não carregou: {}", &[&curto(&erro, 40)]),
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
        ItemDados { titulo: i18n::t("▶ Tocar tudo"), sub: String::new(), verde: true, acao: Acao::Tocar(f.clone(), 0) },
        ItemDados { titulo: i18n::t("Aleatório"), sub: String::new(), verde: false, acao: Acao::Aleatorio(f.clone()) },
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
            sub: i18n::tf("{} álbuns", &[&a["albumCount"].as_i64().unwrap_or(0).to_string()]),
            verde: false,
            acao: Acao::Artista(a),
        })
        .collect()
}

fn tocar(ui: &AppWindow, faixas: Vec<Value>, inicio: usize) {
    ui.set_tela(ss("tocando"));
    ui.set_aviso(ss(i18n::t("abrindo...")));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = api().tocar(&faixas, inicio);
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            ui.set_aviso(ss(match r {
                Ok(()) => String::new(),
                Err(e) => i18n::tf("não tocou: {}", &[&curto(&e, 40)]),
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
            abrir_lista(ui, a["name"].as_str().unwrap_or(&i18n::t("Álbum")), move |s| Ok(itens_faixas(s.album(&id)?, false)));
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
    abrir_lista(ui, &i18n::tf("Artistas · {}", &[&letra]), move |s| {
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
    let espaco = i18n::t("espaço");
    v.push(especial(&espaco, 350.0, 780.0, 26.0, tecla_cor));
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
            Modo::Senha(_) => i18n::t("Conectar"),
            Modo::Busca => i18n::t("Buscar"),
            Modo::Campo(_) => i18n::t("OK"),
            Modo::Tg(_) => i18n::t("Enviar"),
            Modo::Cidade => i18n::t("Buscar"),
            Modo::Mesh(c) if c == "host" => i18n::t("OK"),
            Modo::Mesh(_) => i18n::t("Enviar"),
        };
        let mostrado = if senha && e.ocultar { "•".repeat(e.texto.chars().count()) } else { e.texto.clone() };
        let ini = mostrado.chars().count().saturating_sub(30);
        ui.set_teclado_texto(ss(mostrado.chars().skip(ini).collect::<String>()));
        ui.set_teclado_vazio(ss(match &e.modo {
            Modo::Senha(s) => i18n::tf("senha de {}", &[&curto(s, 18)]),
            Modo::Busca => i18n::t("buscar música"),
            Modo::Cidade => i18n::t("nome da cidade"),
            Modo::Mesh(c) if c == "host" => i18n::t("IP do nó Meshtastic (Wi-Fi)"),
            Modo::Mesh(_) => i18n::t("mensagem"),
            Modo::Campo(c) => {
                let rotulo = i18n::t(c);
                i18n::tf("{} do Navidrome", &[&rotulo])
            }
            Modo::Tg(c) => match c.as_str() {
                "telefone" => i18n::t("telefone com DDI, ex.: +5511999999999"),
                "codigo" => i18n::t("código que chegou no Telegram"),
                "senha" => i18n::t("senha de duas etapas"),
                _ => i18n::t("mensagem"),
            },
        }));
        ui.set_teclado_senha(senha);
        ui.set_teclado_ocultar(e.ocultar);
        ui.set_teclado_confirmar(ss(confirmar.clone()));
        ui.set_teclas(ModelRc::new(VecModel::from(teclas(e.shift, e.sym, &confirmar))));
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
            Modo::Cidade => Modo::Cidade,
            Modo::Mesh(c) => Modo::Mesh(c.clone()),
        };
        (m, e.texto.clone())
    });
    match modo {
        Modo::Senha(ssid) => conectar_wifi(ui, ssid, texto),
        Modo::Busca => buscar(ui, texto.trim().to_string()),
        Modo::Cidade => buscar_cidade(ui, texto.trim().to_string()),
        Modo::Mesh(c) => mesh_teclado(ui, &c, texto.trim().to_string()),
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
                    ui.set_tg_mensagem(ss(i18n::t("aguarde...")));
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
        Modo::Cidade => 5,
        Modo::Mesh(_) => 6,
    }) {
        6 => ui.set_tela(ss("mesh")),
        5 => ui.set_tela(ss("clima-cidade")),
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
        s if s == i18n::t("espaço").as_str() => EST.with_borrow_mut(|e| e.texto.push(' ')),
        "⌫" => EST.with_borrow_mut(|e| {
            e.texto.pop();
        }),
        s if [i18n::t("Conectar"), i18n::t("Buscar"), i18n::t("OK"), i18n::t("Enviar")]
            .iter()
            .any(|c| c.as_str() == s) =>
        {
            return confirmar(ui)
        }
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
        camera_parar();
        ui.set_tela(ss("inicio"));
    } else if t == " " && tela == "tocando" {
        std::thread::spawn(|| musica::controle("pausar"));
    }
}

// ---------------------------------------------------------------------- Wi-Fi

fn ir(ui: &AppWindow, tela: &str) {
    ui.set_aviso(ss(""));
    // a câmera só fica ligada na tela dela (STREAMOFF ao sair)
    if tela != "camera" {
        camera_parar();
    }
    if tela == "camera" {
        abrir_camera(ui);
    }
    if tela == "camera-galeria" {
        abrir_galeria(ui);
    }
    if let Some(id) = tela.strip_prefix("m:") {
        return abrir_manifesto(ui, id);
    }
    if tela == "cripto" {
        carregar_cripto(ui);
    }
    if tela == "telegram" {
        return abrir_telegram(ui);
    }
    if tela == "doom" {
        // o jogo roda dentro do próprio revive-ui (src/doom.rs)
        crate::doom::abrir();
        return;
    }
    if tela == "clima" {
        return abrir_clima(ui);
    }
    if tela == "mesh" {
        abrir_mesh(ui);
    }
    if tela == "sobre" {
        abrir_sobre(ui);
    }
    if tela == "loja" {
        abrir_loja(ui);
    }
    if tela == "fila" {
        carregar_fila(ui);
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
                ui.set_aviso(ss(i18n::t("serviço revive-bt parado")));
            }
        }
        _ => {}
    }
}

/// Volta para a tela inicial (usado quando o DOOM devolve a tela para a interface).
/// Botões físicos de volume: ±5 no volume do player (mpv), com aviso na tela.
pub fn volume_tecla(ui: &AppWindow, passo: i32) {
    let atual = dados::mpv(serde_json::json!(["get_property", "volume"])).and_then(|v| v.as_f64()).unwrap_or(80.0);
    let novo = (atual + 5.0 * passo as f64).clamp(0.0, 130.0).round();
    dados::mpv(serde_json::json!(["set_property", "volume", novo]));
    let msg = i18n::tf("volume {}%", &[&format!("{novo:.0}")]);
    ui.set_toc_volume(ss(msg.clone()));
    ui.set_toc_vol((novo / 100.0).clamp(0.0, 1.0) as f32);
    ui.set_aviso(ss(msg.clone()));
    let fraca = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(2), move || {
        if let Some(ui) = fraca.upgrade() {
            if ui.get_aviso() == msg.as_str() {
                ui.set_aviso(ss(""));
            }
        }
    });
}

pub fn voltar_inicio(ui: &AppWindow) {
    ir(ui, "inicio");
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
    let h = chrono::Local::now().format("%H").to_string().parse::<u32>().unwrap_or(12);
    ui.set_mus_saudacao(ss(match h {
        5..=11 => i18n::t("Bom dia"),
        12..=17 => i18n::t("Boa tarde"),
        _ => i18n::t("Boa noite"),
    }));
    if !api().configurado() {
        ui.set_mus_mensagem(ss(i18n::t("Configure o Navidrome em ⚙ Ajustes")));
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
            let n = n.to_string();
            ui.set_album_info(ss(if info.is_empty() {
                i18n::tf("{} faixas", &[&n])
            } else {
                i18n::tf("{} · {} faixas", &[info.as_str(), &n])
            }));
            if let Some(b) = buf {
                ui.set_album_capa(Image::from_rgba8(b));
            }
            FAIXAS_ALBUM.with_borrow_mut(|f| *f = faixas);
        });
    });
}

/// Fila do mpv na tela "Fila" (a faixa atual destacada).
fn carregar_fila(ui: &AppWindow) {
    let pos = dados::mpv_prop("playlist-pos").and_then(|v| v.as_i64()).unwrap_or(-1);
    let itens: Vec<FilaItem> = musica::fila()
        .iter()
        .enumerate()
        .map(|(i, f)| FilaItem {
            titulo: ss(f["title"].as_str().unwrap_or("?")),
            artista: ss(f["artist"].as_str().unwrap_or("")),
            tocando: i as i64 == pos,
        })
        .collect();
    ui.set_fila_itens(ModelRc::new(VecModel::from(itens)));
}

fn mus_navegar(ui: &AppWindow, k: &str) {
    match k {
        "favoritas" => musica_abrir(ui, "favoritas"),
        "fila" => ir(ui, "fila"),
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
        e.lista = Lista { titulo: titulo.into(), itens, pagina, vazio: i18n::t("nada aqui") };
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
        "" => (i18n::t("conectando ao Telegram..."), String::new()),
        "sem-conf" => (i18n::t("Falta /etc/revive/telegram.conf (api_id e api_hash do my.telegram.org)."), String::new()),
        "telefone" => (
            i18n::t("Entre com a sua conta: toque em Digitar e escreva o telefone com DDI."),
            i18n::t("Digitar"),
        ),
        "codigo" => (i18n::t("O Telegram mandou um código para a sua conta. Toque em Digitar."), i18n::t("Digitar")),
        "senha" => (i18n::t("A conta tem verificação em duas etapas. Digite a senha."), i18n::t("Digitar")),
        "pronto" => (i18n::t("carregando conversas..."), String::new()),
        erro => (erro.to_string(), i18n::t("Digitar")),
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
        ui.set_tg_mensagem(ss(i18n::t("carregando conversas...")));
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
            ui.set_tg_mensagem(ss(i18n::t("nenhuma conversa")));
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
        telegram::Evento::FotoGrande(buf) => {
            if tela == "tg-foto" {
                ui.set_tg_foto(Image::from_rgba8(buf));
                ui.set_tg_foto_carregando(false);
            }
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
                ui.set_aviso(ss(curto(&i18n::tf("✉ {}: {}", &[&nome, &m.texto.replace('\n', " ")]), 60)));
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
        ui.set_conf_mensagem(ss(i18n::t("preencha a URL e o usuário")));
        return;
    }
    ui.set_conf_mensagem(ss(i18n::t("testando...")));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let novo = Subsonic::com(&url, &user, &pass);
        let r = novo.testar().and_then(|_| novo.salvar()).map(|_| novo);
        let _ = fraca.upgrade_in_event_loop(move |ui| match r {
            Ok(novo) => {
                trocar_api(novo);
                ui.set_musica_configurada(true);
                ui.set_conf_mensagem(ss(i18n::t("conectado e salvo ✓")));
            }
            Err(e) => ui.set_conf_mensagem(ss(i18n::tf("não conectou: {}", &[&curto(&e, 40)]))),
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
    ui.set_cripto_mensagem(ss(i18n::t("carregando...")));
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
                ui.set_cripto_mensagem(ss(i18n::tf("não carregou: {}", &[&curto(&e, 40)])));
            }
        });
    });
}

fn abrir_grafico(ui: &AppWindow, id: String, nome: String) {
    ui.set_grafico_titulo(ss(i18n::tf("{} · últimas 24 h", &[&nome])));
    ui.set_grafico_info(ss(i18n::t("carregando...")));
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
                ui.set_grafico_maximo(ss(i18n::tf("máx {}", &[&preco(g.maximo, &moeda)])));
                ui.set_grafico_minimo(ss(i18n::tf("mín {}", &[&preco(g.minimo, &moeda)])));
                let pct = format!("{var:+.2}%").replace('.', ",");
                ui.set_grafico_info(ss(i18n::tf(
                    "{} → {}  ({})",
                    &[&preco(g.abertura, &moeda), &preco(g.fechamento, &moeda), &pct],
                )));
                ui.set_grafico_subiu(var >= 0.0);
                ui.set_velas(ModelRc::new(VecModel::from(velas)));
            }
            Err(e) => ui.set_grafico_info(ss(i18n::tf("não carregou: {}", &[&curto(&e, 40)]))),
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
    ui.set_aviso(ss(i18n::tf("conectando a {}...", &[&ssid])));
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
            i.sub = i18n::t("artista");
        }
        itens.extend(itens_albuns(albuns));
        let f = Arc::new(faixas);
        itens.extend(f.iter().enumerate().map(|(i, x)| ItemDados {
            titulo: x["title"].as_str().unwrap_or("?").into(),
            sub: i18n::tf("{} · música", &[x["artist"].as_str().unwrap_or("")]),
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
                "newest" => i18n::t("Álbuns recentes"),
                "frequent" => i18n::t("Mais tocados"),
                _ => i18n::t("Álbuns aleatórios"),
            };
            let tipo = chave.to_string();
            abrir_lista(ui, &titulo, move |s| Ok(itens_albuns(s.lista_albuns(&tipo)?)));
        }
        "favoritas" => abrir_lista(ui, &i18n::t("Favoritas"), |s| Ok(itens_faixas(s.favoritas()?, true))),
        "aleatorias" => abrir_lista(ui, &i18n::t("Músicas aleatórias"), |s| Ok(itens_faixas(s.aleatorias()?, true))),
        "playlists" => abrir_lista(ui, &i18n::t("Playlists"), |s| {
            Ok(s.playlists()?
                .into_iter()
                .map(|p| ItemDados {
                    titulo: p["name"].as_str().unwrap_or("?").into(),
                    sub: i18n::tf("{} músicas", &[&p["songCount"].as_i64().unwrap_or(0).to_string()]),
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
    ui.set_toc_posicao(ss(i18n::tf("{} de {}", &[&(st.pos + 1).to_string(), &st.total.to_string()])));
    ui.set_toc_pausa(st.pausa);
    ui.set_toc_volume(ss(i18n::tf("volume {}%", &[&format!("{:.0}", st.volume)])));
    ui.set_toc_vol((st.volume / 100.0).clamp(0.0, 1.0) as f32);
    ui.set_toc_favorita(st.favorita);
    ui.set_toc_repetir(ss(st.repetir.clone()));
    if ui.get_tela() == "fila" {
        carregar_fila(ui);
    }
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
    ui.set_bt_qtd(conectados.len() as i32);
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
                i18n::t("aguarde...")
            } else if b("conectado") {
                i18n::t("conectado")
            } else if b("pareado") {
                i18n::t("pareado")
            } else {
                i18n::t("toque para parear")
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
        "pair" => i18n::t("pareando..."),
        "connect" => i18n::t("conectando..."),
        "disconnect" => i18n::t("desconectando..."),
        "remove" => i18n::t("esquecido"),
        "som" => i18n::t("tocando..."),
        _ => String::new(),
    };
    ui.set_aviso(ss(if dados::bt_cmd(&linha) { aviso } else { i18n::t("serviço revive-bt parado") }));
}

// ------------------------------------------------------------------- ligação

/// Tema salvo (/var/lib/revive/tema: "escuro 0" = modo e esquema de cores).
const TEMA_FILE: &str = "/var/lib/revive/tema";

pub fn aplicar_tema(ui: &AppWindow, escuro: bool) {
    ui.global::<Tema>().set_escuro(escuro);
    ui.invoke_aplicar_tema();
    salvar_tema(ui);
}

fn salvar_tema(ui: &AppWindow) {
    let t = ui.global::<Tema>();
    let _ = std::fs::write(TEMA_FILE, format!("{} {}", if t.get_escuro() { "escuro" } else { "claro" }, t.get_paleta()));
}

thread_local! {
    static ORDEM: RefCell<Vec<(String, bool)>> = RefCell::default();
    static TODOS_APPS: RefCell<Vec<AppInfo>> = RefCell::default();
    static ACORDADO: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Ícone em imagem de alguns apps. DOOM II: a arte que o jogador pôs no aparelho
/// (/var/lib/revive/doom/icone.png) ou, sem ela, o rosto do Doomguy tirado do próprio WAD.
fn icone_app(id: &str) -> Option<Image> {
    if id != "doom" {
        return None;
    }
    if let Ok(img) = image::open("/var/lib/revive/doom/icone.png") {
        let img = img.to_rgba8();
        let (w, h) = img.dimensions();
        return Some(Image::from_rgba8(SharedPixelBuffer::clone_from_slice(img.as_raw(), w, h)));
    }
    ["/var/lib/revive/doom/doom2.wad", "/usr/share/games/doom/freedoom2.wad"]
        .iter()
        .find_map(|w| crate::wad::figura(w, "STFST01"))
        .map(Image::from_rgba8)
}

thread_local! {
    static MESH_TX: RefCell<Option<std::sync::mpsc::Sender<mesh::Cmd>>> = RefCell::default();
    static MESH_NOS: RefCell<Vec<mesh::No>> = RefCell::default();
    static MESH_MSGS: RefCell<Vec<mesh::Msg>> = RefCell::default();
}

fn agora_epoch() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as u32).unwrap_or(0)
}

/// "há 3 min", "há 2 h" a partir do last_heard do nó.
fn ha(epoch: u32) -> String {
    let d = agora_epoch().saturating_sub(epoch);
    if d < 60 {
        i18n::t("agora")
    } else if d < 3600 {
        i18n::tf("há {} min", &[&(d / 60).to_string()])
    } else if d < 86400 {
        i18n::tf("há {} h", &[&(d / 3600).to_string()])
    } else {
        i18n::tf("há {} d", &[&(d / 86400).to_string()])
    }
}

fn mesh_mostrar(ui: &AppWindow) {
    let nos: Vec<NoMesh> = MESH_NOS.with_borrow(|v| {
        let mut v = v.clone();
        v.sort_by_key(|n| std::cmp::Reverse(n.visto.unwrap_or(0)));
        v.iter()
            .map(|n| NoMesh {
                num: n.num as i32,
                nome: ss(if n.nome.is_empty() { format!("!{:08x}", n.num) } else { n.nome.clone() }),
                curto: ss(if n.curto.is_empty() { format!("{:04x}", n.num & 0xffff) } else { n.curto.clone() }),
                bateria: ss(n.bateria.map(|b| format!("{b}%")).unwrap_or_else(|| "—".into())),
                snr: ss(n.snr.map(|s| format!("{s:.1}")).unwrap_or_else(|| "—".into())),
                visto: ss(n.visto.map(ha).unwrap_or_default()),
                online: n.visto.map(|t| agora_epoch().saturating_sub(t) < 900).unwrap_or(false),
            })
            .collect()
    });
    ui.set_mesh_nos(ModelRc::new(VecModel::from(nos)));
    let msgs: Vec<MsgMesh> = MESH_MSGS.with_borrow(|v| {
        v.iter()
            .rev()
            .take(100)
            .rev()
            .map(|m| MsgMesh {
                autor: ss(if m.minha { i18n::t("Eu") } else { m.de_nome.clone() }),
                texto: ss(m.texto.clone()),
                hora: ss(chrono::DateTime::from_timestamp(m.hora as i64, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                    .unwrap_or_default()),
                minha: m.minha,
            })
            .collect()
    });
    ui.set_mesh_msgs(ModelRc::new(VecModel::from(msgs)));
}

/// App Meshtastic: liga a conexão TCP com o nó (uma vez) e mostra nós e mensagens.
fn abrir_mesh(ui: &AppWindow) {
    if !mesh::configurado() {
        ui.set_mesh_estado(ss(i18n::t("erro: configure o IP do nó")));
        mesh_mostrar(ui);
        return;
    }
    if MESH_TX.with_borrow(|t| t.is_some()) {
        mesh_mostrar(ui);
        return;
    }
    MESH_MSGS.with_borrow_mut(|v| *v = mesh::historico());
    let fraca = ui.as_weak();
    let tx = mesh::iniciar(move |ev| {
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            match ev {
                mesh::Evento::Estado(s) => ui.set_mesh_estado(ss(s)),
                mesh::Evento::Nos(n) => MESH_NOS.with_borrow_mut(|v| *v = n),
                mesh::Evento::Mensagem(m) => MESH_MSGS.with_borrow_mut(|v| v.push(m)),
                mesh::Evento::Eu(_) => {}
            }
            mesh_mostrar(&ui);
        });
    });
    let _ = tx.send(mesh::Cmd::Conectar);
    MESH_TX.with_borrow_mut(|t| *t = Some(tx));
    mesh_mostrar(ui);
}

fn mesh_teclado(ui: &AppWindow, campo: &str, texto: String) {
    ui.set_tela(ss("mesh"));
    if campo == "host" {
        if texto.is_empty() {
            return;
        }
        let _ = std::fs::write("/etc/revive/mesh.conf", format!("host={texto}\n"));
        match MESH_TX.with_borrow(|t| t.clone()) {
            Some(tx) => {
                let _ = tx.send(mesh::Cmd::Conectar);
            }
            None => abrir_mesh(ui),
        }
    } else {
        ui.set_mesh_rascunho(ss(texto));
    }
}

thread_local! {
    static LOJA: RefCell<Vec<loja::AppLoja>> = RefCell::default();
}

fn loja_ui(a: &loja::AppLoja) -> AppLojaUi {
    AppLojaUi {
        id: ss(a.id.clone()),
        nome: ss(a.nome.clone()),
        descricao: ss(a.descricao.clone()),
        autor: ss(a.autor.clone()),
        icone: ss(a.icone.clone()),
        cor: Color::from_rgb_u8(a.cor.0, a.cor.1, a.cor.2),
        categoria: ss(a.categoria.clone()),
        instalado: a.instalado,
        requer: ss(a.requer.join(", ")),
    }
}

/// Loja: baixa o catálogo (index.json no GitHub) em segundo plano.
fn abrir_loja(ui: &AppWindow) {
    ui.set_loja_carregando(true);
    ui.set_loja_msg(ss(""));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = loja::catalogo();
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            ui.set_loja_carregando(false);
            match r {
                Ok(l) => {
                    let v: Vec<AppLojaUi> = l.iter().map(loja_ui).collect();
                    ui.set_loja_apps(ModelRc::new(VecModel::from(v)));
                    if l.is_empty() {
                        ui.set_loja_msg(ss(i18n::t("Nenhum app no catálogo")));
                    }
                    LOJA.with_borrow_mut(|g| *g = l);
                }
                Err(e) => ui.set_loja_msg(ss(e)),
            }
        });
    });
}

/// Depois de instalar/remover um app de manifesto: refaz a lista de apps do início.
fn recarregar_apps(ui: &AppWindow) {
    let todos = todos_apps();
    let ids: Vec<String> = todos.iter().map(|a| a.id.to_string()).collect();
    ORDEM.with_borrow_mut(|o| *o = inicio::carregar(&ids));
    TODOS_APPS.with_borrow_mut(|t| *t = todos);
    aplicar_ordem(ui);
}

/// Instala ou remove o app aberto na loja.
fn loja_acao(ui: &AppWindow, instalar: bool) {
    let Some(a) = LOJA.with_borrow(|g| g.iter().find(|x| x.id == ui.get_loja_app().id.as_str()).cloned()) else {
        return;
    };
    ui.set_loja_ocupado(true);
    ui.set_loja_app_msg(ss(""));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = if instalar { loja::instalar(&a) } else { loja::remover(&a.id) };
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            ui.set_loja_ocupado(false);
            match r {
                Ok(()) => {
                    let mut a = a;
                    a.instalado = instalar;
                    ui.set_loja_app(loja_ui(&a));
                    LOJA.with_borrow_mut(|g| {
                        if let Some(x) = g.iter_mut().find(|x| x.id == a.id) {
                            x.instalado = instalar;
                        }
                    });
                    let v: Vec<AppLojaUi> = LOJA.with_borrow(|g| g.iter().map(loja_ui).collect());
                    ui.set_loja_apps(ModelRc::new(VecModel::from(v)));
                    recarregar_apps(&ui);
                }
                Err(e) => ui.set_loja_app_msg(ss(format!("erro: {e}"))),
            }
        });
    });
}

/// Tela "Sobre o sistema": coleta em segundo plano; o resumo da CPU atualiza a cada 2 s.
fn abrir_sobre(ui: &AppWindow) {
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let s = crate::sobre::coletar();
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            let v: Vec<SecaoSobre> = s
                .into_iter()
                .map(|x| SecaoSobre {
                    titulo: ss(x.titulo),
                    itens: ModelRc::new(VecModel::from(
                        x.itens.into_iter().map(|i| ItemSobre { rotulo: ss(i.rotulo), valor: ss(i.valor) }).collect::<Vec<_>>(),
                    )),
                })
                .collect();
            ui.set_sobre_secoes(ModelRc::new(VecModel::from(v)));
        });
    });
    let fraca = ui.as_weak();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(2), move || {
        if let Some(ui) = fraca.upgrade() {
            if ui.get_tela() == "sobre" {
                ui.set_sobre_cpu(ss(crate::sobre::cpu_resumo()));
            }
        }
    });
    std::mem::forget(t);
    ui.set_sobre_cpu(ss(crate::sobre::cpu_resumo()));
}

// ---------------------------------------------------------------------- Câmera

thread_local! {
    /// captura ligada (só enquanto a tela "camera" está aberta)
    static CAMERA: RefCell<Option<camera::Captura>> = RefCell::default();
    /// fotos da galeria, na ordem da grade
    static FOTOS: RefCell<Vec<std::path::PathBuf>> = RefCell::default();
    static GAL_MODELO: RefCell<Option<Rc<VecModel<FotoUi>>>> = RefCell::default();
}
/// muda a cada abertura da galeria: descarta miniaturas de uma abertura anterior
static GAL_GERACAO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn nome_foto(p: &std::path::Path) -> String {
    p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string()
}

/// Desliga a captura (a thread faz STREAMOFF e fecha o dispositivo).
fn camera_parar() {
    let c = CAMERA.with_borrow_mut(|c| c.take());
    drop(c);
}

fn abrir_camera(ui: &AppWindow) {
    camera_parar();
    ui.set_cam_ativa(false);
    ui.set_cam_ocupado(false);
    ui.set_cam_preview(Image::default());
    ui.set_cam_tem_traseira(camera::tem_traseira());
    ui.set_cam_mensagem(ss(i18n::t("abrindo a câmera...")));
    let fraca = ui.as_weak();
    let cap = camera::iniciar(move || {
        let _ = fraca.upgrade_in_event_loop(|ui| camera_drenar(&ui));
    });
    CAMERA.with_borrow_mut(|c| *c = Some(cap));
    // miniatura da última foto
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let ultima = camera::listar_fotos(&camera::config().pasta).into_iter().next();
        let mini = ultima.as_deref().and_then(camera::miniatura);
        let _ = fraca.upgrade_in_event_loop(move |ui| match mini {
            Some(m) => {
                ui.set_cam_ultima(Image::from_rgb8(m));
                ui.set_cam_tem_ultima(true);
            }
            None => ui.set_cam_tem_ultima(false),
        });
    });
}

/// Pega o quadro novo e os eventos da thread de captura.
fn camera_drenar(ui: &AppWindow) {
    if ui.get_tela() != "camera" {
        // saiu da tela por um caminho que não passou por ir()
        return camera_parar();
    }
    let (quadro, eventos) = CAMERA.with_borrow(|c| match c {
        Some(c) => (c.quadro(), c.eventos()),
        None => (None, vec![]),
    });
    for ev in eventos {
        match ev {
            camera::Evento::Pronta(w, h) => {
                eprintln!("câmera pronta: {w}x{h}");
                ui.set_cam_ativa(true);
                if !ui.get_cam_ocupado() {
                    ui.set_cam_mensagem(ss(""));
                }
            }
            camera::Evento::Erro(m) => {
                ui.set_cam_ativa(false);
                ui.set_cam_ocupado(false);
                ui.set_cam_mensagem(ss(m));
            }
            camera::Evento::Foto(Ok((caminho, mini))) => {
                ui.set_cam_ocupado(false);
                let nome = caminho.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                let msg = i18n::tf("foto salva: {}", &[&nome]);
                ui.set_cam_mensagem(ss(msg.clone()));
                if let Some(m) = mini {
                    ui.set_cam_ultima(Image::from_rgb8(m));
                    ui.set_cam_tem_ultima(true);
                }
                // a mensagem some depois de 3 s (se ninguém a trocou)
                let fraca = ui.as_weak();
                slint::Timer::single_shot(Duration::from_secs(3), move || {
                    if let Some(ui) = fraca.upgrade() {
                        if ui.get_cam_mensagem() == msg.as_str() {
                            ui.set_cam_mensagem(ss(""));
                        }
                    }
                });
            }
            camera::Evento::Foto(Err(e)) => {
                ui.set_cam_ocupado(false);
                ui.set_cam_mensagem(ss(i18n::tf("não consegui salvar a foto: {}", &[&curto(&e, 60)])));
            }
        }
    }
    if let Some(q) = quadro {
        ui.set_cam_preview(Image::from_rgb8(q));
    }
}

fn abrir_galeria(ui: &AppWindow) {
    let geracao = GAL_GERACAO.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    ui.set_gal_aberta(-1);
    ui.set_gal_foto(Image::default());
    ui.set_gal_mensagem(ss(i18n::t("carregando...")));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let atual = || GAL_GERACAO.load(std::sync::atomic::Ordering::Relaxed) == geracao;
        let fotos = camera::listar_fotos(&camera::config().pasta);
        let lista = fotos.clone();
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            if GAL_GERACAO.load(std::sync::atomic::Ordering::Relaxed) != geracao {
                return;
            }
            let v: Vec<FotoUi> =
                lista.iter().map(|p| FotoUi { nome: ss(nome_foto(p)), miniatura: Image::default() }).collect();
            let modelo = Rc::new(VecModel::from(v));
            ui.set_gal_fotos(ModelRc::from(modelo.clone()));
            GAL_MODELO.with_borrow_mut(|m| *m = Some(modelo));
            FOTOS.with_borrow_mut(|f| *f = lista.clone());
            ui.set_gal_mensagem(ss(if lista.is_empty() { i18n::t("Nenhuma foto ainda") } else { String::new() }));
        });
        // miniaturas uma a uma (do cache ou geradas)
        for p in fotos {
            if !atual() {
                return;
            }
            let Some(m) = camera::miniatura(&p) else { continue };
            let _ = fraca.upgrade_in_event_loop(move |_| {
                if GAL_GERACAO.load(std::sync::atomic::Ordering::Relaxed) != geracao {
                    return;
                }
                // procura pelo caminho: a lista pode ter mudado (foto apagada)
                let Some(i) = FOTOS.with_borrow(|f| f.iter().position(|x| *x == p)) else { return };
                GAL_MODELO.with_borrow(|modelo| {
                    if let Some(modelo) = modelo {
                        if let Some(mut f) = modelo.row_data(i) {
                            f.miniatura = Image::from_rgb8(m);
                            modelo.set_row_data(i, f);
                        }
                    }
                });
            });
        }
    });
}

fn galeria_abrir(ui: &AppWindow, i: i32) {
    let Some(p) = FOTOS.with_borrow(|f| f.get(i.max(0) as usize).cloned()) else { return };
    ui.set_gal_foto(Image::default());
    ui.set_gal_aberta(i);
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let Some(q) = camera::carregar_foto(&p) else { return };
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            // só se a mesma foto continua aberta
            let ainda = FOTOS.with_borrow(|f| f.get(ui.get_gal_aberta().max(0) as usize) == Some(&p));
            if ui.get_gal_aberta() >= 0 && ainda {
                ui.set_gal_foto(Image::from_rgb8(q));
            }
        });
    });
}

fn galeria_apagar(ui: &AppWindow, i: i32) {
    let i = i.max(0) as usize;
    let Some(p) = FOTOS.with_borrow(|f| f.get(i).cloned()) else { return };
    ui.set_gal_aberta(-1);
    ui.set_gal_foto(Image::default());
    match camera::apagar(&p) {
        Ok(()) => {
            FOTOS.with_borrow_mut(|f| {
                f.remove(i);
            });
            GAL_MODELO.with_borrow(|m| {
                if let Some(m) = m {
                    if i < m.row_count() {
                        m.remove(i);
                    }
                }
            });
            if FOTOS.with_borrow(|f| f.is_empty()) {
                ui.set_gal_mensagem(ss(i18n::t("Nenhuma foto ainda")));
            }
        }
        Err(e) => ui.set_gal_mensagem(ss(i18n::tf("não consegui apagar: {}", &[&curto(&e, 60)]))),
    }
}

/// Apps nativos e de manifesto, na ordem padrão.
fn todos_apps() -> Vec<AppInfo> {
    APPS.iter()
        .map(|(id, nome, icone, (r, g, b))| {
            let imagem = icone_app(id);
            AppInfo {
                id: ss(*id),
                nome: ss(i18n::t(*nome)),
                icone: ss(*icone),
                cor: Color::from_rgb_u8(*r, *g, *b),
                tem_imagem: imagem.is_some(),
                imagem: imagem.unwrap_or_default(),
            }
        })
        .chain(manifesto::todos().iter().map(|m| AppInfo {
            id: ss(format!("m:{}", m.id)),
            nome: ss(m.nome.clone()),
            icone: ss(m.icone.clone()),
            cor: Color::from_rgb_u8(m.cor.0, m.cor.1, m.cor.2),
            imagem: Image::default(),
            tem_imagem: false,
        }))
        .collect()
}

/// Lançador (só os visíveis, na ordem escolhida) e a tela "Organizar início".
fn aplicar_ordem(ui: &AppWindow) {
    let (visiveis, org) = TODOS_APPS.with_borrow(|todos| {
        ORDEM.with_borrow(|ordem| {
            let achar = |id: &str| todos.iter().find(|a| a.id == id).cloned();
            let visiveis: Vec<AppInfo> = ordem.iter().filter(|(_, v)| *v).filter_map(|(id, _)| achar(id)).collect();
            let org: Vec<AppOrdem> = ordem
                .iter()
                .filter_map(|(id, v)| {
                    achar(id).map(|a| AppOrdem { id: a.id, nome: a.nome, icone: a.icone, cor: a.cor, visivel: *v })
                })
                .collect();
            (visiveis, org)
        })
    });
    ui.set_apps(ModelRc::new(VecModel::from(visiveis)));
    ui.set_org_apps(ModelRc::new(VecModel::from(org)));
}

thread_local! {
    static LUGARES: RefCell<Vec<clima::Lugar>> = RefCell::default();
}

/// App Clima: mostra a tela e busca a previsão completa em segundo plano.
fn abrir_clima(ui: &AppWindow) {
    ui.set_tela(ss("clima"));
    ui.set_cl_carregando(true);
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = clima::previsao();
        let _ = fraca.upgrade_in_event_loop(move |ui| {
            ui.set_cl_carregando(false);
            let p = match r {
                Ok(p) => p,
                Err(e) => return ui.set_cl_erro(ss(e)),
            };
            ui.set_cl_erro(ss(""));
            ui.set_cl_cidade(ss(p.cidade));
            ui.set_cl_temp(ss(format!("{}°", p.temp)));
            ui.set_cl_sensacao(ss(i18n::tf("Sensação {}°", &[&p.sensacao.to_string()])));
            ui.set_cl_desc(ss(p.descricao));
            ui.set_cl_icone(ss(p.icone));
            ui.set_cl_umidade(ss(format!("{}%", p.umidade)));
            ui.set_cl_vento(ss(format!("{} km/h", p.vento)));
            ui.set_cl_chuva(ss(format!("{}%", p.chuva_hoje)));
            ui.set_cl_uv(ss(p.uv.to_string()));
            ui.set_cl_nascer(ss(p.nascer));
            ui.set_cl_por(ss(p.por));
            let g = ui.global::<IconesClima>();
            let horas: Vec<HoraPrevUi> = p
                .horas
                .iter()
                .map(|h| HoraPrevUi {
                    hora: ss(h.hora.clone()),
                    temp: ss(format!("{}°", h.temp)),
                    icone: g.invoke_img(ss(h.icone)),
                    chuva: ss(format!("{}%", h.chuva)),
                })
                .collect();
            ui.set_cl_horas(ModelRc::new(VecModel::from(horas)));
            ui.set_cl_sem_min(p.dias.iter().map(|d| d.min).min().unwrap_or(0));
            ui.set_cl_sem_max(p.dias.iter().map(|d| d.max).max().unwrap_or(1));
            let dias: Vec<DiaUi> = p
                .dias
                .iter()
                .map(|d| DiaUi {
                    nome: ss(d.nome.clone()),
                    icone: g.invoke_img(ss(d.icone)),
                    max: d.max,
                    min: d.min,
                    chuva: ss(format!("{}%", d.chuva)),
                })
                .collect();
            ui.set_cl_dias(ModelRc::new(VecModel::from(dias)));
        });
    });
}

/// Busca de cidade (geocodificação do Open-Meteo) para o app Clima.
fn buscar_cidade(ui: &AppWindow, nome: String) {
    ui.set_tela(ss("clima-cidade"));
    ui.set_cid_busca(ss(nome.clone()));
    if nome.is_empty() {
        return;
    }
    ui.set_cid_mensagem(ss(i18n::t("buscando...")));
    let fraca = ui.as_weak();
    std::thread::spawn(move || {
        let r = clima::buscar_cidade(&nome);
        let _ = fraca.upgrade_in_event_loop(move |ui| match r {
            Ok(l) => {
                ui.set_cid_mensagem(ss(if l.is_empty() { i18n::t("nenhuma cidade encontrada") } else { String::new() }));
                let v: Vec<LugarUi> =
                    l.iter().map(|x| LugarUi { nome: ss(x.nome.clone()), detalhe: ss(x.detalhe.clone()) }).collect();
                ui.set_cid_resultados(ModelRc::new(VecModel::from(v)));
                LUGARES.with_borrow_mut(|g| *g = l);
            }
            Err(e) => ui.set_cid_mensagem(ss(i18n::tf("erro: {}", &[&curto(&e, 50)]))),
        });
    });
}

/// Preço curto para o cartão do Painel: "98,4 mil", "3.120", "0,52".
fn compacto(p: f64) -> String {
    if p >= 10_000.0 {
        i18n::tf("{} mil", &[&format!("{:.1}", p / 1000.0).replace('.', ",")])
    } else if p >= 100.0 {
        dados::milhar(p)
    } else {
        format!("{p:.2}").replace('.', ",")
    }
}

/// Painel (vitrine): clima e sistema a cada minuto, cripto a cada 5 min, modo noite.
fn ligar_painel(ui: &AppWindow) {
    let f = ui.as_weak();
    ui.on_acordar(move || {
        ACORDADO.set(chrono::Local::now().timestamp());
        f.upgrade().iter().for_each(|ui| ui.set_pn_noite(false));
    });
    let fraca = ui.as_weak();
    let n = std::cell::Cell::new(0u32);
    let atualizar = move || {
        let vez = n.get();
        n.set(vez + 1);
        let Some(ui) = fraca.upgrade() else { return };
        // noite: 23h às 6h, exceto 2 min depois de um toque
        let agora = chrono::Local::now();
        let h = agora.format("%H").to_string().parse::<u32>().unwrap_or(12);
        ui.set_pn_noite((h >= 23 || h < 6) && agora.timestamp() - ACORDADO.get() > 120);
        if ui.get_tela() != "painel" && vez > 0 {
            return;
        }
        let cripto_agora = vez % 5 == 0;
        let fraca = ui.as_weak();
        std::thread::spawn(move || {
            let sis = sistema::sistema();
            let cl = clima::clima();
            let moedas = if cripto_agora { cripto::mercado(&cripto::moeda()).ok() } else { None };
            let _ = fraca.upgrade_in_event_loop(move |ui| {
                ui.set_pn_tomada(sis.na_tomada);
                ui.set_pn_carregando(sis.carregando);
                ui.set_pn_temp_cpu(ss(sis.temp_cpu.map(|t| format!("{t}°")).unwrap_or_default()));
                match cl {
                    Ok(c) => {
                        ui.set_pn_erro(ss(""));
                        ui.set_pn_cidade(ss(c.cidade));
                        ui.set_pn_temp(ss(format!("{}°", c.temp)));
                        ui.set_pn_desc(ss(c.descricao));
                        ui.set_pn_maxmin(ss(format!("{}° / {}°", c.max, c.min)));
                        ui.set_pn_icone(ss(c.icone));
                        let g = ui.global::<IconesClima>();
                        let horas: Vec<HoraClima> = c
                            .proximas
                            .iter()
                            .map(|x| HoraClima {
                                hora: ss(x.hora.clone()),
                                temp: ss(format!("{}°", x.temp)),
                                icone: g.invoke_img(ss(x.icone)),
                            })
                            .collect();
                        ui.set_pn_proximas(ModelRc::new(VecModel::from(horas)));
                    }
                    Err(e) => ui.set_pn_erro(ss(e)),
                }
                if let Some(m) = moedas {
                    let v: Vec<MoedaMini> = m
                        .iter()
                        .take(3)
                        .map(|x| MoedaMini {
                            simbolo: ss(x.simbolo.to_uppercase()),
                            preco: ss(compacto(x.preco)),
                            variacao: ss(format!("{:+.1}%", x.variacao).replace('.', ",")),
                            subiu: x.variacao >= 0.0,
                        })
                        .collect();
                    ui.set_pn_moedas(ModelRc::new(VecModel::from(v)));
                }
            });
        });
    };
    atualizar();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(60), move || {
        if !crate::espera::ativa() {
            atualizar()
        }
    });
    std::mem::forget(t);
}

pub fn ligar(ui: &AppWindow) {
    let salvo = std::fs::read_to_string(TEMA_FILE).unwrap_or_default();
    let mut partes = salvo.split_whitespace();
    let escuro = partes.next() != Some("claro");
    ui.global::<Tema>().set_paleta(partes.next().and_then(|p| p.parse().ok()).unwrap_or(0));
    aplicar_tema(ui, escuro);
    ui.set_brilho_disponivel(brilho::disponivel());
    ui.set_brilho(brilho::ler());
    ui.on_brilho_mudar(|v| brilho::definir(v));
    ui.set_musica_configurada(api().configurado());
    let todos = todos_apps();
    let ids: Vec<String> = todos.iter().map(|a| a.id.to_string()).collect();
    ORDEM.with_borrow_mut(|o| *o = inicio::carregar(&ids));
    TODOS_APPS.with_borrow_mut(|t| *t = todos);
    aplicar_ordem(ui);
    let f = ui.as_weak();
    ui.on_org_alternar(move |i| {
        ORDEM.with_borrow_mut(|o| {
            inicio::alternar(o, i.max(0) as usize);
            inicio::salvar(o);
        });
        f.upgrade().iter().for_each(aplicar_ordem);
    });
    let f = ui.as_weak();
    ui.on_org_mover(move |i, d| {
        ORDEM.with_borrow_mut(|o| {
            inicio::mover(o, i.max(0) as usize, d);
            inicio::salvar(o);
        });
        f.upgrade().iter().for_each(aplicar_ordem);
    });
    ligar_painel(ui);
    let f = ui.as_weak();
    ui.on_mesh_digitar(move || f.upgrade().iter().for_each(|ui| abrir_teclado(ui, Modo::Mesh("mensagem".into()))));
    let f = ui.as_weak();
    ui.on_mesh_configurar(move || f.upgrade().iter().for_each(|ui| abrir_teclado(ui, Modo::Mesh("host".into()))));
    let f = ui.as_weak();
    ui.on_mesh_enviar(move || {
        let Some(ui) = f.upgrade() else { return };
        let texto = ui.get_mesh_rascunho().to_string();
        if texto.trim().is_empty() {
            return abrir_teclado(&ui, Modo::Mesh("mensagem".into()));
        }
        if let Some(tx) = MESH_TX.with_borrow(|t| t.clone()) {
            let _ = tx.send(mesh::Cmd::Enviar { texto, para: None, canal: 0 });
            ui.set_mesh_rascunho(ss(""));
        }
    });
    let f = ui.as_weak();
    let f2 = f.clone();
    ui.on_cam_trocar(move || {
        let Some(ui) = f2.upgrade() else { return };
        if ui.get_cam_ocupado() {
            return;
        }
        camera::trocar_lado();
        abrir_camera(&ui);
    });
    ui.on_cam_disparar(move || {
        let Some(ui) = f.upgrade() else { return };
        if !ui.get_cam_ativa() || ui.get_cam_ocupado() {
            return;
        }
        CAMERA.with_borrow(|c| {
            if let Some(c) = c {
                c.tirar_foto();
            }
        });
        ui.set_cam_ocupado(true);
        ui.set_cam_mensagem(ss(i18n::t("salvando a foto...")));
    });
    let f = ui.as_weak();
    ui.on_gal_abrir(move |i| f.upgrade().iter().for_each(|ui| galeria_abrir(ui, i)));
    let f = ui.as_weak();
    ui.on_gal_fechar(move || {
        if let Some(ui) = f.upgrade() {
            ui.set_gal_aberta(-1);
            ui.set_gal_foto(Image::default());
        }
    });
    let f = ui.as_weak();
    ui.on_gal_apagar(move |i| f.upgrade().iter().for_each(|ui| galeria_apagar(ui, i)));
    let f = ui.as_weak();
    ui.on_loja_atualizar(move || f.upgrade().iter().for_each(abrir_loja));
    let f = ui.as_weak();
    ui.on_loja_abrir(move |i| {
        let Some(ui) = f.upgrade() else { return };
        if let Some(a) = LOJA.with_borrow(|g| g.get(i.max(0) as usize).cloned()) {
            ui.set_loja_app(loja_ui(&a));
            ui.set_loja_app_msg(ss(""));
            ui.set_tela(ss("loja-app"));
        }
    });
    let f = ui.as_weak();
    ui.on_loja_instalar(move || f.upgrade().iter().for_each(|ui| loja_acao(ui, true)));
    let f = ui.as_weak();
    ui.on_loja_remover(move || f.upgrade().iter().for_each(|ui| loja_acao(ui, false)));
    let f = ui.as_weak();
    ui.on_cl_atualizar(move || f.upgrade().iter().for_each(abrir_clima));
    let f = ui.as_weak();
    ui.on_cid_digitar(move || f.upgrade().iter().for_each(|ui| abrir_teclado(ui, Modo::Cidade)));
    let f = ui.as_weak();
    ui.on_cid_escolher(move |i| {
        let Some(ui) = f.upgrade() else { return };
        if let Some(l) = LUGARES.with_borrow(|g| g.get(i.max(0) as usize).cloned()) {
            match clima::salvar_local(&l) {
                Ok(()) => abrir_clima(&ui),
                Err(e) => ui.set_cid_mensagem(ss(i18n::tf("não consegui salvar: {}", &[&curto(&e, 40)]))),
            }
        }
    });
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
    let f = ui.as_weak();
    ui.on_ajustes_tema(move |escuro| {
        if let Some(ui) = f.upgrade() {
            aplicar_tema(&ui, escuro);
        }
    });
    let f = ui.as_weak();
    const IDIOMAS: [&str; 3] = ["pt", "en", "es"];
    ui.set_idioma_atual(IDIOMAS.iter().position(|l| *l == i18n::idioma()).unwrap_or(0) as i32);
    ui.on_ajustes_idioma(|i| {
        let l = IDIOMAS.get(i.max(0) as usize).copied().unwrap_or("pt");
        if l != i18n::idioma() {
            i18n::definir(l);
            // os textos montados no Rust (apps, listas, avisos) são refeitos do zero
            crate::doom::reiniciar();
        }
    });
    ui.set_apagar_indice(crate::espera::apagar_indice());
    ui.on_ajustes_apagar(move |i| crate::espera::definir_apagar(i));
    ui.set_girar_auto(crate::espera::girar_auto());
    let fg = ui.as_weak();
    ui.on_ajustes_girar(move |g| {
        crate::espera::definir_girar(g);
        fg.upgrade().iter().for_each(|ui| ui.set_girar_auto(g));
    });
    ui.on_reiniciar(|| {
        let _ = std::process::Command::new("reboot").spawn();
    });
    ui.on_desligar(|| {
        let _ = std::process::Command::new("poweroff").spawn();
    });
    ui.on_ajustes_paleta(move |i| {
        if let Some(ui) = f.upgrade() {
            ui.global::<Tema>().set_paleta(i);
            ui.invoke_aplicar_tema();
            salvar_tema(&ui);
        }
    });
    ui.on_tg_abrir(|i| tg_cmd(telegram::Cmd::Abrir(i.max(0) as usize)));
    let f = ui.as_weak();
    ui.on_tg_ver_foto(move |i| {
        if let Some(ui) = f.upgrade() {
            // mostra já a versão do balão; a original chega depois (Evento::FotoGrande)
            let atual = TG_BOLHAS.with_borrow(|b| b.as_ref().and_then(|b| b.row_data(i as usize)));
            let Some(b) = atual else { return };
            if b.foto.size().width == 0 {
                return; // ainda não carregou
            }
            ui.set_tg_foto(b.foto.clone());
            ui.set_tg_foto_carregando(!b.video);
            ui.set_tela(ss("tg-foto"));
            if !b.video {
                tg_cmd(telegram::Cmd::FotoGrande(i.max(0) as usize));
            }
        }
    });
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
        if crate::espera::ativa() {
            return;
        }
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
                            ui.set_btc_variacao(ss(i18n::tf(
                                "{}% em 24 h",
                                &[&format!("{v:+.2}").replace('.', ",")],
                            )));
                            ui.set_btc_subiu(v >= 0.0);
                        }
                        ui.set_btc_info(ss(i18n::tf(
                            "atualizado às {}",
                            &[&chrono::Local::now().format("%H:%M").to_string()],
                        )));
                    }
                    Err(e) => ui.set_btc_info(ss(i18n::tf("erro: {}", &[&curto(&e, 40)]))),
                });
            });
        }
    };
    atualizar_btc();
    let a = atualizar_btc.clone();
    ui.on_atualizar_btc(move || a());
    let t = slint::Timer::default();
    // a API gratuita da CoinGecko limita as chamadas por minuto (o app Cripto também usa)
    t.start(slint::TimerMode::Repeated, Duration::from_secs(300), move || {
        if !crate::espera::ativa() {
            atualizar_btc()
        }
    });
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
    let f = ui.as_weak();
    ui.on_controle(move |c| {
        let Some(ui) = f.upgrade() else { return };
        match c.as_str() {
            "aleatorio" => {
                let ligar = !ui.get_toc_aleatorio();
                ui.set_toc_aleatorio(ligar);
                if ligar {
                    std::thread::spawn(|| {
                        if let Err(e) = api().embaralhar_fila() {
                            eprintln!("embaralhar: {e}");
                        }
                    });
                }
            }
            "repetir" => {
                let novo = match ui.get_toc_repetir().as_str() {
                    "nao" => "tudo",
                    "tudo" => "uma",
                    _ => "nao",
                };
                ui.set_toc_repetir(ss(novo));
                std::thread::spawn(move || musica::repetir(novo));
            }
            "favoritar" => {
                let sim = !ui.get_toc_favorita();
                ui.set_toc_favorita(sim);
                let id = EST.with_borrow(|e| e.ultimo_id.clone());
                std::thread::spawn(move || {
                    if let Err(e) = api().favoritar(&id, sim) {
                        eprintln!("favoritar: {e}");
                    }
                });
            }
            "artista" | "album" => {}
            outro => {
                let c = outro.to_string();
                std::thread::spawn(move || musica::controle(&c));
            }
        }
    });
    ui.on_volume_mudar(|v| {
        std::thread::spawn(move || {
            dados::mpv(serde_json::json!(["set_property", "volume", (v as f64 * 100.0).round()]));
        });
    });
    let f = ui.as_weak();
    ui.on_fila_tocar(move |i| {
        musica::pular_para(i.max(0) as usize);
        f.upgrade().iter().for_each(|ui| ir(ui, "tocando"));
    });
    let f = ui.as_weak();
    ui.on_fila_remover(move |i| {
        musica::remover_da_fila(i.max(0) as usize);
        f.upgrade().iter().for_each(carregar_fila);
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
                ui.set_wifi_ssid(ss(if w.ssid.is_empty() { i18n::t("desconectado") } else { w.ssid.clone() }));
                ui.set_wifi_ip(ss(if w.ip.is_empty() { w.estado.to_lowercase() } else { w.ip.clone() }));
                aplicar_bt(&ui, bt);
                aplicar_player(&ui, player);
            });
            n += 1;
            std::thread::sleep(Duration::from_secs(1));
            // tela apagada: status a cada 5 s (o Wi-Fi conta pelo n, então só espera)
            if crate::espera::ativa() {
                std::thread::sleep(Duration::from_secs(4));
            }
        }
    });
}
