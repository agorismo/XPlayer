//! Modelos de playlist e sessão (formato compatível com o session.json da versão Electron).

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Aceita número, string numérica, "" ou null.
fn lenient_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Number(n) => n.as_u64().map(|x| x as u32),
        Value::String(s) => s.trim().split('/').next().and_then(|x| x.trim().parse().ok()),
        _ => None,
    })
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Track {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    #[serde(deserialize_with = "lenient_u32")]
    pub year: Option<u32>,
    #[serde(deserialize_with = "lenient_u32")]
    pub track_no: Option<u32>,
    pub rg_track_gain: Option<f32>,
    pub rg_track_peak: Option<f32>,
    pub rg_album_gain: Option<f32>,
    pub rg_album_peak: Option<f32>,
    #[serde(skip)]
    pub id: u64,
    #[serde(skip)]
    pub hay: String,
    #[serde(skip)]
    pub meta_loaded: bool,
}

impl Track {
    pub fn from_path(path: &str) -> Track {
        let title = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        Track { path: path.to_string(), title, id: next_id(), ..Default::default() }
    }

    pub fn haystack(&mut self) -> &str {
        if self.hay.is_empty() {
            self.hay = format!("{} {} {}", self.artist, self.title, self.album).to_lowercase();
        }
        &self.hay
    }

    pub fn apply_meta(&mut self, m: Track) {
        self.title = m.title;
        self.artist = m.artist;
        self.album = m.album;
        self.duration = m.duration;
        self.year = m.year;
        self.track_no = m.track_no;
        self.rg_track_gain = m.rg_track_gain;
        self.rg_track_peak = m.rg_track_peak;
        self.rg_album_gain = m.rg_album_gain;
        self.rg_album_peak = m.rg_album_peak;
        self.hay.clear();
        self.meta_loaded = true;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SortSpec {
    pub key: String,
    pub dir: i32,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct ListData {
    pub name: String,
    pub sort: Option<SortSpec>,
    pub tracks: Vec<Track>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default, Debug)]
#[serde(rename_all = "lowercase")]
pub enum RgMode {
    #[default]
    Off,
    Track,
    Album,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    pub preamp: f32,
    pub bands: Vec<f32>,
    pub preset: String,
}
impl Default for EqSettings {
    fn default() -> Self {
        Self { enabled: true, preamp: 0.0, bands: vec![0.0; 10], preset: "Flat".into() }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct RgSettings {
    pub mode: RgMode,
    pub preamp: f32,
    pub prevent_clip: bool,
}
impl Default for RgSettings {
    fn default() -> Self {
        Self { mode: RgMode::Off, preamp: 0.0, prevent_clip: true }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub eq: EqSettings,
    pub rg: RgSettings,
}
impl Default for Settings {
    fn default() -> Self {
        Self { volume: 80.0, eq: EqSettings::default(), rg: RgSettings::default() }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct NowRef {
    pub list: usize,
    pub track: i64,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub v: u32,
    pub view: usize,
    pub shuffle: bool,
    pub settings: Settings,
    pub lists: Vec<ListData>,
    pub now: Option<NowRef>,
}

pub const PRESETS: &[(&str, [f32; 10])] = &[
    ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Rock", [5.0, 4.0, 2.0, -1.0, -2.0, -1.0, 2.0, 4.0, 5.0, 5.0]),
    ("Pop", [-1.0, 1.0, 3.0, 4.0, 3.0, 0.0, -1.0, -1.0, 1.0, 2.0]),
    ("Jazz", [3.0, 2.0, 1.0, 2.0, -2.0, -2.0, 0.0, 1.0, 2.0, 3.0]),
    ("Clássica", [4.0, 3.0, 2.0, 1.0, -1.0, -1.0, 0.0, 2.0, 3.0, 4.0]),
    ("Reforço de graves", [7.0, 6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Reforço de agudos", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 6.0, 7.0]),
    ("Vocal", [-2.0, -2.0, -1.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0]),
    ("Eletrônica", [6.0, 5.0, 1.0, 0.0, -2.0, 2.0, 1.0, 2.0, 5.0, 6.0]),
];

pub fn load_session(dir: &std::path::Path) -> Option<Session> {
    let s = std::fs::read_to_string(dir.join("session.json")).ok()?;
    let mut sess: Session = serde_json::from_str(&s).ok()?;
    for l in &mut sess.lists {
        for t in &mut l.tracks {
            t.id = next_id();
            t.meta_loaded = !t.artist.is_empty() || t.duration > 0.0;
        }
    }
    if sess.settings.eq.bands.len() != 10 {
        sess.settings.eq.bands = vec![0.0; 10];
    }
    Some(sess)
}

pub fn save_session(dir: &std::path::Path, s: &Session) {
    if let Ok(json) = serde_json::to_string(s) {
        let tmp = dir.join("session.json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("session.json"));
        }
    }
}
