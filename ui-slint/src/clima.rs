//! Clima: temperatura agora, máxima/mínima do dia e as próximas horas, pelo
//! Open-Meteo. A resposta crua fica em cache por 30 min (a rede é instável na rua).

use std::time::{Duration, SystemTime};

use serde_json::Value;

const CONF: &str = "/etc/revive/painel.conf";
const CACHE: &str = "/var/cache/revive/clima.json";
const VALIDADE: Duration = Duration::from_secs(30 * 60);

fn agente() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into()
}

/// lat, lon e cidade do painel; cidade pode faltar.
fn ler_conf() -> Option<(String, String, String)> {
    let mut lat = None;
    let mut lon = None;
    let mut cidade = String::new();
    for linha in std::fs::read_to_string(CONF).ok()?.lines() {
        let Some((k, v)) = linha.split_once('=') else { continue };
        match k.trim() {
            "lat" => lat = Some(v.trim().to_string()),
            "lon" => lon = Some(v.trim().to_string()),
            "cidade" => cidade = v.trim().to_string(),
            _ => {}
        }
    }
    Some((lat?, lon?, cidade))
}

/// Resposta crua da API (texto, para gravar no cache do jeito que veio).
fn buscar(lat: &str, lon: &str) -> Result<String, String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,weather_code,is_day\
         &hourly=temperature_2m,weather_code,is_day\
         &daily=temperature_2m_max,temperature_2m_min\
         &timezone=auto&forecast_days=2"
    );
    let bytes = agente()
        .get(&url)
        .header("User-Agent", "revive")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn gravar(texto: &str) {
    let _ = std::fs::create_dir_all("/var/cache/revive");
    let _ = std::fs::write(CACHE, texto);
}

/// O cache foi gravado há menos de 30 min?
fn cache_fresco() -> bool {
    std::fs::metadata(CACHE)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|idade| idade < VALIDADE)
}

#[derive(Clone)]
pub struct Clima {
    pub cidade: String,
    pub temp: i32,
    pub max: i32,
    pub min: i32,
    pub descricao: String,
    pub icone: &'static str,
    pub proximas: Vec<Hora>,
}

#[derive(Clone)]
pub struct Hora {
    pub hora: String, // "14h"
    pub temp: i32,
    pub icone: &'static str,
}

pub fn clima() -> Result<Clima, String> {
    let (lat, lon, cidade) = ler_conf().ok_or("configure /etc/revive/painel.conf (lat, lon, cidade)")?;
    // cache novo: nem toca na rede
    if cache_fresco() {
        if let Ok(texto) = std::fs::read_to_string(CACHE) {
            if let Ok(c) = montar(&cidade, &texto) {
                return Ok(c);
            }
        }
    }
    match buscar(&lat, &lon) {
        Ok(texto) => {
            gravar(&texto);
            montar(&cidade, &texto)
        }
        // sem rede: o cache velho ainda serve
        Err(e) => match std::fs::read_to_string(CACHE) {
            Ok(texto) => montar(&cidade, &texto),
            Err(_) => Err(e),
        },
    }
}

fn montar(cidade: &str, texto: &str) -> Result<Clima, String> {
    let v: Value = serde_json::from_str(texto).map_err(|e| e.to_string())?;
    let atual = &v["current"];
    let agora = atual["time"].as_str().ok_or("resposta inesperada")?;
    let codigo = atual["weather_code"].as_i64().unwrap_or(0);
    let dia = atual["is_day"].as_i64().unwrap_or(1) != 0;
    let previsao = &v["daily"];
    let horaria = &v["hourly"];

    // as 5 próximas horas cheias; horários ISO comparam bem como texto
    let horas = horaria["time"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let inicio = horas.iter().position(|t| t.as_str().is_some_and(|t| t > agora)).unwrap_or(0);
    let mut proximas = Vec::new();
    for i in inicio..horas.len() {
        if proximas.len() == 5 {
            break;
        }
        let Some(hora) = horas[i].as_str() else { continue };
        let Some(hh) = hora.get(11..13) else { continue };
        proximas.push(Hora {
            hora: format!("{hh}h"),
            temp: numero(&horaria["temperature_2m"], i).round() as i32,
            icone: icone(numero(&horaria["weather_code"], i) as i64, numero(&horaria["is_day"], i) != 0.0),
        });
    }

    Ok(Clima {
        cidade: cidade.to_string(),
        temp: atual["temperature_2m"].as_f64().unwrap_or(0.0).round() as i32,
        max: numero(&previsao["temperature_2m_max"], 0).round() as i32,
        min: numero(&previsao["temperature_2m_min"], 0).round() as i32,
        descricao: descricao(codigo).to_string(),
        icone: icone(codigo, dia),
        proximas,
    })
}

/// Valor numérico da posição `i` de uma lista (0 se faltar).
fn numero(lista: &Value, i: usize) -> f64 {
    lista.get(i).and_then(Value::as_f64).unwrap_or(0.0)
}

fn descricao(codigo: i64) -> &'static str {
    match codigo {
        0 => "Céu limpo",
        1 => "Poucas nuvens",
        2 => "Parcialmente nublado",
        3 => "Nublado",
        45 | 48 => "Neblina",
        51..=57 => "Garoa",
        61..=67 => "Chuva",
        71..=77 => "Neve",
        80..=82 => "Pancadas de chuva",
        85 | 86 => "Neve",
        95..=99 => "Trovoadas",
        _ => "—",
    }
}

fn icone(codigo: i64, dia: bool) -> &'static str {
    match codigo {
        0 | 1 if dia => "sunny",
        0 | 1 => "clear_night",
        2 if dia => "partly_cloudy_day",
        2 => "partly_cloudy_night",
        3 => "cloud",
        45 | 48 => "foggy",
        51..=67 | 80..=82 => "rainy",
        71..=77 | 85 | 86 => "weather_snowy",
        95..=99 => "thunderstorm",
        _ => "cloud",
    }
}
