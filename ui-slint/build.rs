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
}
