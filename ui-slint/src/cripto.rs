//! App Cripto: mosaico das maiores moedas (verde/vermelho pela variação de 24 h,
//! com o ícone de cada uma) e gráfico de velas das últimas 24 h. Dados da API
//! pública da CoinGecko.

use std::time::Duration;

use serde_json::Value;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::i18n;

const API: &str = "https://api.coingecko.com/api/v3";

fn agente() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into()
}

fn json(url: &str) -> Result<Value, String> {
    agente()
        .get(url)
        .header("User-Agent", "revive")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(429) => i18n::t("a CoinGecko pediu uma pausa; tente em 1 minuto"),
            e => e.to_string(),
        })?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())
}

#[derive(Clone)]
pub struct Moeda {
    pub id: String,
    pub simbolo: String,
    pub nome: String,
    pub preco: f64,
    pub variacao: f64,
    pub icone_url: String,
}

/// Moeda de exibição ("brl" ou "usd"), escolhida no app e lembrada entre reinícios.
const MOEDA_FILE: &str = "/var/lib/revive/cripto-moeda";

pub fn moeda() -> String {
    match std::fs::read_to_string(MOEDA_FILE).map(|s| s.trim().to_string()).as_deref() {
        Ok("usd") => "usd".into(),
        _ => "brl".into(),
    }
}

pub fn trocar_moeda() -> String {
    let nova = if moeda() == "usd" { "brl" } else { "usd" };
    let _ = std::fs::write(MOEDA_FILE, nova);
    nova.into()
}

pub fn mercado(moeda: &str) -> Result<Vec<Moeda>, String> {
    let v = json(&format!("{API}/coins/markets?vs_currency={moeda}&per_page=8&price_change_percentage=24h"))?;
    Ok(v.as_array()
        .ok_or_else(|| i18n::t("resposta inesperada"))?
        .iter()
        .map(|m| Moeda {
            id: m["id"].as_str().unwrap_or("").into(),
            simbolo: m["symbol"].as_str().unwrap_or("").to_uppercase(),
            nome: m["name"].as_str().unwrap_or("").into(),
            preco: m["current_price"].as_f64().unwrap_or(0.0),
            variacao: m["price_change_percentage_24h"].as_f64().unwrap_or(0.0),
            icone_url: m["image"].as_str().unwrap_or("").into(),
        })
        .collect())
}

/// Ícone da moeda reduzido para 64x64 (decodificado fora da thread da interface).
pub fn icone(url: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let bytes = agente()
        .get(url)
        .call()
        .ok()?
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    let img = image::load_from_memory(&bytes)
        .ok()?
        .resize(64, 64, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some(SharedPixelBuffer::clone_from_slice(img.as_raw(), w, h))
}

/// Uma vela já em coordenadas 0..1 do gráfico (y cresce para baixo).
#[derive(Clone, Default)]
pub struct VelaN {
    pub x: f32,
    pub topo: f32,
    pub base: f32,
    pub corpo_topo: f32,
    pub corpo_base: f32,
    pub subiu: bool,
}

pub struct Grafico {
    pub velas: Vec<VelaN>,
    pub maximo: f64,
    pub minimo: f64,
    pub abertura: f64,
    pub fechamento: f64,
}

/// Velas de 30 min das últimas 24 h ([tempo, abertura, máxima, mínima, fechamento]).
pub fn grafico(id: &str, moeda: &str) -> Result<Grafico, String> {
    let v = json(&format!("{API}/coins/{id}/ohlc?vs_currency={moeda}&days=1"))?;
    let velas: Vec<[f64; 4]> = v
        .as_array()
        .ok_or_else(|| i18n::t("sem dados"))?
        .iter()
        .filter_map(|c| {
            let c = c.as_array()?;
            Some([c.get(1)?.as_f64()?, c.get(2)?.as_f64()?, c.get(3)?.as_f64()?, c.get(4)?.as_f64()?])
        })
        .collect();
    if velas.is_empty() {
        return Err(i18n::t("sem velas"));
    }
    let maximo = velas.iter().map(|c| c[1]).fold(f64::MIN, f64::max);
    let minimo = velas.iter().map(|c| c[2]).fold(f64::MAX, f64::min);
    let faixa = (maximo - minimo).max(f64::EPSILON);
    let y = |p: f64| (1.0 - (p - minimo) / faixa) as f32 * 0.94 + 0.03; // margem em cima e embaixo
    let n = velas.len() as f32;
    let normal = velas
        .iter()
        .enumerate()
        .map(|(i, &[a, h, l, f])| VelaN {
            x: (i as f32 + 0.5) / n,
            topo: y(h),
            base: y(l),
            corpo_topo: y(a.max(f)),
            corpo_base: y(a.min(f)),
            subiu: f >= a,
        })
        .collect();
    Ok(Grafico {
        velas: normal,
        maximo,
        minimo,
        abertura: velas[0][0],
        fechamento: velas[velas.len() - 1][3],
    })
}
