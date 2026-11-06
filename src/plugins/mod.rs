//! Sistema de plugins. Cada plugin roda na própria thread e recebe eventos do player
//! (track, play, pause, stop, time, ended). Configurações ficam no config.json.

#[cfg(not(target_os = "ios"))]
pub mod discord;
pub mod lastfm;

use crate::config::Config;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub year: Option<u32>,
    pub track_no: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerState {
    pub track: Option<TrackInfo>,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Track,
    Play,
    Pause,
    Stop,
    Time,
    Ended,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    Password,
    Bool,
}

pub struct SettingDef {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub default: Value,
}

pub struct Meta {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub default_enabled: bool,
    pub settings: Vec<SettingDef>,
}

/// O que um plugin enxerga do player.
pub struct Api {
    pub id: &'static str,
    pub data_dir: PathBuf,
    cfg: Arc<Mutex<Config>>,
    status: Arc<Mutex<String>>,
    state: Arc<Mutex<PlayerState>>,
    defaults: Arc<Vec<(&'static str, Value)>>,
}

impl Api {
    pub fn get(&self, key: &str) -> Value {
        self.cfg
            .lock()
            .unwrap()
            .plugin_setting(self.id, key)
            .or_else(|| self.defaults.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone()))
            .unwrap_or(Value::Null)
    }
    pub fn get_str(&self, key: &str) -> String {
        match self.get(key) {
            Value::String(s) => s,
            Value::Null => String::new(),
            v => v.to_string(),
        }
    }
    pub fn get_bool(&self, key: &str) -> bool {
        self.get(key).as_bool().unwrap_or(false)
    }
    pub fn set(&self, key: &str, v: Value) {
        self.cfg.lock().unwrap().set_plugin_setting(self.id, key, v);
    }
    pub fn state(&self) -> PlayerState {
        self.state.lock().unwrap().clone()
    }
    pub fn set_status(&self, s: impl Into<String>) {
        *self.status.lock().unwrap() = s.into();
    }
    pub fn log(&self, msg: impl AsRef<str>) {
        eprintln!("[{}] {}", self.id, msg.as_ref());
    }
}

pub trait Plugin: Send {
    fn start(&mut self, api: &Api);
    fn stop(&mut self, api: &Api);
    fn on_event(&mut self, api: &Api, ev: Event, st: &PlayerState);
    fn on_setting(&mut self, api: &Api, key: &str);
    /// Chamado a cada ~250 ms para trabalho agendado.
    fn tick(&mut self, api: &Api);
}

enum Msg {
    Event(Event, PlayerState),
    Setting(String),
    Shutdown,
}

struct Entry {
    meta: Meta,
    make: fn() -> Box<dyn Plugin>,
    status: Arc<Mutex<String>>,
    tx: Option<Sender<Msg>>,
    handle: Option<JoinHandle<()>>,
}

pub struct PluginInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub enabled: bool,
    pub status: String,
}

pub struct PluginHost {
    entries: Vec<Entry>,
    cfg: Arc<Mutex<Config>>,
    data_dir: PathBuf,
    state: Arc<Mutex<PlayerState>>,
}

impl PluginHost {
    pub fn new(cfg: Arc<Mutex<Config>>, data_dir: PathBuf) -> Self {
        let mut registry: Vec<(Meta, fn() -> Box<dyn Plugin>)> = Vec::new();
        // Discord usa IPC local do desktop: não existe no iOS.
        #[cfg(not(target_os = "ios"))]
        registry.push((discord::meta(), || Box::new(discord::Discord::default())));
        registry.push((lastfm::meta(), || Box::new(lastfm::LastFm::default())));
        let entries = registry
            .into_iter()
            .map(|(meta, make)| Entry { meta, make, status: Arc::default(), tx: None, handle: None })
            .collect();
        Self { entries, cfg, data_dir, state: Arc::default() }
    }

    pub fn start_enabled(&mut self) {
        for i in 0..self.entries.len() {
            let (id, def) = (self.entries[i].meta.id, self.entries[i].meta.default_enabled);
            if self.cfg.lock().unwrap().plugin_enabled(id, def) {
                self.activate(i);
            }
        }
    }

    fn activate(&mut self, i: usize) {
        if self.entries[i].tx.is_some() {
            return;
        }
        let e = &mut self.entries[i];
        let dir = self.data_dir.join("plugins").join(e.meta.id);
        let _ = std::fs::create_dir_all(&dir);
        let api = Api {
            id: e.meta.id,
            data_dir: dir,
            cfg: self.cfg.clone(),
            status: e.status.clone(),
            state: self.state.clone(),
            defaults: Arc::new(e.meta.settings.iter().map(|s| (s.key, s.default.clone())).collect()),
        };
        let mut plugin = (e.make)();
        let (tx, rx) = mpsc::channel::<Msg>();
        let name = e.meta.id;
        e.handle = std::thread::Builder::new()
            .name(format!("plugin-{name}"))
            .spawn(move || {
                plugin.start(&api);
                loop {
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(Msg::Event(ev, st)) => plugin.on_event(&api, ev, &st),
                        Ok(Msg::Setting(k)) => plugin.on_setting(&api, &k),
                        Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    plugin.tick(&api);
                }
                plugin.stop(&api);
                api.set_status("");
            })
            .ok();
        e.tx = Some(tx);
    }

    fn deactivate(&mut self, i: usize) {
        let e = &mut self.entries[i];
        if let Some(tx) = e.tx.take() {
            let _ = tx.send(Msg::Shutdown);
        }
        if let Some(h) = e.handle.take() {
            let _ = h.join();
        }
        e.status.lock().unwrap().clear();
    }

    pub fn shutdown(&mut self) {
        for i in 0..self.entries.len() {
            self.deactivate(i);
        }
    }

    pub fn set_enabled(&mut self, i: usize, on: bool) {
        self.cfg.lock().unwrap().set_plugin_enabled(self.entries[i].meta.id, on);
        if on {
            self.activate(i);
        } else {
            self.deactivate(i);
        }
    }

    pub fn set_setting(&mut self, i: usize, key: &str, v: Value) {
        let e = &self.entries[i];
        self.cfg.lock().unwrap().set_plugin_setting(e.meta.id, key, v);
        if let Some(tx) = &e.tx {
            let _ = tx.send(Msg::Setting(key.to_string()));
        }
    }

    pub fn emit(&mut self, ev: Event, st: PlayerState) {
        *self.state.lock().unwrap() = st.clone();
        for e in &self.entries {
            if let Some(tx) = &e.tx {
                let _ = tx.send(Msg::Event(ev, st.clone()));
            }
        }
    }

    pub fn infos(&self) -> Vec<PluginInfo> {
        self.entries
            .iter()
            .map(|e| PluginInfo {
                id: e.meta.id,
                name: e.meta.name,
                version: e.meta.version,
                description: e.meta.description,
                enabled: self.cfg.lock().unwrap().plugin_enabled(e.meta.id, e.meta.default_enabled),
                status: e.status.lock().unwrap().clone(),
            })
            .collect()
    }

    pub fn settings_of(&self, i: usize) -> &[SettingDef] {
        &self.entries[i].meta.settings
    }

    /// Valor atual de uma configuração (com o padrão do plugin, se nunca foi alterada).
    pub fn setting_value(&self, i: usize, key: &str) -> Value {
        let e = &self.entries[i];
        self.cfg
            .lock()
            .unwrap()
            .plugin_setting(e.meta.id, key)
            .or_else(|| e.meta.settings.iter().find(|s| s.key == key).map(|s| s.default.clone()))
            .unwrap_or(Value::Null)
    }
}
