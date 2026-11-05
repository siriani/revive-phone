//! Modo de espera: o botão liga/desliga apaga a tela e deixa o aparelho gastando pouco.
//!
//! Com a tela apagada o relógio, o painel, a cotação e o acelerômetro param, o status
//! (Wi-Fi, Bluetooth, música) passa a cada 5 s e a CPU fica limitada a ~600 MHz, o que
//! ainda toca música. O painel do display NÃO é desligado: no msm8226 o DRM dá oops ao
//! religar (mdp5_crtc_atomic_flush), então só o backlight vai a zero.

use std::sync::atomic::{AtomicBool, Ordering};

static ATIVA: AtomicBool = AtomicBool::new(false);
const CPU: &str = "/sys/devices/system/cpu/cpu0/cpufreq";
const TETO_ESPERA: u64 = 600_000;
const TETO_ARQ: &str = "/run/revive-teto-cpu";
static TETO_ANTES: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Se a tela está apagada (modo de espera).
pub fn ativa() -> bool {
    ATIVA.load(Ordering::Relaxed)
}

/// Frequência mais baixa da lista que ainda seja >= TETO_ESPERA (ou a maior abaixo dele).
fn teto_espera() -> Option<u64> {
    let s = std::fs::read_to_string(format!("{CPU}/scaling_available_frequencies")).ok()?;
    let mut f: Vec<u64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    f.sort();
    f.iter().copied().find(|&x| x >= TETO_ESPERA).or(f.last().copied())
}

pub fn entrar() {
    ATIVA.store(true, Ordering::Relaxed);
    crate::brilho::apagar();
    if let Some(teto) = teto_espera() {
        if let Ok(atual) = std::fs::read_to_string(format!("{CPU}/scaling_max_freq")) {
            // também em /run: se a interface reiniciar apagada, o teto volta no início
            let _ = std::fs::write(TETO_ARQ, atual.trim());
            *TETO_ANTES.lock().unwrap() = Some(atual.trim().to_string());
        }
        // o mínimo pode estar alto (boost do toque): baixa antes do máximo
        if let Ok(min) = std::fs::read_to_string(format!("{CPU}/cpuinfo_min_freq")) {
            let _ = std::fs::write(format!("{CPU}/scaling_min_freq"), min.trim());
        }
        let _ = std::fs::write(format!("{CPU}/scaling_max_freq"), teto.to_string());
    }
}

pub fn sair() {
    if let Some(t) = TETO_ANTES.lock().unwrap().take() {
        let _ = std::fs::write(format!("{CPU}/scaling_max_freq"), t);
    }
    let _ = std::fs::remove_file(TETO_ARQ);
    crate::brilho::acender();
    ATIVA.store(false, Ordering::Relaxed);
}

// ------------------------------------------------ apagar sozinho e girar a tela

use std::sync::atomic::AtomicI32;

const ARQ_APAGAR: &str = "/var/lib/revive/apagar";
const ARQ_GIRAR: &str = "/var/lib/revive/girar";
/// opções do Ajustes > Tela, em segundos (0 = nunca)
const TEMPOS: [u64; 5] = [60, 120, 300, 600, 0];
static APAGAR: AtomicI32 = AtomicI32::new(-1);
static GIRAR: AtomicI32 = AtomicI32::new(-1);

fn ler_num(arq: &str) -> Option<i32> {
    std::fs::read_to_string(arq).ok()?.trim().parse().ok()
}

/// Índice escolhido em Ajustes > Tela > Apagar a tela (padrão: 2 min).
pub fn apagar_indice() -> i32 {
    let v = APAGAR.load(Ordering::Relaxed);
    if v >= 0 {
        return v;
    }
    let v = ler_num(ARQ_APAGAR).filter(|i| (0..TEMPOS.len() as i32).contains(i)).unwrap_or(1);
    APAGAR.store(v, Ordering::Relaxed);
    v
}

pub fn definir_apagar(i: i32) {
    let i = i.clamp(0, TEMPOS.len() as i32 - 1);
    APAGAR.store(i, Ordering::Relaxed);
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let _ = std::fs::write(ARQ_APAGAR, i.to_string());
}

/// Tempo sem toque até apagar a tela; None = nunca.
pub fn tempo_apagar() -> Option<std::time::Duration> {
    let s = TEMPOS[apagar_indice() as usize];
    (s > 0).then(|| std::time::Duration::from_secs(s))
}

/// Se a tela gira sozinha pelo acelerômetro (padrão: sim).
pub fn girar_auto() -> bool {
    let v = GIRAR.load(Ordering::Relaxed);
    if v >= 0 {
        return v == 1;
    }
    let v = ler_num(ARQ_GIRAR).map(|g| (g != 0) as i32).unwrap_or(1);
    GIRAR.store(v, Ordering::Relaxed);
    v == 1
}

pub fn definir_girar(g: bool) {
    GIRAR.store(g as i32, Ordering::Relaxed);
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let _ = std::fs::write(ARQ_GIRAR, if g { "1" } else { "0" });
}

/// No início da interface: desfaz um modo de espera que ficou pela metade
/// (a interface foi reiniciada com a tela apagada).
pub fn restaurar() {
    if let Ok(t) = std::fs::read_to_string(TETO_ARQ) {
        let _ = std::fs::write(format!("{CPU}/scaling_max_freq"), t.trim());
        let _ = std::fs::remove_file(TETO_ARQ);
    }
}
