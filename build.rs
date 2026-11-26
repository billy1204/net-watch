//! On Windows: put Net Watch's icon into the program, for Explorer, the
//! taskbar and the Start menu.
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "Net Watch");
        res.set("ProductName", "Net Watch");
        if let Err(e) = res.compile() {
            // Without the Windows SDK's resource compiler it still builds,
            // just with the plain program icon.
            println!("cargo:warning=no icon in the program: {e}");
        }
    }
}
