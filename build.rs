fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        // Embed the modern manifest for UAC level asInvoker, common controls v6, and high-DPI scaling
        res.set_manifest_file("app.manifest");
        res.set(
            "FileDescription",
            "SeanPak - Zero-Overhead Native Windows Installer Packager",
        );
        res.set("ProductName", "SeanPak");
        res.set("InternalName", "seanpak");
        res.set("OriginalFilename", "seanpak.exe");
        res.set("LegalCopyright", "Copyright (C) 2026 SeanPak Systems Team");
        // The icon is optional: embed it when present so source checkouts
        // without the binary asset still build cleanly.
        if std::path::Path::new("resources/seanpak.ico").exists() {
            res.set_icon("resources/seanpak.ico");
        }

        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=Failed to compile Windows resources: {}", e);
        }
    }
}
