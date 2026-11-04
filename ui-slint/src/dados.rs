//! Dados mostrados na tela: Wi-Fi, Bitcoin, Bluetooth (serviço revive-bt) e a música
//! do mpv (serviço revive-mpv). Tudo lido fora da thread da interface.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::i18n;

const WPA_CTRL: &str = "/run/revive-wpa";
const IFACE: &str = "wlan0";
const BT_STATE: &str = "/run/revive-bt/state.json";
const MPV_SOCK: &str = "/run/revive-mpv.sock";
const FILA: &str = "/run/revive-queue.json";

fn saida(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

// --------------------------------------------------------------------- Wi-Fi

#[derive(Clone, Default, PartialEq)]
pub struct Wifi {
    pub ssid: String,
    pub estado: String,
    pub ip: String,
}

pub fn wifi() -> Wifi {
    let st = saida("wpa_cli", &["-p", WPA_CTRL, "-i", IFACE, "status"]);
    let campo = |k: &str| {
        st.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or("")
            .to_string()
    };
    let ip = saida("ip", &["-4", "-o", "addr", "show", IFACE])
        .split(" inet ")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("")
        .to_string();
    Wifi { ssid: campo("ssid"), estado: campo("wpa_state"), ip }
}

// ------------------------------------------------------------------- Bitcoin

#[derive(Clone, Default)]
pub struct Btc {
    pub brl: f64,
    pub usd: f64,
    pub variacao: Option<f64>,
}

pub fn btc() -> Result<Btc, String> {
    let agente: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let url = "https://api.coingecko.com/api/v3/simple/price?ids=bitcoin\
               &vs_currencies=brl,usd&include_24hr_change=true";
    let v: Value = agente
        .get(url)
        .header("User-Agent", "revive")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let b = &v["bitcoin"];
    Ok(Btc {
        brl: b["brl"].as_f64().ok_or_else(|| i18n::t("sem preço"))?,
        usd: b["usd"].as_f64().unwrap_or(0.0),
        variacao: b["brl_24h_change"].as_f64().or(b["usd_24h_change"].as_f64()),
    })
}

/// 418381.2 -> "418.381"
pub fn milhar(v: f64) -> String {
    let s = format!("{:.0}", v);
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

// ----------------------------------------------------------------- Bluetooth

pub fn bt_estado() -> Value {
    std::fs::read_to_string(BT_STATE)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

pub fn bt_conectados(st: &Value) -> Vec<String> {
    st["aparelhos"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|d| d["conectado"].as_bool().unwrap_or(false))
                .filter_map(|d| d["nome"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// --------------------------------------------------------------------- mpv

pub fn mpv(comando: Value) -> Option<Value> {
    let mut s = UnixStream::connect(MPV_SOCK).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let msg = serde_json::json!({ "command": comando });
    s.write_all(format!("{msg}\n").as_bytes()).ok()?;
    // o mpv também manda eventos pelo socket: procura a resposta
    for linha in BufReader::new(s).lines() {
        let v: Value = serde_json::from_str(&linha.ok()?).ok()?;
        if let Some(erro) = v.get("error") {
            return if erro == "success" { v.get("data").cloned() } else { None };
        }
    }
    None
}

pub fn mpv_prop(nome: &str) -> Option<Value> {
    mpv(serde_json::json!(["get_property", nome]))
}
// ---------------------------------------------------------- Wi-Fi: procurar e conectar

const NO_HT_FLAG: &str = "/etc/revive/wifi-no-ht";

fn wpa(args: &[&str]) -> String {
    let mut a = vec!["-p", WPA_CTRL, "-i", IFACE];
    a.extend_from_slice(args);
    saida("wpa_cli", &a).trim().to_string()
}

#[derive(Clone)]
pub struct RedeVista {
    pub ssid: String,
    pub sinal: i32,
    pub segura: bool,
}

pub fn wifi_procurar() -> Vec<RedeVista> {
    wpa(&["scan"]);
    std::thread::sleep(Duration::from_secs(4));
    let mut redes: Vec<RedeVista> = vec![];
    for linha in wpa(&["scan_results"]).lines().skip(1) {
        let p: Vec<&str> = linha.split('\t').collect();
        if p.len() < 5 || p[4].is_empty() {
            continue;
        }
        let sinal: i32 = p[2].parse().unwrap_or(-100);
        let segura = p[3].contains("WPA") || p[3].contains("WEP");
        match redes.iter_mut().find(|r| r.ssid == p[4]) {
            Some(r) if r.sinal < sinal => r.sinal = sinal,
            Some(_) => {}
            None => redes.push(RedeVista { ssid: p[4].into(), sinal, segura }),
        }
    }
    redes.sort_by_key(|r| -r.sinal);
    redes.truncate(8);
    redes
}

pub fn barras(sinal: i32) -> i32 {
    if sinal > -55 { 4 } else if sinal > -65 { 3 } else if sinal > -75 { 2 } else { 1 }
}

/// Cria (ou substitui) a rede no wpa_supplicant, salva e pede IP.
pub fn wifi_conectar(ssid: &str, senha: &str) {
    for linha in wpa(&["list_networks"]).lines().skip(1) {
        let p: Vec<&str> = linha.split('\t').collect();
        if p.len() > 1 && p[1] == ssid {
            wpa(&["remove_network", p[0]]);
        }
    }
    let id = wpa(&["add_network"]).lines().last().unwrap_or("0").to_string();
    let q = |s: &str| serde_json::to_string(s).unwrap_or_default(); // aspas e escapes
    wpa(&["set_network", &id, "ssid", &q(ssid)]);
    if senha.is_empty() {
        wpa(&["set_network", &id, "key_mgmt", "NONE"]);
    } else {
        wpa(&["set_network", &id, "psk", &q(senha)]);
    }
    if std::path::Path::new(NO_HT_FLAG).exists() {
        wpa(&["set_network", &id, "disable_ht", "1"]); // ex.: firmware do WCN36xx trava com 802.11n
    }
    wpa(&["select_network", &id]);
    wpa(&["save_config"]);
    saida("udhcpc", &["-i", IFACE, "-n", "-q", "-t", "6"]);
    eprintln!("wifi: conectar {ssid} id={id}");
}

// ------------------------------------------------------------ Bluetooth: comandos

/// Manda um comando para o serviço revive-bt (FIFO). false se ele não está rodando.
pub fn bt_cmd(linha: &str) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/run/revive-bt/cmd")
    {
        Ok(mut f) => f.write_all(format!("{linha}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}
