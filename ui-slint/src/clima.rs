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

// ---------------------------------------------------------------------------
// Previsão completa (7 dias + 24 horas), busca de cidade e troca de local.
// ---------------------------------------------------------------------------

const CACHE_COMPLETO: &str = "/var/cache/revive/clima-completo.json";

#[derive(Clone)]
pub struct Previsao {
    pub cidade: String,
    pub temp: i32,
    pub sensacao: i32,
    pub umidade: i32,
    pub vento: i32, // km/h
    pub descricao: String,
    pub icone: &'static str,
    pub chuva_hoje: i32, // %
    pub uv: i32,
    pub nascer: String, // "06:12"
    pub por: String,
    pub horas: Vec<HoraPrev>,
    pub dias: Vec<Dia>,
}

#[derive(Clone)]
pub struct HoraPrev {
    pub hora: String, // "14h" ou "Agora" na primeira
    pub temp: i32,
    pub icone: &'static str,
    pub chuva: i32,
}

#[derive(Clone)]
pub struct Dia {
    pub nome: String, // "Hoje", "Amanhã", "Qua"...
    pub icone: &'static str,
    pub max: i32,
    pub min: i32,
    pub chuva: i32,
}

#[derive(Clone)]
pub struct Lugar {
    pub nome: String,
    pub detalhe: String, // "São Paulo, Brasil"
    pub lat: f64,
    pub lon: f64,
}

/// GET simples com o User-Agent do painel.
fn baixar(url: &str) -> Result<String, String> {
    let bytes = agente()
        .get(url)
        .header("User-Agent", "revive")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Grava um cache qualquer criando a pasta.
fn gravar_em(caminho: &str, texto: &str) {
    let _ = std::fs::create_dir_all("/var/cache/revive");
    let _ = std::fs::write(caminho, texto);
}

/// O arquivo de cache foi gravado há menos de 30 min?
fn fresco(caminho: &str) -> bool {
    std::fs::metadata(caminho)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|idade| idade < VALIDADE)
}

fn buscar_completo(lat: &str, lon: &str) -> Result<String, String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,weather_code,is_day\
         &hourly=temperature_2m,weather_code,is_day,precipitation_probability\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,uv_index_max,sunrise,sunset\
         &timezone=auto&forecast_days=7"
    );
    baixar(&url)
}

/// Temperatura, sensação, 24 horas e 7 dias; mesmo cache de 30 min do resumo.
pub fn previsao() -> Result<Previsao, String> {
    let (lat, lon, cidade) = ler_conf().ok_or("configure /etc/revive/painel.conf (lat, lon, cidade)")?;
    if fresco(CACHE_COMPLETO) {
        if let Ok(texto) = std::fs::read_to_string(CACHE_COMPLETO) {
            if let Ok(p) = montar_previsao(&cidade, &texto) {
                return Ok(p);
            }
        }
    }
    match buscar_completo(&lat, &lon) {
        Ok(texto) => {
            gravar_em(CACHE_COMPLETO, &texto);
            montar_previsao(&cidade, &texto)
        }
        // sem rede: o cache velho ainda serve
        Err(e) => match std::fs::read_to_string(CACHE_COMPLETO) {
            Ok(texto) => montar_previsao(&cidade, &texto),
            Err(_) => Err(e),
        },
    }
}

/// Índice da primeira hora cheia igual ou depois da hora atual.
fn inicio_horas(horas: &[Value], agora: &str) -> usize {
    let Some(chave) = agora.get(0..13) else { return 0 };
    horas
        .iter()
        .position(|t| t.as_str().and_then(|t| t.get(0..13)).is_some_and(|h| h >= chave))
        .unwrap_or(0)
}

/// "2026-11-03T06:12" -> "06:12".
fn hora_curta(iso: &str) -> String {
    iso.get(11..16).unwrap_or("").to_string()
}

/// Abreviação pt-BR do dia da semana; "" se a data for inválida.
fn dia_semana(data: &str) -> &'static str {
    use chrono::Datelike;
    let Ok(d) = chrono::NaiveDate::parse_from_str(data, "%Y-%m-%d") else {
        return "";
    };
    match d.weekday().num_days_from_monday() {
        0 => "Seg",
        1 => "Ter",
        2 => "Qua",
        3 => "Qui",
        4 => "Sex",
        5 => "Sáb",
        _ => "Dom",
    }
}

fn nome_dia(i: usize, data: &str) -> String {
    match i {
        0 => "Hoje".to_string(),
        1 => "Amanhã".to_string(),
        _ => dia_semana(data).to_string(),
    }
}

fn montar_previsao(cidade: &str, texto: &str) -> Result<Previsao, String> {
    let v: Value = serde_json::from_str(texto).map_err(|e| e.to_string())?;
    let atual = &v["current"];
    let agora = atual["time"].as_str().ok_or("resposta inesperada")?;
    let codigo = atual["weather_code"].as_i64().unwrap_or(0);
    let dia = atual["is_day"].as_i64().unwrap_or(1) != 0;
    let horaria = &v["hourly"];
    let diaria = &v["daily"];

    // 24 horas a partir da hora atual; a primeira é "Agora"
    let horas_iso = horaria["time"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let inicio = inicio_horas(horas_iso, agora);
    let mut horas = Vec::new();
    for i in inicio..horas_iso.len().min(inicio + 24) {
        let Some(iso) = horas_iso[i].as_str() else { continue };
        let primeira = horas.is_empty();
        let hh = iso.get(11..13).unwrap_or("");
        let claro = horaria["is_day"].get(i).and_then(Value::as_f64).unwrap_or(1.0) != 0.0;
        horas.push(HoraPrev {
            hora: if primeira { "Agora".to_string() } else { format!("{hh}h") },
            temp: numero(&horaria["temperature_2m"], i).round() as i32,
            icone: icone(numero(&horaria["weather_code"], i) as i64, claro),
            chuva: numero(&horaria["precipitation_probability"], i).round() as i32,
        });
    }

    // 7 dias; o ícone é sempre a versão de dia
    let dias_iso = diaria["time"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut dias = Vec::new();
    for i in 0..dias_iso.len().min(7) {
        let data = dias_iso[i].as_str().unwrap_or("");
        dias.push(Dia {
            nome: nome_dia(i, data),
            icone: icone(numero(&diaria["weather_code"], i) as i64, true),
            max: numero(&diaria["temperature_2m_max"], i).round() as i32,
            min: numero(&diaria["temperature_2m_min"], i).round() as i32,
            chuva: numero(&diaria["precipitation_probability_max"], i).round() as i32,
        });
    }

    Ok(Previsao {
        cidade: cidade.to_string(),
        temp: atual["temperature_2m"].as_f64().unwrap_or(0.0).round() as i32,
        sensacao: atual["apparent_temperature"].as_f64().unwrap_or(0.0).round() as i32,
        umidade: atual["relative_humidity_2m"].as_f64().unwrap_or(0.0).round() as i32,
        vento: atual["wind_speed_10m"].as_f64().unwrap_or(0.0).round() as i32,
        descricao: descricao(codigo).to_string(),
        icone: icone(codigo, dia),
        chuva_hoje: numero(&diaria["precipitation_probability_max"], 0).round() as i32,
        uv: numero(&diaria["uv_index_max"], 0).round() as i32,
        nascer: hora_curta(diaria["sunrise"].get(0).and_then(Value::as_str).unwrap_or("")),
        por: hora_curta(diaria["sunset"].get(0).and_then(Value::as_str).unwrap_or("")),
        horas,
        dias,
    })
}

/// Percent-encoding manual: mantém [A-Za-z0-9-_.~] e escapa os bytes UTF-8.
fn pct(texto: &str) -> String {
    let mut saida = String::with_capacity(texto.len());
    for b in texto.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                saida.push(b as char)
            }
            _ => saida.push_str(&format!("%{b:02X}")),
        }
    }
    saida
}

/// Busca cidades no geocoding do Open-Meteo; lista vazia se nada casar.
pub fn buscar_cidade(nome: &str) -> Result<Vec<Lugar>, String> {
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=8&language=pt&format=json",
        pct(nome.trim())
    );
    let texto = baixar(&url)?;
    let v: Value = serde_json::from_str(&texto).map_err(|e| e.to_string())?;
    let Some(resultados) = v["results"].as_array() else {
        return Ok(Vec::new());
    };
    Ok(resultados
        .iter()
        .filter_map(|r| {
            let nome = r["name"].as_str()?.to_string();
            let detalhe = [r["admin1"].as_str(), r["country"].as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            Some(Lugar {
                nome,
                detalhe,
                lat: r["latitude"].as_f64().unwrap_or(0.0),
                lon: r["longitude"].as_f64().unwrap_or(0.0),
            })
        })
        .collect())
}

/// Grava lat/lon/cidade no painel e invalida os caches.
pub fn salvar_local(l: &Lugar) -> Result<(), String> {
    let atual = std::fs::read_to_string(CONF).unwrap_or_default();
    let mut saida = String::new();
    for linha in atual.lines() {
        let chave = linha.split_once('=').map(|(k, _)| k.trim()).unwrap_or("");
        if matches!(chave, "lat" | "lon" | "cidade") {
            continue;
        }
        saida.push_str(linha);
        saida.push('\n');
    }
    saida.push_str(&format!("lat={}\nlon={}\ncidade={}\n", l.lat, l.lon, l.nome));

    std::fs::create_dir_all("/etc/revive").map_err(|e| e.to_string())?;
    std::fs::write(CONF, saida).map_err(|e| e.to_string())?;
    // força nova busca no próximo acesso
    let _ = std::fs::remove_file(CACHE);
    let _ = std::fs::remove_file(CACHE_COMPLETO);
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn percent_encoding() {
        assert_eq!(pct("São Paulo"), "S%C3%A3o%20Paulo");
        assert_eq!(pct("Rio de Janeiro"), "Rio%20de%20Janeiro");
        assert_eq!(pct("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(pct("ç"), "%C3%A7");
        assert_eq!(pct(""), "");
    }

    #[test]
    fn nome_dia_semana() {
        assert_eq!(dia_semana("2026-11-03"), "Ter");
        assert_eq!(dia_semana("2026-11-02"), "Seg");
        assert_eq!(dia_semana("2026-11-07"), "Sáb");
        assert_eq!(dia_semana("2026-11-08"), "Dom");
        assert_eq!(dia_semana("lixo"), "");
        assert_eq!(nome_dia(0, "2026-11-03"), "Hoje");
        assert_eq!(nome_dia(1, "2026-11-04"), "Amanhã");
        assert_eq!(nome_dia(2, "2026-11-05"), "Qui");
    }
}
