fn main() {
    // Sous Windows, wpcap.dll (fournie par Npcap) est chargée en différé :
    // l'application démarre même si Npcap n'est pas installé, seule la
    // capture par application est alors désactivée.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-arg=/DELAYLOAD:wpcap.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }
    tauri_build::build()
}
