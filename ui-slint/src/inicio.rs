//! Ordem e visibilidade dos apps da tela inicial.
//!
//! O estado fica em /var/lib/revive/inicio, uma linha por id de app. Uma linha
//! começando com "-" indica app oculto (ex.: "-bt").

use std::fs;
use std::path::Path;

const ARQUIVO: &str = "/var/lib/revive/inicio";
const FIXO: &str = "ajustes"; // nunca pode ser ocultado

/// Lê a ordem salva, mantendo só ids conhecidos. Sem arquivo, tudo visível na
/// ordem recebida.
pub fn carregar(todos: &[String]) -> Vec<(String, bool)> {
    let texto = fs::read_to_string(ARQUIVO).unwrap_or_default();
    interpretar(&texto, todos)
}

/// Converte o texto do arquivo em pares (id, visível).
fn interpretar(texto: &str, todos: &[String]) -> Vec<(String, bool)> {
    let mut lista: Vec<(String, bool)> = Vec::new();

    for linha in texto.lines() {
        let linha = linha.trim();
        if linha.is_empty() {
            continue;
        }
        // "-" no começo significa oculto.
        let (id, visivel) = match linha.strip_prefix('-') {
            Some(resto) => (resto.trim(), false),
            None => (linha, true),
        };
        if id.is_empty() || !todos.iter().any(|t| t == id) {
            continue; // id removido/desconhecido some
        }
        if lista.iter().any(|(x, _)| x == id) {
            continue; // sem duplicatas
        }
        lista.push((id.to_string(), visivel));
    }

    // Ids novos vão para o fim, visíveis, na ordem de `todos`.
    for id in todos {
        if !lista.iter().any(|(x, _)| x == id) {
            lista.push((id.clone(), true));
        }
    }

    lista
}

/// Grava a lista no arquivo. Erros de escrita são só avisados.
pub fn salvar(lista: &[(String, bool)]) {
    if let Some(pai) = Path::new(ARQUIVO).parent() {
        let _ = fs::create_dir_all(pai);
    }

    let mut texto = String::new();
    for (id, visivel) in lista {
        if !visivel {
            texto.push('-');
        }
        texto.push_str(id);
        texto.push('\n');
    }

    if let Err(e) = fs::write(ARQUIVO, texto) {
        eprintln!("inicio: não consegui salvar {ARQUIVO}: {e}");
    }
}

/// Inverte a visibilidade do item i, sem esconder "ajustes" nem o último visível.
pub fn alternar(lista: &mut [(String, bool)], i: usize) {
    if i >= lista.len() {
        return;
    }

    // "ajustes" sempre fica visível.
    if lista[i].0 == FIXO {
        lista[i].1 = true;
        return;
    }

    if lista[i].1 {
        // Não deixa todos ocultos.
        let visiveis = lista.iter().filter(|(_, v)| *v).count();
        if visiveis <= 1 {
            return;
        }
    }

    lista[i].1 = !lista[i].1;
}

/// Move o item i para i+delta, limitado às bordas.
pub fn mover(lista: &mut Vec<(String, bool)>, i: usize, delta: i32) {
    if i >= lista.len() {
        return;
    }

    let ultimo = lista.len() as i64 - 1;
    let destino = (i as i64 + delta as i64).clamp(0, ultimo) as usize;
    if destino == i {
        return;
    }

    let item = lista.remove(i);
    lista.insert(destino, item);
}

/// Ids visíveis, na ordem atual.
pub fn visiveis(lista: &[(String, bool)]) -> Vec<String> {
    lista
        .iter()
        .filter(|(_, visivel)| *visivel)
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod testes {
    use super::*;

    fn ids(lista: &[(String, bool)]) -> Vec<&str> {
        lista.iter().map(|(id, _)| id.as_str()).collect()
    }

    fn todos(itens: &[&str]) -> Vec<String> {
        itens.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arquivo_ausente_mostra_tudo_na_ordem() {
        // Texto vazio é o mesmo que arquivo inexistente.
        let lista = interpretar("", &todos(&["painel", "musica", "bt"]));
        assert_eq!(lista, vec![
            ("painel".to_string(), true),
            ("musica".to_string(), true),
            ("bt".to_string(), true),
        ]);
    }

    #[test]
    fn app_novo_vai_para_o_fim() {
        let lista = interpretar("musica\n", &todos(&["painel", "musica", "bt"]));
        assert_eq!(ids(&lista), vec!["musica", "painel", "bt"]);
        assert!(lista.iter().all(|(_, v)| *v));
    }

    #[test]
    fn id_removido_some() {
        let lista = interpretar("painel\nfantasma\n-musica\n", &todos(&["painel", "musica"]));
        assert_eq!(lista, vec![
            ("painel".to_string(), true),
            ("musica".to_string(), false),
        ]);
    }

    #[test]
    fn sem_duplicatas() {
        let lista = interpretar("painel\npainel\n", &todos(&["painel"]));
        assert_eq!(lista, vec![("painel".to_string(), true)]);
    }

    #[test]
    fn alternar_o_ultimo_visivel_nao_muda() {
        let mut lista = vec![
            ("painel".to_string(), true),
            ("musica".to_string(), false),
        ];
        alternar(&mut lista, 0);
        assert!(lista[0].1, "o último visível não pode ser ocultado");

        // Com outro visível, esconder funciona.
        lista[1].1 = true;
        alternar(&mut lista, 0);
        assert!(!lista[0].1);
    }

    #[test]
    fn alternar_ajustes_nunca_oculta() {
        let mut lista = vec![
            ("ajustes".to_string(), true),
            ("painel".to_string(), true),
        ];
        alternar(&mut lista, 0);
        assert!(lista[0].1);

        // Mesmo se estiver oculto no arquivo, volta a aparecer.
        lista[0].1 = false;
        alternar(&mut lista, 0);
        assert!(lista[0].1);
    }

    #[test]
    fn mover_nas_bordas() {
        let mut lista = vec![
            ("a".to_string(), true),
            ("b".to_string(), true),
            ("c".to_string(), true),
        ];

        mover(&mut lista, 0, -1);
        assert_eq!(ids(&lista), vec!["a", "b", "c"]);

        mover(&mut lista, 2, 5);
        assert_eq!(ids(&lista), vec!["a", "b", "c"]);

        mover(&mut lista, 0, 1);
        assert_eq!(ids(&lista), vec!["b", "a", "c"]);

        mover(&mut lista, 9, 1); // índice inválido não faz nada
        assert_eq!(ids(&lista), vec!["b", "a", "c"]);
    }

    #[test]
    fn visiveis_ignora_ocultos() {
        let lista = vec![
            ("painel".to_string(), true),
            ("bt".to_string(), false),
            ("m:home-assistant".to_string(), true),
        ];
        assert_eq!(visiveis(&lista), vec!["painel", "m:home-assistant"]);
    }
}
