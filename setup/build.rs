include!("../build/resources.rs");

/// The installer embeds the application executable. Its path is given by
/// the `LGSTRAY_PAYLOAD` environment variable (see `build.sh`); without it
/// an empty placeholder is embedded and the installer refuses to run.
fn main() {
    println!("cargo:rerun-if-env-changed=LGSTRAY_PAYLOAD");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("lgstray.exe");
    match std::env::var_os("LGSTRAY_PAYLOAD") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", Path::new(&path).display());
            std::fs::copy(&path, &out).expect("LGSTRAY_PAYLOAD must point to the built lgstray.exe");
        }
        None => {
            println!("cargo:warning=LGSTRAY_PAYLOAD is not set: building an installer without payload");
            std::fs::write(&out, b"").unwrap();
        }
    }
    embed("LGSTray Setup", "LGSTray-Setup.exe");
}
