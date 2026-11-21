// Shared by the build scripts of both executables (`include!`d): compiles a
// Windows resource script (icon, version info, manifest) and links it.
//
// Resource compilers tried: `rc.exe` / `llvm-rc` for MSVC targets, `zig rc`
// otherwise (the project is cross-compiled with cargo-zigbuild, whose
// linker accepts .res files directly). Without one the build still
// succeeds, the executable just has no icon / version info.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn embed(description: &str, original_name: &str) {
    println!("cargo:rerun-if-changed=../build/resources.rs");
    println!("cargo:rerun-if-changed=../build/app.manifest");
    println!("cargo:rerun-if-changed=../app/assets/logo_black.ico");
    println!("cargo:rerun-if-env-changed=LGSTRAY_NO_RESOURCES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") || std::env::var_os("LGSTRAY_NO_RESOURCES").is_some() {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let version = env!("CARGO_PKG_VERSION");
    let numbers: Vec<&str> = version.split(['.', '-', '+']).take(3).collect();
    let quad = format!("{},{},{},0", numbers[0], numbers.get(1).unwrap_or(&"0"), numbers.get(2).unwrap_or(&"0"));
    let slashes = |p: PathBuf| p.display().to_string().replace('\\', "/");

    let rc = out.join("app.rc");
    let res = out.join("app.res");
    let script = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {quad}
PRODUCTVERSION {quad}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "LGSTray contributors"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{original_name}"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "{original_name}"
      VALUE "ProductName", "LGSTray"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = slashes(root.join("app/assets/logo_black.ico")),
        manifest = slashes(root.join("build/app.manifest")),
    );
    std::fs::write(&rc, script).unwrap();

    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let fo = format!("/fo{}", res.display());
    let candidates: &[(&str, &[&str])] =
        if msvc { &[("rc.exe", &["/nologo"]), ("llvm-rc", &[]), ("zig", &["rc"])] } else { &[("zig", &["rc"]), ("llvm-rc", &[])] };
    let _ = std::fs::remove_file(&res);
    for (tool, prefix) in candidates {
        let ok = Command::new(tool).args(*prefix).arg(&fo).arg(&rc).output().map(|o| o.status.success()).unwrap_or(false);
        if ok && res.exists() {
            println!("cargo:rustc-link-arg-bins={}", res.display());
            return;
        }
    }
    println!("cargo:warning=no resource compiler found (zig rc / rc.exe / llvm-rc): building without icon and version info");
}
