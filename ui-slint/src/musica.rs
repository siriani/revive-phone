//! Música do Navidrome (API Subsonic) tocada pelo mpv do serviço revive-mpv.
//!
//! A fila (dados das faixas) fica em /run/revive-queue.json: o mpv só conhece as
//! URLs, e a música continua tocando se a interface reiniciar.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde_json::{json, Value};

use crate::dados::{mpv, mpv_prop};

const CONF: &str = "/etc/revive/navidrome.conf";
const FILA: &str = "/run/revive-queue.json";

pub struct Subsonic {
    urls: Vec<String>,
    user: String,
    pass: String,
    base: Mutex<Option<String>>,
    agente: ureq::Agent,
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Subsonic {
    pub fn novo() -> Subsonic {
        let mut urls = vec![];
        let (mut user, mut pass) = (String::new(), String::new());
        for linha in std::fs::read_to_string(CONF).unwrap_or_default().lines() {
            let Some((k, v)) = linha.split_once('=') else { continue };
            let v = v.trim().to_string();
            match k.trim() {
                "urls" => urls = v.split_whitespace().map(String::from).collect(),
                "user" => user = v,
                "pass" => pass = v,
                _ => {}
            }
        }
        let agente = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        Subsonic { urls, user, pass, base: Mutex::new(None), agente }
    }

    /// Cliente com dados digitados na tela de configuração (antes de salvar).
    pub fn com(url: &str, user: &str, pass: &str) -> Subsonic {
        let mut s = Subsonic::novo();
        s.urls = url.split_whitespace().map(|u| u.trim_end_matches('/').to_string()).collect();
        s.user = user.to_string();
        s.pass = pass.to_string();
        *s.base.lock().unwrap() = None;
        s
    }

    /// Testa a conexão e o login (ping autenticado).
    pub fn testar(&self) -> Result<(), String> {
        self.get("ping", &[]).map(|_| ())
    }

    /// Grava /etc/revive/navidrome.conf (só root lê: tem a senha).
    pub fn salvar(&self) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        let txt = format!("[navidrome]\nurls = {}\nuser = {}\npass = {}\n", self.urls.join(" "), self.user, self.pass);
        let tmp = format!("{CONF}.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(txt.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, CONF).map_err(|e| e.to_string())
    }

    pub fn dados(&self) -> (String, String, String) {
        // todos os endereços, separados por espaço: o primeiro que responder é usado
        (self.urls.join(" "), self.user.clone(), self.pass.clone())
    }

    pub fn configurado(&self) -> bool {
        !self.urls.is_empty() && !self.user.is_empty()
    }

    fn auth(&self) -> String {
        let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let sal = format!("{:x}", n ^ 0x5bd1e995);
        let token = format!("{:x}", Md5::digest(format!("{}{}", self.pass, sal)));
        format!("u={}&t={token}&s={sal}&v=1.16.1&c=revive&f=json", enc(&self.user))
    }

    /// Em casa usa o endereço da rede local; fora, o público (o primeiro que responder).
    fn base(&self) -> Result<String, String> {
        if let Some(b) = self.base.lock().unwrap().clone() {
            return Ok(b);
        }
        for url in &self.urls {
            let r = self.agente.get(format!("{url}/rest/ping.view?{}", self.auth())).call();
            if let Ok(mut r) = r {
                if let Ok(v) = r.body_mut().read_json::<Value>() {
                    if v["subsonic-response"]["status"] == "ok" {
                        *self.base.lock().unwrap() = Some(url.clone());
                        return Ok(url.clone());
                    }
                }
            }
        }
        Err("Navidrome fora do ar".into())
    }

    pub fn url(&self, metodo: &str, params: &[(&str, &str)]) -> Result<String, String> {
        let extra: String = params.iter().map(|(k, v)| format!("&{k}={}", enc(v))).collect();
        Ok(format!("{}/rest/{metodo}.view?{}{extra}", self.base()?, self.auth()))
    }

    pub fn get(&self, metodo: &str, params: &[(&str, &str)]) -> Result<Value, String> {
        for tentativa in 0..2 {
            let r = self.url(metodo, params).and_then(|u| {
                self.agente
                    .get(u)
                    .call()
                    .map_err(|e| e.to_string())?
                    .body_mut()
                    .read_json::<Value>()
                    .map_err(|e| e.to_string())
            });
            match r {
                Ok(v) if v["subsonic-response"]["status"] == "ok" => return Ok(v["subsonic-response"].clone()),
                Ok(v) => return Err(v["subsonic-response"]["error"]["message"].as_str().unwrap_or("erro").into()),
                Err(e) if tentativa == 0 => {
                    eprintln!("musica: {metodo}: {e}; trocando de servidor");
                    *self.base.lock().unwrap() = None; // mudou de rede
                }
                Err(e) => return Err(e),
            }
        }
        Err("sem resposta".into())
    }

    fn lista(v: &Value) -> Vec<Value> {
        v.as_array().cloned().unwrap_or_default()
    }

    pub fn artistas(&self) -> Result<Vec<Value>, String> {
        let r = self.get("getArtists", &[])?;
        Ok(Self::lista(&r["artists"]["index"]).iter().flat_map(|i| Self::lista(&i["artist"])).collect())
    }
    pub fn albuns_do_artista(&self, id: &str) -> Result<Vec<Value>, String> {
        Ok(Self::lista(&self.get("getArtist", &[("id", id)])?["artist"]["album"]))
    }
    pub fn album(&self, id: &str) -> Result<Vec<Value>, String> {
        Ok(Self::lista(&self.get("getAlbum", &[("id", id)])?["album"]["song"]))
    }
    pub fn lista_albuns(&self, tipo: &str) -> Result<Vec<Value>, String> {
        Ok(Self::lista(&self.get("getAlbumList2", &[("type", tipo), ("size", "40")])?["albumList2"]["album"]))
    }
    pub fn aleatorias(&self) -> Result<Vec<Value>, String> {
        Ok(Self::lista(&self.get("getRandomSongs", &[("size", "50")])?["randomSongs"]["song"]))
    }
    pub fn playlists(&self) -> Result<Vec<Value>, String> {
        Ok(Self::lista(&self.get("getPlaylists", &[])?["playlists"]["playlist"]))
    }
    pub fn playlist(&self, id: &str) -> Result<Vec<Value>, String> {
        Ok(Self::lista(&self.get("getPlaylist", &[("id", id)])?["playlist"]["entry"]))
    }
    pub fn buscar(&self, texto: &str) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>), String> {
        let r = self.get("search3", &[("query", texto), ("artistCount", "10"), ("albumCount", "10"), ("songCount", "30")])?;
        let s = &r["searchResult3"];
        Ok((Self::lista(&s["artist"]), Self::lista(&s["album"]), Self::lista(&s["song"])))
    }
    pub fn capa(&self, id: &str, tamanho: u32) -> Result<Vec<u8>, String> {
        let url = self.url("getCoverArt", &[("id", id), ("size", &tamanho.to_string())])?;
        self.agente
            .get(url)
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| e.to_string())
    }
    /// Capa reduzida e guardada em disco (/var/cache/revive/capas/<id>-<tamanho>.jpg):
    /// a segunda vez abre sem rede. Devolve RGBA pronto para a tela.
    pub fn capa_cache(&self, id: &str, tamanho: u32) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
        let dir = "/var/cache/revive/capas";
        let nome: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        let arq = format!("{dir}/{nome}-{tamanho}.jpg");
        let bytes = match std::fs::read(&arq) {
            Ok(b) => b,
            Err(_) => {
                let b = self.capa(id, tamanho).ok()?;
                let _ = std::fs::create_dir_all(dir);
                let _ = std::fs::write(format!("{arq}.tmp"), &b).and_then(|_| std::fs::rename(format!("{arq}.tmp"), &arq));
                b
            }
        };
        let img = image::load_from_memory(&bytes).ok()?;
        let img = if img.width() > tamanho || img.height() > tamanho {
            img.resize(tamanho, tamanho, image::imageops::FilterType::Triangle)
        } else {
            img
        }
        .to_rgba8();
        let (w, h) = img.dimensions();
        Some(slint::SharedPixelBuffer::clone_from_slice(img.as_raw(), w, h))
    }

    pub fn scrobble(&self, id: &str, terminou: bool) {
        let _ = self.get("scrobble", &[("id", id), ("submission", if terminou { "true" } else { "false" })]);
    }

    /// Troca a fila do mpv por estas faixas, começando da escolhida.
    pub fn tocar(&self, faixas: &[Value], inicio: usize) -> Result<(), String> {
        if faixas.is_empty() {
            return Ok(());
        }
        let inicio = inicio.min(faixas.len() - 1);
        let fila: Vec<Value> = faixas[inicio..].iter().chain(&faixas[..inicio]).cloned().collect();
        let urls: Vec<String> = fila
            .iter()
            .map(|f| self.url("stream", &[("id", f["id"].as_str().unwrap_or(""))]))
            .collect::<Result<_, _>>()?;
        std::fs::write(FILA, serde_json::to_string(&fila).unwrap_or_default()).map_err(|e| e.to_string())?;
        mpv(json!(["playlist-clear"]));
        for (i, u) in urls.iter().enumerate() {
            mpv(json!(["loadfile", u, if i == 0 { "replace" } else { "append" }]));
        }
        mpv(json!(["set_property", "pause", false]));
        Ok(())
    }
}

// ------------------------------------------------------------------ player

#[derive(Clone, Default, PartialEq)]
pub struct Estado {
    pub id: String,
    pub capa: String,
    pub titulo: String,
    pub artista: String,
    pub album: String,
    pub t: f64,
    pub dur: f64,
    pub pausa: bool,
    pub pos: usize,
    pub total: usize,
    pub volume: f64,
}

pub fn estado() -> Option<Estado> {
    let pos = mpv_prop("playlist-pos")?.as_i64()?;
    if pos < 0 {
        return None;
    }
    let fila: Value = serde_json::from_str(&std::fs::read_to_string(FILA).ok()?).ok()?;
    let total = fila.as_array()?.len();
    let f = fila.get(pos as usize)?;
    Some(Estado {
        id: f["id"].as_str().unwrap_or("").into(),
        capa: f["coverArt"].as_str().unwrap_or("").into(),
        titulo: f["title"].as_str().unwrap_or("").into(),
        artista: f["artist"].as_str().unwrap_or("").into(),
        album: f["album"].as_str().unwrap_or("").into(),
        t: mpv_prop("time-pos").and_then(|v| v.as_f64()).unwrap_or(0.0),
        dur: mpv_prop("duration").and_then(|v| v.as_f64()).or(f["duration"].as_f64()).unwrap_or(0.0),
        pausa: mpv_prop("pause").and_then(|v| v.as_bool()).unwrap_or(false),
        pos: pos as usize,
        total,
        volume: mpv_prop("volume").and_then(|v| v.as_f64()).unwrap_or(80.0),
    })
}

pub fn controle(c: &str) {
    match c {
        "pausar" => {
            mpv(json!(["cycle", "pause"]));
        }
        "proxima" => {
            mpv(json!(["playlist-next", "force"]));
        }
        "anterior" => {
            let t = mpv_prop("time-pos").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if t > 5.0 {
                mpv(json!(["seek", 0, "absolute"]));
            } else {
                mpv(json!(["playlist-prev", "force"]));
            }
        }
        "vol+" | "vol-" => {
            let v = mpv_prop("volume").and_then(|v| v.as_f64()).unwrap_or(80.0);
            let novo = (v + if c == "vol+" { 10.0 } else { -10.0 }).clamp(0.0, 130.0);
            mpv(json!(["set_property", "volume", novo]));
        }
        _ => {}
    }
}

pub fn pular(fracao: f32) {
    if let Some(d) = mpv_prop("duration").and_then(|v| v.as_f64()) {
        mpv(json!(["seek", d * fracao as f64, "absolute"]));
    }
}

pub fn mmss(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
