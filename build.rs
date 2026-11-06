use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(mobile_ui)");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ios/platform.m");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "ios" || env::var("CARGO_FEATURE_MOBILE").is_ok() {
        println!("cargo:rustc-cfg=mobile_ui");
    }
    if target_os == "ios" {
        // Pequena ponte em Objective-C: sessão de áudio (tocar com tela bloqueada / chave de silêncio)
        // e "safe area" (notch e barra home).
        cc::Build::new().file("ios/platform.m").flag("-fobjc-arc").compile("meu_player_ios");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=UIKit");
    }
}
