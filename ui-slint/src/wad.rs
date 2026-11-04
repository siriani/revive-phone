//! Lê uma figura de dentro do WAD do próprio jogador (ex.: o rosto do Doomguy, STFST01)
//! para servir de ícone do app DOOM II. Nada da arte do jogo vai para o repositório:
//! o ícone sai do arquivo que o jogador já tem no aparelho.

use slint::{Rgba8Pixel, SharedPixelBuffer};

fn u16le(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32le(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Conteúdo de um "lump" do WAD pelo nome.
fn lump<'a>(wad: &'a [u8], nome: &str) -> Option<&'a [u8]> {
    if wad.get(1..4)? != b"WAD" {
        return None;
    }
    let (n, dir) = (u32le(wad, 4)?, u32le(wad, 8)?);
    (0..n).find_map(|i| {
        let e = dir + i * 16;
        let nm = wad.get(e + 8..e + 16)?;
        let nm = &nm[..nm.iter().position(|&c| c == 0).unwrap_or(8)];
        if nm.eq_ignore_ascii_case(nome.as_bytes()) {
            let (pos, tam) = (u32le(wad, e)?, u32le(wad, e + 4)?);
            wad.get(pos..pos + tam)
        } else {
            None
        }
    })
}

/// Figura no formato "patch" do DOOM (colunas de postes) -> RGBA, com a paleta do jogo.
pub fn figura(caminho: &str, nome: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let wad = std::fs::read(caminho).ok()?;
    let pal = lump(&wad, "PLAYPAL")?;
    let p = lump(&wad, nome)?;
    let (w, h) = (u16le(p, 0)?, u16le(p, 2)?);
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return None;
    }
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
    let px = buf.make_mut_slice();
    for x in 0..w {
        let mut o = u32le(p, 8 + x * 4)?;
        loop {
            let topo = *p.get(o)? as usize;
            if topo == 0xFF {
                break;
            }
            let n = *p.get(o + 1)? as usize;
            for k in 0..n {
                let y = topo + k;
                let c = *p.get(o + 3 + k)? as usize * 3;
                if y < h {
                    px[y * w + x] = Rgba8Pixel { r: *pal.get(c)?, g: *pal.get(c + 1)?, b: *pal.get(c + 2)?, a: 255 };
                }
            }
            o += n + 4;
        }
    }
    Some(buf)
}
