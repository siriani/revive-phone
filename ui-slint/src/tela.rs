//! Framebuffer e entrada (touch, mouse, teclado) para o Slint, sem servidor gráfico.
//!
//! O Slint desenha direto na memória do /dev/fb0, já girado e no formato do painel
//! (BGR 24 bits do framebuffer do bootloader, BGRX 32 bits do DRM, RGB565), e só nas
//! regiões que mudaram desde o quadro anterior.

use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use evdev::Device;
use memmap2::MmapMut;
use slint::platform::software_renderer::{
    PremultipliedRgbaColor, RenderingRotation, RepaintBufferType, Rgb565Pixel, SoftwareRenderer,
    TargetPixel,
};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::LogicalPosition;

// ------------------------------------------------------------------ pixels

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bgr888 {
    b: u8,
    g: u8,
    r: u8,
}

impl TargetPixel for Bgr888 {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = (u8::MAX - c.alpha) as u16;
        self.r = (self.r as u16 * a / 255) as u8 + c.red;
        self.g = (self.g as u16 * a / 255) as u8 + c.green;
        self.b = (self.b as u16 * a / 255) as u8 + c.blue;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Bgr888 { b, g, r }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bgrx8888 {
    b: u8,
    g: u8,
    r: u8,
    x: u8,
}

impl TargetPixel for Bgrx8888 {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = (u8::MAX - c.alpha) as u16;
        self.r = (self.r as u16 * a / 255) as u8 + c.red;
        self.g = (self.g as u16 * a / 255) as u8 + c.green;
        self.b = (self.b as u16 * a / 255) as u8 + c.blue;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Bgrx8888 { b, g, r, x: 0xff }
    }
}

// ------------------------------------------------------------- framebuffer

/// Rotação no sentido da interface antiga: 270 = topo da interface no lado esquerdo
/// do aparelho (Slint: Rotate90, horário); 90 = no lado direito (Slint: Rotate270).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Rotacao {
    R90,
    R270,
}

impl Rotacao {
    fn slint(self) -> RenderingRotation {
        match self {
            Rotacao::R270 => RenderingRotation::Rotate90,
            Rotacao::R90 => RenderingRotation::Rotate270,
        }
    }
}

pub struct Tela {
    mmap: MmapMut,
    pub bpp: usize,
    stride_px: usize,
    /// tamanho do painel (retrato), ex.: 720x1280
    pub pw: u32,
    pub ph: u32,
    pub rot: Rotacao,
    driver: String,
    /// depois de girar, o quadro seguinte é desenhado inteiro (não só o que mudou)
    pub redesenhar_tudo: bool,
}

fn ler(path: &str) -> std::io::Result<String> {
    std::fs::read_to_string(path).map(|s| s.trim().to_string())
}

impl Tela {
    pub fn abrir() -> std::io::Result<Tela> {
        let sys = "/sys/class/graphics/fb0";
        let tam = ler(&format!("{sys}/virtual_size"))?;
        let (w, h) = tam.split_once(',').unwrap_or(("720", "1280"));
        let (pw, ph): (u32, u32) = (w.parse().unwrap_or(720), h.parse().unwrap_or(1280));
        let bpp: usize = ler(&format!("{sys}/bits_per_pixel"))?.parse().unwrap_or(24);
        let stride: usize = ler(&format!("{sys}/stride"))?.parse().unwrap_or(pw as usize * 3);
        let f = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        // /dev/fb0 é um dispositivo: o tamanho não vem do arquivo, vem do stride
        let mmap = unsafe { memmap2::MmapOptions::new().len(stride * ph as usize).map_mut(&f)? };
        // tira o console de texto da tela (KDSETMODE, KD_GRAPHICS)
        if let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty1") {
            unsafe { libc::ioctl(tty.as_raw_fd(), 0x4B3A, 1) };
            std::mem::forget(tty);
        }
        let driver = ler(&format!("{sys}/name")).unwrap_or_default();
        Ok(Tela { mmap, bpp, stride_px: stride / (bpp / 8), pw, ph, rot: Rotacao::R270, driver, redesenhar_tudo: true })
    }

    /// O driver de vídeo de verdade (DRM) assume e cria outro /dev/fb0.
    pub fn trocou(&self) -> bool {
        ler("/sys/class/graphics/fb0/name").unwrap_or_default() != self.driver
    }

    pub fn desenhar(&mut self, r: &SoftwareRenderer) {
        r.set_rendering_rotation(self.rot.slint());
        let tudo = std::mem::take(&mut self.redesenhar_tudo);
        if tudo {
            r.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
        }
        let ptr = self.mmap.as_mut_ptr();
        let len = self.mmap.len();
        unsafe {
            match self.bpp {
                32 => {
                    let buf = std::slice::from_raw_parts_mut(ptr as *mut Bgrx8888, len / 4);
                    r.render(buf, self.stride_px);
                }
                16 => {
                    let buf = std::slice::from_raw_parts_mut(ptr as *mut Rgb565Pixel, len / 2);
                    r.render(buf, self.stride_px);
                }
                _ => {
                    let buf = std::slice::from_raw_parts_mut(ptr as *mut Bgr888, len / 3);
                    r.render(buf, self.stride_px);
                }
            }
        }
        if tudo {
            r.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        }
    }

    /// Ponto da tela física (retrato) -> ponto da interface (paisagem).
    fn para_logico(&self, px: f32, py: f32) -> (f32, f32) {
        let (w, h) = (self.ph as f32, self.pw as f32); // paisagem
        match self.rot {
            Rotacao::R90 => (w - 1.0 - py, px),
            Rotacao::R270 => (py, h - 1.0 - px),
        }
    }
}

// ----------------------------------------------------------------- entrada

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tipo {
    Touch,
    Mouse,
    Teclado,
}

struct Disp {
    dev: Device,
    tipo: Tipo,
    caminho: PathBuf,
    max_x: f32,
    max_y: f32,
}

pub struct Entrada {
    devs: Vec<Disp>,
    ultima_busca: Instant,
    // touch (coordenadas físicas)
    tx: f32,
    ty: f32,
    encostou: bool,
    soltou: bool,
    // mouse (coordenadas lógicas)
    pub mx: f32,
    pub my: f32,
    pub mouse_usado: bool,
    shift: bool,
    caps: bool,
    pub ultimo_toque: Option<Instant>,
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const BTN_TOUCH: u16 = 0x14a;
const BTN_LEFT: u16 = 0x110;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const KEY_A: u16 = 30;
const KEY_Z: u16 = 44;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_CAPSLOCK: u16 = 58;

/// Teclado físico (padrão US): código -> (normal, com shift)
fn tecla(code: u16) -> Option<(char, char)> {
    let linhas: [(u16, &str, &str); 4] = [
        (2, "1234567890-=", "!@#$%^&*()_+"),
        (16, "qwertyuiop[]", "QWERTYUIOP{}"),
        (30, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
        (44, "zxcvbnm,./", "ZXCVBNM<>?"),
    ];
    for (ini, n, s) in linhas {
        if code >= ini && ((code - ini) as usize) < n.chars().count() {
            let i = (code - ini) as usize;
            return Some((n.chars().nth(i)?, s.chars().nth(i)?));
        }
    }
    match code {
        43 => Some(('\\', '|')),
        57 => Some((' ', ' ')),
        _ => None,
    }
}

impl Entrada {
    pub fn nova() -> Entrada {
        let mut e = Entrada {
            devs: vec![],
            ultima_busca: Instant::now() - Duration::from_secs(60),
            tx: 0.0,
            ty: 0.0,
            encostou: false,
            soltou: false,
            mx: 640.0,
            my: 360.0,
            mouse_usado: false,
            shift: false,
            caps: false,
            ultimo_toque: None,
        };
        e.buscar();
        e
    }

    /// Teclados e mouses Bluetooth aparecem e somem: refaz a lista a cada 3 s.
    pub fn buscar(&mut self) {
        if self.ultima_busca.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.ultima_busca = Instant::now();
        let Ok(dir) = std::fs::read_dir("/dev/input") else { return };
        for ent in dir.flatten() {
            let caminho = ent.path();
            let nome = caminho.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !nome.starts_with("event") || self.devs.iter().any(|d| d.caminho == caminho) {
                continue;
            }
            let Ok(dev) = Device::open(&caminho) else { continue };
            let abs = dev.supported_absolute_axes().map(|a| a.contains(evdev::AbsoluteAxisCode(ABS_MT_POSITION_X))).unwrap_or(false);
            let rel = dev.supported_relative_axes().map(|r| r.contains(evdev::RelativeAxisCode(REL_X))).unwrap_or(false);
            let teclas = dev.supported_keys();
            let tem = |k: u16| teclas.map(|t| t.contains(evdev::KeyCode(k))).unwrap_or(false);
            let tipo = if abs {
                Tipo::Touch
            } else if rel && tem(BTN_LEFT) {
                Tipo::Mouse
            } else if tem(KEY_A) && tem(KEY_Z) {
                Tipo::Teclado
            } else {
                continue;
            };
            let (mut max_x, mut max_y) = (720.0, 1280.0);
            if tipo == Tipo::Touch {
                if let Ok(st) = dev.get_abs_state() {
                    max_x = st[ABS_MT_POSITION_X as usize].maximum.max(1) as f32;
                    max_y = st[ABS_MT_POSITION_Y as usize].maximum.max(1) as f32;
                }
            }
            eprintln!("entrada: {:?} {} {}", tipo, dev.name().unwrap_or("?"), caminho.display());
            self.devs.push(Disp { dev, tipo, caminho, max_x, max_y });
        }
    }

    pub fn fds(&self) -> Vec<RawFd> {
        self.devs.iter().map(|d| d.dev.as_raw_fd()).collect()
    }

    /// Lê o dispositivo pronto e devolve eventos do Slint (já na paisagem).
    pub fn ler(&mut self, fd: RawFd, tela: &Tela) -> Vec<WindowEvent> {
        let Some(i) = self.devs.iter().position(|d| d.dev.as_raw_fd() == fd) else { return vec![] };
        let tipo = self.devs[i].tipo;
        let (max_x, max_y) = (self.devs[i].max_x, self.devs[i].max_y);
        let lidos = self.devs[i]
            .dev
            .fetch_events()
            .map(|evs| evs.map(|e| (e.event_type().0, e.code(), e.value())).collect::<Vec<_>>());
        let eventos = match lidos {
            Ok(evs) => evs,
            Err(_) => {
                eprintln!("entrada: {:?} saiu", tipo);
                self.devs.remove(i);
                return vec![];
            }
        };
        let mut saida = vec![];
        for (t, c, v) in eventos {
            match tipo {
                Tipo::Touch => self.touch(t, c, v, max_x, max_y, tela, &mut saida),
                Tipo::Mouse => self.mouse(t, c, v, tela, &mut saida),
                Tipo::Teclado => self.teclado(t, c, v, &mut saida),
            }
        }
        saida
    }

    #[allow(clippy::too_many_arguments)]
    fn touch(&mut self, t: u16, c: u16, v: i32, max_x: f32, max_y: f32, tela: &Tela, out: &mut Vec<WindowEvent>) {
        match (t, c) {
            (EV_ABS, ABS_MT_POSITION_X) => self.tx = v as f32 * tela.pw as f32 / max_x,
            (EV_ABS, ABS_MT_POSITION_Y) => self.ty = v as f32 * tela.ph as f32 / max_y,
            (EV_ABS, ABS_MT_TRACKING_ID) if v >= 0 => self.encostou = true,
            (EV_ABS, ABS_MT_TRACKING_ID) => self.soltou = true,
            (EV_KEY, BTN_TOUCH) if v == 1 => self.encostou = true,
            (EV_KEY, BTN_TOUCH) => self.soltou = true,
            (EV_SYN, SYN_REPORT) => {
                let (x, y) = tela.para_logico(self.tx, self.ty);
                let position = LogicalPosition::new(x, y);
                self.mouse_usado = false;
                out.push(WindowEvent::PointerMoved { position });
                if self.encostou {
                    self.encostou = false;
                    self.ultimo_toque = Some(Instant::now());
                    out.push(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
                }
                if self.soltou {
                    self.soltou = false;
                    out.push(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
                    out.push(WindowEvent::PointerExited);
                }
            }
            _ => {}
        }
    }

    fn mouse(&mut self, t: u16, c: u16, v: i32, tela: &Tela, out: &mut Vec<WindowEvent>) {
        let (w, h) = (tela.ph as f32, tela.pw as f32);
        let position = |s: &Entrada| LogicalPosition::new(s.mx, s.my);
        match (t, c) {
            (EV_REL, REL_X) => {
                self.mx = (self.mx + v as f32 * 2.0).clamp(0.0, w - 1.0);
                self.mouse_usado = true;
                out.push(WindowEvent::PointerMoved { position: position(self) });
            }
            (EV_REL, REL_Y) => {
                self.my = (self.my + v as f32 * 2.0).clamp(0.0, h - 1.0);
                self.mouse_usado = true;
                out.push(WindowEvent::PointerMoved { position: position(self) });
            }
            (EV_KEY, BTN_LEFT) => {
                self.mouse_usado = true;
                self.ultimo_toque = Some(Instant::now());
                let button = PointerEventButton::Left;
                out.push(if v == 1 {
                    WindowEvent::PointerPressed { position: position(self), button }
                } else {
                    WindowEvent::PointerReleased { position: position(self), button }
                });
            }
            _ => {}
        }
    }

    fn teclado(&mut self, t: u16, c: u16, v: i32, out: &mut Vec<WindowEvent>) {
        if t != EV_KEY {
            return;
        }
        if c == KEY_LEFTSHIFT || c == KEY_RIGHTSHIFT {
            self.shift = v != 0;
            return;
        }
        if c == KEY_CAPSLOCK && v == 1 {
            self.caps = !self.caps;
            return;
        }
        let texto: slint::SharedString = match c {
            28 | 96 => Key::Return.into(),
            14 => Key::Backspace.into(),
            1 => Key::Escape.into(),
            15 => Key::Tab.into(),
            _ => match tecla(c) {
                Some((n, s)) => {
                    let maiuscula = self.shift != (self.caps && n.is_alphabetic());
                    (if maiuscula { s } else { n }).to_string().into()
                }
                None => return,
            },
        };
        self.ultimo_toque = Some(Instant::now());
        out.push(match v {
            0 => WindowEvent::KeyReleased { text: texto },
            2 => WindowEvent::KeyPressRepeated { text: texto },
            _ => WindowEvent::KeyPressed { text: texto },
        });
    }
}
