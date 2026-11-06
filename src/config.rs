//! Pastas de dados e configuração em JSON.
//!
//! Usa a mesma pasta e o mesmo formato da versão Electron (`%APPDATA%\meu-player` no Windows,
//! `~/.config/meu-player` no Linux): `session.json` e `config.json` antigos continuam valendo.

use crate::cover::CoverSettings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "ios")]
    let base = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
        .unwrap_or_else(|| PathBuf::from("."));
    #[cfg(not(target_os = "ios"))]
    let base = directories::BaseDirs::new()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("meu-player");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct PluginCfg {
    pub enabled: Option<bool>,
    pub settings: HashMap<String, Value>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub plugins: HashMap<String, PluginCfg>,
    pub cover: CoverSettings,
    /// Capas escolhidas manualmente: "track|<caminho>" ou "album|<artista>|<álbum>" -> arquivo de imagem.
    pub custom_covers: HashMap<String, String>,
    #[serde(skip)]
    path: PathBuf,
}

impl Config {
    pub fn load(dir: &std::path::Path) -> Config {
        let path = dir.join("config.json");
        let mut c: Config = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        c.path = path;
        c.cover.normalize();
        c
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn plugin_enabled(&self, id: &str, default: bool) -> bool {
        self.plugins.get(id).and_then(|p| p.enabled).unwrap_or(default)
    }

    pub fn set_plugin_enabled(&mut self, id: &str, on: bool) {
        self.plugins.entry(id.to_string()).or_default().enabled = Some(on);
        self.save();
    }

    pub fn plugin_setting(&self, id: &str, key: &str) -> Option<Value> {
        self.plugins.get(id).and_then(|p| p.settings.get(key)).cloned()
    }

    pub fn set_plugin_setting(&mut self, id: &str, key: &str, v: Value) {
        self.plugins.entry(id.to_string()).or_default().settings.insert(key.to_string(), v);
        self.save();
    }
}
