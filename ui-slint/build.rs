fn main() {
    // compila as telas (.slint) para Rust na hora do build: nada é interpretado no celular
    slint_build::compile_with_config(
        "ui/app.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent".into())
            // fontes pré-renderizadas no binário: nada de rasterizar texto no celular
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}
