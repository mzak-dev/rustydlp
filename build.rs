fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/app-icon.ico");
        if let Err(e) = winresource::WindowsResource::new().set_icon("assets/app-icon.ico").compile() {
            // A missing resource compiler shouldn't fail the whole build --
            // the app still runs fine without a custom .exe icon, just with
            // the generic one Windows falls back to.
            println!("cargo:warning=could not embed the .exe icon: {e}");
        }
    }
}
