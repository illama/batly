include!("../build/resources.rs");

/// The installer embeds the application executable. Its path is given by
/// the `BATLY_PAYLOAD` environment variable (see `build.sh`); without it
/// an empty placeholder is embedded and the installer refuses to run.
fn main() {
    println!("cargo:rerun-if-env-changed=BATLY_PAYLOAD");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("batly.exe");
    match std::env::var_os("BATLY_PAYLOAD") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", Path::new(&path).display());
            std::fs::copy(&path, &out).expect("BATLY_PAYLOAD must point to the built batly.exe");
        }
        None => {
            println!("cargo:warning=BATLY_PAYLOAD is not set: building an installer without payload");
            std::fs::write(&out, b"").unwrap();
        }
    }
    embed("Batly Setup", "Batly-Setup.exe");
}
