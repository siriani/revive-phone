//! Estado da interface e ligação das telas (Slint) com os dados.
//!
//! O estado mora numa thread-local da thread da interface; buscas de rede rodam em
//! outras threads e voltam por `upgrade_in_event_loop`.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;
use slint::{Color, ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::dados;
use crate::musica::{self, Subsonic};
use crate::{AppWindow, Aparelho, Item, Rede, Tecla};

const POR_PAGINA: usize = 8;

#[derive(Clone)]
enum Acao {
    Tocar(Arc<Vec<Value>>, usize),
    Aleatorio(Arc<Vec<Value>>),
    Album(Value),
    Artista(Value),
    Playlist(Value),
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
}

thread_local! {
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
    });
}

fn api() -> &'static Subsonic {
    static API: OnceLock<Subsonic> = OnceLock::new();
    API.get_or_init(Subsonic::novo)
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
        let r = carregar(api());
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
        let senha = matches!(e.modo, Modo::Senha(_));
        let confirmar = if senha { "Conectar" } else { "Buscar" };
        let mostrado = if senha && e.ocultar { "•".repeat(e.texto.chars().count()) } else { e.texto.clone() };
        let ini = mostrado.chars().count().saturating_sub(30);
        ui.set_teclado_texto(ss(mostrado.chars().skip(ini).collect::<String>()));
        ui.set_teclado_vazio(ss(match &e.modo {
            Modo::Senha(s) => format!("senha de {}", curto(s, 18)),
            Modo::Busca => "buscar música".into(),
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
    let (modo_senha, texto) = EST.with_borrow(|e| {
        (match &e.modo { Modo::Senha(s) => Some(s.clone()), Modo::Busca => None }, e.texto.clone())
    });
    match modo_senha {
        Some(ssid) => conectar_wifi(ui, ssid, texto),
        None => buscar(ui, texto.trim().to_string()),
    }
}

fn cancelar_teclado(ui: &AppWindow) {
    let senha = EST.with_borrow(|e| matches!(e.modo, Modo::Senha(_)));
    if senha {
        ir(ui, "wifi");
    } else {
        ui.set_tela(ss("musica"));
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
        "Conectar" | "Buscar" => return confirmar(ui),
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

pub fn ligar(ui: &AppWindow) {
    ui.set_musica_configurada(api().configurado());

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
    t.start(slint::TimerMode::Repeated, Duration::from_secs(60), atualizar_btc);
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
