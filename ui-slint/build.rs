use std::path::{Path, PathBuf};

/// .c do doomgeneric que precisam de SDL/Allegro (backends de música e som):
/// o Makefile do doom também não os compila, e nada do jogo depende deles.
const FORA: &[&str] = &[
    "gusconf.c",
    "i_allegromusic.c",
    "i_allegrosound.c",
    "i_sdlmusic.c",
    "i_sdlsound.c",
    "icon.c",
];

fn main() {
    // tamanhos de texto da biblioteca Material (escalada 1,5x): chegam aos componentes por
    // variável, então o compilador não os vê; sem isto a fonte não seria pré-renderizada neles
    // (set_var é unsafe na edição 2024; o build script é de uma thread só)
    unsafe { std::env::set_var("SLINT_FONT_SIZES", "16,17,18,21,24,26,33,34,36,42,44,48,54,64,68,84,86,96,110,120") };
    // compila as telas (.slint) para Rust na hora do build: nada é interpretado no celular
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent".into())
            // fontes pré-renderizadas no binário: nada de rasterizar texto no celular
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
            // biblioteca oficial Material 3 do Slint (MIT), copiada da v1.18.1:
            // import { ... } from "@material";
            .with_library_paths(std::collections::HashMap::from([(
                "material".to_string(),
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("material/material.slint"),
            )])),
    )
    .unwrap();

    // O backend do DOOM (doom/revive/doomgeneric_revive.c) não entra: ele é substituído
    // por src/doom.rs. O motor continua sendo C e vira uma biblioteca estática.
    let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join("../doom/engine");
    let mut fontes: Vec<PathBuf> = std::fs::read_dir(&engine)
        .unwrap_or_else(|e| panic!("doom/engine: {e}"))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("c"))
        .filter(|p| {
            let nome = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !FORA.contains(&nome)
        })
        .collect();
    fontes.sort();
    assert!(!fontes.is_empty(), "nenhum .c em {}", engine.display());

    let mut build = cc::Build::new();
    build
        .files(&fontes)
        .include(&engine)
        .flag("-O2")
        .flag("-w")
        .define("NORMALUNIX", None)
        .define("LINUX", None)
        .define("_DEFAULT_SOURCE", None)
        // "Quit" no menu (e qualquer exit do motor) volta para a interface: revive_doom_sair
        .define("exit", "revive_doom_sair")
        .compile("doomengine");

    println!("cargo:rerun-if-changed=../doom/engine");
}
