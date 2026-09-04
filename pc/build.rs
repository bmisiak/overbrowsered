fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=icons/tray.ico");
        println!("cargo:rerun-if-changed=icons/app-windows.ico");
        println!("cargo:rerun-if-changed=package/windows/overbrowsered.exe.manifest");
        winresource::WindowsResource::new()
            .set_icon_with_id("icons/tray.ico", "2")
            .set_icon("icons/app-windows.ico")
            .set_manifest_file("package/windows/overbrowsered.exe.manifest")
            .compile()
            .expect("embedding the icon and manifest resources");
    }
}
