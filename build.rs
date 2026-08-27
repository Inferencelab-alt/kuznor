#[cfg(target_os = "windows")]
fn main() {
    println!("cargo:rerun-if-changed=assets/kuznor.ico");

    let mut resources = winres::WindowsResource::new();
    resources.set_icon("assets/kuznor.ico");
    resources
        .compile()
        .expect("No se pudo incrustar assets/kuznor.ico en el ejecutable de Kuznor");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
