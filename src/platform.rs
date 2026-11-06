//! Diferenças entre plataformas (principalmente iOS): pastas, sessão de áudio e "safe area".

use std::path::{Path, PathBuf};

#[cfg(target_os = "ios")]
extern "C" {
    fn meu_player_setup_audio_session();
    fn meu_player_safe_insets(out: *mut f32);
}

/// Configura a sessão de áudio do iOS (no-op nas outras plataformas).
pub fn setup_audio_session() {
    #[cfg(target_os = "ios")]
    unsafe {
        meu_player_setup_audio_session();
    }
}

/// Margens seguras [topo, base, esquerda, direita] em pontos (zero fora do iOS).
pub fn safe_insets() -> [f32; 4] {
    #[allow(unused_mut)]
    let mut out = [0.0f32; 4];
    #[cfg(target_os = "ios")]
    unsafe {
        meu_player_safe_insets(out.as_mut_ptr());
    }
    out
}

/// Pasta "Documents" do app. No iOS ela aparece no app Arquivos (UIFileSharingEnabled).
#[allow(dead_code)]
pub fn docs_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let d = home.join("Documents");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// No iOS o UUID da pasta do app muda a cada (re)instalação. Se `path` não existe mais mas contém
/// `marker` (ex.: "/Documents/"), tenta o mesmo caminho relativo dentro de `base`.
#[allow(dead_code)]
pub fn rebase(path: &str, marker: &str, base: &Path) -> String {
    if Path::new(path).exists() {
        return path.to_string();
    }
    if let Some(i) = path.find(marker) {
        let cand = base.join(&path[i + marker.len()..]);
        if cand.exists() {
            return cand.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebase_finds_file_in_new_container() {
        let base = std::env::temp_dir().join(format!("meu-player-rebase-{}", std::process::id()));
        std::fs::create_dir_all(base.join("Albuns")).unwrap();
        std::fs::write(base.join("Albuns/a.mp3"), b"x").unwrap();
        let old = "/var/mobile/Containers/Data/Application/AAAA-1111/Documents/Albuns/a.mp3";
        assert_eq!(rebase(old, "/Documents/", &base), base.join("Albuns/a.mp3").to_string_lossy());
        // arquivo que não existe em lugar nenhum: mantém o caminho
        let gone = "/var/x/Documents/nada.mp3";
        assert_eq!(rebase(gone, "/Documents/", &base), gone);
    }
}
