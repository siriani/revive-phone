//! App Câmera: captura da câmera traseira (MT9M114 no CAMSS do MSM8226) por V4L2,
//! em Rust puro (ioctl pela libc, sem crate nova).
//!
//! Caminho da imagem (montado com o `media-ctl`, os mesmos argumentos validados no aparelho):
//! sensor "mt9m114 ifp 3-0048" -> msm_csiphy1 -> msm_csid0 -> msm_ispif0 -> msm_vfe0_rdi0.
//! O RDI grava o que o sensor manda (UYVY8_1X16), sem ISP. O nó de vídeo do RDI é
//! multiplanar (V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 1 plano); o caso de 1 plano só
//! (VIDEO_CAPTURE) também é tratado, decidido pelo VIDIOC_QUERYCAP.
//!
//! A thread de captura converte cada quadro para RGB pela metade (1288x968 -> 644x484,
//! um macropixel = um ponto) e deixa o último num "slot"; a interface é avisada por
//! `upgrade_in_event_loop` e pega o quadro. Enquanto a interface não pegou, os quadros
//! seguintes nem são convertidos (a CPU não trabalha à toa). Fotos, erros e o aviso de
//! "pronta" vão por um canal (mpsc).
//!
//! Ajustes opcionais em /var/lib/revive/camera.conf (chave=valor, # comenta):
//!   pasta=/home/allan/Fotos      onde as fotos são salvas
//!   ordem=uyvy                   ordem dos bytes: uyvy, yuyv, vyuy ou yvyu (padrão: a do driver)
//!   faixa=limitada               limitada (BT.601 16-235) ou cheia (0-255)
//!   girar=0                      0 ou 180
//!   espelhar=0                   1 espelha na horizontal
//!   largura=1288 / altura=968    formato pedido ao pipeline
//!   altura_util=0                corta linhas vazias no pé (0 = detectar sozinho)
//!   sensor=mt9m114 ifp 3-0048    nome da entidade do sensor no media controller
//!   media=/dev/media0            dispositivo do media controller
//!   video=/dev/video0            nó de captura (padrão: o do msm_vfe0_video0, via media-ctl -e)
//!   fps=12                       limite de quadros por segundo da pré-visualização

use std::fs;
use std::io::{BufWriter, Write};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use slint::{Rgb8Pixel, SharedPixelBuffer};

use crate::i18n;

/// Quadro RGB pronto para a interface (Image::from_rgb8).
pub type Quadro = SharedPixelBuffer<Rgb8Pixel>;

const CONF: &str = "/var/lib/revive/camera.conf";
/// Tamanho máximo das miniaturas da galeria (4:3).
pub const MINI_L: u32 = 240;
pub const MINI_A: u32 = 180;

// ------------------------------------------------------------------ ajustes

/// Ordem dos bytes no macropixel de 4 bytes (2 pontos).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ordem {
    Uyvy,
    Yuyv,
    Vyuy,
    Yvyu,
}

impl Ordem {
    fn de_texto(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "uyvy" => Some(Self::Uyvy),
            "yuyv" | "yuy2" => Some(Self::Yuyv),
            "vyuy" => Some(Self::Vyuy),
            "yvyu" => Some(Self::Yvyu),
            _ => None,
        }
    }
    fn de_fourcc(f: u32) -> Option<Self> {
        [Self::Uyvy, Self::Yuyv, Self::Vyuy, Self::Yvyu].into_iter().find(|o| o.fourcc() == f)
    }
    fn fourcc(self) -> u32 {
        match self {
            Self::Uyvy => fourcc(b"UYVY"),
            Self::Yuyv => fourcc(b"YUYV"),
            Self::Vyuy => fourcc(b"VYUY"),
            Self::Yvyu => fourcc(b"YVYU"),
        }
    }
    /// Posições (y0, u, y1, v) dentro do macropixel.
    fn pos(self) -> [usize; 4] {
        match self {
            Self::Uyvy => [1, 0, 3, 2], // U Y0 V Y1
            Self::Yuyv => [0, 1, 2, 3], // Y0 U Y1 V
            Self::Vyuy => [1, 2, 3, 0], // V Y0 U Y1
            Self::Yvyu => [0, 3, 2, 1], // Y0 V Y1 U
        }
    }
}

const fn fourcc(c: &[u8; 4]) -> u32 {
    (c[0] as u32) | ((c[1] as u32) << 8) | ((c[2] as u32) << 16) | ((c[3] as u32) << 24)
}

fn fourcc_texto(f: u32) -> String {
    f.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

#[derive(Clone, Debug)]
pub struct Config {
    pub pasta: PathBuf,
    /// None = a ordem do pixelformat que o driver devolver no G_FMT
    pub ordem: Option<Ordem>,
    pub faixa_cheia: bool,
    pub girar180: bool,
    pub espelhar: bool,
    pub largura: u32,
    pub altura: u32,
    /// 0 = detectar linhas vazias no pé da imagem
    pub altura_util: u32,
    pub sensor: String,
    pub media: String,
    pub video: Option<String>,
    pub fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            pasta: PathBuf::from("/home/allan/Fotos"),
            ordem: None,
            faixa_cheia: false,
            girar180: false,
            espelhar: false,
            largura: 1288,
            altura: 968,
            altura_util: 0,
            sensor: "mt9m114 ifp 3-0048".into(),
            media: "/dev/media0".into(),
            video: None,
            fps: 12,
        }
    }
}

/// Lê /var/lib/revive/camera.conf (tudo opcional).
pub fn config() -> Config {
    let mut c = Config::default();
    let texto = fs::read_to_string(CONF).unwrap_or_default();
    for linha in texto.lines() {
        let linha = linha.trim();
        if linha.is_empty() || linha.starts_with('#') {
            continue;
        }
        let Some((k, v)) = linha.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let sim = matches!(v, "1" | "sim" | "true" | "yes");
        match k {
            "pasta" if !v.is_empty() => c.pasta = PathBuf::from(v),
            "ordem" => c.ordem = Ordem::de_texto(v),
            "faixa" => c.faixa_cheia = v == "cheia" || v == "full",
            "girar" => c.girar180 = v == "180",
            "espelhar" => c.espelhar = sim,
            "largura" => c.largura = v.parse().unwrap_or(c.largura),
            "altura" => c.altura = v.parse().unwrap_or(c.altura),
            "altura_util" => c.altura_util = v.parse().unwrap_or(0),
            "sensor" if !v.is_empty() => c.sensor = v.to_string(),
            "media" if !v.is_empty() => c.media = v.to_string(),
            "video" if !v.is_empty() => c.video = Some(v.to_string()),
            "fps" => c.fps = v.parse::<u32>().unwrap_or(c.fps).clamp(1, 30),
            _ => {}
        }
    }
    c.largura &= !1; // macropixel = 2 pontos
    c
}

// ------------------------------------------------------------------ V4L2 (videodev2.h)

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
const V4L2_MEMORY_MMAP: u32 = 1;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
const V4L2_CAP_VIDEO_CAPTURE_MPLANE: u32 = 0x0000_1000;
const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const VIDEO_MAX_PLANES: usize = 8;

#[allow(dead_code)]
#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

/// struct v4l2_format: o union `fmt` tem 200 bytes e o alinhamento de um ponteiro
/// (por causa do v4l2_window), então 204 bytes em ARM 32 bits e 208 em 64 bits.
/// Os campos são lidos/escritos por deslocamento (v4l2_pix_format_mplane é packed).
#[allow(dead_code)]
#[repr(C)]
struct Formato {
    tipo: u32,
    _alinhar: [usize; 0],
    raw: [u8; 200],
}

impl Formato {
    fn novo(tipo: u32) -> Self {
        Formato { tipo, _alinhar: [], raw: [0; 200] }
    }
    fn ler(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.raw[off..off + 4].try_into().unwrap())
    }
    fn escrever(&mut self, off: usize, v: u32) {
        self.raw[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
}

// deslocamentos em v4l2_pix_format_mplane
const MP_LARGURA: usize = 0;
const MP_ALTURA: usize = 4;
const MP_PIXFMT: usize = 8;
const MP_FIELD: usize = 12;
const MP_PLANO0_TAMANHO: usize = 20; // plane_fmt[0].sizeimage
const MP_PLANO0_BPL: usize = 24; // plane_fmt[0].bytesperline
const MP_NUM_PLANOS: usize = 20 + 20 * VIDEO_MAX_PLANES; // u8
// deslocamentos em v4l2_pix_format (1 plano)
const SP_LARGURA: usize = 0;
const SP_ALTURA: usize = 4;
const SP_PIXFMT: usize = 8;
const SP_FIELD: usize = 12;
const SP_BPL: usize = 16;
const SP_TAMANHO: usize = 20;

#[allow(dead_code)]
#[repr(C)]
#[derive(Default)]
struct ReqBufs {
    count: u32,
    tipo: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

/// struct v4l2_plane (o union `m` tem o tamanho de unsigned long).
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
struct Plano {
    bytesused: u32,
    length: u32,
    m: usize, // mem_offset (u32) no começo, little-endian
    data_offset: u32,
    reserved: [u32; 11],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
struct Timecode {
    tipo: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

/// struct v4l2_buffer com timestamp de 64 bits (long long; em ARM 32 bits é a variante
/// "time64" que o kernel aceita desde o 5.6; o número do ioctl leva o tamanho certo).
#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
struct Buffer {
    index: u32,
    tipo: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    ts_seg: i64,
    ts_usec: i64,
    timecode: Timecode,
    sequence: u32,
    memory: u32,
    m: usize, // offset (1 plano) ou ponteiro para os planos (multiplanar)
    length: u32,
    reserved2: u32,
    request_fd: u32,
}

// tamanhos do videodev2.h conferidos na compilação (entram no número de cada ioctl)
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Formato>() == 208 && size_of::<Buffer>() == 88 && size_of::<Plano>() == 64);
#[cfg(target_arch = "arm")]
const _: () = assert!(size_of::<Formato>() == 204 && size_of::<Buffer>() == 80 && size_of::<Plano>() == 60);
const _: () = assert!(size_of::<Capability>() == 104 && size_of::<ReqBufs>() == 20);

const fn ioc(dir: u32, nr: u32, tamanho: usize) -> u32 {
    (dir << 30) | ((tamanho as u32) << 16) | ((b'V' as u32) << 8) | nr
}
const LER: u32 = 2;
const GRAVAR: u32 = 1;
const VIDIOC_QUERYCAP: u32 = ioc(LER, 0, size_of::<Capability>());
const VIDIOC_G_FMT: u32 = ioc(LER | GRAVAR, 4, size_of::<Formato>());
const VIDIOC_S_FMT: u32 = ioc(LER | GRAVAR, 5, size_of::<Formato>());
const VIDIOC_REQBUFS: u32 = ioc(LER | GRAVAR, 8, size_of::<ReqBufs>());
const VIDIOC_QUERYBUF: u32 = ioc(LER | GRAVAR, 9, size_of::<Buffer>());
const VIDIOC_QBUF: u32 = ioc(LER | GRAVAR, 15, size_of::<Buffer>());
const VIDIOC_DQBUF: u32 = ioc(LER | GRAVAR, 17, size_of::<Buffer>());
const VIDIOC_STREAMON: u32 = ioc(GRAVAR, 18, size_of::<libc::c_int>());
const VIDIOC_STREAMOFF: u32 = ioc(GRAVAR, 19, size_of::<libc::c_int>());

fn xioctl<T>(fd: RawFd, req: u32, arg: *mut T) -> std::io::Result<()> {
    loop {
        let r = unsafe { libc::ioctl(fd, req as _, arg) };
        if r != -1 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    }
}

/// Nó de captura aberto, com os buffers mapeados. O Drop desliga tudo (STREAMOFF).
struct Dispositivo {
    fd: RawFd,
    tipo: u32,
    mplane: bool,
    mapas: Vec<(*mut u8, usize)>,
    ligado: bool,
    largura: usize,
    altura: usize,
    bpl: usize,
    fourcc: u32,
}

// os ponteiros dos mapas só são usados pela thread de captura, dona do Dispositivo
unsafe impl Send for Dispositivo {}

impl Dispositivo {
    fn abrir(caminho: &str, c: &Config) -> Result<Self, String> {
        let cpath = std::ffi::CString::new(caminho).map_err(|e| e.to_string())?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("{caminho}: {}", std::io::Error::last_os_error()));
        }
        let mut d = Dispositivo {
            fd,
            tipo: 0,
            mplane: false,
            mapas: vec![],
            ligado: false,
            largura: 0,
            altura: 0,
            bpl: 0,
            fourcc: 0,
        };

        // multiplanar ou não
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        xioctl(fd, VIDIOC_QUERYCAP, &mut cap).map_err(|e| format!("QUERYCAP: {e}"))?;
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 { cap.device_caps } else { cap.capabilities };
        d.mplane = if caps & V4L2_CAP_VIDEO_CAPTURE_MPLANE != 0 {
            true
        } else if caps & V4L2_CAP_VIDEO_CAPTURE != 0 {
            false
        } else {
            return Err(format!("{caminho}: não é captura de vídeo (caps {caps:#x})"));
        };
        d.tipo = if d.mplane { V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE } else { V4L2_BUF_TYPE_VIDEO_CAPTURE };

        // formato: UYVY (o código UYVY8_1X16 do pipeline; o STREAMON confere os dois)
        let mut f = Formato::novo(d.tipo);
        let pix = Ordem::Uyvy.fourcc();
        if d.mplane {
            f.escrever(MP_LARGURA, c.largura);
            f.escrever(MP_ALTURA, c.altura);
            f.escrever(MP_PIXFMT, pix);
            f.escrever(MP_FIELD, V4L2_FIELD_NONE);
            f.raw[MP_NUM_PLANOS] = 1;
        } else {
            f.escrever(SP_LARGURA, c.largura);
            f.escrever(SP_ALTURA, c.altura);
            f.escrever(SP_PIXFMT, pix);
            f.escrever(SP_FIELD, V4L2_FIELD_NONE);
        }
        xioctl(fd, VIDIOC_S_FMT, &mut f).map_err(|e| format!("S_FMT: {e}"))?;
        // o que vale é o que o driver devolve (bytesperline pode ter preenchimento)
        let mut f = Formato::novo(d.tipo);
        xioctl(fd, VIDIOC_G_FMT, &mut f).map_err(|e| format!("G_FMT: {e}"))?;
        let (w, h, fcc, bpl, tam) = if d.mplane {
            (f.ler(MP_LARGURA), f.ler(MP_ALTURA), f.ler(MP_PIXFMT), f.ler(MP_PLANO0_BPL), f.ler(MP_PLANO0_TAMANHO))
        } else {
            (f.ler(SP_LARGURA), f.ler(SP_ALTURA), f.ler(SP_PIXFMT), f.ler(SP_BPL), f.ler(SP_TAMANHO))
        };
        d.largura = (w & !1) as usize;
        d.altura = h as usize;
        d.fourcc = fcc;
        d.bpl = if bpl as usize >= d.largura * 2 { bpl as usize } else { d.largura * 2 };
        eprintln!(
            "câmera: {caminho} {} {w}x{h} {} bytesperline {bpl} sizeimage {tam}",
            if d.mplane { "MPLANE" } else { "1 plano" },
            fourcc_texto(fcc)
        );
        if d.largura < 2 || d.altura < 2 {
            return Err(format!("formato inválido {w}x{h}"));
        }

        // buffers mmap
        let mut req = ReqBufs { count: 4, tipo: d.tipo, memory: V4L2_MEMORY_MMAP, ..Default::default() };
        xioctl(fd, VIDIOC_REQBUFS, &mut req).map_err(|e| format!("REQBUFS: {e}"))?;
        if req.count < 2 {
            return Err(format!("REQBUFS: só {} buffer(s)", req.count));
        }
        for i in 0..req.count {
            let mut planos: [Plano; 1] = unsafe { std::mem::zeroed() };
            let mut b = d.buffer_vazio(&mut planos);
            b.index = i;
            xioctl(fd, VIDIOC_QUERYBUF, &mut b).map_err(|e| format!("QUERYBUF {i}: {e}"))?;
            let (tamanho, offset) = if d.mplane {
                (planos[0].length as usize, planos[0].m as u32)
            } else {
                (b.length as usize, b.m as u32)
            };
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    tamanho,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    offset as libc::off_t,
                )
            };
            if p == libc::MAP_FAILED {
                return Err(format!("mmap {i}: {}", std::io::Error::last_os_error()));
            }
            d.mapas.push((p as *mut u8, tamanho));
        }
        for i in 0..d.mapas.len() {
            d.devolver(i as u32).map_err(|e| format!("QBUF {i}: {e}"))?;
        }
        let mut t = d.tipo as libc::c_int;
        xioctl(fd, VIDIOC_STREAMON, &mut t).map_err(|e| {
            // EPIPE aqui = formato do nó diferente do formato do pipeline
            format!("STREAMON: {e}")
        })?;
        d.ligado = true;
        Ok(d)
    }

    fn buffer_vazio(&self, planos: &mut [Plano; 1]) -> Buffer {
        let mut b: Buffer = unsafe { std::mem::zeroed() };
        b.tipo = self.tipo;
        b.memory = V4L2_MEMORY_MMAP;
        if self.mplane {
            b.m = planos.as_mut_ptr() as usize;
            b.length = 1;
        }
        b
    }

    fn devolver(&self, i: u32) -> std::io::Result<()> {
        let mut planos: [Plano; 1] = unsafe { std::mem::zeroed() };
        let mut b = self.buffer_vazio(&mut planos);
        b.index = i;
        xioctl(self.fd, VIDIOC_QBUF, &mut b)
    }

    /// Pega um buffer cheio: (índice, início dos dados, fim dos dados). None = ainda não há.
    fn pegar(&self) -> std::io::Result<Option<(usize, usize, usize)>> {
        let mut planos: [Plano; 1] = unsafe { std::mem::zeroed() };
        let mut b = self.buffer_vazio(&mut planos);
        match xioctl(self.fd, VIDIOC_DQBUF, &mut b) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => return Ok(None),
            Err(e) => return Err(e),
        }
        let i = b.index as usize;
        let tamanho = self.mapas.get(i).map(|m| m.1).unwrap_or(0);
        let (ini, fim) = if self.mplane {
            // bytesused inclui o data_offset
            (planos[0].data_offset as usize, planos[0].bytesused as usize)
        } else {
            (0, b.bytesused as usize)
        };
        let fim = if fim == 0 { tamanho } else { fim.min(tamanho) };
        Ok(Some((i, ini.min(fim), fim)))
    }

    fn dados(&self, i: usize, ini: usize, fim: usize) -> &[u8] {
        let (p, _) = self.mapas[i];
        unsafe { std::slice::from_raw_parts(p.add(ini), fim - ini) }
    }
}

impl Drop for Dispositivo {
    fn drop(&mut self) {
        unsafe {
            if self.ligado {
                let mut t = self.tipo as libc::c_int;
                let _ = xioctl(self.fd, VIDIOC_STREAMOFF, &mut t);
            }
            for &(p, n) in &self.mapas {
                libc::munmap(p as *mut libc::c_void, n);
            }
            if !self.mapas.is_empty() {
                let mut req = ReqBufs { count: 0, tipo: self.tipo, memory: V4L2_MEMORY_MMAP, ..Default::default() };
                let _ = xioctl(self.fd, VIDIOC_REQBUFS, &mut req);
            }
            libc::close(self.fd);
        }
    }
}

// ------------------------------------------------------------------ media controller

fn media_ctl(c: &Config, args: &[&str]) -> Result<String, String> {
    let saida = Command::new("media-ctl")
        .arg("-d")
        .arg(&c.media)
        .args(args)
        .output()
        .map_err(|e| format!("media-ctl: {e}"))?;
    if !saida.status.success() {
        return Err(format!(
            "media-ctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&saida.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&saida.stdout).trim().to_string())
}

static MODPROBE: Once = Once::new();

/// Carrega os módulos (uma vez), monta o pipeline e devolve o nó de captura.
fn preparar(c: &Config) -> Result<String, String> {
    if !Path::new(&c.media).exists() {
        MODPROBE.call_once(|| {
            // -a: os três são módulos (sem -a o resto viraria parâmetro do primeiro)
            let r = Command::new("modprobe").args(["-a", "i2c_qcom_cci", "mt9m114", "qcom_camss"]).status();
            eprintln!("câmera: modprobe {r:?}");
        });
        for _ in 0..20 {
            if Path::new(&c.media).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if !Path::new(&c.media).exists() {
        return Err(i18n::t("Câmera indisponível"));
    }
    let links = "\"msm_csiphy1\":1->\"msm_csid0\":0[1],\
                 \"msm_csid0\":1->\"msm_ispif0\":0[1],\
                 \"msm_ispif0\":1->\"msm_vfe0_rdi0\":0[1]";
    media_ctl(c, &["-l", links])?;
    let f = format!("[fmt:UYVY8_1X16/{}x{}]", c.largura, c.altura);
    let formatos = format!(
        "\"{s}\":1{f},\"msm_csiphy1\":0{f},\"msm_csid0\":0{f},\"msm_ispif0\":0{f},\"msm_vfe0_rdi0\":0{f}",
        s = c.sensor
    );
    media_ctl(c, &["-V", &formatos])?;
    Ok(match &c.video {
        Some(v) => v.clone(),
        None => media_ctl(c, &["-e", "msm_vfe0_video0"])
            .ok()
            .filter(|s| s.starts_with("/dev/"))
            .unwrap_or_else(|| "/dev/video0".into()),
    })
}

// ------------------------------------------------------------------ YUV 4:2:2 -> RGB

/// Conversão inteira com tabelas (BT.601), já com a ordem dos bytes e o giro.
struct Conversor {
    pos: [usize; 4],
    ty: [i32; 256],
    rv: [i32; 256],
    gu: [i32; 256],
    gv: [i32; 256],
    bu: [i32; 256],
    girar180: bool,
    espelhar: bool,
}

impl Conversor {
    fn novo(ordem: Ordem, c: &Config) -> Self {
        let mut k = Conversor {
            pos: ordem.pos(),
            ty: [0; 256],
            rv: [0; 256],
            gu: [0; 256],
            gv: [0; 256],
            bu: [0; 256],
            girar180: c.girar180,
            espelhar: c.espelhar,
        };
        for i in 0..256i32 {
            let d = i - 128;
            if c.faixa_cheia {
                // JFIF: Y 0..255
                k.ty[i as usize] = 256 * i + 128;
                k.rv[i as usize] = 359 * d;
                k.gu[i as usize] = -88 * d;
                k.gv[i as usize] = -183 * d;
                k.bu[i as usize] = 454 * d;
            } else {
                // BT.601 faixa limitada: Y 16..235, C 16..240
                k.ty[i as usize] = 298 * (i - 16) + 128;
                k.rv[i as usize] = 409 * d;
                k.gu[i as usize] = -100 * d;
                k.gv[i as usize] = -208 * d;
                k.bu[i as usize] = 516 * d;
            }
        }
        k
    }

    #[inline(always)]
    fn rgb(&self, y: u8, u: u8, v: u8, saida: &mut [u8]) {
        let l = self.ty[y as usize];
        saida[0] = ((l + self.rv[v as usize]) >> 8).clamp(0, 255) as u8;
        saida[1] = ((l + self.gu[u as usize] + self.gv[v as usize]) >> 8).clamp(0, 255) as u8;
        saida[2] = ((l + self.bu[u as usize]) >> 8).clamp(0, 255) as u8;
    }

    /// Metade da resolução: cada macropixel (2 pontos) de uma linha par vira 1 ponto.
    fn previa(&self, src: &[u8], bpl: usize, w: usize, h: usize) -> Quadro {
        let (ow, oh) = (w / 2, h / 2);
        let mut q = Quadro::new(ow as u32, oh as u32);
        let dst = q.make_mut_bytes();
        let [p0, pu, p1, pv] = self.pos;
        let inverter = self.girar180 ^ self.espelhar;
        for oy in 0..oh {
            let ini = 2 * oy * bpl;
            let linha = &src[ini..ini + w * 2];
            let dy = if self.girar180 { oh - 1 - oy } else { oy };
            let base = dy * ow * 3;
            for (ox, m) in linha.chunks_exact(4).enumerate() {
                let y = ((m[p0] as u16 + m[p1] as u16 + 1) >> 1) as u8;
                let dx = if inverter { ow - 1 - ox } else { ox };
                self.rgb(y, m[pu], m[pv], &mut dst[base + dx * 3..base + dx * 3 + 3]);
            }
        }
        q
    }

    /// Resolução cheia (para a foto): RGB8 w*h*3.
    fn cheia(&self, src: &[u8], bpl: usize, w: usize, h: usize) -> Vec<u8> {
        let mut dst = vec![0u8; w * h * 3];
        let [p0, pu, p1, pv] = self.pos;
        let inverter = self.girar180 ^ self.espelhar;
        for y in 0..h {
            let ini = y * bpl;
            let linha = &src[ini..ini + w * 2];
            let dy = if self.girar180 { h - 1 - y } else { y };
            let base = dy * w * 3;
            for (mx, m) in linha.chunks_exact(4).enumerate() {
                let (x0, x1) = if inverter { (w - 1 - 2 * mx, w - 2 - 2 * mx) } else { (2 * mx, 2 * mx + 1) };
                self.rgb(m[p0], m[pu], m[pv], &mut dst[base + x0 * 3..base + x0 * 3 + 3]);
                self.rgb(m[p1], m[pu], m[pv], &mut dst[base + x1 * 3..base + x1 * 3 + 3]);
            }
        }
        dst
    }
}

/// Quantas linhas têm imagem: linhas do pé com todos os bytes zerados (amostrados) são
/// a "faixa vazia" (o DMA não escreveu ali).
fn linhas_com_imagem(src: &[u8], bpl: usize, w: usize, h: usize) -> usize {
    let mut n = h;
    while n > 2 {
        let ini = (n - 1) * bpl;
        let vazia = src[ini..ini + w * 2].iter().step_by(7).all(|&b| b == 0);
        if !vazia {
            break;
        }
        n -= 1;
    }
    n
}

// ------------------------------------------------------------------ captura

pub enum Evento {
    /// a captura começou (largura x altura úteis)
    Pronta(u32, u32),
    Erro(String),
    /// foto salva: caminho e miniatura
    Foto(Result<(PathBuf, Option<Quadro>), String>),
}

/// Captura em andamento. Soltar (drop) para a thread, que faz STREAMOFF.
pub struct Captura {
    parar: Arc<AtomicBool>,
    foto: Arc<AtomicBool>,
    quadro: Arc<Mutex<Option<Quadro>>>,
    rx: Receiver<Evento>,
}

impl Captura {
    /// Pede uma foto: o próximo quadro é convertido em resolução cheia e salvo em JPEG.
    pub fn tirar_foto(&self) {
        self.foto.store(true, Ordering::Relaxed);
    }
    /// Último quadro da pré-visualização (se chegou um novo).
    pub fn quadro(&self) -> Option<Quadro> {
        self.quadro.lock().unwrap().take()
    }
    pub fn eventos(&self) -> Vec<Evento> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Captura {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
    }
}

/// Só uma thread de captura usa o dispositivo por vez (reabrir logo depois de sair
/// espera a anterior terminar o STREAMOFF, em vez de dar EBUSY).
static EXCLUSIVO: Mutex<()> = Mutex::new(());

/// Liga a câmera numa thread. `avisar` é chamado (de outras threads) quando há quadro
/// ou evento novo; a interface então chama `quadro()` e `eventos()`.
pub fn iniciar<F>(avisar: F) -> Captura
where
    F: Fn() + Send + Clone + 'static,
{
    let parar = Arc::new(AtomicBool::new(false));
    let foto = Arc::new(AtomicBool::new(false));
    let quadro: Arc<Mutex<Option<Quadro>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = channel();
    {
        let (parar, foto, quadro) = (parar.clone(), foto.clone(), quadro.clone());
        std::thread::spawn(move || {
            let _vez = EXCLUSIVO.lock().unwrap_or_else(|e| e.into_inner());
            if parar.load(Ordering::Relaxed) {
                return;
            }
            if let Err(e) = capturar(&parar, &foto, &quadro, &tx, &avisar) {
                eprintln!("câmera: {e}");
                let _ = tx.send(Evento::Erro(e));
                avisar();
            }
        });
    }
    Captura { parar, foto, quadro, rx }
}

fn capturar<F>(
    parar: &AtomicBool,
    foto: &AtomicBool,
    slot: &Mutex<Option<Quadro>>,
    tx: &Sender<Evento>,
    avisar: &F,
) -> Result<(), String>
where
    F: Fn() + Send + Clone + 'static,
{
    let c = config();
    let video = preparar(&c).map_err(|e| {
        if e == i18n::t("Câmera indisponível") { e } else { i18n::tf("Câmera indisponível: {}", &[&e]) }
    })?;
    if parar.load(Ordering::Relaxed) {
        return Ok(());
    }
    let dev = Dispositivo::abrir(&video, &c).map_err(|e| i18n::tf("erro na câmera: {}", &[&e]))?;
    let ordem = c.ordem.or_else(|| Ordem::de_fourcc(dev.fourcc)).unwrap_or(Ordem::Uyvy);
    eprintln!("câmera: convertendo como {ordem:?}");
    let conv = Arc::new(Conversor::novo(ordem, &c));
    let intervalo = Duration::from_millis(1000 / c.fps.max(1) as u64);
    let (w, bpl) = (dev.largura, dev.bpl);

    // altura útil: a do driver, a do arquivo de ajustes, ou detectada nos primeiros quadros
    let mut altura = if c.altura_util > 0 { (c.altura_util as usize).min(dev.altura) } else { dev.altura };
    let mut detectar = if c.altura_util > 0 { 0 } else { 4 };
    let mut pronta = false;
    let mut ultimo = Instant::now() - intervalo;
    let mut sem_quadro = Instant::now();
    let mut avisou_sem_quadro = false;
    let mut pfd = libc::pollfd { fd: dev.fd, events: libc::POLLIN, revents: 0 };

    while !parar.load(Ordering::Relaxed) {
        let r = unsafe { libc::poll(&mut pfd, 1, 300) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(i18n::tf("erro na câmera: {}", &[&format!("poll: {e}")]));
        }
        if r == 0 {
            if !avisou_sem_quadro && sem_quadro.elapsed() > Duration::from_secs(4) {
                avisou_sem_quadro = true;
                let _ = tx.send(Evento::Erro(i18n::t("a câmera não está mandando imagens")));
                avisar();
            }
            continue;
        }
        let Some((i, ini, fim)) = dev.pegar().map_err(|e| i18n::tf("erro na câmera: {}", &[&format!("DQBUF: {e}")]))?
        else {
            continue;
        };
        sem_quadro = Instant::now();
        let dados = dev.dados(i, ini, fim);
        // linhas que cabem no que o driver entregou
        let cabem = if dados.len() >= w * 2 { (dados.len() - w * 2) / bpl + 1 } else { 0 };
        let h = altura.min(cabem);

        if h >= 2 {
            if detectar > 0 {
                detectar -= 1;
                if detectar == 0 {
                    let n = linhas_com_imagem(dados, bpl, w, h);
                    if n < h {
                        eprintln!("câmera: {} linhas vazias no pé; usando {w}x{n}", h - n);
                        altura = n & !1;
                    }
                }
            }
            let h = altura.min(cabem);
            if !pronta || avisou_sem_quadro {
                pronta = true;
                avisou_sem_quadro = false;
                eprintln!("câmera: quadros chegando ({} bytes, {w}x{h}, bpl {bpl})", dados.len());
                let _ = tx.send(Evento::Pronta(w as u32, h as u32));
                avisar();
            }
            if foto.swap(false, Ordering::Relaxed) {
                // copia o quadro cru e devolve o buffer logo; a conversão e o JPEG
                // ficam numa thread à parte
                let cru = dados[..(h - 1) * bpl + w * 2].to_vec();
                let (conv, tx, avisar, pasta) = (conv.clone(), tx.clone(), avisar.clone(), c.pasta.clone());
                std::thread::spawn(move || {
                    let r = salvar_foto(&conv, &cru, bpl, w, h, &pasta);
                    let _ = tx.send(Evento::Foto(r));
                    avisar();
                });
            }
            // pré-visualização: só converte se a interface já pegou o anterior
            if ultimo.elapsed() >= intervalo && slot.lock().unwrap().is_none() {
                ultimo = Instant::now();
                let q = conv.previa(dados, bpl, w, h);
                *slot.lock().unwrap() = Some(q);
                avisar();
            }
        }
        dev.devolver(i as u32).map_err(|e| i18n::tf("erro na câmera: {}", &[&format!("QBUF: {e}")]))?;
    }
    Ok(())
    // dev cai aqui: STREAMOFF, munmap, close
}

// ------------------------------------------------------------------ fotos

fn agora_nome(pasta: &Path) -> PathBuf {
    let base = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut p = pasta.join(format!("{base}.jpg"));
    let mut n = 2;
    while p.exists() {
        p = pasta.join(format!("{base}_{n}.jpg"));
        n += 1;
    }
    p
}

fn gravar_jpeg(caminho: &Path, rgb: &[u8], w: u32, h: u32, qualidade: u8) -> Result<(), String> {
    let tmp = caminho.with_extension("jpg.parcial");
    let arq = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut saida = BufWriter::new(arq);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut saida, qualidade)
        .encode(rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    saida.flush().map_err(|e| e.to_string())?;
    drop(saida);
    fs::rename(&tmp, caminho).map_err(|e| e.to_string())
}

fn salvar_foto(
    conv: &Conversor,
    cru: &[u8],
    bpl: usize,
    w: usize,
    h: usize,
    pasta: &Path,
) -> Result<(PathBuf, Option<Quadro>), String> {
    fs::create_dir_all(pasta).map_err(|e| format!("{}: {e}", pasta.display()))?;
    let rgb = conv.cheia(cru, bpl, w, h);
    let caminho = agora_nome(pasta);
    gravar_jpeg(&caminho, &rgb, w as u32, h as u32, 90)?;
    eprintln!("câmera: foto {}", caminho.display());
    let mini = image::RgbImage::from_raw(w as u32, h as u32, rgb).map(|img| {
        let m = reduzir(&img, MINI_L, MINI_A);
        let _ = guardar_miniatura(&caminho, &m);
        para_quadro(&m)
    });
    Ok((caminho, mini))
}

fn reduzir(img: &image::RgbImage, lmax: u32, amax: u32) -> image::RgbImage {
    let (w, h) = img.dimensions();
    if w <= lmax && h <= amax {
        return img.clone();
    }
    let k = (lmax as f32 / w as f32).min(amax as f32 / h as f32);
    let (nw, nh) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
    image::imageops::thumbnail(img, nw, nh)
}

fn para_quadro(img: &image::RgbImage) -> Quadro {
    let (w, h) = img.dimensions();
    Quadro::clone_from_slice(img.as_raw(), w, h)
}

fn caminho_miniatura(foto: &Path) -> Option<PathBuf> {
    let nome = foto.file_name()?;
    Some(foto.parent()?.join(".miniaturas").join(nome))
}

fn guardar_miniatura(foto: &Path, m: &image::RgbImage) -> Result<(), String> {
    let p = caminho_miniatura(foto).ok_or("sem nome")?;
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    gravar_jpeg(&p, m.as_raw(), m.width(), m.height(), 80)
}

/// Fotos da pasta (.jpg/.jpeg), da mais nova para a mais antiga.
pub fn listar_fotos(pasta: &Path) -> Vec<PathBuf> {
    let Ok(dir) = fs::read_dir(pasta) else { return vec![] };
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = dir
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let nome = p.file_name()?.to_str()?.to_string();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            if nome.starts_with('.') || !(ext == "jpg" || ext == "jpeg") {
                return None;
            }
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), p))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    v.into_iter().map(|(_, p)| p).collect()
}

/// Miniatura da foto (do cache em <pasta>/.miniaturas ou gerada e guardada lá).
pub fn miniatura(foto: &Path) -> Option<Quadro> {
    if let Some(p) = caminho_miniatura(foto) {
        let novo = |a: &Path| fs::metadata(a).and_then(|m| m.modified()).ok();
        if novo(&p).is_some() && novo(&p) >= novo(foto) {
            if let Ok(img) = image::open(&p) {
                return Some(para_quadro(&img.to_rgb8()));
            }
        }
    }
    let img = image::open(foto).ok()?.to_rgb8();
    let m = reduzir(&img, MINI_L, MINI_A);
    let _ = guardar_miniatura(foto, &m);
    Some(para_quadro(&m))
}

/// Foto inteira para a tela cheia (reduzida a no máximo 1280x960).
pub fn carregar_foto(foto: &Path) -> Option<Quadro> {
    let img = image::open(foto).ok()?.to_rgb8();
    Some(para_quadro(&reduzir(&img, 1280, 960)))
}

/// Apaga a foto e a miniatura.
pub fn apagar(foto: &Path) -> Result<(), String> {
    fs::remove_file(foto).map_err(|e| e.to_string())?;
    if let Some(p) = caminho_miniatura(foto) {
        let _ = fs::remove_file(p);
    }
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn tamanhos_das_estruturas() {
        // conferidos com o videodev2.h (64 bits: 208/88/64; ARM 32 bits: 204/80/60)
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<Formato>(), 208);
            assert_eq!(size_of::<Buffer>(), 88);
            assert_eq!(size_of::<Plano>(), 64);
        } else {
            assert_eq!(size_of::<Formato>(), 204);
            assert_eq!(size_of::<Buffer>(), 80);
            assert_eq!(size_of::<Plano>(), 60);
        }
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<ReqBufs>(), 20);
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
    }

    #[test]
    fn cinza_e_ordem() {
        let c = Config::default();
        let k = Conversor::novo(Ordem::Uyvy, &c);
        // U Y0 V Y1 = cinza médio
        let src = [128u8, 126, 128, 126];
        let q = k.cheia(&src, 4, 2, 1);
        assert!(q.iter().all(|&b| (b as i32 - 128).abs() <= 2), "{q:?}");
        // zeros em UYVY viram verde (era a faixa "vazia" esverdeada)
        let z = k.cheia(&[0, 0, 0, 0], 4, 2, 1);
        assert!(z[1] > z[0] && z[1] > z[2]);
    }
}
