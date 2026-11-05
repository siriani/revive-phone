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
    crate::brilho::acender();
    ATIVA.store(false, Ordering::Relaxed);
}
