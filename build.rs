fn main() {
    println!("cargo:rerun-if-changed=assets/gpx.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let mut cli = winresource::WindowsResource::new();
    cli.set_icon("assets/gpx.ico");
    if let Err(error) = cli.compile_for("gpx-tool") {
        panic!("could not embed assets/gpx.ico into gpx-tool: {error}");
    }

    let mut gui = winresource::WindowsResource::new();
    gui.set_icon("assets/gpx.ico");
    gui.set("ProductName", "Featherweight");
    gui.set("FileDescription", "Featherweight");
    if let Err(error) = gui.compile_for("featherweight") {
        panic!("could not embed assets/gpx.ico into featherweight: {error}");
    }
}
