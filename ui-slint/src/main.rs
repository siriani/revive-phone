//! Revive: interface em Slint para celulares antigos com Linux (postmarketOS).
//!
//! Um binário só: desenha no framebuffer com o renderizador por software do Slint
//! (girado para paisagem e só nas regiões que mudam) e lê touch, teclado e mouse
//! pelo evdev. Os serviços (revive-bt, revive-mpv) continuam como antes.

mod app;
mod clima;
mod cripto;
mod dados;
mod i18n;
mod doom;
mod inicio;
mod manifesto;
mod musica;
mod sistema;
mod tela;
mod wad;
mod telegram;

use std::os::fd::RawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize};

slint::include_modules!();

type Tarefa = Box<dyn FnOnce() + Send>;

/// Fila de tarefas mandadas por outras threads + pipe que acorda o laço principal.
#[derive(Clone)]
struct Correio {
    fila: Arc<Mutex<Vec<Tarefa>>>,
    pipe_w: RawFd,
}

impl slint::platform::EventLoopProxy for Correio {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        std::process::exit(0)
    }
    fn invoke_from_event_loop(&self, t: Tarefa) -> Result<(), slint::EventLoopError> {
        self.fila.lock().unwrap().push(t);
        unsafe { libc::write(self.pipe_w, b"x".as_ptr() as *const _, 1) };
        Ok(())
    }
}

struct Plataforma {
    janela: Rc<MinimalSoftwareWindow>,
    correio: Correio,
}

impl Platform for Plataforma {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.janela.clone())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn slint::platform::EventLoopProxy>> {
        Some(Box::new(self.correio.clone()))
    }
}

/// Input boost: frequência mínima no máximo enquanto a pessoa mexe na tela.
fn cpu_min(alto: bool) {
    let base = "/sys/devices/system/cpu/cpu0/cpufreq";
    let qual = if alto { "max" } else { "min" };
    if let Ok(v) = std::fs::read_to_string(format!("{base}/cpuinfo_{qual}_freq")) {
        let _ = std::fs::write(format!("{base}/scaling_min_freq"), v.trim());
    }
}

/// Lado em que o celular está deitado (acelerômetro); None = deitado/indefinido.
fn rotacao_acelerometro() -> Option<tela::Rotacao> {
    let dir = std::fs::read_dir("/sys/bus/iio/devices").ok()?;
    for d in dir.flatten() {
        let p = d.path().join("in_accel_y_raw");
        if let Ok(v) = std::fs::read_to_string(&p) {
            let y: i32 = v.trim().parse().ok()?;
            return if y > 600 {
                Some(tela::Rotacao::R270)
            } else if y < -600 {
                Some(tela::Rotacao::R90)
            } else {
                None
            };
        }
    }
    None
}

const ROT_FILE: &str = "/var/lib/revive/rotation";

fn main() {
    let mut tela = tela::Tela::abrir().expect("não abriu /dev/fb0");
    tela.rot = match std::fs::read_to_string(ROT_FILE).map(|s| s.trim().to_string()).as_deref() {
        Ok("90") => tela::Rotacao::R90,
        _ => tela::Rotacao::R270,
    };
    eprintln!("fb {}x{} {} bpp, rotação {:?}", tela.pw, tela.ph, tela.bpp, tela.rot);

    let janela = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    janela.set_size(PhysicalSize::new(tela.ph, tela.pw)); // paisagem
    let mut pipe = [0; 2];
    unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
    let correio = Correio { fila: Arc::new(Mutex::new(vec![])), pipe_w: pipe[1] };
    slint::platform::set_platform(Box::new(Plataforma { janela: janela.clone(), correio: correio.clone() }))
        .unwrap();

    let ui = AppWindow::new().unwrap();
    // idioma escolhido em Ajustes (traduções .po embutidas; "pt" é o texto original)
    if let Err(e) = slint::select_bundled_translation(&i18n::idioma()) {
        eprintln!("idioma {}: {e:?}", i18n::idioma());
    }
    app::ligar(&ui);
    ui.show().unwrap();

    // Acelerômetro em thread própria: cada leitura do LIS3DH espera uma amostra nova
    // (~2 s). No laço principal isso travava o toque.
    let giro = Arc::new(AtomicU8::new(0)); // 0 = sem decisão, 90 ou 27 (=270)
    {
        let giro = giro.clone();
        std::thread::spawn(move || {
            let mut votos = 0;
            let mut ultimo = None;
            loop {
                let r = rotacao_acelerometro();
                if r.is_some() && r == ultimo {
                    votos += 1;
                } else {
                    votos = 0;
                }
                ultimo = r;
                if votos >= 1 {
                    let v = match r {
                        Some(tela::Rotacao::R90) => 90,
                        Some(tela::Rotacao::R270) => 27,
                        None => 0,
                    };
                    giro.store(v, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }

    let mut entrada = tela::Entrada::nova();
    let mut boost_ate: Option<Instant> = None;
    let mut quadros = 0u32;

    // cronômetro de cada etapa do laço: avisa se alguma passar de 50 ms
    let etapa = |nome: &str, t: Instant| {
        let d = t.elapsed();
        if d > Duration::from_millis(50) {
            eprintln!("laço lento: {nome} levou {:.0} ms", d.as_secs_f64() * 1000.0);
        }
    };
    loop {
        // O DOOM roda como app, desenhando direto no framebuffer: aqui só se espera.
        // A entrada é lida pelo jogo, então nada é despachado para a interface.
        if doom::ativo() {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if doom::saiu() {
            // descarta o que ficou na fila do evdev enquanto o jogo tinha os dispositivos
            for fd in entrada.fds() {
                let _ = entrada.ler(fd, &tela);
            }
            // o jogo pausou/encerrou: a tela inteira precisa ser redesenhada
            tela.redesenhar_tudo = true;
            janela.request_redraw();
            app::voltar_inicio(&ui);
        }
        let t = Instant::now();
        slint::platform::update_timers_and_animations();
        etapa("timers", t);

        // tarefas das outras threads (dados novos)
        let tarefas: Vec<Tarefa> = std::mem::take(&mut *correio.fila.lock().unwrap());
        let t = Instant::now();
        for tarefa in tarefas {
            tarefa();
        }
        etapa("tarefas das threads", t);

        // redesenha só o que mudou
        let t0 = Instant::now();
        let desenhou = janela.draw_if_needed(|r| tela.desenhar(r));
        if desenhou {
            quadros += 1;
            let toque = entrada.ultimo_toque.take();
            if let Some(t) = toque {
                eprintln!(
                    "toque: kernel->leitura {:.1} ms, leitura->tela {:.1} ms (desenho {:.1} ms)",
                    entrada.atraso_evento.take().map(|d| d.as_secs_f64() * 1000.0).unwrap_or(-1.0),
                    t.elapsed().as_secs_f64() * 1000.0,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
            } else if t0.elapsed() > Duration::from_millis(20) {
                eprintln!("quadro lento: desenho {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
            }
        }

        // espera entrada, tarefa ou o próximo timer do Slint
        let t = Instant::now();
        entrada.buscar();
        etapa("buscar dispositivos", t);
        let mut fds: Vec<libc::pollfd> = entrada
            .fds()
            .into_iter()
            .chain(std::iter::once(pipe[0]))
            .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        let espera = if janela.has_active_animations() {
            Duration::ZERO
        } else {
            slint::platform::duration_until_next_timer_update()
                .unwrap_or(Duration::from_millis(500))
                .min(Duration::from_millis(500))
        };
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, espera.as_millis() as i32) };

        let t_ent = Instant::now();
        let mut houve_entrada = false;
        for p in &fds {
            if p.revents == 0 {
                continue;
            }
            if p.fd == pipe[0] {
                let mut lixo = [0u8; 64];
                unsafe { libc::read(pipe[0], lixo.as_mut_ptr() as *mut _, lixo.len()) };
                continue;
            }
            for ev in entrada.ler(p.fd, &tela) {
                houve_entrada = true;
                janela.window().dispatch_event(ev);
            }
        }
        etapa("ler e despachar entrada", t_ent);
        let t = Instant::now();
        ui.set_mouse_visivel(entrada.mouse_usado);
        ui.set_mouse_x(entrada.mx);
        ui.set_mouse_y(entrada.my);

        if houve_entrada {
            if boost_ate.is_none() {
                cpu_min(true);
            }
            boost_ate = Some(Instant::now() + Duration::from_secs(3));
        } else if boost_ate.is_some_and(|t| Instant::now() > t) {
            boost_ate = None;
            cpu_min(false);
        }

        etapa("mouse e boost", t);
        let t = Instant::now();
        // gira conforme o lado em que o celular está (decidido pela thread do acelerômetro)
        let alvo = match giro.load(Ordering::Relaxed) {
            90 => Some(tela::Rotacao::R90),
            27 => Some(tela::Rotacao::R270),
            _ => None,
        };
        if let Some(r) = alvo.filter(|r| *r != tela.rot) {
            tela.rot = r;
            let _ = std::fs::write(ROT_FILE, if r == tela::Rotacao::R90 { "90" } else { "270" });
            // girou: a próxima tela precisa ser desenhada inteira
            tela.redesenhar_tudo = true;
            janela.request_redraw();
        }

        etapa("acelerômetro", t);
        if tela.trocou() {
            eprintln!("framebuffer trocou (driver DRM assumiu): reiniciando");
            std::process::exit(1); // o OpenRC (ou o laço de quem chamou) sobe de novo
        }
    }
}
