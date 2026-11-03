//! Apps por manifesto: cada /etc/revive/apps/<nome>.json vira um app no lançador.
//!
//! O app busca uma URL (JSON) e mostra o resultado na lista da interface, sem
//! recompilar nada. Dois tipos:
//!
//! - "lista": um item por elemento de um array ("itens" é o caminho até o array;
//!   "titulo" e "sub" são caminhos dentro de cada elemento).
//! - "valores": cartões fixos, cada um com um rótulo e o caminho do valor.
//!
//! Caminhos usam pontos e índices: "data.monitors", "0.state", "attributes.unit".
//! Exemplos em ui-slint/apps/.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

const DIR: &str = "/etc/revive/apps";

pub struct Manifesto {
    pub id: String,
    pub nome: String,
    pub icone: String,
    pub cor: (u8, u8, u8),
    tipo: String,
    url: String,
    headers: Vec<(String, String)>,
    itens: String,
    titulo: String,
    sub: String,
    verde_campo: String,
    verde_igual: Value,
    valores: Vec<(String, String)>,
}

fn txt(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn cor(s: &str) -> (u8, u8, u8) {
    let h = s.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("40"), 16).unwrap_or(0x40);
    if h.len() == 6 { (p(0), p(2), p(4)) } else { (45, 52, 64) }
}

impl Manifesto {
    fn de_json(v: &Value) -> Option<Manifesto> {
        let id = txt(v, "id");
        let url = txt(&v["fonte"], "url");
        if id.is_empty() || url.is_empty() {
            return None;
        }
        let headers = v["fonte"]["headers"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
            .unwrap_or_default();
        let valores = v["valores"]
            .as_array()
            .map(|a| a.iter().map(|x| (txt(x, "rotulo"), txt(x, "caminho"))).collect())
            .unwrap_or_default();
        Some(Manifesto {
            nome: Some(txt(v, "nome")).filter(|s| !s.is_empty()).unwrap_or_else(|| id.clone()),
            icone: Some(txt(v, "icone")).filter(|s| !s.is_empty()).unwrap_or_else(|| "★".into()),
            cor: cor(&txt(v, "cor")),
            tipo: Some(txt(v, "tipo")).filter(|s| !s.is_empty()).unwrap_or_else(|| "lista".into()),
            url,
            headers,
            itens: txt(v, "itens"),
            titulo: txt(v, "titulo"),
            sub: txt(v, "sub"),
            verde_campo: txt(&v["verde_se"], "campo"),
            verde_igual: v["verde_se"]["igual"].clone(),
            valores,
            id,
        })
    }

    /// Busca a URL e devolve (título, subtítulo, destaque verde) por item.
    pub fn buscar(&self) -> Result<Vec<(String, String, bool)>, String> {
        let agente: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into();
        let mut req = agente.get(&self.url).header("User-Agent", "revive");
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let dados: Value = req
            .call()
            .map_err(|e| e.to_string())?
            .body_mut()
            .read_json()
            .map_err(|e| e.to_string())?;
        if self.tipo == "valores" {
            return Ok(self
                .valores
                .iter()
                .map(|(rotulo, c)| (rotulo.clone(), texto(&caminho(&dados, c)), false))
                .collect());
        }
        let lista = caminho(&dados, &self.itens);
        let elementos: Vec<Value> = match lista {
            Value::Array(a) => a,
            Value::Object(o) => o.into_values().collect(), // mapas (id -> objeto) também valem
            outro => vec![outro],
        };
        Ok(elementos
            .iter()
            .map(|e| {
                let titulo = if self.titulo.is_empty() { texto(e) } else { texto(&caminho(e, &self.titulo)) };
                let sub = if self.sub.is_empty() { String::new() } else { texto(&caminho(e, &self.sub)) };
                let verde = !self.verde_campo.is_empty() && caminho(e, &self.verde_campo) == self.verde_igual;
                (titulo, sub, verde)
            })
            .collect())
    }
}

/// "a.b.0.c" dentro de um JSON (vazio = o próprio valor).
fn caminho(v: &Value, c: &str) -> Value {
    let mut atual = v;
    for parte in c.split('.').filter(|p| !p.is_empty()) {
        atual = match (atual, parte.parse::<usize>()) {
            (Value::Array(a), Ok(i)) => a.get(i).unwrap_or(&Value::Null),
            _ => atual.get(parte).unwrap_or(&Value::Null),
        };
    }
    atual.clone()
}

fn texto(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "—".into(),
        outro => outro.to_string(),
    }
}

/// Manifestos de /etc/revive/apps, lidos uma vez (ordem alfabética do arquivo).
pub fn todos() -> &'static [Manifesto] {
    static M: OnceLock<Vec<Manifesto>> = OnceLock::new();
    M.get_or_init(|| {
        let mut arquivos: Vec<_> = std::fs::read_dir(DIR)
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
            .unwrap_or_default();
        arquivos.sort();
        arquivos
            .iter()
            .filter_map(|p| {
                let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?)
                    .map_err(|e| eprintln!("app {}: JSON inválido: {e}", p.display()))
                    .ok()?;
                let m = Manifesto::de_json(&v);
                if m.is_none() {
                    eprintln!("app {}: faltam \"id\" ou \"fonte.url\"", p.display());
                }
                m
            })
            .collect()
    })
}

pub fn achar(id: &str) -> Option<&'static Manifesto> {
    todos().iter().find(|m| m.id == id)
}
