//! DOOM rodando dentro do revive-ui.
//!
//! O motor (doomgeneric, em C) é compilado para uma biblioteca estática pelo build.rs
//! e aqui ficam as funções que ele chama: DG_Init, DG_DrawFrame, DG_SleepMs,
//! DG_GetTicksMs, DG_GetKey e DG_SetWindowTitle. Este arquivo é o porte do antigo
//! backend `doom/revive/doomgeneric_revive.c`, que continua no repositório como
//! referência e para compilar o binário avulso.
//!
//! O jogo roda numa thread própria. A interface não para: enquanto o DOOM está
//! ativo o laço do main só dorme, e quando a pessoa toca no "✕" o jogo congela
//! dentro de DG_DrawFrame (Condvar) e o Revive volta para o início. Ao abrir o
//! app de novo, o mesmo jogo continua de onde parou.
//!
//! O "Quit" do menu do DOOM chama exit(), que o build.rs troca por
//! revive_doom_sair: o motor não sabe voltar, então a interface volta para o
//! início e um próximo abrir() reinicia o revive-ui (execv) para começar do zero.

use std::collections::VecDeque;
use std::ffi::{c_char, CString};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use memmap2::MmapMut;

// ------------------------------------------------------------------ motor C

unsafe extern "C" {
    fn doomgeneric_Create(argc: i32, argv: *mut *mut c_char);
    fn doomgeneric_Tick();
    static mut DG_ScreenBuffer: *mut u32;
}

// resolução de doom/engine/doomgeneric.h
const RESX: i32 = 640;
const RESY: i32 = 400;

const IWAD_PROPRIO: &str = "/var/lib/revive/doom/doom2.wad";
const IWAD_FREEDOOM: &str = "/usr/share/games/doom/freedoom2.wad";
const ROT_FILE: &str = "/var/lib/revive/rotation";

// ------------------------------------------------------- estado global

static ATIVO: AtomicBool = AtomicBool::new(false);
static SAIU: AtomicBool = AtomicBool::new(false);
static ENCERRADO: AtomicBool = AtomicBool::new(false);
static INICIADO: AtomicBool = AtomicBool::new(false);

/// true = jogo congelado (a thread do DOOM espera na Condvar)
static PAUSADO: Mutex<bool> = Mutex::new(true);
static ACORDA: Condvar = Condvar::new();

static FB: Mutex<Option<Fb>> = Mutex::new(None);
static ENTRADA: Mutex<Entrada> = Mutex::new(Entrada::nova());

// geometria do framebuffer publicada para a leitura do touch (sem lock)
static PW: AtomicI32 = AtomicI32::new(720);
static PH: AtomicI32 = AtomicI32::new(1280);
static ROT: AtomicI32 = AtomicI32::new(270);
static LW: AtomicI32 = AtomicI32::new(1280);
static LH: AtomicI32 = AtomicI32::new(720);

// ------------------------------------------------------------ interface

/// Chamado pela tela do DOOM (app.rs). Na primeira vez sobe o motor; depois só
/// reativa o jogo que estava pausado.
pub fn abrir() {
    if ENCERRADO.load(SeqCst) {
        reiniciar();
    }

    // a rotação pode ter mudado enquanto o jogo estava pausado: refaz o mapeamento
    match Fb::abrir() {
        Ok(fb) => *FB.lock().unwrap() = Some(fb),
        Err(e) => eprintln!("doom: framebuffer: {e}"),
    }
    ENTRADA.lock().unwrap().abrir();

    SAIU.store(false, SeqCst);
    ATIVO.store(true, SeqCst);
    {
        let mut pausado = PAUSADO.lock().unwrap();
        *pausado = false;
        ACORDA.notify_all();
    }
    if !INICIADO.swap(true, SeqCst) {
        if let Err(e) = std::thread::Builder::new().name("doom".into()).spawn(thread_jogo) {
            eprintln!("doom: não subiu a thread do jogo: {e}");
            ATIVO.store(false, SeqCst);
            SAIU.store(true, SeqCst);
        }
    }
}

/// O jogo está com a tela (o laço da interface só dorme).
pub fn ativo() -> bool {
    ATIVO.load(SeqCst)
}

/// true uma única vez, depois que o jogo voltou para a interface.
pub fn saiu() -> bool {
    SAIU.swap(false, SeqCst)
}

/// A thread do motor não pode ser reiniciada no mesmo processo: recomeça o
/// revive-ui do zero (mesmo que o script fazia antes com rc-service).
fn reiniciar() -> ! {
    eprintln!("doom: jogo encerrado, reiniciando o revive-ui");
    let exe = CString::new("/proc/self/exe").unwrap();
    let mut args: Vec<CString> = std::env::args_os()
        .map(|a| CString::new(a.to_string_lossy().as_bytes()).unwrap_or_else(|_| CString::new("revive-ui").unwrap()))
        .collect();
    if args.is_empty() {
        args.push(CString::new("revive-ui").unwrap());
    }
    let mut ponteiros: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    ponteiros.push(std::ptr::null());
    unsafe { libc::execv(exe.as_ptr(), ponteiros.as_ptr()) };
    eprintln!("doom: execv: {}", std::io::Error::last_os_error());
    std::process::exit(1);
}

fn thread_jogo() {
    let iwad = if Path::new(IWAD_PROPRIO).exists() { IWAD_PROPRIO } else { IWAD_FREEDOOM };
    eprintln!("doom: abrindo {iwad}");
    // o motor guarda argv para sempre: os CString ficam vazados de propósito
    let args: Vec<CString> = vec![
        CString::new("revive-doom").unwrap(),
        CString::new("-iwad").unwrap(),
        CString::new(iwad).unwrap(),
    ];
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    let argc = argv.len() as i32;
    argv.push(std::ptr::null_mut());
    let ponteiro = argv.as_mut_ptr();
    std::mem::forget(args);
    std::mem::forget(argv);

    // sem chdir: o diretório de trabalho é o do processo inteiro; o doom salva
    // os jogos no diretório padrão dele
    unsafe { doomgeneric_Create(argc, ponteiro) };
    loop {
        unsafe { doomgeneric_Tick() };
    }
}

// ------------------------------------------------- funções que o motor chama

#[unsafe(no_mangle)]
pub extern "C" fn DG_Init() {
    let precisa = FB.lock().unwrap().is_none();
    if precisa {
        match Fb::abrir() {
            Ok(fb) => *FB.lock().unwrap() = Some(fb),
            Err(e) => eprintln!("doom: framebuffer: {e}"),
        }
    }
    ENTRADA.lock().unwrap().abrir();
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_DrawFrame() {
    // pausado: para aqui, sem busy-wait, até o próximo abrir()
    {
        let mut pausado = PAUSADO.lock().unwrap();
        while *pausado {
            pausado = ACORDA.wait(pausado).unwrap();
        }
    }
    if let Some(fb) = FB.lock().unwrap().as_mut() {
        fb.desenhar();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_SleepMs(ms: u32) {
    std::thread::sleep(Duration::from_millis(ms as u64));
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ((ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000) & 0xffff_ffff) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_GetKey(pressed: *mut i32, key: *mut u8) -> i32 {
    let mut entrada = ENTRADA.lock().unwrap();
    let mut sair = false;
    if entrada.fila.is_empty() {
        sair = entrada.ler();
    }
    if sair {
        drop(entrada);
        pausar();
        return 0;
    }
    match entrada.fila.pop_front() {
        Some(v) => {
            if !pressed.is_null() && !key.is_null() {
                unsafe {
                    *pressed = v >> 8;
                    *key = (v & 0xff) as u8;
                }
            }
            1
        }
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_SetWindowTitle(_title: *const c_char) {
    // não existe gerenciador de janelas num framebuffer
}

/// exit() do motor (build.rs: -Dexit=revive_doom_sair): "Quit" no menu ou erro
/// fatal. Devolve a tela para a interface e para a thread do jogo de vez.
#[unsafe(no_mangle)]
pub extern "C" fn revive_doom_sair(_codigo: i32) -> ! {
    ENTRADA.lock().unwrap().soltar();
    ATIVO.store(false, SeqCst);
    SAIU.store(true, SeqCst);
    ENCERRADO.store(true, SeqCst);
    loop {
        std::thread::park();
    }
}

/// O "✕" do overlay: solta os controles, congela o jogo e devolve a tela.
fn pausar() {
    ENTRADA.lock().unwrap().soltar();
    ATIVO.store(false, SeqCst);
    SAIU.store(true, SeqCst);
    *PAUSADO.lock().unwrap() = true;
}

// ------------------------------------------------------------- framebuffer

// layout em coordenadas "virtuais" 1280x720
const LAND_W: i32 = 1280;
const LAND_H: i32 = 720;

const DPAD_CX: i32 = 180;
const DPAD_CY: i32 = 540;
const DPAD_R: i32 = 150;

const FIRE_CX: i32 = 1110;
const FIRE_CY: i32 = 560;
const FIRE_R: i32 = 110;

const USE_CX: i32 = 940;
const USE_CY: i32 = 640;
const USE_R: i32 = 70;

const WEAPON_CX: i32 = 1180;
const WEAPON_CY: i32 = 380;
const WEAPON_R: i32 = 60;

// "✕" sai do jogo; o ESC andou para a direita
const SAI_X0: i32 = 10;
const SAI_Y0: i32 = 10;
const SAI_X1: i32 = 90;
const SAI_Y1: i32 = 80;

const ESC_X0: i32 = 100;
const ESC_Y0: i32 = 10;
const ESC_X1: i32 = 240;
const ESC_Y1: i32 = 80;

const MAP_X0: i32 = 1130;
const MAP_Y0: i32 = 10;
const MAP_X1: i32 = 1270;
const MAP_Y1: i32 = 80;

const ENTER_X0: i32 = 990;
const ENTER_Y0: i32 = 10;
const ENTER_X1: i32 = 1120;
const ENTER_Y1: i32 = 80;

/// 35% de branco, em ponto fixo 8.8
const OVERLAY_ALPHA: u32 = 89;
const OVERLAY_BLACK: u32 = (255 * OVERLAY_ALPHA) >> 8;

const DEADZONE2: i32 = 30 * 30;

struct Fb {
    mmap: MmapMut,
    bpp: usize,
    stride: usize,
    pw: i32,
    ph: i32,
    rot: i32,
    lw: i32,
    lh: i32,
    /// canvas em paisagem (0x00RRGGBB)
    canvas: Vec<u32>,
    /// 1 = passar o branco do overlay
    mask: Vec<u8>,
    dw: i32,
    dh: i32,
    x0: i32,
    y0: i32,
    colmap: Vec<usize>,
    rowmap: Vec<usize>,
}

fn ler(path: &str) -> std::io::Result<String> {
    std::fs::read_to_string(path).map(|s| s.trim().to_string())
}

fn rotacao() -> i32 {
    let rot = ler(ROT_FILE).ok().and_then(|s| s.parse::<i32>().ok()).unwrap_or(270);
    if rot != 90 && rot != 270 {
        eprintln!("doom: rotação inválida '{rot}', usando 270");
        return 270;
    }
    rot
}

impl Fb {
    fn abrir() -> std::io::Result<Fb> {
        let sys = "/sys/class/graphics/fb0";
        let tam = ler(&format!("{sys}/virtual_size"))?;
        let (w, h) = tam.split_once(',').unwrap_or(("720", "1280"));
        let pw: i32 = w.trim().parse().unwrap_or(720);
        let ph: i32 = h.trim().parse().unwrap_or(1280);
        let bpp: usize = ler(&format!("{sys}/bits_per_pixel"))?.parse().unwrap_or(24);
        if bpp != 24 && bpp != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("framebuffer de {bpp} bpp não suportado"),
            ));
        }
        let stride: usize = ler(&format!("{sys}/stride"))
            .unwrap_or_default()
            .parse()
            .unwrap_or_else(|_| pw as usize * (bpp / 8));
        let f = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        // /dev/fb0 é um dispositivo: o tamanho não vem do arquivo, vem do stride
        let mmap = unsafe { memmap2::MmapOptions::new().len(stride * ph as usize).map_mut(&f)? };

        let mut rot = rotacao();
        if pw > ph {
            eprintln!("doom: framebuffer já em paisagem, sem rotação");
            rot = 0;
        }
        let (lw, lh) = if rot == 0 { (pw, ph) } else { (ph, pw) };

        let n = lw as usize * lh as usize;
        let mut fb = Fb {
            mmap,
            bpp,
            stride,
            pw,
            ph,
            rot,
            lw,
            lh,
            canvas: vec![0; n],
            mask: vec![0; n],
            dw: 0,
            dh: 0,
            x0: 0,
            y0: 0,
            colmap: Vec::new(),
            rowmap: Vec::new(),
        };
        fb.montar();

        PW.store(pw, SeqCst);
        PH.store(ph, SeqCst);
        ROT.store(rot, SeqCst);
        LW.store(lw, SeqCst);
        LH.store(lh, SeqCst);

        fb.present();
        eprintln!(
            "doom: fb {pw}x{ph} {bpp}bpp stride {stride} -> paisagem {lw}x{lh} rotação {rot}, jogo {}x{} em {},{}, alvo {}x{}",
            fb.dw, fb.dh, fb.x0, fb.y0, LAND_W, LAND_H
        );
        Ok(fb)
    }

    /// Escala 640x400 dentro da paisagem + o desenho dos controles (uma vez só).
    fn montar(&mut self) {
        self.dh = self.lh;
        self.dw = (self.lh as i64 * RESX as i64 / RESY as i64) as i32;
        if self.dw > self.lw {
            self.dw = self.lw;
            self.dh = (self.lw as i64 * RESY as i64 / RESX as i64) as i32;
        }
        self.x0 = (self.lw - self.dw) / 2;
        self.y0 = (self.lh - self.dh) / 2;

        self.colmap = (0..self.dw)
            .map(|d| (d as i64 * RESX as i64 / self.dw as i64) as usize)
            .collect();
        self.rowmap = (0..self.dh)
            .map(|d| (d as i64 * RESY as i64 / self.dh as i64) as usize)
            .collect();

        // direcional
        self.mring(DPAD_CX, DPAD_CY, DPAD_R, 10);
        self.mtri(DPAD_CX, DPAD_CY - 135, DPAD_CX - 38, DPAD_CY - 70, DPAD_CX + 38, DPAD_CY - 70);
        self.mtri(DPAD_CX - 135, DPAD_CY, DPAD_CX - 70, DPAD_CY - 38, DPAD_CX - 70, DPAD_CY + 38);
        self.mtri(DPAD_CX, DPAD_CY + 135, DPAD_CX + 38, DPAD_CY + 70, DPAD_CX - 38, DPAD_CY + 70);
        self.mtri(DPAD_CX + 135, DPAD_CY, DPAD_CX + 70, DPAD_CY + 38, DPAD_CX + 70, DPAD_CY - 38);

        // ATIRAR: círculo grande com o gatilho no meio
        self.mring(FIRE_CX, FIRE_CY, FIRE_R, 9);
        self.mdisc(FIRE_CX, FIRE_CY, 40);

        // USAR: círculo pequeno com anel interno
        self.mring(USE_CX, USE_CY, USE_R, 8);
        self.mring(USE_CX, USE_CY, 34, 8);

        // ARMA: círculo com três barras de pente
        self.mring(WEAPON_CX, WEAPON_CY, WEAPON_R, 8);
        self.mfill(WEAPON_CX - 24, WEAPON_CY - 26, WEAPON_CX - 14, WEAPON_CY + 26);
        self.mfill(WEAPON_CX - 5, WEAPON_CY - 26, WEAPON_CX + 5, WEAPON_CY + 26);
        self.mfill(WEAPON_CX + 14, WEAPON_CY - 26, WEAPON_CX + 24, WEAPON_CY + 26);

        // barra de cima: ✕, ESC, MAPA, ENTER
        self.mrect(SAI_X0, SAI_Y0, SAI_X1, SAI_Y1, 6);
        self.mrect(ESC_X0, ESC_Y0, ESC_X1, ESC_Y1, 6);
        self.mrect(MAP_X0, MAP_Y0, MAP_X1, MAP_Y1, 6);
        self.mrect(ENTER_X0, ENTER_Y0, ENTER_X1, ENTER_Y1, 6);

        // fundo estático: preto e o overlay fora do retângulo do jogo (dentro
        // dele o desenho é refeito a cada quadro)
        for y in 0..self.lh {
            for x in 0..self.lw {
                let dentro = x >= self.x0 && x < self.x0 + self.dw && y >= self.y0 && y < self.y0 + self.dh;
                if dentro {
                    continue;
                }
                let i = y as usize * self.lw as usize + x as usize;
                self.canvas[i] = if self.mask[i] != 0 { OVERLAY_BLACK } else { 0 };
            }
        }
    }

    fn mset(&mut self, vx: i32, vy: i32) {
        let x = (vx as i64 * self.lw as i64 / LAND_W as i64) as i32;
        let y = (vy as i64 * self.lh as i64 / LAND_H as i64) as i32;
        if x >= 0 && y >= 0 && x < self.lw && y < self.lh {
            let i = y as usize * self.lw as usize + x as usize;
            self.mask[i] = 1;
        }
    }

    fn mring(&mut self, cx: i32, cy: i32, r: i32, t: i32) {
        let ro = r + t / 2;
        let ri = (r - t / 2).max(0);
        for y in (cy - ro - 1)..=(cy + ro + 1) {
            for x in (cx - ro - 1)..=(cx + ro + 1) {
                let dx = x - cx;
                let dy = y - cy;
                let d2 = dx as i64 * dx as i64 + dy as i64 * dy as i64;
                if d2 <= (ro as i64) * (ro as i64) && d2 >= (ri as i64) * (ri as i64) {
                    self.mset(x, y);
                }
            }
        }
    }

    fn mdisc(&mut self, cx: i32, cy: i32, r: i32) {
        for y in (cy - r)..=(cy + r) {
            for x in (cx - r)..=(cx + r) {
                let dx = x - cx;
                let dy = y - cy;
                if (dx as i64) * (dx as i64) + (dy as i64) * (dy as i64) <= (r as i64) * (r as i64) {
                    self.mset(x, y);
                }
            }
        }
    }

    fn mrect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, t: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                if x < x0 + t || x > x1 - t || y < y0 + t || y > y1 - t {
                    self.mset(x, y);
                }
            }
        }
    }

    fn mfill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.mset(x, y);
            }
        }
    }

    fn mtri(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, x2: i32, y2: i32) {
        let minx = x0.min(x1).min(x2);
        let maxx = x0.max(x1).max(x2);
        let miny = y0.min(y1).min(y2);
        let maxy = y0.max(y1).max(y2);
        for y in miny..=maxy {
            for x in minx..=maxx {
                let e0 = borda(x0, y0, x1, y1, x, y);
                let e1 = borda(x1, y1, x2, y2, x, y);
                let e2 = borda(x2, y2, x0, y0, x, y);
                if (e0 >= 0 && e1 >= 0 && e2 >= 0) || (e0 <= 0 && e1 <= 0 && e2 <= 0) {
                    self.mset(x, y);
                }
            }
        }
    }

    fn desenhar(&mut self) {
        self.escala();
        self.present();
    }

    /// 640x400 -> retângulo do canvas, com o branco do overlay por cima.
    fn escala(&mut self) {
        let src = unsafe { DG_ScreenBuffer };
        if src.is_null() {
            return;
        }
        let lw = self.lw as usize;
        let x0 = self.x0 as usize;
        for dy in 0..self.dh as usize {
            let sy = self.rowmap[dy];
            let ly = self.y0 as usize + dy;
            for dx in 0..self.dw as usize {
                let sx = self.colmap[dx];
                let i = ly * lw + x0 + dx;
                let mut c = unsafe { *src.add(sy * RESX as usize + sx) };
                if self.mask[i] != 0 {
                    c = clarear(c);
                }
                self.canvas[i] = c;
            }
        }
    }

    /// Uma passada de rotação: canvas 0x00RRGGBB -> /dev/fb0.
    fn present(&mut self) {
        let stride = self.stride;
        let mem = self.mmap.as_mut_ptr();
        let canvas = &self.canvas;
        let (pw, ph, lw, lh) = (self.pw as usize, self.ph as usize, self.lw as usize, self.lh as usize);
        let (rot, bpp) = (self.rot, self.bpp);
        // 32 bpp: o byte que sobra fica em 0xff (o painel do DRM é BGRX)
        let alpha: u32 = if bpp == 32 { 0xff00_0000 } else { 0 };

        unsafe {
            if rot == 0 {
                for y in 0..lh {
                    let s = &canvas[y * lw..y * lw + lw];
                    if bpp == 32 {
                        let o = mem.add(y * stride) as *mut u32;
                        for x in 0..lw {
                            o.add(x).write_unaligned(s[x] | alpha);
                        }
                    } else {
                        let o = mem.add(y * stride);
                        for x in 0..lw {
                            let c = s[x];
                            o.add(x * 3).write(c as u8);
                            o.add(x * 3 + 1).write((c >> 8) as u8);
                            o.add(x * 3 + 2).write((c >> 16) as u8);
                        }
                    }
                }
                return;
            }

            for py in 0..ph {
                let x = if rot == 90 { ph - 1 - py } else { py };
                let o = mem.add(py * stride);
                if bpp == 32 {
                    let o = o as *mut u32;
                    for px in 0..pw {
                        let i = if rot == 90 { x + px * lw } else { x + (pw - 1 - px) * lw };
                        o.add(px).write_unaligned(canvas[i] | alpha);
                    }
                } else {
                    for px in 0..pw {
                        let i = if rot == 90 { x + px * lw } else { x + (pw - 1 - px) * lw };
                        let c = canvas[i];
                        o.add(px * 3).write(c as u8);
                        o.add(px * 3 + 1).write((c >> 8) as u8);
                        o.add(px * 3 + 2).write((c >> 16) as u8);
                    }
                }
            }
        }
    }
}

fn borda(ax: i32, ay: i32, bx: i32, by: i32, px: i32, py: i32) -> i64 {
    (bx - ax) as i64 * (py - ay) as i64 - (by - ay) as i64 * (px - ax) as i64
}

fn clarear(c: u32) -> u32 {
    let mut r = (c >> 16) & 0xff;
    let mut g = (c >> 8) & 0xff;
    let mut b = c & 0xff;
    r += ((255 - r) * OVERLAY_ALPHA) >> 8;
    g += ((255 - g) * OVERLAY_ALPHA) >> 8;
    b += ((255 - b) * OVERLAY_ALPHA) >> 8;
    (r << 16) | (g << 8) | b
}

// ------------------------------------------------------------------ entrada

// teclas do DOOM (doom/engine/doomkeys.h)
const KEY_RIGHTARROW: i32 = 0xae;
const KEY_LEFTARROW: i32 = 0xac;
const KEY_UPARROW: i32 = 0xad;
const KEY_DOWNARROW: i32 = 0xaf;
const KEY_USE: i32 = 0xa2;
const KEY_FIRE: i32 = 0xa3;
const KEY_ESCAPE: i32 = 27;
const KEY_ENTER: i32 = 13;
const KEY_TAB: i32 = 9;
const KEY_RSHIFT: i32 = 0x80 + 0x36;
const KEY_LALT: i32 = 0x80 + 0x38;

// evdev (linux/input-event-codes.h); nomes diferentes das teclas do DOOM
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const BTN_TOUCH: u16 = 0x14a;
const BTN_SOUTH: u16 = 0x130;
const BTN_EAST: u16 = 0x131;
const BTN_START: u16 = 0x13b;
const KEY_A: u16 = 30;
const KEY_ESC: u16 = 1;
const KEY_1: u16 = 2;
const KEY_TAB_EV: u16 = 15;
const KEY_Y: u16 = 21;
const KEY_ENTER_EV: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_N: u16 = 49;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_SPACE: u16 = 57;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_UP: u16 = 103;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_DOWN: u16 = 108;

/// Doom keys que o overlay de toque pode manter pressionadas.
const GERIDAS: [i32; 9] = [
    KEY_UPARROW,
    KEY_DOWNARROW,
    KEY_LEFTARROW,
    KEY_RIGHTARROW,
    KEY_FIRE,
    KEY_USE,
    KEY_ESCAPE,
    KEY_TAB,
    KEY_ENTER,
];
const NGERIDAS: usize = GERIDAS.len();

const MAX_SLOTS: usize = 16;
/// um lugar da fila fica vazio, como no backend C
const FILA_MAX: usize = 64;

fn map_key(code: u16) -> Option<i32> {
    Some(match code {
        KEY_UP => KEY_UPARROW,
        KEY_DOWN => KEY_DOWNARROW,
        KEY_LEFT => KEY_LEFTARROW,
        KEY_RIGHT => KEY_RIGHTARROW,
        KEY_LEFTCTRL | KEY_RIGHTCTRL => KEY_FIRE,
        KEY_SPACE => KEY_USE,
        KEY_ESC => KEY_ESCAPE,
        KEY_ENTER_EV => KEY_ENTER,
        KEY_TAB_EV => KEY_TAB,
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => KEY_RSHIFT,
        KEY_LEFTALT | KEY_RIGHTALT => KEY_LALT,
        KEY_1 => i32::from(b'1'),
        3 => i32::from(b'2'),
        4 => i32::from(b'3'),
        5 => i32::from(b'4'),
        6 => i32::from(b'5'),
        7 => i32::from(b'6'),
        8 => i32::from(b'7'),
        KEY_Y => i32::from(b'y'),
        KEY_N => i32::from(b'n'),
        // gamepad
        BTN_SOUTH => KEY_FIRE,
        BTN_EAST => KEY_USE,
        BTN_START => KEY_ESCAPE,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct Slot {
    ativo: bool,
    x: i32,
    y: i32,
    /// estava no botão de arma (borda: troca de arma uma vez por toque)
    arma: bool,
    /// estava no "✕"
    sair: bool,
}

impl Default for Slot {
    fn default() -> Slot {
        Slot { ativo: false, x: 0, y: 0, arma: false, sair: false }
    }
}

struct Dev {
    dev: evdev::Device,
    #[allow(dead_code)]
    toque: bool,
    /// protocolo B (ABS_MT_SLOT) já visto
    tem_slot: bool,
    btn_touch: bool,
    slot_atual: usize,
    hat: [i32; 2],
    slots: [Slot; MAX_SLOTS],
}

struct Entrada {
    devs: Vec<Dev>,
    /// (pressionada << 8) | tecla
    fila: VecDeque<i32>,
    tocados: [bool; NGERIDAS],
    /// próxima arma escolhida pelo botão ARMA
    arma: i32,
    /// os /dev/input/event* já estão abertos (ficam abertos entre pausas)
    abertos: bool,
}

impl Entrada {
    const fn nova() -> Entrada {
        Entrada { devs: Vec::new(), fila: VecDeque::new(), tocados: [false; NGERIDAS], arma: 1, abertos: false }
    }

    /// Abre os dispositivos (uma vez) e agarra touch, teclado e gamepad.
    fn abrir(&mut self) {
        if !self.abertos {
            self.abertos = true;
            self.escanear();
        }
        for i in 0..self.devs.len() {
            self.grab(i, true);
        }
    }

    /// Solta os grabs: a interface volta a receber a entrada.
    fn soltar(&mut self) {
        for i in 0..self.devs.len() {
            self.grab(i, false);
        }
    }

    fn grab(&self, i: usize, pegar: bool) {
        // EVIOCGRAB = _IOW('E', 0x90, int)
        let pedido = ((1u32 << 30) | (4u32 << 16) | (u32::from(b'E') << 8) | 0x90) as libc::c_ulong;
        let r = unsafe { libc::ioctl(self.devs[i].dev.as_raw_fd(), pedido as _, pegar as libc::c_int) };
        if r < 0 {
            eprintln!("doom: EVIOCGRAB {}: {}", pegar, std::io::Error::last_os_error());
        }
    }

    fn escanear(&mut self) {
        let Ok(dir) = std::fs::read_dir("/dev/input") else {
            eprintln!("doom: abrir /dev/input: {}", std::io::Error::last_os_error());
            return;
        };
        for entrada in dir.flatten() {
            let nome = entrada.file_name();
            let Some(nome) = nome.to_str() else { continue };
            if !nome.starts_with("event") {
                continue;
            }
            let caminho = entrada.path();
            let Ok(dev) = evdev::Device::open(&caminho) else { continue };
            // leitura sem bloquear: o motor lê quando quer
            unsafe {
                let flags = libc::fcntl(dev.as_raw_fd(), libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(dev.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
            let toque = dev
                .supported_absolute_axes()
                .map(|a| a.contains(evdev::AbsoluteAxisCode(ABS_MT_POSITION_X)))
                .unwrap_or(false);
            let teclas = dev.supported_keys();
            let tem = |k: u16| teclas.map(|t| t.contains(evdev::KeyCode(k))).unwrap_or(false);
            let teclado = tem(KEY_A) || tem(BTN_SOUTH);
            if !toque && !teclado {
                continue;
            }
            eprintln!(
                "doom: entrada {} ({})",
                caminho.display(),
                if toque { "touchscreen" } else { "teclado/gamepad" }
            );
            self.devs.push(Dev {
                dev,
                toque,
                tem_slot: false,
                btn_touch: false,
                slot_atual: 0,
                hat: [0, 0],
                slots: [Slot::default(); MAX_SLOTS],
            });
        }
    }

    fn enfileirar(&mut self, premida: bool, key: i32) {
        if self.fila.len() + 1 >= FILA_MAX {
            return; // cheia: descarta, nunca trava o desenho
        }
        self.fila.push_back(((premida as i32) << 8) | (key & 0xff));
    }

    /// Lê o que os dispositivos têm e devolve true se a pessoa tocou no "✕".
    fn ler(&mut self) -> bool {
        let mut sujo = false;
        let mut sair = false;

        for i in 0..self.devs.len() {
            let mut eventos: Vec<(u16, u16, i32)> = Vec::new();
            if let Ok(stream) = self.devs[i].dev.fetch_events() {
                for e in stream {
                    eventos.push((e.event_type().0, e.code(), e.value()));
                }
            }
            for (tipo, code, valor) in eventos {
                match tipo {
                    EV_ABS => sujo |= self.abs(i, code, valor),
                    EV_KEY => sujo |= self.tecla(i, code, valor),
                    // avalia os dedos a cada quadro do touchscreen: num toque rápido o
                    // "encostou" e o "soltou" podem chegar no mesmo lote
                    0 if sujo => {
                        sair |= self.atualizar_toque();
                        sujo = false;
                    }
                    _ => {}
                }
            }
        }

        if sujo {
            sair = self.atualizar_toque();
        }
        sair
    }

    fn abs(&mut self, i: usize, code: u16, valor: i32) -> bool {
        match code {
            ABS_HAT0X => {
                self.hat(i, 0, valor, KEY_LEFTARROW, KEY_RIGHTARROW);
                return false;
            }
            ABS_HAT0Y => {
                self.hat(i, 1, valor, KEY_UPARROW, KEY_DOWNARROW);
                return false;
            }
            _ => {}
        }

        let d = &mut self.devs[i];
        let slot = d.slot_atual;
        match code {
            ABS_MT_SLOT => {
                d.tem_slot = true;
                if valor >= 0 && (valor as usize) < MAX_SLOTS {
                    d.slot_atual = valor as usize;
                }
                true
            }
            ABS_MT_TRACKING_ID => {
                if valor < 0 {
                    d.slots[slot].ativo = false;
                    d.slots[slot].arma = false;
                    d.slots[slot].sair = false;
                } else {
                    d.slots[slot].ativo = true;
                }
                true
            }
            ABS_MT_POSITION_X | ABS_MT_POSITION_Y => {
                if code == ABS_MT_POSITION_X {
                    d.slots[slot].x = valor;
                } else {
                    d.slots[slot].y = valor;
                }
                if !d.tem_slot {
                    d.slots[slot].ativo = true;
                    d.slot_atual = 0;
                }
                true
            }
            ABS_X => {
                d.slots[0].x = valor;
                if !d.tem_slot && d.btn_touch {
                    d.slots[0].ativo = true;
                }
                true
            }
            ABS_Y => {
                d.slots[0].y = valor;
                if !d.tem_slot && d.btn_touch {
                    d.slots[0].ativo = true;
                }
                true
            }
            _ => false,
        }
    }

    fn tecla(&mut self, i: usize, code: u16, valor: i32) -> bool {
        if valor == 2 {
            return false; // autorepeat
        }
        let premida = valor != 0;
        if code == BTN_TOUCH {
            let d = &mut self.devs[i];
            d.btn_touch = premida;
            if !d.tem_slot {
                d.slots[0].ativo = premida;
                if !premida {
                    d.slots[0].arma = false;
                    d.slots[0].sair = false;
                }
            }
            return true;
        }
        if let Some(key) = map_key(code) {
            self.enfileirar(premida, key);
        }
        false
    }

    fn hat(&mut self, i: usize, eixo: usize, valor: i32, negativo: i32, positivo: i32) {
        let velho = self.devs[i].hat[eixo];
        if velho == valor {
            return;
        }
        if velho < 0 {
            self.enfileirar(false, negativo);
        }
        if velho > 0 {
            self.enfileirar(false, positivo);
        }
        self.devs[i].hat[eixo] = valor;
        if valor < 0 {
            self.enfileirar(true, negativo);
        }
        if valor > 0 {
            self.enfileirar(true, positivo);
        }
    }

    /// Religa/desliga as teclas conforme onde estão os dedos.
    /// Devolve true quando o "✕" acabou de ser tocado.
    fn atualizar_toque(&mut self) -> bool {
        let mut quer = [false; NGERIDAS];
        let mut trocar_arma = false;
        let mut sair = false;
        let (pw, ph, rot, lw, lh) = (PW.load(SeqCst), PH.load(SeqCst), ROT.load(SeqCst), LW.load(SeqCst), LH.load(SeqCst));

        for d in &mut self.devs {
            for s in 0..MAX_SLOTS {
                if !d.slots[s].ativo {
                    d.slots[s].arma = false;
                    d.slots[s].sair = false;
                    continue;
                }
                let (tx, ty) = para_toque(d.slots[s].x, d.slots[s].y, pw, ph, rot, lw, lh);
                let ponto = alvo(tx, ty);
                for k in ponto.chaves.iter().take(ponto.n) {
                    if let Some(idx) = GERIDAS.iter().position(|g| g == k) {
                        quer[idx] = true;
                    }
                }
                if ponto.arma && !d.slots[s].arma {
                    trocar_arma = true;
                }
                if ponto.sair && !d.slots[s].sair {
                    sair = true;
                }
                d.slots[s].arma = ponto.arma;
                d.slots[s].sair = ponto.sair;
            }
        }

        if trocar_arma {
            let key = i32::from(b'1') + (self.arma - 1);
            self.enfileirar(true, key);
            self.enfileirar(false, key);
            self.arma = self.arma % 7 + 1;
        }

        for i in 0..NGERIDAS {
            if quer[i] != self.tocados[i] {
                self.enfileirar(quer[i], GERIDAS[i]);
                self.tocados[i] = quer[i];
            }
        }

        sair
    }
}

/// físico -> paisagem -> layout virtual 1280x720
fn para_toque(px: i32, py: i32, pw: i32, ph: i32, rot: i32, lw: i32, lh: i32) -> (i32, i32) {
    let (x, y) = match rot {
        90 => (ph - 1 - py, px),
        270 => (py, pw - 1 - px),
        _ => (px, py),
    };
    ((x as i64 * LAND_W as i64 / lw as i64) as i32, (y as i64 * LAND_H as i64 / lh as i64) as i32)
}

fn dentro(x: i32, y: i32, x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
    x >= x0 && x <= x1 && y >= y0 && y <= y1
}

fn dist2(x: i32, y: i32, cx: i32, cy: i32) -> i32 {
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy
}

struct Alvo {
    chaves: [i32; 2],
    n: usize,
    arma: bool,
    sair: bool,
}

/// O que um dedo em (x,y) segura: até duas direções no direcional.
fn alvo(x: i32, y: i32) -> Alvo {
    let mut a = Alvo { chaves: [0; 2], n: 0, arma: false, sair: false };

    if dentro(x, y, SAI_X0, SAI_Y0, SAI_X1, SAI_Y1) {
        a.sair = true;
        return a;
    }
    if dentro(x, y, ESC_X0, ESC_Y0, ESC_X1, ESC_Y1) {
        a.chaves[0] = KEY_ESCAPE;
        a.n = 1;
        return a;
    }
    if dentro(x, y, MAP_X0, MAP_Y0, MAP_X1, MAP_Y1) {
        a.chaves[0] = KEY_TAB;
        a.n = 1;
        return a;
    }
    if dentro(x, y, ENTER_X0, ENTER_Y0, ENTER_X1, ENTER_Y1) {
        a.chaves[0] = KEY_ENTER;
        a.n = 1;
        return a;
    }
    if dist2(x, y, FIRE_CX, FIRE_CY) <= FIRE_R * FIRE_R {
        a.chaves[0] = KEY_FIRE;
        a.n = 1;
        return a;
    }
    if dist2(x, y, USE_CX, USE_CY) <= USE_R * USE_R {
        a.chaves[0] = KEY_USE;
        a.n = 1;
        return a;
    }
    if dist2(x, y, WEAPON_CX, WEAPON_CY) <= WEAPON_R * WEAPON_R {
        a.arma = true;
        return a;
    }

    let dx = x - DPAD_CX;
    let dy = y - DPAD_CY;
    let d2 = dx * dx + dy * dy;
    if d2 > DPAD_R * DPAD_R || d2 < DEADZONE2 {
        return a;
    }

    let ax = dx.abs();
    let ay = dy.abs();
    if ax * 2 > ay {
        a.chaves[a.n] = if dx < 0 { KEY_LEFTARROW } else { KEY_RIGHTARROW };
        a.n += 1;
    }
    if ay * 2 > ax {
        a.chaves[a.n] = if dy < 0 { KEY_UPARROW } else { KEY_DOWNARROW };
        a.n += 1;
    }
    if a.n == 0 {
        a.chaves[a.n] = if ax > ay {
            if dx < 0 { KEY_LEFTARROW } else { KEY_RIGHTARROW }
        } else if dy < 0 {
            KEY_UPARROW
        } else {
            KEY_DOWNARROW
        };
        a.n += 1;
    }
    a
}
