//! Estado do aparelho: energia, bateria, temperatura da CPU, memória e uptime.
//!
//! Tudo lido direto do sysfs/procfs. Qualquer leitura que falhar vira o valor
//! padrão; nada aqui pode entrar em panic.

use std::fs;

pub struct Sistema {
    pub carregando: bool,
    pub na_tomada: bool,
    pub bateria: Option<i32>,
    pub temp_cpu: Option<i32>,
    pub ram_livre_mb: i32,
    pub uptime: String,
}

/// Lê o estado atual do aparelho.
pub fn sistema() -> Sistema {
    let (carregando, na_tomada, bateria) = energia();
    Sistema {
        carregando,
        na_tomada,
        bateria,
        temp_cpu: temperatura(),
        ram_livre_mb: ram_livre(),
        uptime: formatar_uptime(uptime_segundos()),
    }
}

/// Percorre /sys/class/power_supply e devolve (carregando, na_tomada, bateria).
fn energia() -> (bool, bool, Option<i32>) {
    let mut carregando = false;
    let mut na_tomada = false;
    let mut bateria = None;

    // Se o diretório não existir, fica tudo no padrão.
    let Ok(dir) = fs::read_dir("/sys/class/power_supply") else {
        return (false, false, None);
    };

    for entrada in dir.flatten() {
        let caminho = entrada.path();
        let nome = entrada.file_name().to_string_lossy().to_lowercase();

        // Status "Charging" indica carga em andamento.
        if ler(&caminho.join("status")).eq_ignore_ascii_case("Charging") {
            carregando = true;
        }
        // Fonte USB/DC conectada também conta como na tomada.
        if (nome.contains("usb") || nome.contains("dcin"))
            && ler(&caminho.join("online")) == "1"
        {
            na_tomada = true;
        }
        // A primeira capacidade encontrada vira a bateria.
        if bateria.is_none() {
            if let Ok(v) = ler(&caminho.join("capacity")).parse::<i32>() {
                bateria = Some(v);
            }
        }
    }

    // Carregar já implica estar ligado na tomada.
    (carregando, na_tomada || carregando, bateria)
}

/// Maior temperatura válida entre as zonas térmicas, em °C.
fn temperatura() -> Option<i32> {
    let dir = fs::read_dir("/sys/class/thermal").ok()?;
    let mut maior: Option<i32> = None;
    for entrada in dir.flatten() {
        let caminho = entrada.path();
        let arquivo = entrada.file_name();
        if !arquivo.to_string_lossy().starts_with("thermal_zone") {
            continue;
        }
        // O arquivo vem em miligraus; descarta leituras absurdas.
        let Ok(mili) = ler(&caminho.join("temp")).parse::<i64>() else {
            continue;
        };
        let graus = mili / 1000;
        if graus > 0 && graus <= 150 {
            maior = Some(maior.map_or(graus as i32, |m| m.max(graus as i32)));
        }
    }
    maior
}

/// MemAvailable de /proc/meminfo, convertido de kB para MB.
fn ram_livre() -> i32 {
    let Ok(txt) = fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for linha in txt.lines() {
        if let Some(resto) = linha.strip_prefix("MemAvailable:") {
            let kb = resto
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            return (kb / 1024).max(0) as i32;
        }
    }
    0
}

/// Primeiro campo de /proc/uptime, em segundos.
fn uptime_segundos() -> u64 {
    ler("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        .max(0.0) as u64
}

/// Formata segundos como "3 d 4 h", "4 h 12 min" ou "12 min".
fn formatar_uptime(seg: u64) -> String {
    let dias = seg / 86_400;
    let horas = (seg % 86_400) / 3_600;
    let minutos = (seg % 3_600) / 60;
    if dias > 0 {
        format!("{dias} d {horas} h")
    } else if horas > 0 {
        format!("{horas} h {minutos} min")
    } else {
        format!("{minutos} min")
    }
}

/// Lê um arquivo de texto e devolve o conteúdo sem espaços nas pontas.
fn ler(caminho: impl AsRef<std::path::Path>) -> String {
    fs::read_to_string(caminho)
        .map(|t| t.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod testes {
    use super::formatar_uptime;

    #[test]
    fn uptime_formatado() {
        assert_eq!(formatar_uptime(0), "0 min");
        assert_eq!(formatar_uptime(90), "1 min");
        assert_eq!(formatar_uptime(12 * 60), "12 min");
        assert_eq!(formatar_uptime(4 * 3_600 + 12 * 60), "4 h 12 min");
        assert_eq!(formatar_uptime(3 * 86_400 + 4 * 3_600), "3 d 4 h");
        assert_eq!(formatar_uptime(3 * 86_400 + 4 * 3_600 + 59 * 60), "3 d 4 h");
    }
}
