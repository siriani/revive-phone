//! Traduções da interface (pt base, en e es).
//!
//! O idioma fica em /var/lib/revive/idioma ("pt", "en" ou "es"). Os catálogos dos
//! outros idiomas são .po embutidos no binário; o texto original é sempre o
//! português usado em `t`/`tf`, então em "pt" nada é traduzido.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const ARQUIVO: &str = "/var/lib/revive/idioma";

const CATALOGO_EN: &str = include_str!("../lang/en/LC_MESSAGES/revive-ui.po");
const CATALOGO_ES: &str = include_str!("../lang/es/LC_MESSAGES/revive-ui.po");

static IDIOMA: Mutex<Option<String>> = Mutex::new(None);

/// Idioma atual; lê /var/lib/revive/idioma uma vez (padrão "pt").
pub fn idioma() -> String {
    let mut atual = IDIOMA.lock().unwrap();
    if let Some(l) = atual.as_ref() {
        return l.clone();
    }
    let l = std::fs::read_to_string(ARQUIVO)
        .ok()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s == "en" || s == "es")
        .unwrap_or_else(|| "pt".to_string());
    *atual = Some(l.clone());
    l
}

/// Grava o idioma no disco e atualiza o estado em memória.
pub fn definir(l: &str) {
    let l = match l.trim().to_lowercase().as_str() {
        "en" => "en",
        "es" => "es",
        _ => "pt",
    };
    let _ = std::fs::create_dir_all("/var/lib/revive");
    let _ = std::fs::write(ARQUIVO, l);
    *IDIOMA.lock().unwrap() = Some(l.to_string());
}

/// Catálogo do idioma, carregado na primeira uso.
fn catalogo(l: &str) -> Option<&'static HashMap<String, String>> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    static ES: OnceLock<HashMap<String, String>> = OnceLock::new();
    match l {
        "en" => Some(EN.get_or_init(|| parse_po(CATALOGO_EN))),
        "es" => Some(ES.get_or_init(|| parse_po(CATALOGO_ES))),
        _ => None,
    }
}

/// Traduz `s`; em "pt" devolve o próprio texto e, sem tradução, também.
pub fn t(s: &str) -> String {
    let l = idioma();
    if l == "pt" {
        return s.to_string();
    }
    catalogo(&l).and_then(|c| c.get(s)).cloned().unwrap_or_else(|| s.to_string())
}

/// Traduz `s` e troca cada "{}" pelos argumentos, em ordem.
pub fn tf(s: &str, args: &[&str]) -> String {
    let mut out = t(s);
    for a in args {
        let Some(i) = out.find("{}") else { break };
        out.replace_range(i..i + 2, a);
    }
    out
}

// --------------------------------------------------------------------- .po

#[derive(Clone, Copy, PartialEq)]
enum Campo {
    Nenhum,
    Id,
    Texto,
}

fn tirar_aspas(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

fn desescapar(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(outro) => {
                out.push('\\');
                out.push(outro);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn guardar(
    id: &mut String,
    texto: &mut String,
    tem_id: &mut bool,
    tem_texto: &mut bool,
    mapa: &mut HashMap<String, String>,
) {
    if *tem_id && *tem_texto && !id.is_empty() && !texto.is_empty() {
        mapa.insert(std::mem::take(id), std::mem::take(texto));
    } else {
        id.clear();
        texto.clear();
    }
    *tem_id = false;
    *tem_texto = false;
}

/// Parser simples de .po: msgid/msgstr, multilinha e escapes; ignora
/// comentários, msgctxt, msgid_plural e msgstr[n]. msgstr vazio = sem tradução.
fn parse_po(conteudo: &str) -> HashMap<String, String> {
    let mut mapa = HashMap::new();
    let mut id = String::new();
    let mut texto = String::new();
    let (mut tem_id, mut tem_texto) = (false, false);
    let mut campo = Campo::Nenhum;

    for linha in conteudo.lines() {
        let l = linha.trim();
        if l.is_empty() {
            guardar(&mut id, &mut texto, &mut tem_id, &mut tem_texto, &mut mapa);
            campo = Campo::Nenhum;
            continue;
        }
        if l.starts_with('#') {
            continue;
        }
        if let Some(resto) = l.strip_prefix("msgid ") {
            if tem_texto {
                guardar(&mut id, &mut texto, &mut tem_id, &mut tem_texto, &mut mapa);
            }
            id.clear();
            id.push_str(&desescapar(tirar_aspas(resto)));
            tem_id = true;
            campo = Campo::Id;
        } else if let Some(resto) = l.strip_prefix("msgstr ") {
            texto.clear();
            texto.push_str(&desescapar(tirar_aspas(resto)));
            tem_texto = true;
            campo = Campo::Texto;
        } else if l.starts_with("msgctxt ") || l.starts_with("msgid_plural ") || l.starts_with("msgstr[") {
            campo = Campo::Nenhum;
        } else if l.starts_with('"') {
            let pedaco = desescapar(tirar_aspas(l));
            match campo {
                Campo::Id => id.push_str(&pedaco),
                Campo::Texto => texto.push_str(&pedaco),
                Campo::Nenhum => {}
            }
        }
    }
    guardar(&mut id, &mut texto, &mut tem_id, &mut tem_texto, &mut mapa);
    mapa
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn entrada_simples_e_cabecalho() {
        let po = "# comentário\nmsgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\nmsgid \"Olá\"\nmsgstr \"Hello\"\n";
        let m = parse_po(po);
        assert_eq!(m.get("Olá").map(String::as_str), Some("Hello"));
        assert!(!m.contains_key(""));
    }

    #[test]
    fn multilinha() {
        let po = "msgid \"linha \"\n\"seguinte\"\nmsgstr \"line \"\n\"next\"\n";
        let m = parse_po(po);
        assert_eq!(m.get("linha seguinte").map(String::as_str), Some("line next"));
    }

    #[test]
    fn escapes() {
        let po = "msgid \"diz \\\"oi\\\"\"\nmsgstr \"say \\\"hi\\\"\"\n\nmsgid \"a\\nb\"\nmsgstr \"a\\nb\"\n\nmsgid \"barra \\\\\"\nmsgstr \"barra \\\\\"\n";
        let m = parse_po(po);
        assert_eq!(m.get("diz \"oi\"").map(String::as_str), Some("say \"hi\""));
        assert_eq!(m.get("a\nb").map(String::as_str), Some("a\nb"));
        assert_eq!(m.get("barra \\").map(String::as_str), Some("barra \\"));
    }

    #[test]
    fn vazio_sem_traducao() {
        let po = "msgid \"Sem tradução\"\nmsgstr \"\"\n";
        let m = parse_po(po);
        assert!(!m.contains_key("Sem tradução"));
    }

    #[test]
    fn ignora_contexto_e_plural() {
        let po = "msgctxt \"menu\"\nmsgid \"Abrir\"\nmsgid_plural \"Abrir\"\nmsgstr[0] \"Open\"\nmsgstr[1] \"Opens\"\n\nmsgid \"Fechar\"\nmsgstr \"Close\"\n";
        let m = parse_po(po);
        assert!(!m.contains_key("Abrir"));
        assert_eq!(m.get("Fechar").map(String::as_str), Some("Close"));
    }
}
