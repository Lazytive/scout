//! Embeds the NSIS installer that `SCOUT_NSIS_PAYLOAD` names (`cargo xtask bundle` sets
//! it). Without it the setup carries no installer: a development build, whose window says
//! so. `SCOUT_INSTALL_BYTES` (what a whole install holds) is read by `main.rs` itself.

fn main() {
    println!("cargo:rerun-if-env-changed=SCOUT_NSIS_PAYLOAD");
    println!("cargo:rerun-if-env-changed=SCOUT_INSTALL_BYTES");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default())
        .join("payload.exe");
    let written = match std::env::var_os("SCOUT_NSIS_PAYLOAD") {
        Some(payload) => {
            println!("cargo:rerun-if-changed={}", payload.to_string_lossy());
            std::fs::copy(&payload, &out).map(|_| ())
        }
        None => std::fs::write(&out, []),
    };
    if let Err(e) = written {
        println!("cargo:warning=setup payload: {e}");
        let _ = std::fs::write(&out, []);
    }
    tauri_build::build();
}
