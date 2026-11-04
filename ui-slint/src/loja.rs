//! Loja de apps: catálogo JSON com manifestos prontos para /etc/revive/apps.
//!
//! Fase 1: só apps de manifesto, que são JSON e não executam código. O catálogo
//! vem de uma URL (padrão: `apps/index.json` do repositório) e é guardado em
//! /var/cache/revive/loja.json para a loja continuar listando apps sem rede.
//! Antes de instalar, o SHA-256 do manifesto é conferido; se não bater, nada é
//! gravado.
//!
//! URL do catálogo: `indice=URL` em /etc/revive/loja.conf.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::i18n;

const CONF: &str = "/etc/revive/loja.conf";
const DIR: &str = "/etc/revive/apps";
const CACHE_DIR: &str = "/var/cache/revive";
const CACHE: &str = "/var/cache/revive/loja.json";
const INDICE: &str = "https://raw.githubusercontent.com/siriani/revive-phone/main/apps/index.json";

/// Um app do catálogo (só os campos que a tela da loja precisa).
#[derive(Clone, Debug)]
pub struct AppLoja {
    pub id: String,
    pub nome: String,
    pub descricao: String,
    pub autor: String,
    pub icone: String,
    pub categoria: String,
    pub cor: (u8, u8, u8),
    pub manifesto: String,
    pub sha256: String,
    pub requer: Vec<String>,
    pub instalado: bool,
}

/// Baixa o catálogo (timeout de 15 s) e devolve os apps, marcando os instalados.
///
/// Se a rede falhar, usa o cache de /var/cache/revive/loja.json; sem cache, o
/// erro do download é devolvido.
pub fn catalogo() -> Result<Vec<AppLoja>, String> {
    let url = indice();
    let (texto, baixado) = match baixar_texto(&url) {
        Ok(t) => (t, true),
        Err(e) => {
            eprintln!("loja: não foi possível baixar o catálogo ({e}); usando o cache");
            let t = std::fs::read_to_string(CACHE).map_err(|_| i18n::tf("não foi possível baixar o catálogo: {}", &[&e]))?;
            (t, false)
        }
    };
    let mut apps = parse(&texto)?;
    if baixado {
        let _ = std::fs::create_dir_all(CACHE_DIR);
        let _ = std::fs::write(CACHE, &texto);
    }
    for a in &mut apps {
        a.instalado = arquivo(&a.id).is_file();
    }
    Ok(apps)
}

/// Baixa o manifesto, confere o SHA-256 e grava /etc/revive/apps/<id>.json.
///
/// Hash diferente ou JSON inválido: devolve erro sem gravar nada.
pub fn instalar(a: &AppLoja) -> Result<(), String> {
    if !id_valido(&a.id) {
        return Err(i18n::t("id de app inválido"));
    }
    let dados = baixar(&a.manifesto)?;
    let hash = sha256::hex(&dados);
    let esperado = a.sha256.trim().to_lowercase();
    if hash != esperado {
        return Err(i18n::tf("o manifesto não confere (esperado {}, obtido {})", &[&esperado, &hash]));
    }
    serde_json::from_slice::<Value>(&dados).map_err(|_| i18n::t("o manifesto não é um JSON válido"))?;
    std::fs::create_dir_all(DIR).map_err(|e| i18n::tf("não foi possível criar {}: {}", &[DIR, &e.to_string()]))?;
    let destino = arquivo(&a.id);
    std::fs::write(&destino, &dados).map_err(|e| {
        i18n::tf("não foi possível gravar {}: {}", &[&destino.display().to_string(), &e.to_string()])
    })
}

/// Apaga /etc/revive/apps/<id>.json.
pub fn remover(id: &str) -> Result<(), String> {
    if !id_valido(id) {
        return Err(i18n::t("id de app inválido"));
    }
    let destino = arquivo(id);
    std::fs::remove_file(&destino).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            i18n::tf("o app {} não está instalado", &[id])
        } else {
            i18n::tf("não foi possível remover {}: {}", &[&destino.display().to_string(), &e.to_string()])
        }
    })
}

// ------------------------------------------------------------------- apoio

fn agente() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into()
}

fn baixar(url: &str) -> Result<Vec<u8>, String> {
    agente()
        .get(url)
        .header("User-Agent", "revive")
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

fn baixar_texto(url: &str) -> Result<String, String> {
    String::from_utf8(baixar(url)?).map_err(|_| i18n::t("o catálogo não é texto UTF-8"))
}

/// URL do catálogo: primeira linha `indice=URL` de /etc/revive/loja.conf.
fn indice() -> String {
    std::fs::read_to_string(CONF)
        .ok()
        .and_then(|c| linha_indice(&c))
        .unwrap_or_else(|| INDICE.to_string())
}

fn linha_indice(conf: &str) -> Option<String> {
    conf.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let (chave, valor) = l.split_once('=')?;
            (chave.trim() == "indice").then(|| valor.trim().to_string())
        })
        .filter(|u| !u.is_empty())
}

fn arquivo(id: &str) -> PathBuf {
    Path::new(DIR).join(format!("{id}.json"))
}

/// Ids viram nome de arquivo: só letras, números, "-" e "_".
fn id_valido(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn txt(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

fn cor(s: &str) -> (u8, u8, u8) {
    let h = s.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("40"), 16).unwrap_or(0x40);
    if h.len() == 6 { (p(0), p(2), p(4)) } else { (45, 52, 64) }
}

/// Lê o `apps` do catálogo; entradas sem "id" ou "manifesto" são ignoradas.
fn parse(texto: &str) -> Result<Vec<AppLoja>, String> {
    let v: Value = serde_json::from_str(texto).map_err(|_| i18n::t("catálogo inválido"))?;
    let lista = v["apps"].as_array().ok_or_else(|| i18n::t("catálogo sem a lista de apps"))?;
    Ok(lista.iter().filter_map(de_json).collect())
}

fn de_json(v: &Value) -> Option<AppLoja> {
    let id = txt(v, "id");
    let manifesto = txt(v, "manifesto");
    if id.is_empty() || manifesto.is_empty() {
        eprintln!("loja: entrada do catálogo sem \"id\" ou \"manifesto\" foi ignorada");
        return None;
    }
    Some(AppLoja {
        nome: Some(txt(v, "nome")).filter(|s| !s.is_empty()).unwrap_or_else(|| id.clone()),
        descricao: txt(v, "descricao"),
        autor: txt(v, "autor"),
        icone: Some(txt(v, "icone")).filter(|s| !s.is_empty()).unwrap_or_else(|| "★".into()),
        categoria: txt(v, "categoria"),
        cor: cor(&txt(v, "cor")),
        sha256: txt(v, "sha256").trim().to_lowercase(),
        requer: v["requer"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
            .unwrap_or_default(),
        manifesto,
        id,
        instalado: false,
    })
}

// ------------------------------------------------------------------ sha256

/// SHA-256 em Rust puro (sem crate nova). Usado só para conferir manifestos.
mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    pub fn hash(dados: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
        ];

        let mut msg = dados.to_vec();
        let bits = (dados.len() as u64).wrapping_mul(8);
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bits.to_be_bytes());

        for bloco in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([bloco[4 * i], bloco[4 * i + 1], bloco[4 * i + 2], bloco[4 * i + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }

            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }

            for (i, v) in [a, b, c, d, e, f, g, hh].iter().enumerate() {
                h[i] = h[i].wrapping_add(*v);
            }
        }

        let mut saida = [0u8; 32];
        for (i, v) in h.iter().enumerate() {
            saida[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        saida
    }

    pub fn hex(dados: &[u8]) -> String {
        hash(dados).iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ------------------------------------------------------------------- testes

#[cfg(test)]
mod testes {
    use super::*;

    const CATALOGO: &str = r##"{
      "versao": 1,
      "apps": [
        {
          "id": "cripto",
          "nome": "Cripto",
          "descricao": "Cotações",
          "autor": "Revive",
          "icone": "★",
          "cor": "#7a4b12",
          "categoria": "Finanças",
          "manifesto": "https://exemplo/cripto.json",
          "sha256": "8B218FE363BDC1CDB3C6F05A02667A9ACDA1C828D82CF34F5F43187001A239E0",
          "requer": []
        },
        {
          "id": "sala",
          "cor": "sem-cor#",
          "requer": ["config: URL do servidor", 7],
          "manifesto": "https://exemplo/sala.json"
        },
        {
          "nome": "sem id nem manifesto"
        }
      ]
    }"##;

    #[test]
    fn parse_do_indice() {
        let apps = parse(CATALOGO).unwrap();
        assert_eq!(apps.len(), 2, "entrada sem id/manifesto deve ser ignorada");
        let a = &apps[0];
        assert_eq!(a.id, "cripto");
        assert_eq!(a.nome, "Cripto");
        assert_eq!(a.cor, (0x7a, 0x4b, 0x12));
        assert_eq!(a.icone, "★");
        assert!(a.requer.is_empty());
        assert!(!a.instalado);
        assert_eq!(a.sha256, "8b218fe363bdc1cdb3c6f05a02667a9acda1c828d82cf34f5f43187001a239e0");
        // nome e ícone caem nos padrões; "requer" ignora valores que não são texto
        assert_eq!(apps[1].nome, "sala");
        assert_eq!(apps[1].icone, "★");
        assert_eq!(apps[1].requer, vec!["config: URL do servidor".to_string()]);
    }

    #[test]
    fn indice_sem_lista_e_json_invalido() {
        assert!(parse(r#"{"versao": 1}"#).is_err());
        assert!(parse("não é json").is_err());
    }

    #[test]
    fn confere_hash_do_manifesto() {
        // vetores conhecidos do SHA-256
        assert_eq!(sha256::hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256::hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256::hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );

        let manifesto = br#"{"id": "x", "fonte": {"url": "http://exemplo"}}"#;
        let esperado = sha256::hex(manifesto);
        assert_eq!(esperado.len(), 64);
        // a checagem feita por `instalar`: hash calculado x hash do catálogo
        assert_eq!(sha256::hex(manifesto), esperado.trim().to_lowercase());
        assert_ne!(sha256::hex(b"outro conteudo"), esperado);
        assert_ne!(sha256::hex(manifesto), "0".repeat(64));
    }

    #[test]
    fn id_que_vira_arquivo() {
        assert!(id_valido("home-assistant"));
        assert!(id_valido("uptime_kuma2"));
        assert!(!id_valido(""));
        assert!(!id_valido("../etc/passwd"));
        assert!(!id_valido("a/b"));
        assert!(!id_valido(".oculto"));
    }

    #[test]
    fn config_do_indice() {
        let conf = "# comentário\nindice = https://exemplo/index.json\noutra=1\n";
        assert_eq!(linha_indice(conf).as_deref(), Some("https://exemplo/index.json"));
        assert_eq!(linha_indice("indice=\n"), None);
        assert_eq!(linha_indice(""), None);
    }
}
