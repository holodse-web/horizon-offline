use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/horizon-offline.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon = root
        .join("assets/horizon-offline.ico")
        .to_string_lossy()
        .replace('\\', "/");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<_> = version.split('.').take(3).collect();
    let numeric = format!("{},{},{},0", numbers[0], numbers[1], numbers[2]);
    let script = out.join("horizon-offline.rc");
    fs::write(
        &script,
        format!(
            r#"
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK 0x3fL
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "RRM Network\0"
      VALUE "FileDescription", "Horizon Offline launcher\0"
      VALUE "FileVersion", "{version}\0"
      VALUE "InternalName", "horizon-offline\0"
      VALUE "OriginalFilename", "horizon-offline.exe\0"
      VALUE "ProductName", "HORIZON OFFLINE\0"
      VALUE "ProductVersion", "{version}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
        ),
    )
    .expect("Cannot write Windows icon resource");

    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let compiler =
        env::var_os("RC").unwrap_or_else(|| if msvc { "rc.exe" } else { "windres.exe" }.into());
    let resource = out.join(if msvc {
        "horizon-offline.res"
    } else {
        "horizon-offline-icon.o"
    });
    let mut command = Command::new(&compiler);
    if msvc {
        command
            .args(["/nologo", "/c65001", "/fo"])
            .arg(&resource)
            .arg(&script);
    } else {
        command
            .args([
                "--codepage=65001",
                "--input-format=rc",
                "--output-format=coff",
                "--input",
            ])
            .arg(&script)
            .arg("--output")
            .arg(&resource);
    }
    let status = command.status().unwrap_or_else(|e| panic!(
        "Cannot run Windows resource compiler {compiler:?}: {e}. Install Windows SDK rc.exe (MSVC) or MinGW-w64 windres.exe (GNU), or set RC."
    ));
    assert!(status.success(), "Windows icon resource compilation failed");
    println!(
        "cargo:rustc-link-arg-bin=horizon-offline={}",
        resource.display()
    );
}
