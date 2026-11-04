//! App Câmera: captura da câmera frontal (MT9M114) e da traseira (AR0543) no CAMSS do
//! MSM8226 por V4L2,
//! em Rust puro (ioctl pela libc, sem crate nova).
//!
//! Caminho da imagem (montado com o `media-ctl`, os mesmos argumentos validados no aparelho):
//! sensor "mt9m114 ifp 3-0048" -> msm_csiphy1 -> msm_csid0 -> msm_ispif0 -> msm_vfe0_rdi0.
//! A traseira ("ar0543 3-0036", RAW10 Bayer GRBG, sem ISP no caminho RDI) vai por
//! msm_csiphy0 -> msm_csid0 -> ... (o mesmo CSID; as duas nunca filmam juntas); o app faz a
//! interpolação Bayer, o balanço de branco (mundo cinza) e a exposição automática.
//! O RDI grava o que o sensor manda (UYVY8_1X16 ou RAW10), sem ISP. O nó de vídeo do RDI é
//! multiplanar (V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, 1 plano); o caso de 1 plano só
//! (VIDEO_CAPTURE) também é tratado, decidido pelo VIDIOC_QUERYCAP.
//!
//! SEGURANÇA (causa dos reinícios secos do aparelho): o RDI do camss usa o write master
//! em modo "frame based" e o driver NÃO programa tamanho nem stride para ele: o hardware
//! escreve tudo o que o sensor manda, sem limite, a partir do endereço do buffer. E o driver
//! só passa ao hardware o endereço do PRIMEIRO segmento do scatter-gather do buffer
//! (videobuf2-dma-sg, sem IOMMU); como o vb2 aloca em pedaços de ordem decrescente, o
//! buffer quase nunca é contíguo, e o DMA passa do pedaço e escreve em memória que é de
//! outros (corrupção, reset). Por isso aqui: (1) buffers de tamanho potência de 2 criados
//! com VIDIOC_CREATE_BUFS (vb2 aloca de uma vez só, um pedaço, quando a ordem existe) e
//! bem maiores que o quadro; (2) cada buffer é conferido em /proc/self/pagemap (todas as
//! páginas físicas consecutivas) antes de qualquer QBUF; se não for contíguo, tenta de
//! novo e depois cai para um quadro menor; (3) a última página de cada buffer é uma
//! "guarda" (0xA5) conferida a cada quadro: se mudar, a captura é abortada.
//!
//! Ao sair TUDO é fechado: STREAMOFF, munmap, REQBUFS 0, close, `media-ctl -r` e
//! `modprobe -r qcom_camss mt9m114` (o driver do kernel não solta o pipeline do media
//! controller no STREAMOFF, porque o "pixel array" do mt9m114 não tem s_stream e o
//! video_stop_streaming() desiste com -ENOIOCTLCMD; sem descarregar os módulos o próximo
//! STREAMON dá EBUSY e `media-ctl -r` também). Cada abertura recarrega os módulos (estado
//! limpo do VFE/sensor: sem os quadros de partida que escreviam além do tamanho).
//!
//! A thread de captura converte cada quadro para RGB pela metade (1280x960 -> 640x480,
//! um macropixel = um ponto; sem escala depois na tela) e deixa o último num "slot"; a interface é avisada por
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
//!   largura=1280 / altura=960    formato pedido ao pipeline (crop 1280x960 + compose do ifp;
//!                                4:3, de 320x240 a 1280x960; se faltar memória contígua cai
//!                                para 640x480 e 320x240)
//!   sensor=mt9m114 ifp 3-0048    nome da entidade do sensor no media controller
//!   media=/dev/media0            dispositivo do media controller
//!   video=/dev/video0            nó de captura (padrão: o do msm_vfe0_video0, via media-ctl -e)
//!   fps=10                       limite de quadros por segundo da pré-visualização

use std::fs;
use std::io::{BufWriter, Write};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{Rgb8Pixel, SharedPixelBuffer};

use crate::i18n;

/// Quadro RGB pronto para a interface (Image::from_rgb8).
pub type Quadro = SharedPixelBuffer<Rgb8Pixel>;

/// existe quando o qcom_camss carregado usa o videobuf2-dma-contig (correção do kernel)
const CAMSS_CONTIGUO: &str = "/sys/module/videobuf2_dma_contig/holders/qcom_camss";
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

/// RAW10 Bayer GRBG empacotado do MIPI (V4L2_PIX_FMT_SGRBG10P)
const PGAA: u32 = fourcc(b"pgAA");

const fn fourcc(c: &[u8; 4]) -> u32 {
    (c[0] as u32) | ((c[1] as u32) << 8) | ((c[2] as u32) << 16) | ((c[3] as u32) << 24)
}

fn fourcc_texto(f: u32) -> String {
    f.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

/// Qual câmera: a escolha fica em /var/lib/revive/camera-lado.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lado {
    Frente,
    Tras,
}

const LADO: &str = "/var/lib/revive/camera-lado";
/// nó do sensor traseiro no device tree (só existe no DTB com as duas câmeras)
const DT_TRASEIRA: &str = "/sys/firmware/devicetree/base/soc/cci@fda0c000/i2c-bus@0/camera-sensor@36";

pub fn tem_traseira() -> bool {
    Path::new(DT_TRASEIRA).exists()
}

pub fn lado() -> Lado {
    let tras = fs::read_to_string(LADO).map(|t| t.trim() == "tras").unwrap_or(false);
    if tras && tem_traseira() { Lado::Tras } else { Lado::Frente }
}

/// Alterna frontal/traseira e grava a escolha.
pub fn trocar_lado() -> Lado {
    let novo = if lado() == Lado::Frente && tem_traseira() { Lado::Tras } else { Lado::Frente };
    let _ = fs::create_dir_all("/var/lib/revive");
    let _ = fs::write(LADO, if novo == Lado::Tras { "tras\n" } else { "frente\n" });
    novo
}

#[derive(Clone, Debug)]
pub struct Config {
    pub lado: Lado,
    pub sensor_tras: String,
    pub pasta: PathBuf,
    /// None = a ordem do pixelformat que o driver devolver no G_FMT
    pub ordem: Option<Ordem>,
    pub faixa_cheia: bool,
    pub girar180: bool,
    pub espelhar: bool,
    pub largura: u32,
    pub altura: u32,
    pub sensor: String,
    pub media: String,
    pub video: Option<String>,
    pub fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            lado: Lado::Frente,
            sensor_tras: "ar0543 3-0036".into(),
            pasta: PathBuf::from("/home/allan/Fotos"),
            ordem: None,
            faixa_cheia: false,
            girar180: false,
            espelhar: false,
            largura: 1280,
            altura: 960,
            sensor: "mt9m114 ifp 3-0048".into(),
            media: "/dev/media0".into(),
            video: None,
            fps: 10,
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
            "largura" => c.largura = v.parse::<u32>().unwrap_or(c.largura).clamp(320, 1280),
            "altura" => c.altura = v.parse::<u32>().unwrap_or(c.altura).clamp(240, 960),
            "sensor" if !v.is_empty() => c.sensor = v.to_string(),
            "sensor_tras" if !v.is_empty() => c.sensor_tras = v.to_string(),
            "media" if !v.is_empty() => c.media = v.to_string(),
            "video" if !v.is_empty() => c.video = Some(v.to_string()),
            "fps" => c.fps = v.parse::<u32>().unwrap_or(c.fps).clamp(1, 30),
            _ => {}
        }
    }
    c.largura &= !7; // macropixel = 2 pontos; múltiplo de 8 para o scaler do ifp
    c.altura &= !1;
    c.lado = lado();
    if c.lado == Lado::Tras {
        // a traseira só tem o modo 1280x960 (binning 2x2) e não passa por giro/espelho
        (c.largura, c.altura) = (1280, 960);
        c.girar180 = false;
        c.espelhar = false;
    }
    c
}

impl Config {
    fn tras(&self) -> bool {
        self.lado == Lado::Tras
    }
    fn sensor_ativo(&self) -> &str {
        if self.tras() { &self.sensor_tras } else { &self.sensor }
    }
}

// ------------------------------------------------------------------ V4L2 (videodev2.h)

const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
const V4L2_MEMORY_MMAP: u32 = 1;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
const V4L2_CAP_VIDEO_CAPTURE_MPLANE: u32 = 0x0000_1000;
const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const V4L2_BUF_FLAG_ERROR: u32 = 0x0000_0040;
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

/// struct v4l2_create_buffers (kernel 6.16: capabilities, flags, max_num_buffers, reserved[5]).
#[allow(dead_code)]
#[repr(C)]
struct CriarBufs {
    index: u32,
    count: u32,
    memory: u32,
    format: Formato,
    capabilities: u32,
    flags: u32,
    max_num_buffers: u32,
    reserved: [u32; 5],
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
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<CriarBufs>() == 256);
#[cfg(target_arch = "arm")]
const _: () = assert!(size_of::<CriarBufs>() == 248);
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
const VIDIOC_CREATE_BUFS: u32 = ioc(LER | GRAVAR, 92, size_of::<CriarBufs>());
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

/// Por que não deu para abrir a captura.
enum Falha {
    /// sem memória fisicamente contígua para os buffers neste tamanho: tentar um quadro menor
    Memoria(String),
    Outra(String),
}

const PAGINA: usize = 4096;
const GUARDA: u8 = 0xA5;
const NUM_BUFFERS: u32 = 3;
/// quantas vezes tenta alocar buffers contíguos antes de desistir deste tamanho
const TENTATIVAS_MEMORIA: usize = 6;

/// Tamanho do buffer: potência de 2 (o vb2 aloca de uma vez, num pedaço só), com no mínimo
/// 50% de folga sobre o quadro e 2 MiB (o hardware não conhece o tamanho do buffer).
fn tamanho_buffer(quadro: usize) -> usize {
    (quadro + quadro / 2).max(2 << 20).next_power_of_two()
}

/// Todas as páginas do mapa estão em endereços físicos consecutivos? (o driver só passa ao
/// hardware o endereço da primeira). Precisa de root: sem PFN visível, nega.
fn contiguo(pagemap: RawFd, p: *mut u8, len: usize) -> Result<bool, String> {
    // camss corrigido (usa videobuf2-dma-contig sem IOMMU): o buffer é contíguo por
    // construção e o mapa dele (VM_PFNMAP) nem aparece no pagemap; a guarda continua valendo
    if Path::new(CAMSS_CONTIGUO).exists() {
        return Ok(true);
    }
    let n = len.div_ceil(PAGINA);
    let mut primeiro = 0u64;
    for i in 0..n {
        // garante a página presente
        unsafe { std::ptr::read_volatile(p.add(i * PAGINA)) };
        let mut e = 0u64;
        let off = ((p as usize / PAGINA + i) * 8) as libc::off_t;
        let r = unsafe { libc::pread(pagemap, &mut e as *mut u64 as *mut libc::c_void, 8, off) };
        if r != 8 {
            return Err(format!("pagemap: {}", std::io::Error::last_os_error()));
        }
        if e >> 63 == 0 {
            return Ok(false);
        }
        let pfn = e & ((1u64 << 55) - 1);
        if pfn == 0 {
            return Err("pagemap sem endereços físicos (precisa de root)".into());
        }
        if i == 0 {
            primeiro = pfn;
        } else if pfn != primeiro + i as u64 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Quadro devolvido pelo driver.
struct Cheio {
    i: usize,
    ini: usize,
    fim: usize,
    /// o driver marcou erro (V4L2_BUF_FLAG_ERROR) ou veio incompleto
    ruim: bool,
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
    /// bytes de um quadro completo (sizeimage do driver)
    quadro: usize,
    fourcc: u32,
}

// os ponteiros dos mapas só são usados pela thread de captura, dona do Dispositivo
unsafe impl Send for Dispositivo {}

impl Dispositivo {
    fn abrir(caminho: &str, c: &Config, largura: u32, altura: u32) -> Result<Self, Falha> {
        let outra = |e: String| Falha::Outra(e);
        let cpath = std::ffi::CString::new(caminho).map_err(|e| outra(e.to_string()))?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(outra(format!("{caminho}: {}", std::io::Error::last_os_error())));
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
            quadro: 0,
            fourcc: 0,
        };

        // multiplanar ou não
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        xioctl(fd, VIDIOC_QUERYCAP, &mut cap).map_err(|e| outra(format!("QUERYCAP: {e}")))?;
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 { cap.device_caps } else { cap.capabilities };
        d.mplane = if caps & V4L2_CAP_VIDEO_CAPTURE_MPLANE != 0 {
            true
        } else if caps & V4L2_CAP_VIDEO_CAPTURE != 0 {
            false
        } else {
            return Err(outra(format!("{caminho}: não é captura de vídeo (caps {caps:#x})")));
        };
        d.tipo = if d.mplane { V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE } else { V4L2_BUF_TYPE_VIDEO_CAPTURE };

        // formato: UYVY (o código UYVY8_1X16 do pipeline; o STREAMON confere os dois)
        let mut f = Formato::novo(d.tipo);
        let pix = if c.tras() { PGAA } else { Ordem::Uyvy.fourcc() };
        if d.mplane {
            f.escrever(MP_LARGURA, largura);
            f.escrever(MP_ALTURA, altura);
            f.escrever(MP_PIXFMT, pix);
            f.escrever(MP_FIELD, V4L2_FIELD_NONE);
            f.raw[MP_NUM_PLANOS] = 1;
        } else {
            f.escrever(SP_LARGURA, largura);
            f.escrever(SP_ALTURA, altura);
            f.escrever(SP_PIXFMT, pix);
            f.escrever(SP_FIELD, V4L2_FIELD_NONE);
        }
        xioctl(fd, VIDIOC_S_FMT, &mut f).map_err(|e| outra(format!("S_FMT: {e}")))?;
        // o que vale é o que o driver devolve
        let mut f = Formato::novo(d.tipo);
        xioctl(fd, VIDIOC_G_FMT, &mut f).map_err(|e| outra(format!("G_FMT: {e}")))?;
        let (w, h, fcc, bpl, tam) = if d.mplane {
            (f.ler(MP_LARGURA), f.ler(MP_ALTURA), f.ler(MP_PIXFMT), f.ler(MP_PLANO0_BPL), f.ler(MP_PLANO0_TAMANHO))
        } else {
            (f.ler(SP_LARGURA), f.ler(SP_ALTURA), f.ler(SP_PIXFMT), f.ler(SP_BPL), f.ler(SP_TAMANHO))
        };
        d.largura = (w & !1) as usize;
        d.altura = h as usize;
        d.fourcc = fcc;
        // no modo RDI o hardware grava as linhas sem preenchimento: o stride real é largura*2
        // (RAW10 empacotado: 5 bytes a cada 4 pontos)
        let min_bpl = if c.tras() { d.largura * 5 / 4 } else { d.largura * 2 };
        d.bpl = if bpl as usize >= min_bpl { bpl as usize } else { min_bpl };
        d.quadro = (tam as usize).max(d.bpl * d.altura);
        eprintln!(
            "câmera: {caminho} {} {w}x{h} {} bytesperline {bpl} sizeimage {tam}",
            if d.mplane { "MPLANE" } else { "1 plano" },
            fourcc_texto(fcc)
        );
        if (w, h) != (largura, altura) {
            return Err(outra(format!("o nó de captura devolveu {w}x{h}, pedi {largura}x{altura}")));
        }
        if d.largura < 2 || d.altura < 2 || d.quadro == 0 {
            return Err(outra(format!("formato inválido {w}x{h}")));
        }

        // buffers: criados de uma vez (CREATE_BUFS) com tamanho potência de 2 e conferidos
        let pagemap = unsafe { libc::open(c"/proc/self/pagemap".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if pagemap < 0 {
            return Err(outra(format!("pagemap: {}", std::io::Error::last_os_error())));
        }
        let r = d.alocar_buffers(&f, pagemap);
        unsafe { libc::close(pagemap) };
        r?;

        // guarda no fim de cada buffer (o DMA não deve chegar ali)
        for &(p, n) in &d.mapas {
            unsafe { std::ptr::write_bytes(p.add(n - PAGINA), GUARDA, PAGINA) };
        }
        for i in 0..d.mapas.len() {
            d.devolver(i as u32).map_err(|e| outra(format!("QBUF {i}: {e}")))?;
        }
        let mut t = d.tipo as libc::c_int;
        xioctl(fd, VIDIOC_STREAMON, &mut t).map_err(|e| {
            // EPIPE aqui = formato do nó diferente do formato do pipeline
            outra(format!("STREAMON: {e}"))
        })?;
        d.ligado = true;
        Ok(d)
    }

    /// CREATE_BUFS + mmap + conferência de contiguidade, com novas tentativas.
    fn alocar_buffers(&mut self, f: &Formato, pagemap: RawFd) -> Result<(), Falha> {
        let outra = |e: String| Falha::Outra(e);
        let len = tamanho_buffer(self.quadro);
        let mut ultimo = String::new();
        for tentativa in 0..TENTATIVAS_MEMORIA {
            if tentativa > 0 {
                std::thread::sleep(Duration::from_millis(150));
            }
            let mut cb: CriarBufs = unsafe { std::mem::zeroed() };
            cb.count = NUM_BUFFERS;
            cb.memory = V4L2_MEMORY_MMAP;
            cb.format = Formato { tipo: f.tipo, _alinhar: [], raw: f.raw };
            // sizeimage pedido = tamanho do buffer (o driver aceita >= o do formato)
            cb.format.escrever(if self.mplane { MP_PLANO0_TAMANHO } else { SP_TAMANHO }, len as u32);
            xioctl(self.fd, VIDIOC_CREATE_BUFS, &mut cb).map_err(|e| outra(format!("CREATE_BUFS: {e}")))?;
            if cb.count < 2 {
                return Err(outra(format!("CREATE_BUFS: só {} buffer(s)", cb.count)));
            }
            let mut bom = true;
            for i in 0..cb.count {
                let mut planos: [Plano; 1] = unsafe { std::mem::zeroed() };
                let mut b = self.buffer_vazio(&mut planos);
                b.index = cb.index + i;
                xioctl(self.fd, VIDIOC_QUERYBUF, &mut b).map_err(|e| outra(format!("QUERYBUF {i}: {e}")))?;
                let (tamanho, offset) = if self.mplane {
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
                        self.fd,
                        offset as libc::off_t,
                    )
                };
                if p == libc::MAP_FAILED {
                    return Err(outra(format!("mmap {i}: {}", std::io::Error::last_os_error())));
                }
                self.mapas.push((p as *mut u8, tamanho));
                if tamanho < len || tamanho < self.quadro + PAGINA {
                    return Err(outra(format!("buffer {i} com {tamanho} bytes, pedi {len}")));
                }
                match contiguo(pagemap, p as *mut u8, tamanho) {
                    Ok(true) => {}
                    Ok(false) => bom = false,
                    Err(e) => return Err(outra(e)),
                }
            }
            if bom {
                eprintln!("câmera: {} buffers de {} KiB, memória contígua conferida", self.mapas.len(), len >> 10);
                return Ok(());
            }
            ultimo = format!("buffers de {} KiB não contíguos (tentativa {})", len >> 10, tentativa + 1);
            eprintln!("câmera: {ultimo}");
            self.liberar_buffers();
        }
        Err(Falha::Memoria(ultimo))
    }

    /// munmap + REQBUFS 0 (só com o streaming desligado).
    fn liberar_buffers(&mut self) {
        unsafe {
            for &(p, n) in &self.mapas {
                libc::munmap(p as *mut libc::c_void, n);
            }
            if !self.mapas.is_empty() {
                let mut req = ReqBufs { count: 0, tipo: self.tipo, memory: V4L2_MEMORY_MMAP, ..Default::default() };
                let _ = xioctl(self.fd, VIDIOC_REQBUFS, &mut req);
            }
        }
        self.mapas.clear();
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

    /// Pega um buffer cheio. None = ainda não há. Erro = a guarda do fim do buffer mudou
    /// (o DMA escreveu além do esperado): a captura tem de parar.
    fn pegar(&self) -> Result<Option<Cheio>, String> {
        let mut planos: [Plano; 1] = unsafe { std::mem::zeroed() };
        let mut b = self.buffer_vazio(&mut planos);
        match xioctl(self.fd, VIDIOC_DQBUF, &mut b) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => return Ok(None),
            Err(e) => return Err(format!("DQBUF: {e}")),
        }
        let i = b.index as usize;
        let (p, tamanho) = *self.mapas.get(i).ok_or("DQBUF: índice de buffer inválido")?;
        let guarda = unsafe { std::slice::from_raw_parts(p.add(tamanho - PAGINA), PAGINA) };
        if guarda.iter().any(|&x| x != GUARDA) {
            return Err("a câmera escreveu além do quadro (guarda do buffer alterada)".into());
        }
        let (ini, fim) = if self.mplane {
            // bytesused inclui o data_offset
            (planos[0].data_offset as usize, planos[0].bytesused as usize)
        } else {
            (0, b.bytesused as usize)
        };
        let fim = fim.min(tamanho);
        let ruim = b.flags & V4L2_BUF_FLAG_ERROR != 0 || fim < ini || fim - ini < self.quadro;
        Ok(Some(Cheio { i, ini: ini.min(fim), fim, ruim }))
    }

    fn dados(&self, i: usize, ini: usize, fim: usize) -> &[u8] {
        let (p, _) = self.mapas[i];
        unsafe { std::slice::from_raw_parts(p.add(ini), fim - ini) }
    }
}

impl Drop for Dispositivo {
    fn drop(&mut self) {
        if self.ligado {
            let mut t = self.tipo as libc::c_int;
            let r = xioctl(self.fd, VIDIOC_STREAMOFF, &mut t);
            eprintln!("câmera: STREAMOFF {r:?}");
        }
        self.liberar_buffers();
        unsafe { libc::close(self.fd) };
        eprintln!("câmera: dispositivo fechado");
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

fn modprobe(args: &[&str]) -> bool {
    match Command::new("modprobe").args(args).status() {
        Ok(st) => {
            if !st.success() {
                eprintln!("câmera: modprobe {} -> {st}", args.join(" "));
            }
            st.success()
        }
        Err(e) => {
            eprintln!("câmera: modprobe {}: {e}", args.join(" "));
            false
        }
    }
}

/// Descarrega o camss e o sensor: solta o pipeline do media controller (o driver não o solta
/// no STREAMOFF), desliga clocks/IRQs do CAMSS e a energia do sensor. Só chamar com o nó de
/// vídeo já fechado.
fn desligar_modulos(c: &Config) {
    if Path::new(&c.media).exists() {
        // com o driver atual dá EBUSY (pipeline não solto); com um kernel corrigido funciona
        let _ = media_ctl(c, &["-r"]);
    }
    let ok = modprobe(&["-r", "qcom_camss", "mt9m114"]);
    if tem_traseira() {
        modprobe(&["-r", "ar0543"]);
    }
    eprintln!("câmera: módulos descarregados: {ok}");
}

/// Carrega os módulos do zero (descarrega antes se sobraram de uma sessão anterior) e espera
/// o media controller e o sensor aparecerem.
fn carregar_modulos(c: &Config) -> Result<(), String> {
    if Path::new(&c.media).exists() {
        desligar_modulos(c);
    }
    modprobe(&["-a", "i2c_qcom_cci", "mt9m114"]);
    // com as duas câmeras no device tree, o camss só fecha o grafo com os dois sensores
    if tem_traseira() {
        modprobe(&["ar0543"]);
    }
    modprobe(&["qcom_camss"]);
    for _ in 0..50 {
        if Path::new(&c.media).exists() {
            if let Ok(t) = media_ctl(c, &["-p"]) {
                if t.contains(c.sensor_ativo()) && t.contains("msm_vfe0_video0") {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(i18n::t("Câmera indisponível"))
}

/// Monta o pipeline para `largura`x`altura` (crop 1280x960 do ifp + compose, o scaler do
/// sensor reduz) e devolve o nó de captura.
fn preparar(c: &Config, largura: u32, altura: u32) -> Result<String, String> {
    if c.tras() {
        preparar_tras(c, largura, altura)?;
    } else {
        preparar_frente(c, largura, altura)?;
    }
    Ok(match &c.video {
        Some(v) => v.clone(),
        None => media_ctl(c, &["-e", "msm_vfe0_video0"])
            .ok()
            .filter(|s| s.starts_with("/dev/"))
            .unwrap_or_else(|| "/dev/video0".into()),
    })
}

/// Traseira: AR0543 -> csiphy0 -> csid0 -> ispif0 -> rdi0, RAW10 do jeito que sai do sensor.
fn preparar_tras(c: &Config, largura: u32, altura: u32) -> Result<(), String> {
    let links = "\"msm_csiphy0\":1->\"msm_csid0\":0[1],\
                 \"msm_csid0\":1->\"msm_ispif0\":0[1],\
                 \"msm_ispif0\":1->\"msm_vfe0_rdi0\":0[1]";
    media_ctl(c, &["-l", links])?;
    let m = format!("SGRBG10_1X10/{largura}x{altura}");
    let f = format!("[fmt:{m}]");
    let formatos = format!(
        "\"{}\":0[fmt:{m} field:none],\"msm_csiphy0\":0{f},\"msm_csid0\":0{f},\"msm_ispif0\":0{f},\"msm_vfe0_rdi0\":0{f}",
        c.sensor_tras
    );
    media_ctl(c, &["-V", &formatos])?;
    let topo = media_ctl(c, &["-p", "-e", &c.sensor_tras])?;
    if !topo.contains(&m) {
        return Err(format!("o sensor não ficou em {largura}x{altura}"));
    }
    Ok(())
}

fn preparar_frente(c: &Config, largura: u32, altura: u32) -> Result<(), String> {
    let links = "\"msm_csiphy1\":1->\"msm_csid0\":0[1],\
                 \"msm_csid0\":1->\"msm_ispif0\":0[1],\
                 \"msm_ispif0\":1->\"msm_vfe0_rdi0\":0[1]";
    media_ctl(c, &["-l", links])?;
    // o crop vem antes (mudar o crop zera o compose)
    media_ctl(c, &["-V", &format!("\"{}\":0[crop:(4,4)/1280x960]", c.sensor)])?;
    media_ctl(c, &["-V", &format!("\"{}\":0[compose:(0,0)/{largura}x{altura}]", c.sensor)])?;
    let f = format!("[fmt:UYVY8_1X16/{largura}x{altura}]");
    let formatos = format!("\"msm_csiphy1\":0{f},\"msm_csid0\":0{f},\"msm_ispif0\":0{f},\"msm_vfe0_rdi0\":0{f}");
    media_ctl(c, &["-V", &formatos])?;
    // confere o que o sensor vai mandar
    let topo = media_ctl(c, &["-p", "-e", &c.sensor])?;
    if !topo.contains(&format!("UYVY8_1X16/{largura}x{altura}")) {
        return Err(format!("o sensor não ficou em {largura}x{altura}"));
    }
    Ok(())
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

// ------------------------------------------------------------------ RAW10 Bayer -> RGB

/// nível de preto do AR0543 (pedestal 42 em 10 bits), nos 8 bits altos
const PRETO: i32 = 10;
/// ganho 1,0 nas contas inteiras (1/256)
const UM: u32 = 256;

/// Somas da pré-visualização (valores lineares, já sem o preto) para o balanço e a exposição.
#[derive(Default)]
struct Estat {
    r: u64,
    g: u64,
    b: u64,
    n: u64,
}

/// Bayer GRBG (linha par: G R, linha ímpar: B G), RAW10 empacotado (4 pontos em 5 bytes:
/// os 8 bits altos de cada ponto e um byte com os 2 bits baixos, que aqui são ignorados).
struct Bayer {
    /// linear (0..255-PRETO) -> sRGB aproximado (gama 1/2,2)
    gama: [u8; 256],
    /// ganhos do balanço de branco (vermelho e azul, em 1/256), mexidos pela thread de captura
    ganho_r: AtomicU32,
    ganho_b: AtomicU32,
}

impl Bayer {
    fn novo() -> Self {
        let mut gama = [0u8; 256];
        let branco = (255 - PRETO) as f32;
        for (i, g) in gama.iter_mut().enumerate() {
            *g = (255.0 * (i as f32 / branco).min(1.0).powf(1.0 / 2.2) + 0.5) as u8;
        }
        Bayer { gama, ganho_r: AtomicU32::new(UM * 3 / 2), ganho_b: AtomicU32::new(UM * 3 / 2) }
    }

    #[inline(always)]
    fn cor(&self, r: i32, g: i32, b: i32, gr: i32, gb: i32, saida: &mut [u8]) {
        saida[0] = self.gama[((r * gr) >> 8).clamp(0, 255) as usize];
        saida[1] = self.gama[g.clamp(0, 255) as usize];
        saida[2] = self.gama[((b * gb) >> 8).clamp(0, 255) as usize];
    }

    fn ganhos(&self) -> (i32, i32) {
        (self.ganho_r.load(Ordering::Relaxed) as i32, self.ganho_b.load(Ordering::Relaxed) as i32)
    }

    /// Metade da resolução: cada quadrado 2x2 (G R / B G) vira 1 ponto.
    fn previa(&self, src: &[u8], bpl: usize, w: usize, h: usize) -> (Quadro, Estat) {
        let (ow, oh) = (w / 2, h / 2);
        let mut q = Quadro::new(ow as u32, oh as u32);
        let dst = q.make_mut_bytes();
        let (gr, gb) = self.ganhos();
        let mut e = Estat::default();
        let v = |b: u8| (b as i32 - PRETO).max(0);
        for oy in 0..oh {
            let l0 = &src[2 * oy * bpl..2 * oy * bpl + w * 5 / 4];
            let l1 = &src[(2 * oy + 1) * bpl..(2 * oy + 1) * bpl + w * 5 / 4];
            let base = oy * ow * 3;
            for ox in 0..ow {
                let x = 2 * ox;
                let i = (x >> 2) * 5 + (x & 3);
                let (g1, r, b, g2) = (v(l0[i]), v(l0[i + 1]), v(l1[i]), v(l1[i + 1]));
                let g = (g1 + g2) >> 1;
                if oy & 3 == 0 && ox & 3 == 0 {
                    e.r += r as u64;
                    e.g += g as u64;
                    e.b += b as u64;
                    e.n += 1;
                }
                self.cor(r, g, b, gr, gb, &mut dst[base + ox * 3..base + ox * 3 + 3]);
            }
        }
        (q, e)
    }

    /// Resolução cheia (foto): interpolação bilinear clássica, RGB8 w*h*3.
    fn cheia(&self, src: &[u8], bpl: usize, w: usize, h: usize) -> Vec<u8> {
        let mut dst = vec![0u8; w * h * 3];
        let (gr, gb) = self.ganhos();
        // espelha nas bordas mantendo a paridade (a mesma cor do mosaico)
        let p = |x: isize, y: isize| -> i32 {
            let x = if x < 0 { -x } else if x >= w as isize { 2 * w as isize - 2 - x } else { x } as usize;
            let y = if y < 0 { -y } else if y >= h as isize { 2 * h as isize - 2 - y } else { y } as usize;
            (src[y * bpl + (x >> 2) * 5 + (x & 3)] as i32 - PRETO).max(0)
        };
        for y in 0..h as isize {
            for x in 0..w as isize {
                let c = p(x, y);
                let lados = |a: i32, b: i32| (a + b) >> 1;
                let cruz = || (p(x - 1, y) + p(x + 1, y) + p(x, y - 1) + p(x, y + 1)) >> 2;
                let diag = || (p(x - 1, y - 1) + p(x + 1, y - 1) + p(x - 1, y + 1) + p(x + 1, y + 1)) >> 2;
                let (r, g, b) = match (y & 1, x & 1) {
                    (0, 0) => (lados(p(x - 1, y), p(x + 1, y)), c, lados(p(x, y - 1), p(x, y + 1))),
                    (0, _) => (c, cruz(), diag()),
                    (_, 0) => (diag(), cruz(), c),
                    _ => (lados(p(x, y - 1), p(x, y + 1)), c, lados(p(x - 1, y), p(x + 1, y))),
                };
                let o = (y as usize * w + x as usize) * 3;
                self.cor(r, g, b, gr, gb, &mut dst[o..o + 3]);
            }
        }
        dst
    }
}

/// Exposição automática e balanço de branco da traseira (o RDI não tem ISP).
struct Auto {
    sub: RawFd,
    /// linhas de exposição (1..1044) e ganho analógico (32 = 1x)
    exp: i32,
    ganho: i32,
}

const V4L2_CID_EXPOSURE: u32 = 0x0098_0911;
const V4L2_CID_ANALOGUE_GAIN: u32 = 0x009e_0903;
const VIDIOC_S_CTRL: u32 = ioc(LER | GRAVAR, 28, 8);
const EXP_MAX: i32 = 1044;
const GANHO_MAX: i32 = 256; // até 8x (acima disso só ruído)
/// média linear do verde desejada (~134 depois da gama)
const ALVO: f32 = 60.0;

impl Auto {
    fn abrir(c: &Config) -> Option<Auto> {
        let no = media_ctl(c, &["-e", &c.sensor_tras]).ok()?;
        let cpath = std::ffi::CString::new(no).ok()?;
        let sub = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if sub < 0 {
            return None;
        }
        let a = Auto { sub, exp: 800, ganho: 64 };
        a.aplicar();
        Some(a)
    }

    fn ctrl(&self, id: u32, valor: i32) {
        let mut c = [id as i32, valor];
        if let Err(e) = xioctl(self.sub, VIDIOC_S_CTRL, &mut c) {
            eprintln!("câmera: S_CTRL {id:#x}={valor}: {e}");
        }
    }

    fn aplicar(&self) {
        self.ctrl(V4L2_CID_EXPOSURE, self.exp);
        self.ctrl(V4L2_CID_ANALOGUE_GAIN, self.ganho);
    }

    fn atualizar(&mut self, e: &Estat, bayer: &Bayer) {
        if e.n == 0 {
            return;
        }
        let (r, g, b) = (e.r as f32 / e.n as f32, e.g as f32 / e.n as f32, e.b as f32 / e.n as f32);
        // balanço de branco "mundo cinza", suavizado
        if r > 2.0 && b > 2.0 && g > 2.0 {
            for (ganho, canal) in [(&bayer.ganho_r, r), (&bayer.ganho_b, b)] {
                let alvo = (g / canal * UM as f32).clamp(UM as f32 / 2.0, UM as f32 * 4.0);
                let atual = ganho.load(Ordering::Relaxed) as f32;
                ganho.store((atual * 0.7 + alvo * 0.3) as u32, Ordering::Relaxed);
            }
        }
        // exposição: anda metade do caminho até o alvo
        let razao = (ALVO / g.max(1.0)).clamp(0.25, 4.0);
        if (0.88..1.12).contains(&razao) {
            return;
        }
        let total = self.exp as f32 * self.ganho as f32 / 32.0 * (1.0 + (razao - 1.0) * 0.5);
        let exp = (total as i32).clamp(1, EXP_MAX);
        let ganho = ((total * 32.0 / exp as f32) as i32).clamp(32, GANHO_MAX);
        if (exp, ganho) != (self.exp, self.ganho) {
            (self.exp, self.ganho) = (exp, ganho);
            self.aplicar();
        }
    }
}

impl Drop for Auto {
    fn drop(&mut self) {
        unsafe { libc::close(self.sub) };
    }
}

/// Conversão do quadro conforme a câmera.
enum Conv {
    Yuv(Conversor),
    Bayer(Bayer),
}

impl Conv {
    fn previa(&self, src: &[u8], bpl: usize, w: usize, h: usize) -> (Quadro, Option<Estat>) {
        match self {
            Conv::Yuv(k) => (k.previa(src, bpl, w, h), None),
            Conv::Bayer(k) => {
                let (q, e) = k.previa(src, bpl, w, h);
                (q, Some(e))
            }
        }
    }
    fn cheia(&self, src: &[u8], bpl: usize, w: usize, h: usize) -> Vec<u8> {
        match self {
            Conv::Yuv(k) => k.cheia(src, bpl, w, h),
            Conv::Bayer(k) => k.cheia(src, bpl, w, h),
        }
    }
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
            let c = config();
            let r = capturar(&c, &parar, &foto, &quadro, &tx, &avisar);
            // o nó de vídeo já foi fechado (o Dispositivo caiu dentro de capturar):
            // solta o pipeline e descarrega os módulos, aconteça o que acontecer
            desligar_modulos(&c);
            if let Err(e) = r {
                eprintln!("câmera: {e}");
                let _ = tx.send(Evento::Erro(e));
                avisar();
            }
        });
    }
    Captura { parar, foto, quadro, rx }
}

fn capturar<F>(
    c: &Config,
    parar: &AtomicBool,
    foto: &AtomicBool,
    slot: &Mutex<Option<Quadro>>,
    tx: &Sender<Evento>,
    avisar: &F,
) -> Result<(), String>
where
    F: Fn() + Send + Clone + 'static,
{
    let indisponivel = i18n::t("Câmera indisponível");
    carregar_modulos(c).map_err(|_| indisponivel.clone())?;

    // tamanhos em ordem de preferência; só desce se faltar memória contígua
    let mut tamanhos = vec![(c.largura, c.altura)];
    for t in if c.tras() { &[][..] } else { &[(640, 480), (320, 240)][..] } {
        let t = *t;
        if t.0 < tamanhos[0].0 && !tamanhos.contains(&t) {
            tamanhos.push(t);
        }
    }
    let mut dev = None;
    let mut ultimo_erro = String::new();
    for (w, h) in tamanhos {
        if parar.load(Ordering::Relaxed) {
            return Ok(());
        }
        let video = preparar(c, w, h).map_err(|e| i18n::tf("Câmera indisponível: {}", &[&e]))?;
        match Dispositivo::abrir(&video, c, w, h) {
            Ok(d) => {
                dev = Some(d);
                break;
            }
            Err(Falha::Memoria(e)) => {
                eprintln!("câmera: {w}x{h} sem memória contígua ({e}); tentando menor");
                ultimo_erro = e;
            }
            Err(Falha::Outra(e)) => return Err(i18n::tf("erro na câmera: {}", &[&e])),
        }
    }
    let Some(dev) = dev else {
        return Err(i18n::tf("erro na câmera: {}", &[&ultimo_erro]));
    };
    if parar.load(Ordering::Relaxed) {
        return Ok(());
    }
    let conv = Arc::new(if c.tras() {
        eprintln!("câmera: traseira, RAW10 Bayer GRBG");
        Conv::Bayer(Bayer::novo())
    } else {
        let ordem = c.ordem.or_else(|| Ordem::de_fourcc(dev.fourcc)).unwrap_or(Ordem::Uyvy);
        eprintln!("câmera: convertendo como {ordem:?}");
        Conv::Yuv(Conversor::novo(ordem, c))
    });
    let mut auto = if c.tras() { Auto::abrir(c) } else { None };
    let intervalo = Duration::from_millis(1000 / c.fps.max(1) as u64);
    let (w, h, bpl) = (dev.largura, dev.altura, dev.bpl);

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
        let Some(cheio) = dev.pegar().map_err(|e| i18n::tf("erro na câmera: {}", &[&e]))? else {
            continue;
        };
        sem_quadro = Instant::now();
        // só quadros completos viram imagem; os outros voltam para a fila sem tocar na tela
        if !cheio.ruim {
            let dados = dev.dados(cheio.i, cheio.ini, cheio.fim);
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
                let cru = dados[..bpl * h].to_vec();
                let (conv, tx, avisar, pasta) = (conv.clone(), tx.clone(), avisar.clone(), c.pasta.clone());
                std::thread::spawn(move || {
                    let r = salvar_foto(&conv, &cru, bpl, w, h, &pasta);
                    let _ = tx.send(Evento::Foto(r));
                    avisar();
                });
            }
            // pré-visualização a taxa fixa: só converte se a interface já pegou o anterior
            if ultimo.elapsed() >= intervalo && slot.lock().unwrap().is_none() {
                ultimo = Instant::now();
                let (q, estat) = conv.previa(dados, bpl, w, h);
                *slot.lock().unwrap() = Some(q);
                if let (Some(a), Some(e), Conv::Bayer(b)) = (auto.as_mut(), estat, conv.as_ref()) {
                    a.atualizar(&e, b);
                }
                avisar();
            }
        }
        dev.devolver(cheio.i as u32).map_err(|e| i18n::tf("erro na câmera: {}", &[&format!("QBUF: {e}")]))?;
    }
    Ok(())
    // dev cai aqui: STREAMOFF, munmap, REQBUFS 0, close
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
    conv: &Conv,
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
    fn bayer_cinza() {
        // 4x2 pontos GRBG todos iguais: com ganhos 1,0 sai cinza
        let b = Bayer::novo();
        b.ganho_r.store(UM, Ordering::Relaxed);
        b.ganho_b.store(UM, Ordering::Relaxed);
        let linha = [100u8, 100, 100, 100, 0];
        let src: Vec<u8> = linha.iter().chain(linha.iter()).copied().collect();
        let (q, e) = b.previa(&src, 5, 4, 2);
        let px = q.as_bytes();
        assert!(px.iter().all(|&v| v == px[0]), "{px:?}");
        assert_eq!(e.n, 1);
        let f = b.cheia(&src, 5, 4, 2);
        assert!(f.iter().all(|&v| v == px[0]));
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
