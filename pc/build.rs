fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=icons/app.ico");
        winresource::WindowsResource::new()
            .set_icon("icons/app.ico")
            .compile()
            .expect("embedding the icon resource");
    }
}
