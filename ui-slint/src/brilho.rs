//! Brilho da tela pelo backlight do kernel e a escolha salva em /var/lib/revive/brilho.
//!
//! O aparelho expõe /sys/class/backlight/backlight/{brightness,max_brightness}
//! (0..4095). A interface trabalha em 0..1; gravar no sysfs pode precisar de root.

const DIR: &str = "/sys/class/backlight/backlight";
const ARQUIVO: &str = "/var/lib/revive/brilho";

/// Maior valor aceito pelo backlight; None se o aparelho não tiver backlight.
fn maximo() -> Option<f32> {
    let s = std::fs::read_to_string(format!("{DIR}/max_brightness")).ok()?;
    s.trim().parse::<f32>().ok().filter(|m| *m > 0.0)
}

/// Se o aparelho tem controle de brilho (os dois arquivos do sysfs existem).
pub fn disponivel() -> bool {
    maximo().is_some() && std::fs::metadata(format!("{DIR}/brightness")).is_ok()
}

/// Brilho atual em 0..1 (1.0 se não der para ler).
pub fn ler() -> f32 {
    let Some(max) = maximo() else { return 1.0 };
    std::fs::read_to_string(format!("{DIR}/brightness"))
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|v| (v / max).clamp(0.0, 1.0))
        .unwrap_or(1.0)
}

/// Aplica o brilho (0..1), com no mínimo 1% para a tela não apagar de vez,
/// e guarda a escolha. Erros de permissão são ignorados.
pub fn definir(v: f32) {
    let v = v.clamp(0.0, 1.0);
    if let Some(max) = maximo() {
        let minimo = (max * 0.01).round().max(1.0);
        let valor = (v * max).round().max(minimo) as u64;
        let _ = std::fs::write(format!("{DIR}/brightness"), valor.to_string());
    }
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let _ = std::fs::write(ARQUIVO, format!("{v:.3}"));
}

/// Reaplica o brilho salvo (chamado no início da interface).
pub fn restaurar() {
    let Ok(s) = std::fs::read_to_string(ARQUIVO) else { return };
    if let Ok(v) = s.trim().parse::<f32>() {
        definir(v);
    }
}

/// Apaga a tela (backlight em 0) sem mexer no brilho salvo.
pub fn apagar() {
    let _ = std::fs::write(format!("{DIR}/brightness"), "0");
}

/// Acende a tela de novo no brilho salvo (ou no máximo, se não houver).
pub fn acender() {
    if std::fs::metadata(ARQUIVO).is_ok() {
        restaurar();
    } else {
        definir(1.0);
    }
}
