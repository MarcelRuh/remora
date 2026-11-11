fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI konnte nicht kompiliert werden");
    println!("cargo:rerun-if-changed=ui");

    // Windows: Programmsymbol und Versionsinformationen in die EXE einbetten
    // (Explorer, Taskleiste, Startmenü, „Apps & Features“).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/remora.ico");
        let version = env!("CARGO_PKG_VERSION");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/remora.ico")
            .set("ProductName", "Remora RDP")
            .set("FileDescription", "Remora RDP Manager")
            .set("InternalName", "remora")
            .set("OriginalFilename", "remora.exe")
            .set("ProductVersion", version)
            .set("FileVersion", version)
            .set("LegalCopyright", "© 2026 Remora Contributors · MIT-Lizenz");
        res.compile()
            .expect("Windows-Ressourcen konnten nicht eingebettet werden");
    }
}
