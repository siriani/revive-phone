//! Tela "Sobre": inventário do aparelho lido direto de /proc e /sys.
//!
//! Toda leitura que falhar vira "—"; nada aqui pode entrar em panic.

use std::fs;
use std::path::{Path, PathBuf};

const VAZIO: &str = "—";

/// Uma linha da tela (rótulo + valor já formatado).
#[derive(Clone)]
pub struct Item {
    pub rotulo: String,
    pub valor: String,
}

/// Um grupo de linhas com título.
#[derive(Clone)]
pub struct Secao {
    pub titulo: String,
    pub itens: Vec<Item>,
}

/// Monta todas as seções da tela "Sobre".
pub fn coletar() -> Vec<Secao> {
    vec![
        secao(
            "Aparelho",
            vec![
                item("Modelo", valor_ou(texto_dt("/proc/device-tree/model"))),
                item("SoC", soc()),
                item("Compatible", valor_ou(compatible())),
            ],
        ),
        secao(
            "Sistema",
            vec![
                item("Versão do Revive", env!("CARGO_PKG_VERSION")),
                item("Kernel", kernel()),
                item("Distribuição", valor_ou(pretty_name())),
                item(
                    "Tempo ligado",
                    uptime_segundos()
                        .map(formatar_uptime)
                        .unwrap_or_else(|| VAZIO.to_string()),
                ),
            ],
        ),
        secao("Processador", processador()),
        secao("Memória", memoria()),
        secao("Vídeo", video()),
        secao("Áudio", audio()),
        secao("Rede e rádio", rede()),
        secao("Energia e sensores", energia()),
        secao(
            "Módulos-chave",
            vec![item("Carregados", modulos_presentes())],
        ),
    ]
}

/// Resumo curto para atualizar a cada 2 s, ex.: "4 núcleos · 787 MHz · 42 °C".
pub fn cpu_resumo() -> String {
    let qtd = contar_online().or_else(|| {
        let n = nucleos_cpu().len();
        (n > 0).then_some(n)
    });
    let freq =
        ler_u64("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq").map(formatar_mhz);
    let temp = temperatura_max().map(|(graus, _)| format!("{graus} °C"));

    let mut partes = Vec::new();
    if let Some(n) = qtd {
        partes.push(format!("{n} {}", crate::i18n::t("núcleos")));
    }
    if let Some(f) = freq {
        partes.push(f);
    }
    if let Some(t) = temp {
        partes.push(t);
    }
    if partes.is_empty() {
        VAZIO.to_string()
    } else {
        partes.join(" · ")
    }
}

// ------------------------------------------------------------------ seções

fn processador() -> Vec<Item> {
    let mut itens = vec![item(
        "Núcleos online",
        valor_ou(texto("/sys/devices/system/cpu/online")),
    )];
    for n in nucleos_cpu() {
        itens.push(Item {
            rotulo: format!("cpu{n}"),
            valor: resumo_cpu(n),
        });
    }
    itens.push(item(
        "Carga (1/5/15 min)",
        valor_ou(carga()),
    ));
    itens.push(item("Temperatura", temperatura_texto()));
    itens
}

fn memoria() -> Vec<Item> {
    let info = meminfo();
    let total = campo_mem(&info, "MemTotal");
    let disponivel =
        campo_mem(&info, "MemAvailable").or_else(|| campo_mem(&info, "MemFree"));
    let usada = match (total, disponivel) {
        (Some(t), Some(d)) => {
            let u = t.saturating_sub(d);
            let pct = if t > 0 { u.saturating_mul(100) / t } else { 0 };
            Some(format!("{} ({}%)", formatar_mb(u), pct))
        }
        _ => None,
    };

    let swap_total = campo_mem(&info, "SwapTotal");
    let swap_livre = campo_mem(&info, "SwapFree");
    let swap = match (swap_total, swap_livre) {
        (Some(t), Some(l)) => Some(format!(
            "{} de {}",
            formatar_mb(t.saturating_sub(l)),
            formatar_mb(t)
        )),
        (Some(t), None) => Some(formatar_mb(t)),
        _ => None,
    };

    let cma_total = campo_mem(&info, "CmaTotal");
    let cma_livre = campo_mem(&info, "CmaFree");
    let cma = match (cma_total, cma_livre) {
        (Some(t), Some(l)) => Some(format!(
            "{} {} de {}",
            formatar_mb(l.min(t)),
            crate::i18n::t("livres"),
            formatar_mb(t)
        )),
        (Some(t), None) => Some(formatar_mb(t)),
        _ => None,
    };

    let armazenamento = armazenamento()
        .map(|(t, l)| {
            format!(
                "{} · {} {}",
                formatar_gb(t),
                formatar_gb(l),
                crate::i18n::t("livre")
            )
        })
        .unwrap_or_else(|| VAZIO.to_string());

    vec![
        item("Total", total.map(formatar_mb).unwrap_or_else(|| VAZIO.to_string())),
        item(
            "Disponível",
            disponivel
                .map(formatar_mb)
                .unwrap_or_else(|| VAZIO.to_string()),
        ),
        item("Usada", valor_ou(usada)),
        item("Swap", valor_ou(swap)),
        item("CMA", valor_ou(cma)),
        item("Armazenamento (raiz)", armazenamento),
    ]
}

fn video() -> Vec<Item> {
    let mut itens = vec![
        item("Framebuffer", descricao_fb()),
        item("Resolução", resolucao()),
    ];
    let placas = drm();
    if placas.is_empty() {
        itens.push(item("DRM", VAZIO));
    } else {
        itens.extend(placas);
    }
    itens.push(item("Luz de fundo", luz_fundo()));
    itens.push(item("GPU", gpu()));
    itens
}

fn audio() -> Vec<Item> {
    vec![
        item("Placas", placas_som()),
        item("Processadores remotos", remoteprocs_texto()),
    ]
}

fn rede() -> Vec<Item> {
    vec![
        item("Wi-Fi", wifi()),
        item("Bluetooth", bluetooth()),
        item(
            "Modem",
            estado_remoteproc("modem").unwrap_or_else(|| VAZIO.to_string()),
        ),
    ]
}

fn energia() -> Vec<Item> {
    let mut itens = fontes_energia();
    if itens.is_empty() {
        itens.push(item("Fontes", VAZIO));
    }
    itens.push(item("Sensores IIO", sensores_iio()));
    itens.push(item("Entradas", entradas_input()));
    itens
}

// ------------------------------------------------------------------ ajuda

fn item(rotulo: &str, valor: impl Into<String>) -> Item {
    Item {
        rotulo: crate::i18n::t(rotulo),
        valor: valor.into(),
    }
}

fn secao(titulo: &str, itens: Vec<Item>) -> Secao {
    Secao {
        titulo: crate::i18n::t(titulo),
        itens,
    }
}

fn valor_ou(v: Option<String>) -> String {
    v.unwrap_or_else(|| VAZIO.to_string())
}

/// Texto de um arquivo, sem espaços nas pontas; vazio conta como falha.
fn texto(caminho: impl AsRef<Path>) -> Option<String> {
    let bruto = fs::read_to_string(caminho).ok()?;
    let limpo = bruto.trim();
    (!limpo.is_empty()).then(|| limpo.to_string())
}

/// Texto de device-tree, que costuma terminar em \0.
fn texto_dt(caminho: &str) -> Option<String> {
    let bytes = fs::read(caminho).ok()?;
    let bruto = String::from_utf8_lossy(&bytes);
    let limpo = bruto.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!limpo.is_empty()).then(|| limpo.to_string())
}

fn ler_u64(caminho: impl AsRef<Path>) -> Option<u64> {
    texto(caminho)?.parse::<u64>().ok()
}

fn entradas(dir: &str) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    v
}

fn nome_base(base: &Path) -> String {
    base.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| VAZIO.to_string())
}

/// Nome do driver apontado por `<base>/device/driver`.
fn driver_em(base: impl AsRef<Path>) -> Option<String> {
    let alvo = fs::read_link(base.as_ref().join("device/driver")).ok()?;
    alvo.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

// -------------------------------------------------------------- aparelho

fn soc() -> String {
    let partes: Vec<String> = ["family", "machine", "soc_id", "revision"]
        .iter()
        .filter_map(|campo| texto(format!("/sys/devices/soc0/{campo}")))
        .collect();
    if partes.is_empty() {
        VAZIO.to_string()
    } else {
        partes.join(" · ")
    }
}

fn compatible() -> Option<String> {
    let bruto = texto_dt("/proc/device-tree/compatible")?;
    let lista: Vec<&str> = bruto
        .split('\0')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    (!lista.is_empty()).then(|| lista.join(", "))
}

// --------------------------------------------------------------- sistema

fn kernel() -> String {
    let osrelease = texto("/proc/sys/kernel/osrelease");
    let versao = texto("/proc/version").map(|v| resumir_versao(&v));
    match (osrelease, versao) {
        (Some(o), Some(v)) if !v.is_empty() => format!("{o} · {v}"),
        (Some(o), _) => o,
        (None, Some(v)) if !v.is_empty() => v,
        _ => VAZIO.to_string(),
    }
}

/// Pega a parte útil do /proc/version (a partir do "#build").
fn resumir_versao(v: &str) -> String {
    let junto = v.split_whitespace().collect::<Vec<_>>().join(" ");
    let trecho = junto.find('#').map(|i| &junto[i..]).unwrap_or(&junto);
    if trecho.chars().count() > 100 {
        let mut s: String = trecho.chars().take(99).collect();
        s.push('…');
        s
    } else {
        trecho.to_string()
    }
}

fn pretty_name() -> Option<String> {
    let bruto = texto("/etc/os-release")?;
    for linha in bruto.lines() {
        if let Some(v) = linha.strip_prefix("PRETTY_NAME=") {
            let v = v.trim().trim_matches('"').trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn uptime_segundos() -> Option<u64> {
    let bruto = texto("/proc/uptime")?;
    let seg = bruto.split_whitespace().next()?.parse::<f64>().ok()?;
    (seg >= 0.0).then_some(seg as u64)
}

/// Formata segundos como "3 d 4 h 12 min", "4 h 12 min" ou "12 min".
fn formatar_uptime(seg: u64) -> String {
    let dias = seg / 86_400;
    let horas = (seg % 86_400) / 3_600;
    let minutos = (seg % 3_600) / 60;
    if dias > 0 {
        format!("{dias} d {horas} h {minutos} min")
    } else if horas > 0 {
        format!("{horas} h {minutos} min")
    } else {
        format!("{minutos} min")
    }
}

// ------------------------------------------------------------ processador

fn nucleos_cpu() -> Vec<usize> {
    let Ok(rd) = fs::read_dir("/sys/devices/system/cpu") else {
        return Vec::new();
    };
    let mut v: Vec<usize> = rd
        .flatten()
        .filter_map(|e| {
            let nome = e.file_name();
            let nome = nome.to_string_lossy();
            nome.strip_prefix("cpu")?.parse::<usize>().ok()
        })
        .collect();
    v.sort_unstable();
    v
}

fn contar_lista_cpus(lista: &str) -> usize {
    lista
        .split(',')
        .filter_map(|parte| {
            let parte = parte.trim();
            if let Some((a, b)) = parte.split_once('-') {
                let a = a.trim().parse::<usize>().ok()?;
                let b = b.trim().parse::<usize>().ok()?;
                (b >= a).then(|| b - a + 1)
            } else {
                parte.parse::<usize>().ok().map(|_| 1)
            }
        })
        .sum()
}

fn contar_online() -> Option<usize> {
    let bruto = texto("/sys/devices/system/cpu/online")?;
    let n = contar_lista_cpus(&bruto);
    (n > 0).then_some(n)
}

fn resumo_cpu(n: usize) -> String {
    let base = format!("/sys/devices/system/cpu/cpu{n}/cpufreq");
    let mut partes = Vec::new();

    if let Some(khz) = ler_u64(format!("{base}/scaling_cur_freq")) {
        partes.push(formatar_mhz(khz));
    }
    if let Some(khz) = ler_u64(format!("{base}/cpuinfo_max_freq")) {
        partes.push(format!("{} {}", crate::i18n::t("máx"), formatar_mhz(khz)));
    }
    if let Some(khz) = ler_u64(format!("{base}/cpuinfo_min_freq")) {
        partes.push(format!("{} {}", crate::i18n::t("mín"), formatar_mhz(khz)));
    }
    if let Some(gov) = texto(format!("{base}/governor")) {
        partes.push(gov);
    }
    if let Some(lista) = texto(format!("{base}/scaling_available_frequencies")) {
        let freqs: Vec<String> = lista
            .split_whitespace()
            .filter_map(|v| v.parse::<u64>().ok())
            .map(|khz| ((khz + 500) / 1000).to_string())
            .collect();
        if !freqs.is_empty() {
            partes.push(format!("{} MHz", freqs.join("/")));
        }
    }

    if partes.is_empty() {
        VAZIO.to_string()
    } else {
        partes.join(" · ")
    }
}

fn formatar_mhz(khz: u64) -> String {
    let mhz = (khz as f64 / 1000.0).round() as u64;
    format!("{mhz} MHz")
}

fn carga() -> Option<String> {
    let bruto = texto("/proc/loadavg")?;
    let campos: Vec<&str> = bruto.split_whitespace().take(3).collect();
    Some(if campos.len() == 3 {
        campos.join(" · ")
    } else {
        bruto
    })
}

fn temperatura_max() -> Option<(i64, String)> {
    let mut maior: Option<(i64, String)> = None;
    for base in entradas("/sys/class/thermal") {
        let nome = nome_base(&base);
        if !nome.starts_with("thermal_zone") {
            continue;
        }
        let Some(mili) = ler_u64(base.join("temp")) else {
            continue;
        };
        let graus = (mili / 1000) as i64;
        if graus <= 0 || graus > 150 {
            continue;
        }
        let tipo = texto(base.join("type")).unwrap_or_else(|| nome.clone());
        if maior.as_ref().map_or(true, |(m, _)| graus > *m) {
            maior = Some((graus, tipo));
        }
    }
    maior
}

fn temperatura_texto() -> String {
    match temperatura_max() {
        Some((graus, tipo)) => format!("{graus} °C ({tipo})"),
        None => VAZIO.to_string(),
    }
}

// ---------------------------------------------------------------- memória

fn meminfo() -> Vec<(String, u64)> {
    let Some(bruto) = texto("/proc/meminfo") else {
        return Vec::new();
    };
    bruto
        .lines()
        .filter_map(|l| {
            let (chave, resto) = l.split_once(':')?;
            let kb = resto.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((chave.trim().to_string(), kb))
        })
        .collect()
}

fn campo_mem(info: &[(String, u64)], nome: &str) -> Option<u64> {
    info.iter().find(|(k, _)| k == nome).map(|(_, v)| *v)
}

fn formatar_mb(kb: u64) -> String {
    format!("{} MB", kb / 1024)
}

fn armazenamento() -> Option<(u64, u64)> {
    use std::ffi::CString;

    let raiz = CString::new("/").ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::statvfs(raiz.as_ptr(), &mut st) };
    if r != 0 {
        return None;
    }
    let bloco = st.f_frsize as u64;
    let total = (st.f_blocks as u64).saturating_mul(bloco);
    let livre = (st.f_bavail as u64).saturating_mul(bloco);
    Some((total, livre))
}

fn formatar_gb(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let v = bytes as f64 / GB;
    if v >= 100.0 || (v - v.round()).abs() < 0.05 {
        format!("{v:.0} GB")
    } else {
        format!("{v:.1} GB")
    }
}

// ------------------------------------------------------------------ vídeo

fn descricao_fb() -> String {
    let Some(nome) = texto("/sys/class/graphics/fb0/name") else {
        return VAZIO.to_string();
    };
    if nome == "msmdrmfb" {
        crate::i18n::t("msm DRM (aceleração do controlador de tela)")
    } else if nome == "simpledrmdrmfb" {
        crate::i18n::t("simpledrm (framebuffer do bootloader)")
    } else {
        nome
    }
}

fn resolucao() -> String {
    let tamanho = texto("/sys/class/graphics/fb0/virtual_size")
        .map(|v| v.replace(',', "×").replace(' ', ""));
    let bpp = texto("/sys/class/graphics/fb0/bits_per_pixel");
    match (tamanho, bpp) {
        (Some(t), Some(b)) => format!("{t} · {b} bpp"),
        (Some(t), None) => t,
        (None, Some(b)) => format!("{b} bpp"),
        _ => VAZIO.to_string(),
    }
}

fn drm() -> Vec<Item> {
    let mut nomes: Vec<String> = entradas("/sys/class/drm")
        .iter()
        .map(|p| nome_base(p))
        .collect();
    nomes.sort();

    let mut cards = Vec::new();
    let mut conectores = Vec::new();
    for nome in &nomes {
        let Some(resto) = nome.strip_prefix("card") else {
            continue;
        };
        if !resto.is_empty() && resto.chars().all(|c| c.is_ascii_digit()) {
            cards.push(nome.clone());
        } else if resto.contains('-') {
            conectores.push(nome.clone());
        }
    }

    let mut itens = Vec::new();
    for card in cards {
        let base = format!("/sys/class/drm/{card}");
        let driver = driver_em(Path::new(&base)).unwrap_or_else(|| VAZIO.to_string());
        let meus: Vec<String> = conectores
            .iter()
            .filter(|c| c.starts_with(&format!("{card}-")))
            .map(|c| resumo_conector(c))
            .collect();
        let mut partes = vec![driver];
        partes.extend(meus);
        itens.push(Item {
            rotulo: card,
            valor: partes.join(" · "),
        });
    }
    itens
}

fn resumo_conector(nome: &str) -> String {
    let base = format!("/sys/class/drm/{nome}");
    let status = texto(format!("{base}/status")).unwrap_or_else(|| VAZIO.to_string());
    let modo = texto(format!("{base}/modes")).and_then(|m| {
        m.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    });
    match modo {
        Some(m) => format!("{nome}: {status} {m}"),
        None => format!("{nome}: {status}"),
    }
}

fn luz_fundo() -> String {
    let mut partes = Vec::new();
    for base in entradas("/sys/class/backlight") {
        let nome = nome_base(&base);
        let brilho = texto(base.join("brightness"));
        let maximo = texto(base.join("max_brightness"));
        match (brilho, maximo) {
            (Some(b), Some(m)) => partes.push(format!("{nome}: {b}/{m}")),
            (Some(b), None) => partes.push(format!("{nome}: {b}")),
            _ => partes.push(nome),
        }
    }
    if partes.is_empty() {
        crate::i18n::t("controlada pelo bootloader")
    } else {
        partes.join(" · ")
    }
}

fn gpu() -> String {
    for base in entradas("/sys/class/drm") {
        let nome = nome_base(&base);
        if nome.starts_with("renderD") {
            if let Some(driver) = driver_em(&base) {
                return format!("{nome} · {driver}");
            }
        }
    }
    if Path::new("/sys/class/kgsl").exists() {
        return crate::i18n::t("kgsl (Adreno)");
    }
    crate::i18n::t("sem driver (Adreno 305)")
}

// ------------------------------------------------------------------ áudio

fn placas_som() -> String {
    let Some(bruto) = texto("/proc/asound/cards") else {
        return crate::i18n::t("sem placa de som interna");
    };
    let linhas: Vec<String> = bruto
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('[') && l.contains(']'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    if linhas.is_empty() {
        crate::i18n::t("sem placa de som interna")
    } else {
        linhas.join(" · ")
    }
}

fn remoteprocs_texto() -> String {
    let mut partes = Vec::new();
    for base in entradas("/sys/class/remoteproc") {
        let nome = texto(base.join("name")).unwrap_or_else(|| nome_base(&base));
        let estado = texto(base.join("state")).unwrap_or_else(|| VAZIO.to_string());
        partes.push(format!("{nome}: {estado}"));
    }
    if partes.is_empty() {
        VAZIO.to_string()
    } else {
        partes.join(" · ")
    }
}

fn estado_remoteproc(contem: &str) -> Option<String> {
    for base in entradas("/sys/class/remoteproc") {
        let nome = texto(base.join("name"))
            .unwrap_or_default()
            .to_lowercase();
        if nome.contains(contem) {
            return Some(texto(base.join("state")).unwrap_or_else(|| VAZIO.to_string()));
        }
    }
    None
}

// ---------------------------------------------------------- rede e rádio

fn wifi() -> String {
    let base = Path::new("/sys/class/net/wlan0");
    if !base.exists() {
        return VAZIO.to_string();
    }
    let mut partes = Vec::new();
    if let Some(driver) = driver_em(base) {
        partes.push(driver);
    }
    if let Some(estado) = texto(base.join("operstate")) {
        partes.push(estado);
    }
    if partes.is_empty() {
        VAZIO.to_string()
    } else {
        partes.join(" · ")
    }
}

fn bluetooth() -> String {
    let base = Path::new("/sys/class/bluetooth/hci0");
    if !base.exists() {
        return VAZIO.to_string();
    }
    driver_em(base).unwrap_or_else(|| crate::i18n::t("presente"))
}

// ------------------------------------------------------ energia e sensores

fn fontes_energia() -> Vec<Item> {
    let mut itens = Vec::new();
    for base in entradas("/sys/class/power_supply") {
        let nome = nome_base(&base);
        let mut partes = Vec::new();
        if let Some(v) = texto(base.join("status")) {
            partes.push(format!("{}: {v}", crate::i18n::t("status")));
        }
        if let Some(v) = texto(base.join("online")) {
            partes.push(format!("{}: {v}", crate::i18n::t("online")));
        }
        if let Some(v) = texto(base.join("capacity")) {
            partes.push(format!("{}: {v}%", crate::i18n::t("capacidade")));
        }
        if !partes.is_empty() {
            itens.push(Item {
                rotulo: nome,
                valor: partes.join(" · "),
            });
        }
    }
    itens
}

fn sensores_iio() -> String {
    let mut nomes: Vec<String> = entradas("/sys/bus/iio/devices")
        .into_iter()
        .filter_map(|base| texto(base.join("name")))
        .collect();
    nomes.sort();
    nomes.dedup();
    if nomes.is_empty() {
        VAZIO.to_string()
    } else {
        nomes.join(", ")
    }
}

fn entradas_input() -> String {
    let Some(bruto) = texto("/proc/bus/input/devices") else {
        return VAZIO.to_string();
    };
    let nomes: Vec<String> = bruto
        .lines()
        .filter_map(|l| l.trim().strip_prefix("N: Name="))
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if nomes.is_empty() {
        VAZIO.to_string()
    } else {
        nomes.join(" · ")
    }
}

// ----------------------------------------------------------- módulos-chave

const MODULOS_CHAVE: &[&str] = &[
    "msm",
    "panel_motorola_falcon_cmi",
    "tps65132_regulator",
    "qcom_wled",
    "qcom_q6v5_pas",
    "wcn36xx",
    "btqcomsmd",
    "qcom_camss",
    "mt9m114",
    "i2c_qcom_cci",
    "snd_soc_wcd9306",
];

fn modulos_presentes() -> String {
    let Some(bruto) = texto("/proc/modules") else {
        return VAZIO.to_string();
    };
    let carregados: Vec<&str> = bruto
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    let presentes: Vec<&str> = MODULOS_CHAVE
        .iter()
        .copied()
        .filter(|m| carregados.contains(m))
        .collect();
    if presentes.is_empty() {
        VAZIO.to_string()
    } else {
        presentes.join(", ")
    }
}

#[cfg(test)]
mod testes {
    use super::{contar_lista_cpus, formatar_gb, formatar_mhz, formatar_uptime};

    #[test]
    fn uptime_formatado() {
        assert_eq!(formatar_uptime(0), "0 min");
        assert_eq!(formatar_uptime(59), "0 min");
        assert_eq!(formatar_uptime(12 * 60), "12 min");
        assert_eq!(formatar_uptime(4 * 3_600 + 12 * 60), "4 h 12 min");
        assert_eq!(
            formatar_uptime(3 * 86_400 + 4 * 3_600 + 12 * 60),
            "3 d 4 h 12 min"
        );
    }

    #[test]
    fn mhz_formatado() {
        assert_eq!(formatar_mhz(300_000), "300 MHz");
        assert_eq!(formatar_mhz(787_000), "787 MHz");
        assert_eq!(formatar_mhz(787_500), "788 MHz");
    }

    #[test]
    fn gb_formatado() {
        assert_eq!(formatar_gb(0), "0 GB");
        assert_eq!(formatar_gb(8 * 1024 * 1024 * 1024), "8 GB");
        assert_eq!(
            formatar_gb(6 * 1024 * 1024 * 1024 + 322 * 1024 * 1024),
            "6.3 GB"
        );
    }

    #[test]
    fn cpus_online_contadas() {
        assert_eq!(contar_lista_cpus("0-3"), 4);
        assert_eq!(contar_lista_cpus("0"), 1);
        assert_eq!(contar_lista_cpus("0-1,3"), 3);
    }
}
