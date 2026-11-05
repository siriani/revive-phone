//! Dados e ações das seções Som, Bateria, Data e hora e Sistema do app Ajustes.
//!
//! Bateria: o PM8226 não tem medidor de carga no kernel mainline, então o percentual é
//! estimado pela tensão (canal vph_pwr do ADC do PMIC) com uma curva típica de Li-ion.
//! Carregando, a tensão sobe e a estimativa fica otimista: a tela avisa.

use std::path::Path;

const PS: &str = "/sys/class/power_supply";
const ADC: &str = "/sys/bus/iio/devices";

fn ler(p: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

// ------------------------------------------------------------------ bateria

pub struct Bateria {
    /// percentual estimado (None = sem leitura de tensão)
    pub pct: Option<i32>,
    /// tensão em volts
    pub tensao: Option<f32>,
    pub carregando: bool,
    pub cheia: bool,
    /// "USB", "carregador" ou "" (sem fonte)
    pub fonte: &'static str,
    pub saude: String,
}

/// Tensão do canal vph_pwr (µV) do ADC do PMIC, em volts.
fn tensao_bateria() -> Option<f32> {
    for d in std::fs::read_dir(ADC).ok()?.flatten() {
        let Ok(itens) = std::fs::read_dir(d.path()) else { continue };
        for f in itens.flatten() {
            let nome = f.file_name().to_string_lossy().to_string();
            let Some(base) = nome.strip_suffix("_label") else { continue };
            if ler(f.path()).as_deref() == Some("vph_pwr") {
                let uv: f32 = ler(d.path().join(format!("{base}_input")))?.parse().ok()?;
                return Some(uv / 1_000_000.0);
            }
        }
    }
    None
}

/// Curva típica de tensão em repouso de uma bateria Li-ion de 4,35 V.
fn pct_por_tensao(v: f32) -> i32 {
    const CURVA: [(f32, f32); 12] = [
        (3.40, 0.0), (3.60, 5.0), (3.70, 10.0), (3.75, 20.0), (3.78, 30.0), (3.81, 40.0),
        (3.85, 50.0), (3.90, 60.0), (3.97, 70.0), (4.05, 80.0), (4.15, 90.0), (4.30, 100.0),
    ];
    if v <= CURVA[0].0 {
        return 0;
    }
    for w in CURVA.windows(2) {
        let ((v0, p0), (v1, p1)) = (w[0], w[1]);
        if v <= v1 {
            return (p0 + (v - v0) / (v1 - v0) * (p1 - p0)).round() as i32;
        }
    }
    100
}

pub fn bateria() -> Bateria {
    let mut b = Bateria { pct: None, tensao: None, carregando: false, cheia: false, fonte: "", saude: String::new() };
    if let Ok(dir) = std::fs::read_dir(PS) {
        for d in dir.flatten() {
            let p = d.path();
            let tipo = ler(p.join("type")).unwrap_or_default();
            let online = ler(p.join("online")).as_deref() == Some("1");
            match tipo.as_str() {
                "Battery" => {
                    let st = ler(p.join("status")).unwrap_or_default();
                    b.carregando = st == "Charging";
                    b.cheia = st == "Full";
                    b.saude = ler(p.join("health")).unwrap_or_default();
                    if let Some(c) = ler(p.join("capacity")).and_then(|c| c.parse().ok()) {
                        b.pct = Some(c);
                    }
                }
                "USB" if online => b.fonte = "USB",
                "Mains" if online => b.fonte = "carregador",
                _ => {}
            }
        }
    }
    b.tensao = tensao_bateria();
    if b.pct.is_none() {
        b.pct = b.tensao.map(pct_por_tensao);
    }
    b
}

// ------------------------------------------------------------------ data e hora

const ARQ_24H: &str = "/var/lib/revive/relogio-24h";

/// Fusos oferecidos (zona, nome). O Brasil primeiro; depois alguns do mundo.
pub const FUSOS: [(&str, &str); 14] = [
    ("America/Sao_Paulo", "Brasília (UTC−3)"),
    ("America/Manaus", "Manaus (UTC−4)"),
    ("America/Cuiaba", "Cuiabá (UTC−4)"),
    ("America/Porto_Velho", "Porto Velho (UTC−4)"),
    ("America/Rio_Branco", "Rio Branco (UTC−5)"),
    ("America/Noronha", "Fernando de Noronha (UTC−2)"),
    ("America/Argentina/Buenos_Aires", "Buenos Aires (UTC−3)"),
    ("America/New_York", "Nova York"),
    ("America/Los_Angeles", "Los Angeles"),
    ("Europe/Lisbon", "Lisboa"),
    ("Europe/Madrid", "Madri"),
    ("Europe/London", "Londres"),
    ("Asia/Tokyo", "Tóquio (UTC+9)"),
    ("UTC", "UTC"),
];

/// Zona do /etc/localtime (link para /usr/share/zoneinfo/<zona>).
pub fn fuso_atual() -> String {
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| p.to_string_lossy().split("zoneinfo/").nth(1).map(str::to_string))
        .unwrap_or_else(|| "UTC".into())
}

pub fn fuso_indice() -> i32 {
    let z = fuso_atual();
    FUSOS.iter().position(|(f, _)| *f == z).map(|i| i as i32).unwrap_or(-1)
}

/// Troca o fuso do sistema (o relógio da interface segue na hora).
pub fn definir_fuso(i: i32) -> bool {
    let Some((zona, _)) = FUSOS.get(i.max(0) as usize) else { return false };
    let alvo = format!("/usr/share/zoneinfo/{zona}");
    if !Path::new(&alvo).exists() {
        return false;
    }
    let _ = std::fs::remove_file("/etc/localtime");
    let ok = std::os::unix::fs::symlink(&alvo, "/etc/localtime").is_ok();
    let _ = std::fs::write("/etc/timezone", format!("{zona}\n"));
    ok
}

pub fn relogio_24h() -> bool {
    ler(ARQ_24H).as_deref() != Some("0")
}

pub fn definir_24h(v: bool) {
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let _ = std::fs::write(ARQ_24H, if v { "1" } else { "0" });
}

/// Formato do relógio da barra (strftime).
pub fn formato_hora() -> &'static str {
    if relogio_24h() { "%H:%M" } else { "%I:%M %p" }
}

/// Se o relógio está sincronizado pela internet (chrony).
pub fn sincronizado() -> Option<bool> {
    let out = std::process::Command::new("chronyc").args(["-n", "tracking"]).output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout);
    let linha = t.lines().find(|l| l.starts_with("Leap status"))?;
    Some(!linha.contains("Not synchronised"))
}

// ------------------------------------------------------------------ sistema

pub fn versao_revive() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("REVIVE_GIT"))
}

pub fn sistema_operacional() -> String {
    ler("/etc/os-release")
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

pub fn kernel() -> String {
    ler("/proc/sys/kernel/osrelease").unwrap_or_default()
}

/// Tempo ligado (segundos).
pub fn ligado_ha() -> u64 {
    ler("/proc/uptime")
        .and_then(|s| s.split('.').next().and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

/// (usado, total) em bytes da partição raiz.
pub fn armazenamento() -> Option<(u64, u64)> {
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::statvfs(c"/".as_ptr(), &mut st) };
    if r != 0 {
        return None;
    }
    let bloco = st.f_frsize as u64;
    let total = st.f_blocks as u64 * bloco;
    let livre = st.f_bavail as u64 * bloco;
    Some((total.saturating_sub(livre), total))
}

// ------------------------------------------------------------------ som

const ARQ_SAIDA: &str = "/var/lib/revive/saida-som";

/// 0 = automático (caixa Bluetooth quando conectar), 1 = sempre o alto-falante.
pub fn saida_som() -> i32 {
    (ler(ARQ_SAIDA).as_deref() == Some("interno")) as i32
}

/// Grava a preferência; o revive-bt lê o arquivo e troca a saída sozinho.
pub fn definir_saida_som(i: i32) {
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let _ = std::fs::write(ARQ_SAIDA, if i == 1 { "interno" } else { "auto" });
}

/// Toca um bipe curto (880 Hz, 0,3 s) na saída atual, sem mexer na música.
pub fn testar_som() {
    std::thread::spawn(|| {
        const TAXA: u32 = 48_000;
        let n = TAXA * 3 / 10;
        let mut wav = Vec::with_capacity(44 + n as usize * 2);
        let dados = n * 2;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + dados).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&TAXA.to_le_bytes());
        wav.extend_from_slice(&(TAXA * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&dados.to_le_bytes());
        for i in 0..n {
            // envelope curto nas pontas para não estalar
            let t = i as f32 / TAXA as f32;
            let env = (i.min(n - i) as f32 / 960.0).min(1.0);
            let v = (t * 880.0 * std::f32::consts::TAU).sin() * env * 0.5;
            wav.extend_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes());
        }
        let arq = "/tmp/revive-bipe.wav";
        if std::fs::write(arq, &wav).is_ok() {
            let _ = std::process::Command::new("aplay").args(["-q", arq]).status();
        }
    });
}
