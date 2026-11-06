//! Interface (egui) e lógica do player: playlists, controles, capas, painéis de áudio/plugins.

use crate::audio::{AudioEvent, Engine};
use crate::config::{self, Config};
use crate::cover::{self, CoverQuery, CoverSource};
use crate::dsp::{self, EqParams};
use crate::meta;
use crate::platform;
use crate::playlist::*;
use crate::plugins::{Event, PlayerState, PluginHost, TrackInfo};
use egui::{Align, Align2, Color32, Key, Layout, RichText, Sense, Vec2};
use egui_extras::{Column, TableBuilder};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering as AO};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCENT: Color32 = Color32::from_rgb(0x4a, 0xa3, 0xff);
const DIM: Color32 = Color32::from_rgb(0x8d, 0x8d, 0x95);
const SEL: Color32 = Color32::from_rgb(0x2c, 0x3a, 0x4d);
const ROW_H: f32 = 24.0;

struct Now {
    list: usize, // usize::MAX = playlist fechada
    track: Track,
}

#[derive(Clone)]
struct CoverTarget {
    path: String,
    artist: String,
    album: String,
}

impl CoverTarget {
    fn of(t: &Track) -> Self {
        Self { path: t.path.clone(), artist: t.artist.clone(), album: t.album.clone() }
    }
}

enum Dlg {
    Add(Vec<String>),
    Note(String),
    CoverFile { target: CoverTarget, album: bool, path: PathBuf },
    CoversDir(PathBuf),
    Fallback(PathBuf),
}

#[derive(Clone)]
enum PickMode {
    Cover { target: CoverTarget, album: bool },
    Fallback,
}

enum CoverAction {
    Choose { target: CoverTarget, album: bool },
    Remove(CoverTarget),
    SaveBeside,
    Settings,
}

type CoverMsg = (u64, Option<(egui::ColorImage, String, Vec<u8>)>);

struct CoverView {
    tex: Option<egui::TextureHandle>,
    origin: String,
    bytes: Option<Vec<u8>>,
    req: u64,
    tx: Sender<CoverMsg>,
    rx: Receiver<CoverMsg>,
}

pub struct App {
    ctx: egui::Context,
    dir: PathBuf,
    cfg: Arc<Mutex<Config>>,
    engine: Option<Engine>,
    ev_rx: Option<Receiver<AudioEvent>>,
    engine_err: Option<String>,
    plugins: PluginHost,

    lists: Vec<ListData>,
    view: usize,
    now: Option<Now>,
    loaded: bool,
    selected: HashSet<u64>,
    anchor: Option<u64>,
    filter: String,
    shuffle: bool,
    settings: Settings,
    rg_info: String,

    meta_tx: Sender<(u64, Track)>,
    meta_rx: Receiver<(u64, Track)>,
    dlg_tx: Sender<Dlg>,
    dlg_rx: Receiver<Dlg>,
    cover: CoverView,

    show_plugins: bool,
    show_audio: bool,
    show_cover: bool,
    rename: Option<(usize, String, bool)>,
    wipe_armed: bool,
    narrow: bool,
    insets: [f32; 4],
    image_picker: Option<(PickMode, Vec<PathBuf>)>,
    confirm_close: Option<usize>,
    seeking: bool,
    seek_val: f32,
    focus_search: bool,
    plugin_buf: HashMap<String, String>,
    toast: Option<(String, Instant)>,

    save_due: Option<Instant>,
    last_time_sent: Instant,
    error_streak: usize,
    next_at: Option<Instant>,
    title_shown: String,
    rng: u64,
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut ia, mut ib) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                let mut nb = String::new();
                while let Some(c) = ia.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ia.next();
                }
                while let Some(c) = ib.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    ib.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ia.next();
                ib.next();
            }
        }
    }
}

fn fmt_time(s: f64) -> String {
    let s = if s.is_finite() && s > 0.0 { s as u64 } else { 0 };
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Expande arquivos/pastas em uma lista ordenada de arquivos de áudio.
fn expand_paths(paths: Vec<PathBuf>) -> Vec<String> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            for e in walkdir::WalkDir::new(&p).into_iter().filter_map(|e| e.ok()) {
                if e.file_type().is_file() && meta::is_audio(e.path()) {
                    out.push(e.path().to_string_lossy().to_string());
                }
            }
        } else if meta::is_audio(&p) {
            out.push(p.to_string_lossy().to_string());
        }
    }
    out.sort_by(|a, b| natural_cmp(a, b));
    out
}

fn setup_fonts(ctx: &egui::Context) {
    // Fontes do sistema como reserva, para títulos em japonês/chinês/coreano etc.
    let candidates = [
        "C:/Windows/Fonts/segoeui.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/YuGothR.ttc",
        "C:/Windows/Fonts/malgun.ttf",
        "C:/Windows/Fonts/seguisym.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ];
    let mut fonts = egui::FontDefinitions::default();
    for (i, path) in candidates.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(path) {
            let name = format!("sys{i}");
            fonts.font_data.insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.clone());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(0x1b, 0x1b, 0x1d);
    v.window_fill = Color32::from_rgb(0x23, 0x23, 0x26);
    v.extreme_bg_color = Color32::from_rgb(0x15, 0x15, 0x17);
    v.selection.bg_fill = SEL;
    v.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    v.hyperlink_color = ACCENT;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(6.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
    });
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Vec<String>) -> Self {
        let ctx = cc.egui_ctx.clone();
        setup_fonts(&ctx);
        setup_style(&ctx);
        let dir = config::data_dir();
        let cfg = Arc::new(Mutex::new(Config::load(&dir)));
        let mut plugins = PluginHost::new(cfg.clone(), dir.clone());
        plugins.start_enabled();

        platform::setup_audio_session();
        let (engine, ev_rx, engine_err) = match Engine::new() {
            Ok((e, rx)) => (Some(e), Some(rx), None),
            Err(e) => (None, None, Some(e)),
        };

        let (meta_tx, meta_rx) = mpsc::channel();
        let (dlg_tx, dlg_rx) = mpsc::channel();
        let (ctx_tx, ctx_rx) = mpsc::channel();

        let mut app = App {
            ctx,
            dir: dir.clone(),
            cfg,
            engine,
            ev_rx,
            engine_err,
            plugins,
            lists: vec![],
            view: 0,
            now: None,
            loaded: false,
            selected: HashSet::new(),
            anchor: None,
            filter: String::new(),
            shuffle: false,
            settings: Settings::default(),
            rg_info: String::new(),
            meta_tx,
            meta_rx,
            dlg_tx,
            dlg_rx,
            cover: CoverView { tex: None, origin: String::new(), bytes: None, req: 0, tx: ctx_tx, rx: ctx_rx },
            show_plugins: false,
            show_audio: false,
            show_cover: false,
            rename: None,
            wipe_armed: false,
            narrow: false,
            insets: [0.0; 4],
            image_picker: None,
            confirm_close: None,
            seeking: false,
            seek_val: 0.0,
            focus_search: false,
            plugin_buf: HashMap::new(),
            toast: None,
            save_due: None,
            last_time_sent: Instant::now(),
            error_streak: 0,
            next_at: None,
            title_shown: String::new(),
            rng: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1,
        };

        if let Some(s) = load_session(&dir) {
            app.lists = s.lists;
            app.view = s.view.min(app.lists.len().saturating_sub(1));
            app.shuffle = s.shuffle;
            app.settings = s.settings;
            if let Some(n) = s.now {
                if let Some(t) = app.lists.get(n.list).and_then(|l| l.tracks.get(n.track.max(0) as usize)) {
                    app.now = Some(Now { list: n.list, track: t.clone() });
                }
            }
        }
        #[cfg(mobile_ui)]
        app.mobile_init();
        if app.lists.is_empty() {
            app.lists.push(ListData { name: "Playlist 1".into(), ..Default::default() });
        }
        app.apply_audio();
        if app.now.is_some() {
            app.reload_cover();
        }
        if let Some(e) = &app.engine_err {
            app.toast = Some((format!("Sem saída de áudio: {e}"), Instant::now() + Duration::from_secs(10)));
        }
        // arquivos passados na linha de comando ("Abrir com...")
        #[cfg(not(mobile_ui))]
        {
            let paths: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
            if !paths.is_empty() {
                app.spawn_expand(paths);
            }
        }
        #[cfg(mobile_ui)]
        {
            let _ = args;
            if app.lists.iter().all(|l| l.tracks.is_empty()) {
                app.import_documents(false);
            }
        }
        app
    }

    // ------------------------------------------------------------ utilidades

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn toast(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now() + Duration::from_secs(4)));
    }

    fn dirty(&mut self) {
        self.save_due = Some(Instant::now() + Duration::from_millis(800));
    }

    fn session(&self) -> Session {
        let now = self.now.as_ref().and_then(|n| {
            let l = self.lists.get(n.list)?;
            let i = l.tracks.iter().position(|t| t.id == n.track.id)?;
            Some(NowRef { list: n.list, track: i as i64 })
        });
        Session { v: 1, view: self.view, shuffle: self.shuffle, settings: self.settings.clone(), lists: self.lists.clone(), now }
    }

    fn save_now(&mut self) {
        self.save_due = None;
        save_session(&self.dir, &self.session());
    }

    fn snapshot(&self) -> PlayerState {
        let (playing, pos) = self.engine.as_ref().map(|e| (e.is_playing() && self.loaded, e.position())).unwrap_or((false, 0.0));
        let track = self.now.as_ref().map(|n| TrackInfo {
            path: n.track.path.clone(),
            title: n.track.title.clone(),
            artist: n.track.artist.clone(),
            album: n.track.album.clone(),
            duration: n.track.duration,
            year: n.track.year,
            track_no: n.track.track_no,
        });
        PlayerState { duration: self.now.as_ref().map(|n| n.track.duration).unwrap_or(0.0), track, playing, position: pos }
    }

    fn emit(&mut self, ev: Event) {
        let st = self.snapshot();
        self.plugins.emit(ev, st);
    }

    fn eq_params(&self) -> EqParams {
        let e = &self.settings.eq;
        let mut bands = [0.0f32; 10];
        for (i, b) in e.bands.iter().take(10).enumerate() {
            bands[i] = *b;
        }
        EqParams { enabled: e.enabled, preamp_db: e.preamp, bands }
    }

    fn apply_audio(&mut self) {
        let p = self.eq_params();
        let vol = self.settings.volume;
        if let Some(e) = &self.engine {
            e.set_eq(p);
            e.set_volume(vol);
        }
        self.apply_rg();
    }

    fn apply_rg(&mut self) {
        let rg = self.settings.rg.clone();
        let (applied, text) = match (&rg.mode, self.now.as_ref().map(|n| &n.track)) {
            (RgMode::Off, _) => (0.0, "ReplayGain desligado.".to_string()),
            (_, None) => (0.0, String::new()),
            (mode, Some(t)) => {
                let album = *mode == RgMode::Album;
                match dsp::replaygain_db(
                    album,
                    (t.rg_track_gain, t.rg_album_gain),
                    (t.rg_track_peak, t.rg_album_peak),
                    rg.preamp,
                    rg.prevent_clip,
                ) {
                    None => (rg.preamp, format!("Faixa sem tag ReplayGain — só o pré-amp ({} dB) é aplicado.", rg.preamp)),
                    Some(db) => (db, format!("Faixa atual: aplicado {db:.2} dB.")),
                }
            }
        };
        self.rg_info = text;
        if let Some(e) = &self.engine {
            e.set_rg_db(applied);
        }
    }

    // ------------------------------------------------------------ reprodução

    fn start_now(&mut self, n: Now) {
        let path = n.track.path.clone();
        self.now = Some(n);
        self.apply_rg();
        if let Some(e) = &self.engine {
            e.load(&path, true);
            self.loaded = true;
        }
        self.reload_cover();
        self.emit(Event::Track);
        self.emit(Event::Play);
        self.dirty();
    }

    fn play_id(&mut self, list: usize, id: u64) {
        let Some(t) = self.lists.get(list).and_then(|l| l.tracks.iter().find(|t| t.id == id)).cloned() else { return };
        self.start_now(Now { list, track: t });
    }

    fn toggle_play(&mut self) {
        if self.now.is_none() {
            let id = self.lists.get(self.view).and_then(|l| {
                l.tracks.iter().find(|t| self.selected.contains(&t.id)).or(l.tracks.first()).map(|t| t.id)
            });
            if let Some(id) = id {
                self.play_id(self.view, id);
            }
            return;
        }
        if !self.loaded {
            // parado ou sessão restaurada: recarrega a faixa atual
            let n = self.now.take().unwrap();
            self.start_now(n);
            return;
        }
        let Some(e) = &self.engine else { return };
        if e.is_playing() {
            e.pause();
            self.emit(Event::Pause);
        } else {
            e.play();
            self.emit(Event::Play);
        }
    }

    fn stop(&mut self) {
        if let Some(e) = &self.engine {
            e.stop();
        }
        self.loaded = false;
        self.emit(Event::Stop);
    }

    fn next(&mut self, dir: i32) {
        let li = self.now.as_ref().map(|n| n.list).filter(|l| *l < self.lists.len()).unwrap_or(self.view);
        let len = self.lists[li].tracks.len();
        if len == 0 {
            return;
        }
        let cur_id = self.now.as_ref().map(|n| n.track.id);
        if self.shuffle && dir > 0 && len > 1 {
            loop {
                let i = (self.rand() % len as u64) as usize;
                let t = &self.lists[li].tracks[i];
                if Some(t.id) != cur_id {
                    let id = t.id;
                    return self.play_id(li, id);
                }
            }
        }
        let cur = cur_id.and_then(|id| self.lists[li].tracks.iter().position(|t| t.id == id)).map(|i| i as i64).unwrap_or(-1);
        let n = cur + dir as i64;
        if n >= 0 && (n as usize) < len {
            let id = self.lists[li].tracks[n as usize].id;
            self.play_id(li, id);
        } else if dir > 0 {
            self.stop();
        }
    }

    fn seek(&mut self, secs: f64) {
        if let Some(e) = &self.engine {
            e.seek(secs);
        }
        self.emit(Event::Time);
    }

    // ------------------------------------------------------------ playlists

    fn spawn_expand(&self, paths: Vec<PathBuf>) {
        let (tx, ctx) = (self.dlg_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Dlg::Add(expand_paths(paths)));
            ctx.request_repaint();
        });
    }

    #[cfg(not(mobile_ui))]
    fn spawn_dialog(&self, f: impl FnOnce() -> Option<Dlg> + Send + 'static) {
        let (tx, ctx) = (self.dlg_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            if let Some(d) = f() {
                let _ = tx.send(d);
                ctx.request_repaint();
            }
        });
    }

    fn add_tracks(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        let view = self.view;
        let first_new = self.lists[view].tracks.len();
        let new: Vec<Track> = paths.iter().map(|p| Track::from_path(p)).collect();
        let jobs: Vec<(u64, String)> = new.iter().map(|t| (t.id, t.path.clone())).collect();
        let list = &mut self.lists[view];
        list.tracks.extend(new);
        list.sort = None;
        // leitura de tags em 4 threads
        let jobs = Arc::new(jobs);
        let next = Arc::new(AtomicUsize::new(0));
        for _ in 0..4 {
            let (jobs, next, tx, ctx) = (jobs.clone(), next.clone(), self.meta_tx.clone(), self.ctx.clone());
            std::thread::spawn(move || loop {
                let i = next.fetch_add(1, AO::Relaxed);
                let Some((id, path)) = jobs.get(i) else { break };
                let _ = tx.send((*id, meta::read_meta(path)));
                if i % 16 == 0 {
                    ctx.request_repaint();
                }
            });
        }
        if self.now.is_none() {
            let id = self.lists[view].tracks[first_new].id;
            self.play_id(view, id);
        }
        self.dirty();
    }

    fn sort_by(&mut self, key: &str) {
        let Some(l) = self.lists.get_mut(self.view) else { return };
        let dir = match &l.sort {
            Some(s) if s.key == key => -s.dir,
            _ => 1,
        };
        l.sort = Some(SortSpec { key: key.to_string(), dir });
        l.tracks.sort_by(|a, b| {
            let o = match key {
                "duration" => a.duration.partial_cmp(&b.duration).unwrap_or(Ordering::Equal),
                "artist" => natural_cmp(&a.artist, &b.artist),
                "album" => natural_cmp(&a.album, &b.album),
                _ => natural_cmp(&a.title, &b.title),
            };
            if dir < 0 { o.reverse() } else { o }
        });
        self.dirty();
    }

    fn close_list(&mut self, i: usize) {
        if i >= self.lists.len() {
            return;
        }
        if self.lists.len() == 1 {
            self.lists[0].tracks.clear();
            self.lists[0].sort = None;
            if self.now.as_ref().map(|n| n.list == 0).unwrap_or(false) {
                self.now.as_mut().unwrap().list = usize::MAX;
            }
        } else {
            self.lists.remove(i);
            if let Some(n) = self.now.as_mut() {
                if n.list == i {
                    n.list = usize::MAX; // continua tocando a faixa atual
                } else if n.list != usize::MAX && n.list > i {
                    n.list -= 1;
                }
            }
        }
        self.view = self.view.min(self.lists.len() - 1);
        self.selected.clear();
        self.dirty();
    }

    fn clear_list(&mut self) {
        let v = self.view;
        if self.now.as_ref().map(|n| n.list == v).unwrap_or(false) {
            self.stop();
            self.now = None;
            self.reload_cover();
        }
        self.lists[v].tracks.clear();
        self.lists[v].sort = None;
        self.selected.clear();
        self.dirty();
    }

    // ------------------------------------------------------------ capas

    fn reload_cover(&mut self) {
        self.cover.req += 1;
        let req = self.cover.req;
        let Some(n) = &self.now else {
            self.cover.tex = None;
            self.cover.origin.clear();
            self.cover.bytes = None;
            return;
        };
        let q = CoverQuery { path: n.track.path.clone(), artist: n.track.artist.clone(), album: n.track.album.clone() };
        let (settings, custom) = {
            let c = self.cfg.lock().unwrap();
            (c.cover.clone(), c.custom_covers.clone())
        };
        let (tx, ctx) = (self.cover.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = cover::resolve(&q, &settings, &custom).and_then(|r| {
                let img = cover::decode(&r.bytes, settings.max_size)?;
                Some((img, r.origin, r.bytes))
            });
            let _ = tx.send((req, r));
            ctx.request_repaint();
        });
    }

    fn set_cover(&mut self, target: CoverTarget, album: bool, img: PathBuf) {
        let (copy, covers_dir) = {
            let c = self.cfg.lock().unwrap();
            (c.cover.copy_into_library, self.dir.join("covers"))
        };
        let (key, what) = match (album, cover::album_key(&target.artist, &target.album)) {
            (true, Some(k)) => (k, format!("o álbum \"{}\"", target.album)),
            (true, None) => (cover::track_key(&target.path), "esta faixa (sem álbum nas tags)".to_string()),
            (false, _) => (cover::track_key(&target.path), "esta faixa".to_string()),
        };
        match cover::import_image(&img, &covers_dir, &key, copy) {
            Ok(stored) => {
                {
                    let mut c = self.cfg.lock().unwrap();
                    c.custom_covers.insert(key, stored.to_string_lossy().to_string());
                    c.save();
                }
                self.toast(format!("Capa definida para {what}."));
                self.reload_cover();
            }
            Err(e) => self.toast(format!("Não foi possível usar a imagem: {e}")),
        }
    }

    fn remove_cover(&mut self, target: CoverTarget) {
        let keys = [Some(cover::track_key(&target.path)), cover::album_key(&target.artist, &target.album)];
        let lib = self.dir.join("covers");
        let mut removed = false;
        {
            let mut c = self.cfg.lock().unwrap();
            for k in keys.into_iter().flatten() {
                if let Some(f) = c.custom_covers.remove(&k) {
                    removed = true;
                    let p = PathBuf::from(&f);
                    // só apaga arquivos que o próprio player copiou, e só se mais ninguém os usa
                    if p.starts_with(&lib) && !c.custom_covers.values().any(|v| *v == f) {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
            if removed {
                c.save();
            }
        }
        self.toast(if removed { "Capa personalizada removida." } else { "Não há capa personalizada para esta faixa." });
        self.reload_cover();
    }

    fn save_cover_beside(&mut self) {
        let (Some(n), Some(bytes)) = (&self.now, &self.cover.bytes) else {
            return self.toast("Não há capa carregada para salvar.");
        };
        match cover::save_beside(&n.track.path, bytes) {
            Ok(p) => self.toast(format!("Capa salva em {}", p.display())),
            Err(e) => self.toast(format!("Não foi possível salvar: {e}")),
        }
    }

    #[cfg(not(mobile_ui))]
    fn choose_cover(&mut self, target: CoverTarget, album: bool) {
        self.spawn_dialog(move || {
            let p = rfd::FileDialog::new().set_title("Escolher imagem de capa").add_filter("Imagens", cover::IMAGE_EXT).pick_file()?;
            Some(Dlg::CoverFile { target, album, path: p })
        });
    }

    /// No iOS não há diálogo de arquivos do sistema: lista as imagens da pasta do app.
    #[cfg(mobile_ui)]
    fn choose_cover(&mut self, target: CoverTarget, album: bool) {
        self.open_image_picker(PickMode::Cover { target, album });
    }

    #[allow(dead_code)]
    fn open_image_picker(&mut self, mode: PickMode) {
        let mut files: Vec<PathBuf> = walkdir::WalkDir::new(platform::docs_dir())
            .max_depth(4)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && cover::is_image(e.path()))
            .map(|e| e.into_path())
            .collect();
        files.sort();
        self.image_picker = Some((mode, files));
    }

    /// Adiciona as músicas da pasta Documents do app que ainda não estão em nenhuma playlist.
    #[allow(dead_code)]
    fn import_documents(&self, announce_empty: bool) {
        let existing: HashSet<String> = self.lists.iter().flat_map(|l| l.tracks.iter().map(|t| t.path.clone())).collect();
        let (tx, ctx) = (self.dlg_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let new: Vec<String> = expand_paths(vec![platform::docs_dir()]).into_iter().filter(|p| !existing.contains(p)).collect();
            if !new.is_empty() {
                let _ = tx.send(Dlg::Add(new));
            } else if announce_empty {
                let _ = tx.send(Dlg::Note("Nenhuma música nova. Copie arquivos para Arquivos › No meu iPhone › Meu Player.".into()));
            }
            ctx.request_repaint();
        });
    }

    /// Ajustes específicos do celular: caminhos que mudam a cada instalação e pasta de capas padrão.
    #[cfg(mobile_ui)]
    fn mobile_init(&mut self) {
        let docs = platform::docs_dir();
        let data = config::data_dir();
        for l in &mut self.lists {
            for t in &mut l.tracks {
                t.path = platform::rebase(&t.path, "/Documents/", &docs);
            }
        }
        if let Some(n) = self.now.as_mut() {
            n.track.path = platform::rebase(&n.track.path, "/Documents/", &docs);
        }
        let capas = docs.join("Capas");
        let _ = std::fs::create_dir_all(&capas);
        let mut c = self.cfg.lock().unwrap();
        for v in c.custom_covers.values_mut() {
            *v = platform::rebase(v, "/meu-player/", &data);
        }
        if c.cover.covers_dir.is_empty() || !std::path::Path::new(&c.cover.covers_dir).exists() {
            c.cover.covers_dir = capas.to_string_lossy().to_string();
            for e in &mut c.cover.sources {
                if e.source == CoverSource::CoversDir {
                    e.enabled = true;
                }
            }
        }
        c.save();
    }

    fn cover_target_now(&self) -> Option<CoverTarget> {
        self.now.as_ref().map(|n| CoverTarget::of(&n.track))
    }

    fn run_cover_action(&mut self, a: CoverAction) {
        match a {
            CoverAction::Choose { target, album } => self.choose_cover(target, album),
            CoverAction::Remove(t) => self.remove_cover(t),
            CoverAction::SaveBeside => self.save_cover_beside(),
            CoverAction::Settings => self.show_cover = true,
        }
    }

    // ------------------------------------------------------------ eventos

    fn poll(&mut self, ctx: &egui::Context) {
        // eventos do motor de áudio
        let mut events = Vec::new();
        if let Some(rx) = &self.ev_rx {
            while let Ok(e) = rx.try_recv() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                AudioEvent::Ended => {
                    self.loaded = false;
                    self.emit(Event::Ended);
                    self.next(1);
                }
                AudioEvent::Duration(d) => {
                    if let Some(n) = self.now.as_mut() {
                        if n.track.duration <= 0.0 {
                            n.track.duration = d;
                        }
                    }
                }
                AudioEvent::Error(msg) => {
                    self.loaded = false;
                    let name = self.now.as_ref().map(|n| n.track.title.clone()).unwrap_or_default();
                    self.toast(format!("Erro ao tocar \"{name}\": {msg}"));
                    // arquivo ausente/ilegível: pula para a próxima, sem loop infinito
                    self.error_streak += 1;
                    let max = self.now.as_ref().and_then(|n| self.lists.get(n.list)).map(|l| l.tracks.len()).unwrap_or(0).max(3);
                    if self.error_streak < max {
                        self.next_at = Some(Instant::now() + Duration::from_millis(300));
                    }
                }
            }
        }
        if let Some(t) = self.next_at {
            if Instant::now() >= t {
                self.next_at = None;
                self.next(1);
            }
        }

        // tags lidas em segundo plano
        let mut got: HashMap<u64, Track> = HashMap::new();
        while let Ok((id, t)) = self.meta_rx.try_recv() {
            got.insert(id, t);
        }
        if !got.is_empty() {
            for l in &mut self.lists {
                for t in &mut l.tracks {
                    if let Some(m) = got.remove(&t.id) {
                        t.apply_meta(m);
                    }
                }
            }
            if let Some(n) = self.now.as_mut() {
                if let Some(t) = self.lists.get(n.list).and_then(|l| l.tracks.iter().find(|t| t.id == n.track.id)) {
                    n.track = t.clone();
                }
            }
            self.dirty();
        }

        // diálogos / arquivos soltos
        while let Ok(d) = self.dlg_rx.try_recv() {
            match d {
                Dlg::Add(p) => self.add_tracks(p),
                Dlg::Note(m) => self.toast(m),
                Dlg::CoverFile { target, album, path } => self.set_cover(target, album, path),
                Dlg::CoversDir(p) => {
                    self.cfg.lock().unwrap().cover.covers_dir = p.to_string_lossy().to_string();
                    self.cfg.lock().unwrap().save();
                    self.reload_cover();
                }
                Dlg::Fallback(p) => {
                    self.cfg.lock().unwrap().cover.fallback_image = p.to_string_lossy().to_string();
                    self.cfg.lock().unwrap().save();
                    self.reload_cover();
                }
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            let (imgs, rest): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| cover::is_image(p));
            if let Some(img) = imgs.into_iter().next() {
                let target = self.cover_target_now().or_else(|| {
                    self.lists.get(self.view).and_then(|l| l.tracks.iter().find(|t| self.selected.contains(&t.id))).map(CoverTarget::of)
                });
                match target {
                    Some(t) => {
                        let album = self.cfg.lock().unwrap().cover.default_scope_album;
                        self.set_cover(t, album, img);
                    }
                    None => self.toast("Toque ou selecione uma faixa antes de soltar uma imagem de capa."),
                }
            }
            if !rest.is_empty() {
                self.spawn_expand(rest);
            }
        }

        // capa pronta
        while let Ok((req, r)) = self.cover.rx.try_recv() {
            if req != self.cover.req {
                continue;
            }
            match r {
                Some((img, origin, bytes)) => {
                    self.cover.tex = Some(ctx.load_texture("capa", img, egui::TextureOptions::LINEAR));
                    self.cover.origin = origin;
                    self.cover.bytes = Some(bytes);
                }
                None => {
                    self.cover.tex = None;
                    self.cover.origin.clear();
                    self.cover.bytes = None;
                }
            }
        }

        // tempo / estado de reprodução
        let playing = self.loaded && self.engine.as_ref().map(|e| e.is_playing()).unwrap_or(false);
        if playing {
            if self.engine.as_ref().map(|e| e.position()).unwrap_or(0.0) > 0.5 {
                self.error_streak = 0;
            }
            if self.last_time_sent.elapsed() >= Duration::from_secs(1) {
                self.last_time_sent = Instant::now();
                self.emit(Event::Time);
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        // título da janela
        let title = match &self.now {
            Some(n) => format!("{} · Meu Player", [n.track.artist.as_str(), n.track.title.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" - ")),
            None => "Meu Player".to_string(),
        };
        if title != self.title_shown {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title_shown = title;
        }

        if let Some(t) = self.save_due {
            if Instant::now() >= t {
                self.save_now();
            }
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        let (ctrl, f, a) = ctx.input(|i| (i.modifiers.command, i.key_pressed(Key::F), i.key_pressed(Key::A)));
        if ctrl && f {
            self.focus_search = true;
        }
        if ctx.wants_keyboard_input() {
            return;
        }
        let (space, enter, del) = ctx.input(|i| (i.key_pressed(Key::Space), i.key_pressed(Key::Enter), i.key_pressed(Key::Delete)));
        if space {
            self.toggle_play();
        } else if enter {
            if let Some(id) = self.lists[self.view].tracks.iter().find(|t| self.selected.contains(&t.id)).map(|t| t.id) {
                self.play_id(self.view, id);
            }
        } else if del && !self.selected.is_empty() {
            let sel = std::mem::take(&mut self.selected);
            self.lists[self.view].tracks.retain(|t| !sel.contains(&t.id));
            self.dirty();
        } else if ctrl && a {
            let rows = self.visible_rows();
            let l = &self.lists[self.view];
            self.selected = rows.iter().map(|&i| l.tracks[i].id).collect();
        }
    }

    /// Índices das faixas visíveis (após o filtro de busca).
    fn visible_rows(&mut self) -> Vec<usize> {
        let terms: Vec<String> = self.filter.to_lowercase().split_whitespace().map(String::from).collect();
        let l = &mut self.lists[self.view];
        if terms.is_empty() {
            return (0..l.tracks.len()).collect();
        }
        l.tracks
            .iter_mut()
            .enumerate()
            .filter_map(|(i, t)| {
                let h = t.haystack();
                terms.iter().all(|w| h.contains(w.as_str())).then_some(i)
            })
            .collect()
    }
}

// ====================================================================== interface

enum RowAction {
    Play(u64),
    Remove(u64),
    Cover(CoverTarget, bool),
    OpenFolder(String),
}

#[allow(dead_code)]
fn open_folder(path: &str) {
    let p = std::path::Path::new(path);
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(dir) = p.parent() {
            let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        }
    }
}

/// Itens do menu de capa (usado no clique direito e no botão "Capa").
fn cover_menu(ui: &mut egui::Ui, target: Option<&CoverTarget>, has_bytes: bool) -> Option<CoverAction> {
    let mut act = None;
    let has = target.is_some();
    if ui.add_enabled(has, egui::Button::new("Escolher imagem para esta faixa…")).clicked() {
        act = target.map(|t| CoverAction::Choose { target: t.clone(), album: false });
        ui.close_menu();
    }
    let album_ok = target.map(|t| !t.album.trim().is_empty()).unwrap_or(false);
    if ui.add_enabled(album_ok, egui::Button::new("Escolher imagem para o álbum inteiro…")).clicked() {
        act = target.map(|t| CoverAction::Choose { target: t.clone(), album: true });
        ui.close_menu();
    }
    if ui.add_enabled(has, egui::Button::new("Remover capa personalizada")).clicked() {
        act = target.map(|t| CoverAction::Remove(t.clone()));
        ui.close_menu();
    }
    ui.separator();
    if ui.add_enabled(has && has_bytes, egui::Button::new("Salvar capa atual na pasta da música")).clicked() {
        act = Some(CoverAction::SaveBeside);
        ui.close_menu();
    }
    if ui.button("Configurações de capa…").clicked() {
        act = Some(CoverAction::Settings);
        ui.close_menu();
    }
    act
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let narrow = ctx.screen_rect().width() < 720.0;
        if narrow != self.narrow {
            self.narrow = narrow;
            ctx.style_mut(|st| {
                // alvos de toque maiores no celular
                st.spacing.interact_size.y = if narrow { 34.0 } else { 18.0 };
                st.spacing.button_padding = if narrow { Vec2::new(12.0, 8.0) } else { Vec2::new(10.0, 5.0) };
            });
        }
        self.insets = platform::safe_insets();
        self.poll(ctx);
        self.keyboard(ctx);
        let rows = self.visible_rows();

        self.ui_toolbar(ctx, &rows);
        self.ui_tabs(ctx);
        self.ui_controls(ctx);
        self.ui_side(ctx);
        self.ui_list(ctx, &rows);
        self.ui_windows(ctx);
        self.win_image_picker(ctx);
        self.ui_toast(ctx);

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let r = ctx.screen_rect().shrink(6.0);
            ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")))
                .rect_stroke(r, 6.0, egui::Stroke::new(2.0_f32, ACCENT), egui::StrokeKind::Inside);
        }
    }

    #[cfg(not(target_os = "ios"))]
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_now();
        self.plugins.shutdown();
    }

    #[cfg(target_os = "ios")]
    fn on_exit(&mut self) {
        self.save_now();
        self.plugins.shutdown();
    }
}

impl App {
    fn panel_frame(&self, ctx: &egui::Context, top: bool, bottom: bool) -> egui::Frame {
        let [t, b, l, r] = self.insets;
        egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin {
            left: (6.0 + l) as i8,
            right: (6.0 + r) as i8,
            top: (4.0 + if top { t } else { 0.0 }) as i8,
            bottom: (4.0 + if bottom { b } else { 0.0 }) as i8,
        })
    }

    fn ui_toolbar(&mut self, ctx: &egui::Context, rows: &[usize]) {
        let narrow = self.narrow;
        let frame = self.panel_frame(ctx, true, false);
        egui::TopBottomPanel::top("toolbar").frame(frame).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                #[cfg(not(mobile_ui))]
                {
                    if ui.button("+ Arquivos").on_hover_text("Adicionar arquivos").clicked() {
                        self.spawn_dialog(|| {
                            let f = rfd::FileDialog::new().add_filter("Áudio", meta::AUDIO_EXT).pick_files()?;
                            Some(Dlg::Add(expand_paths(f)))
                        });
                    }
                    if ui.button("+ Pasta").on_hover_text("Adicionar pasta").clicked() {
                        self.spawn_dialog(|| {
                            let f = rfd::FileDialog::new().pick_folder()?;
                            Some(Dlg::Add(expand_paths(vec![f])))
                        });
                    }
                }
                #[cfg(mobile_ui)]
                if ui.button("+ Importar").on_hover_text("Adiciona as músicas da pasta do app (Arquivos › Meu Player)").clicked() {
                    self.import_documents(true);
                }
                if ui.button("Limpar").on_hover_text("Limpar a playlist atual").clicked() {
                    self.clear_list();
                }
                let id = egui::Id::new("search");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .id(id)
                        .hint_text(if narrow { "Buscar…" } else { "Buscar na playlist (Ctrl+F)" })
                        .desired_width(if narrow { 120.0 } else { 240.0 }),
                );
                if self.focus_search {
                    r.request_focus();
                    self.focus_search = false;
                }
                if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.filter.clear();
                    r.surrender_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    if let Some(&i) = rows.first() {
                        let id = self.lists[self.view].tracks[i].id;
                        self.play_id(self.view, id);
                    }
                }
                let l = &self.lists[self.view];
                if !l.tracks.is_empty() && !narrow {
                    let total: f64 = rows.iter().map(|&i| l.tracks[i].duration).sum();
                    let pre = if rows.len() != l.tracks.len() { format!("{} de ", rows.len()) } else { String::new() };
                    ui.label(RichText::new(format!("{pre}{} faixas · {}", l.tracks.len(), fmt_time(total))).color(DIM));
                }
                if narrow {
                    ui.menu_button("Menu", |ui| {
                        if ui.button("Capas").clicked() {
                            self.show_cover = true;
                            ui.close_menu();
                        }
                        if ui.button("Áudio (EQ)").clicked() {
                            self.show_audio = true;
                            self.apply_rg();
                            ui.close_menu();
                        }
                        if ui.button("Plugins").clicked() {
                            self.show_plugins = true;
                            ui.close_menu();
                        }
                    });
                } else {
                    ui.add_space((ui.available_width() - 290.0).max(8.0));
                    if ui.button("Capas").clicked() {
                        self.show_cover = true;
                    }
                    if ui.button("Áudio (EQ)").clicked() {
                        self.show_audio = true;
                        self.apply_rg();
                    }
                    if ui.button("Plugins").clicked() {
                        self.show_plugins = true;
                    }
                }
            });
        });
    }

    fn ui_tabs(&mut self, ctx: &egui::Context) {
        let tabs_frame = self.panel_frame(ctx, false, false);
        egui::TopBottomPanel::top("tabs").frame(tabs_frame).show(ctx, |ui| {
            let mut switch = None;
            let mut close = None;
            let mut commit: Option<(usize, String)> = None;
            let mut cancel = false;
            let mut add = false;
            let mut start_rename = None;
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.horizontal(|ui| {
                    for i in 0..self.lists.len() {
                        if let Some((ri, buf, focus)) = self.rename.as_mut() {
                            if *ri == i {
                                let r = ui.add(egui::TextEdit::singleline(buf).desired_width(120.0));
                                if *focus {
                                    r.request_focus();
                                    *focus = false;
                                }
                                if r.lost_focus() {
                                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                                        cancel = true;
                                    } else {
                                        commit = Some((i, buf.clone()));
                                    }
                                }
                                continue;
                            }
                        }
                        let playing = self.now.as_ref().map(|n| n.list == i).unwrap_or(false);
                        let name = &self.lists[i].name;
                        let text = if playing { format!("▶ {name}") } else { name.clone() };
                        let r = ui.selectable_label(i == self.view, text);
                        if r.double_clicked() {
                            start_rename = Some((i, name.clone(), true));
                        } else if r.clicked() {
                            switch = Some(i);
                        }
                        if ui.add(egui::Button::new(RichText::new("×").color(DIM)).frame(false)).on_hover_text("Fechar playlist").clicked() {
                            close = Some(i);
                        }
                    }
                    if ui.add(egui::Button::new("+").frame(false)).on_hover_text("Nova playlist").clicked() {
                        add = true;
                    }
                });
            });
            if let Some(r) = start_rename {
                self.rename = Some(r);
            }
            if cancel {
                self.rename = None;
            }
            if let Some((i, name)) = commit {
                if !name.trim().is_empty() {
                    self.lists[i].name = name.trim().to_string();
                }
                self.rename = None;
                self.dirty();
            }
            if let Some(i) = switch {
                if i != self.view {
                    self.view = i;
                    self.selected.clear();
                    self.anchor = None;
                    self.dirty();
                }
            }
            if let Some(i) = close {
                if self.lists[i].tracks.len() > 20 {
                    self.confirm_close = Some(i);
                } else {
                    self.close_list(i);
                }
            }
            if add {
                let n = self.lists.len() + 1;
                self.lists.push(ListData { name: format!("Playlist {n}"), ..Default::default() });
                self.view = self.lists.len() - 1;
                self.selected.clear();
                self.dirty();
            }
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let big = if self.narrow { Vec2::new(44.0, 36.0) } else { Vec2::new(34.0, 26.0) };
        if ui.add_sized(big, egui::Button::new("⏮")).on_hover_text("Anterior").clicked() {
            let pos = self.engine.as_ref().map(|e| e.position()).unwrap_or(0.0);
            if pos > 3.0 {
                self.seek(0.0);
            } else {
                self.next(-1);
            }
        }
        let playing = self.loaded && self.engine.as_ref().map(|e| e.is_playing()).unwrap_or(false);
        if ui.add_sized(big, egui::Button::new(if playing { "⏸" } else { "▶" })).on_hover_text("Tocar/Pausar").clicked() {
            self.toggle_play();
        }
        if ui.add_sized(big, egui::Button::new("⏹")).on_hover_text("Parar").clicked() {
            self.stop();
        }
        if ui.add_sized(big, egui::Button::new("⏭")).on_hover_text("Próxima").clicked() {
            self.next(1);
        }
    }

    fn seek_row(&mut self, ui: &mut egui::Ui, reserve: f32) {
        let dur = self.now.as_ref().map(|n| n.track.duration).unwrap_or(0.0).max(0.0);
        let pos = if self.loaded { self.engine.as_ref().map(|e| e.position()).unwrap_or(0.0) } else { 0.0 };
        ui.label(RichText::new(fmt_time(if self.seeking { self.seek_val as f64 } else { pos })).color(DIM).monospace());
        ui.spacing_mut().slider_width = (ui.available_width() - reserve).max(60.0);
        let mut v = if self.seeking { self.seek_val } else { pos.min(dur) as f32 };
        let r = ui.add_enabled(dur > 0.0, egui::Slider::new(&mut v, 0.0..=dur.max(0.1) as f32).show_value(false).trailing_fill(true));
        if r.drag_started() {
            self.seeking = true;
        }
        if r.changed() {
            self.seek_val = v;
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            self.seek(v as f64);
            self.seeking = false;
        }
        ui.label(RichText::new(fmt_time(dur)).color(DIM).monospace());
    }

    fn extras(&mut self, ui: &mut egui::Ui, vol_w: f32) {
        if ui.selectable_label(self.shuffle, "🔀").on_hover_text("Aleatório").clicked() {
            self.shuffle = !self.shuffle;
            self.dirty();
        }
        ui.spacing_mut().slider_width = vol_w;
        let r = ui.add(egui::Slider::new(&mut self.settings.volume, 0.0..=100.0).show_value(false)).on_hover_text("Volume");
        if r.changed() {
            let v = self.settings.volume;
            if let Some(e) = &self.engine {
                e.set_volume(v);
            }
            self.dirty();
        }
    }

    /// Desenha a capa atual dentro de `rect` (com fundo). Retorna false se não há capa.
    fn paint_cover(&self, ui: &egui::Ui, rect: egui::Rect) -> bool {
        ui.painter().rect_filled(rect, 6.0, Color32::from_rgb(0x15, 0x15, 0x17));
        ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(0x33, 0x33, 0x38)), egui::StrokeKind::Inside);
        let Some(tex) = &self.cover.tex else { return false };
        let sz = tex.size_vec2();
        let scale = (rect.width() / sz.x).min(rect.height() / sz.y);
        let r = egui::Rect::from_center_size(rect.center(), sz * scale);
        ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        true
    }

    /// Mini "tocando agora" (layout estreito): capa pequena + título/artista. Pressão longa na capa abre o menu.
    fn mini_player(&mut self, ui: &mut egui::Ui) -> Option<CoverAction> {
        let mut action = None;
        let target = self.cover_target_now();
        let has_bytes = self.cover.bytes.is_some();
        let (title, sub) = match &self.now {
            Some(n) => (
                n.track.title.clone(),
                [n.track.artist.as_str(), n.track.album.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" — "),
            ),
            None => ("—".to_string(), String::new()),
        };
        ui.horizontal(|ui| {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::click());
            self.paint_cover(ui, rect);
            resp.context_menu(|ui| {
                if let Some(a) = cover_menu(ui, target.as_ref(), has_bytes) {
                    action = Some(a);
                }
            });
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
                ui.add(egui::Label::new(RichText::new(sub).color(DIM)).truncate());
                if !self.cover.origin.is_empty() {
                    ui.add(egui::Label::new(RichText::new(format!("Capa: {} · segure a capa para trocar", self.cover.origin)).small().color(DIM)).truncate());
                }
            });
        });
        action
    }

    fn ui_controls(&mut self, ctx: &egui::Context) {
        let narrow = self.narrow;
        let frame = self.panel_frame(ctx, false, true);
        let mut action = None;
        egui::TopBottomPanel::bottom("controls").frame(frame).show(ctx, |ui| {
            if narrow {
                action = self.mini_player(ui);
                ui.horizontal(|ui| self.seek_row(ui, 100.0));
                ui.horizontal(|ui| {
                    self.transport(ui);
                    self.extras(ui, 60.0);
                });
            } else {
                ui.horizontal(|ui| {
                    self.transport(ui);
                    self.seek_row(ui, 260.0);
                    self.extras(ui, 90.0);
                });
            }
        });
        if let Some(a) = action {
            self.run_cover_action(a);
        }
    }

    fn ui_side(&mut self, ctx: &egui::Context) {
        if self.narrow {
            return;
        }
        let mut action = None;
        egui::SidePanel::right("side").exact_width(260.0).resizable(false).show(ctx, |ui| {
            let w = ui.available_width();
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(w), Sense::click());
            if !self.paint_cover(ui, rect) {
                let msg = if self.now.is_some() { "sem capa\n\nclique com o botão direito\nou solte uma imagem aqui" } else { "sem capa" };
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, msg, egui::FontId::proportional(13.0), DIM);
            }
            let target = self.cover_target_now();
            let has_bytes = self.cover.bytes.is_some();
            resp.context_menu(|ui| {
                if let Some(a) = cover_menu(ui, target.as_ref(), has_bytes) {
                    action = Some(a);
                }
            });
            ui.add_space(4.0);
            let (title, sub) = match &self.now {
                Some(n) => (
                    n.track.title.clone(),
                    [n.track.artist.as_str(), n.track.album.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" — "),
                ),
                None => ("—".to_string(), String::new()),
            };
            ui.label(RichText::new(title).size(15.0).strong());
            ui.label(RichText::new(sub).color(DIM));
            if !self.cover.origin.is_empty() {
                ui.label(RichText::new(format!("Capa: {}", self.cover.origin)).small().color(DIM));
            }
            ui.add_space(4.0);
            ui.menu_button("Capa…", |ui| {
                if let Some(a) = cover_menu(ui, target.as_ref(), has_bytes) {
                    action = Some(a);
                }
            });
        });
        if let Some(a) = action {
            self.run_cover_action(a);
        }
    }

    fn ui_list(&mut self, ctx: &egui::Context, rows: &[usize]) {
        let mut click: Option<(usize, bool, bool)> = None;
        let mut dbl: Option<u64> = None;
        let mut sort: Option<&'static str> = None;
        let mut act: Option<RowAction> = None;

        let narrow = self.narrow;
        let row_h = if narrow { 38.0 } else { ROW_H };
        let [_, _, il, ir] = self.insets;
        let frame = egui::Frame::new().inner_margin(egui::Margin { left: (6.0 + il) as i8, right: (6.0 + ir) as i8, top: 0, bottom: 0 });
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let list = &self.lists[self.view];
            if list.tracks.is_empty() {
                ui.centered_and_justified(|ui| {
                    let msg = if cfg!(mobile_ui) {
                        "Copie suas músicas para Arquivos › No meu iPhone › Meu Player\ne toque em \"+ Importar\"."
                    } else {
                        "Arraste arquivos ou pastas aqui, ou use os botões acima."
                    };
                    ui.label(RichText::new(msg).color(DIM));
                });
                return;
            }
            let playing_id = self.now.as_ref().map(|n| n.track.id);
            let (mods_ctrl, mods_shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
            let sort_spec = list.sort.clone();
            let heads: Vec<(&str, Option<&'static str>)> = if narrow {
                vec![("Título", Some("title")), ("Tempo", Some("duration"))]
            } else {
                vec![("#", None), ("Artista", Some("artist")), ("Título", Some("title")), ("Álbum", Some("album")), ("Tempo", Some("duration"))]
            };
            let mut tb = TableBuilder::new(ui).sense(Sense::click()).resizable(!narrow).cell_layout(Layout::left_to_right(Align::Center));
            tb = if narrow {
                tb.column(Column::remainder().clip(true)).column(Column::exact(56.0))
            } else {
                tb.column(Column::exact(48.0))
                    .column(Column::initial(200.0).at_least(60.0).clip(true))
                    .column(Column::initial(320.0).at_least(80.0).clip(true))
                    .column(Column::remainder().at_least(80.0).clip(true))
                    .column(Column::exact(70.0))
            };
            tb
                .header(if narrow { 28.0 } else { 22.0 }, |mut h| {
                    for (label, key) in heads {
                        h.col(|ui| {
                            let arrow = match (&sort_spec, key) {
                                (Some(s), Some(k)) if s.key == k => if s.dir > 0 { " ▲" } else { " ▼" },
                                _ => "",
                            };
                            let r = ui.add(egui::Label::new(RichText::new(format!("{label}{arrow}")).color(DIM)).sense(Sense::click()));
                            if let (true, Some(k)) = (r.clicked(), key) {
                                sort = Some(k);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, rows.len(), |mut row| {
                        let ri = row.index();
                        let ti = rows[ri];
                        let t = &list.tracks[ti];
                        let is_playing = playing_id == Some(t.id);
                        row.set_selected(self.selected.contains(&t.id));
                        let cell = |ui: &mut egui::Ui, s: String| {
                            let mut rt = RichText::new(s);
                            if is_playing {
                                rt = rt.color(ACCENT).strong();
                            }
                            ui.add(egui::Label::new(rt).truncate().selectable(false));
                        };
                        let dur = if t.duration > 0.0 { fmt_time(t.duration) } else { String::new() };
                        if narrow {
                            let name = if t.artist.is_empty() { t.title.clone() } else { format!("{} — {}", t.title, t.artist) };
                            row.col(|ui| cell(ui, if is_playing { format!("▶ {name}") } else { name }));
                            row.col(|ui| cell(ui, dur));
                        } else {
                            row.col(|ui| cell(ui, if is_playing { "▶".into() } else { (ti + 1).to_string() }));
                            row.col(|ui| cell(ui, t.artist.clone()));
                            row.col(|ui| cell(ui, t.title.clone()));
                            row.col(|ui| cell(ui, t.album.clone()));
                            row.col(|ui| cell(ui, dur));
                        }
                        let resp = row.response();
                        // no celular um toque já toca a faixa; no desktop é duplo clique
                        if resp.double_clicked() || (narrow && resp.clicked()) {
                            dbl = Some(t.id);
                        } else if resp.clicked() {
                            click = Some((ri, mods_ctrl, mods_shift));
                        }
                        if resp.secondary_clicked() && !self.selected.contains(&t.id) {
                            click = Some((ri, false, false));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Tocar").clicked() {
                                act = Some(RowAction::Play(t.id));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Escolher capa para esta faixa…").clicked() {
                                act = Some(RowAction::Cover(CoverTarget::of(t), false));
                                ui.close_menu();
                            }
                            if ui.add_enabled(!t.album.trim().is_empty(), egui::Button::new("Escolher capa para o álbum…")).clicked() {
                                act = Some(RowAction::Cover(CoverTarget::of(t), true));
                                ui.close_menu();
                            }
                            ui.separator();
                            #[cfg(not(mobile_ui))]
                            if ui.button("Mostrar na pasta").clicked() {
                                act = Some(RowAction::OpenFolder(t.path.clone()));
                                ui.close_menu();
                            }
                            if ui.button("Remover da playlist").clicked() {
                                act = Some(RowAction::Remove(t.id));
                                ui.close_menu();
                            }
                        });
                    });
                });
        });

        if let Some(k) = sort {
            self.sort_by(k);
        }
        if let Some((ri, ctrl, shift)) = click {
            let l = &self.lists[self.view];
            let id = l.tracks[rows[ri]].id;
            if shift && self.anchor.is_some() {
                let a = rows.iter().position(|&i| Some(l.tracks[i].id) == self.anchor).unwrap_or(ri);
                let (lo, hi) = (a.min(ri), a.max(ri));
                self.selected = rows[lo..=hi].iter().map(|&i| l.tracks[i].id).collect();
            } else if ctrl {
                if !self.selected.remove(&id) {
                    self.selected.insert(id);
                }
                self.anchor = Some(id);
            } else {
                self.selected = HashSet::from([id]);
                self.anchor = Some(id);
            }
        }
        if let Some(id) = dbl {
            self.play_id(self.view, id);
        }
        match act {
            Some(RowAction::Play(id)) => self.play_id(self.view, id),
            Some(RowAction::Remove(id)) => {
                // remove a seleção inteira se a faixa clicada faz parte dela
                let mut sel = if self.selected.contains(&id) { std::mem::take(&mut self.selected) } else { HashSet::new() };
                sel.insert(id);
                self.lists[self.view].tracks.retain(|t| !sel.contains(&t.id));
                self.selected.clear();
                self.dirty();
            }
            Some(RowAction::Cover(t, album)) => self.choose_cover(t, album),
            Some(RowAction::OpenFolder(p)) => open_folder(&p),
            None => {}
        }
    }

    fn ui_toast(&mut self, ctx: &egui::Context) {
        if let Some((msg, until)) = &self.toast {
            if Instant::now() >= *until {
                self.toast = None;
                return;
            }
            egui::Area::new(egui::Id::new("toast")).anchor(Align2::RIGHT_BOTTOM, Vec2::new(-12.0, if self.narrow { -170.0 } else { -56.0 })).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(380.0);
                    ui.label(msg.as_str());
                });
            });
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

// ====================================================================== janelas

impl App {
    /// Janela com tamanho limitado à tela (e centralizada no celular).
    fn win<'a>(&self, ctx: &egui::Context, title: &str, w: f32) -> egui::Window<'a> {
        let sr = ctx.screen_rect();
        let mut win = egui::Window::new(title.to_string())
            .default_width(w.min(sr.width() - 16.0))
            .max_width(sr.width() - 16.0)
            .max_height((sr.height() - 90.0).max(200.0));
        if self.narrow {
            win = win.anchor(Align2::CENTER_CENTER, Vec2::ZERO);
        }
        win
    }

    fn win_image_picker(&mut self, ctx: &egui::Context) {
        let Some((mode, files)) = self.image_picker.clone() else { return };
        let docs = platform::docs_dir();
        let mut chosen: Option<PathBuf> = None;
        let mut close = false;
        let mut open = true;
        self.win(ctx, "Escolher imagem", 420.0).open(&mut open).show(ctx, |ui| {
            if files.is_empty() {
                ui.label("Nenhuma imagem encontrada. Copie imagens (jpg/png) para a pasta do app, em Arquivos › No meu iPhone › Meu Player.");
            }
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for f in &files {
                    let name = f.strip_prefix(&docs).unwrap_or(f).to_string_lossy().to_string();
                    if ui.add_sized([ui.available_width(), 34.0], egui::Button::new(name).truncate()).clicked() {
                        chosen = Some(f.clone());
                    }
                }
            });
            if ui.button("Cancelar").clicked() {
                close = true;
            }
        });
        if let Some(f) = chosen {
            match mode {
                PickMode::Cover { target, album } => self.set_cover(target, album, f),
                PickMode::Fallback => {
                    {
                        let mut c = self.cfg.lock().unwrap();
                        c.cover.fallback_image = f.to_string_lossy().to_string();
                        c.save();
                    }
                    self.reload_cover();
                }
            }
            self.image_picker = None;
        } else if close || !open {
            self.image_picker = None;
        }
    }

    fn ui_windows(&mut self, ctx: &egui::Context) {
        self.win_plugins(ctx);
        self.win_audio(ctx);
        self.win_cover(ctx);
        self.win_confirm(ctx);
    }

    fn win_confirm(&mut self, ctx: &egui::Context) {
        let Some(i) = self.confirm_close else { return };
        let Some(l) = self.lists.get(i) else {
            self.confirm_close = None;
            return;
        };
        let msg = format!("Fechar \"{}\" com {} faixas?", l.name, l.tracks.len());
        let mut done = None;
        egui::Window::new("Fechar playlist").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(msg);
            ui.horizontal(|ui| {
                if ui.button("Fechar").clicked() {
                    done = Some(true);
                }
                if ui.button("Cancelar").clicked() {
                    done = Some(false);
                }
            });
        });
        if let Some(yes) = done {
            self.confirm_close = None;
            if yes {
                self.close_list(i);
            }
        }
    }

    fn win_plugins(&mut self, ctx: &egui::Context) {
        if !self.show_plugins {
            return;
        }
        let mut open = true;
        let tw = if self.narrow { 170.0 } else { 280.0 };
        self.win(ctx, "Plugins", 540.0).open(&mut open).default_height(480.0).show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                let infos = self.plugins.infos();
                for (i, info) in infos.iter().enumerate() {
                    ui.group(|ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(info.name).strong().size(15.0));
                            ui.label(RichText::new(format!("v{}", info.version)).color(DIM));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let mut on = info.enabled;
                                if ui.checkbox(&mut on, "ativo").changed() {
                                    self.plugins.set_enabled(i, on);
                                }
                            });
                        });
                        ui.label(RichText::new(info.description).color(DIM));
                        let defs: Vec<_> = self.plugins.settings_of(i).iter().map(|s| (s.key, s.label, s.kind)).collect();
                        egui::Grid::new(("plugin-grid", i)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                            for (key, label, kind) in defs {
                                ui.label(label);
                                let cur = self.plugins.setting_value(i, key);
                                match kind {
                                    crate::plugins::Kind::Bool => {
                                        let mut b = cur.as_bool().unwrap_or(false);
                                        if ui.checkbox(&mut b, "").changed() {
                                            self.plugins.set_setting(i, key, serde_json::json!(b));
                                        }
                                    }
                                    _ => {
                                        let cur_s = cur.as_str().map(String::from).unwrap_or_default();
                                        let bk = format!("{}.{}", info.id, key);
                                        let buf = self.plugin_buf.entry(bk).or_insert_with(|| cur_s.clone());
                                        let r = ui.add(egui::TextEdit::singleline(buf).password(kind == crate::plugins::Kind::Password).desired_width(tw));
                                        if r.lost_focus() && *buf != cur_s {
                                            let v = buf.clone();
                                            self.plugins.set_setting(i, key, serde_json::json!(v));
                                        } else if !r.has_focus() {
                                            *buf = cur_s; // reflete mudanças feitas pelo plugin (ex.: senha apagada após o login)
                                        }
                                    }
                                }
                                ui.end_row();
                            }
                        });
                        if !info.status.is_empty() {
                            ui.label(RichText::new(&info.status).color(ACCENT));
                        }
                    });
                }
            });
        });
        self.show_plugins = open;
    }

    fn win_audio(&mut self, ctx: &egui::Context) {
        if !self.show_audio {
            return;
        }
        let mut open = true;
        let mut eq_changed = false;
        let mut rg_changed = false;
        let mut custom = false;
        self.win(ctx, "Áudio", 680.0).open(&mut open).show(ctx, |ui| {
            ui.label(RichText::new("EQUALIZADOR").small().color(DIM));
            ui.horizontal(|ui| {
                if ui.checkbox(&mut self.settings.eq.enabled, "Ativo").changed() {
                    eq_changed = true;
                }
                let mut preset = self.settings.eq.preset.clone();
                egui::ComboBox::from_id_salt("eq-preset").selected_text(&preset).show_ui(ui, |ui| {
                    for (name, _) in PRESETS {
                        ui.selectable_value(&mut preset, name.to_string(), *name);
                    }
                    ui.selectable_value(&mut preset, "Personalizado".to_string(), "Personalizado");
                });
                if preset != self.settings.eq.preset {
                    if let Some((_, b)) = PRESETS.iter().find(|(n, _)| *n == preset) {
                        self.settings.eq.bands = b.to_vec();
                    }
                    self.settings.eq.preset = preset;
                    eq_changed = true;
                }
                if ui.button("Zerar").clicked() {
                    self.settings.eq.bands = vec![0.0; 10];
                    self.settings.eq.preamp = 0.0;
                    self.settings.eq.preset = "Flat".into();
                    eq_changed = true;
                }
            });
            egui::ScrollArea::horizontal().show(ui, |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 130.0;
                    let mut band = |ui: &mut egui::Ui, v: &mut f32, name: String| {
                        ui.vertical(|ui| {
                            ui.set_width(48.0);
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new(format!("{v}")).small().color(DIM));
                                if ui.add(egui::Slider::new(v, -12.0..=12.0).vertical().step_by(0.5).show_value(false)).changed() {
                                    custom = true;
                                }
                                ui.label(RichText::new(name).small());
                            });
                        });
                    };
                    band(ui, &mut self.settings.eq.preamp, "Pré".into());
                    for (i, f) in dsp::FREQS.iter().enumerate() {
                        let name = if *f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
                        band(ui, &mut self.settings.eq.bands[i], name);
                    }
                });
            });
            });
            if custom {
                self.settings.eq.preset = "Personalizado".into();
                eq_changed = true;
            }

            ui.add_space(8.0);
            ui.label(RichText::new("REPLAYGAIN").small().color(DIM));
            egui::Grid::new("rg-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Modo");
                let label = |m: RgMode| match m {
                    RgMode::Off => "Desligado",
                    RgMode::Track => "Por faixa",
                    RgMode::Album => "Por álbum",
                };
                egui::ComboBox::from_id_salt("rg-mode").selected_text(label(self.settings.rg.mode)).show_ui(ui, |ui| {
                    for m in [RgMode::Off, RgMode::Track, RgMode::Album] {
                        if ui.selectable_value(&mut self.settings.rg.mode, m, label(m)).changed() {
                            rg_changed = true;
                        }
                    }
                });
                ui.end_row();
                ui.label(format!("Pré-amp: {} dB", self.settings.rg.preamp));
                if ui.add(egui::Slider::new(&mut self.settings.rg.preamp, -12.0..=12.0).step_by(0.5).show_value(false)).changed() {
                    rg_changed = true;
                }
                ui.end_row();
                ui.label("Evitar clipping");
                if ui.checkbox(&mut self.settings.rg.prevent_clip, "").changed() {
                    rg_changed = true;
                }
                ui.end_row();
            });
            ui.label(RichText::new(&self.rg_info).small().color(DIM));
        });
        if eq_changed {
            let p = self.eq_params();
            if let Some(e) = &self.engine {
                e.set_eq(p);
            }
            self.dirty();
        }
        if rg_changed {
            self.apply_rg();
            self.dirty();
        }
        self.show_audio = open;
    }

    fn win_cover(&mut self, ctx: &egui::Context) {
        if !self.show_cover {
            return;
        }
        let mut open = true;
        let orig = self.cfg.lock().unwrap().cover.clone();
        let mut cs = orig.clone();
        let n_custom = self.cfg.lock().unwrap().custom_covers.len();
        let mut pick_dir = false;
        let mut pick_fb = false;
        let mut wipe = false;
        let mut reload = false;
        let tw = if self.narrow { 150.0 } else { 280.0 };
        self.win(ctx, "Capas / arte do álbum", 560.0).open(&mut open).show(ctx, |ui| {
          egui::ScrollArea::both().auto_shrink([true, true]).show(ui, |ui| {
            ui.label(RichText::new("De onde buscar a capa (de cima para baixo; a primeira que existir vence)").color(DIM));
            let n = cs.sources.len();
            let mut swap: Option<(usize, usize)> = None;
            for i in 0..n {
                ui.horizontal(|ui| {
                    let lbl = cs.sources[i].source.label();
                    ui.checkbox(&mut cs.sources[i].enabled, lbl);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add_enabled(i + 1 < n, egui::Button::new("↓").small()).on_hover_text("Descer prioridade").clicked() {
                            swap = Some((i, i + 1));
                        }
                        if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Subir prioridade").clicked() {
                            swap = Some((i, i - 1));
                        }
                    });
                });
            }
            if let Some((a, b)) = swap {
                cs.sources.swap(a, b);
            }
            ui.separator();

            egui::Grid::new("cover-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label("Nomes na pasta da música");
                ui.add(egui::TextEdit::singleline(&mut cs.folder_names).desired_width(tw * 1.2).hint_text("cover, folder, front"));
                ui.end_row();

                ui.label("Biblioteca de capas");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut cs.covers_dir).desired_width(tw * 0.9).hint_text("pasta com \"Artista - Álbum.jpg\""));
                    #[cfg(not(mobile_ui))]
                    if ui.button("Escolher…").clicked() {
                        pick_dir = true;
                    }
                });
                ui.end_row();

                ui.label("Imagem padrão");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut cs.fallback_image).desired_width(tw * 0.9).hint_text("usada quando não há capa"));
                    if ui.button("Escolher…").clicked() {
                        pick_fb = true;
                    }
                    if ui.button("Limpar").clicked() {
                        cs.fallback_image.clear();
                    }
                });
                ui.end_row();

                ui.label("Ao escolher uma imagem");
                ui.vertical(|ui| {
                    ui.checkbox(&mut cs.copy_into_library, "copiar para a pasta do player (continua valendo se o original for movido)");
                    ui.radio_value(&mut cs.default_scope_album, true, "aplicar ao álbum inteiro (ao soltar uma imagem na janela)");
                    ui.radio_value(&mut cs.default_scope_album, false, "aplicar só à faixa (ao soltar uma imagem na janela)");
                });
                ui.end_row();

                ui.label("Tamanho máximo (px)");
                ui.add(egui::DragValue::new(&mut cs.max_size).range(128..=4096).speed(16));
                ui.end_row();
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Capas escolhidas por você: {n_custom}")).color(DIM));
                if n_custom > 0 {
                    if self.wipe_armed {
                        if ui.button("Confirmar: esquecer todas").clicked() {
                            wipe = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            self.wipe_armed = false;
                        }
                    } else if ui.button("Esquecer todas…").clicked() {
                        self.wipe_armed = true;
                    }
                }
                if ui.button("Recarregar capa").clicked() {
                    reload = true;
                }
            });
            ui.label(RichText::new("Dica: clique com o botão direito na capa (ou numa faixa) para escolher uma imagem, ou solte um arquivo de imagem na janela.").small().color(DIM));
          });
        });

        cs.normalize();
        if cs != orig {
            let mut c = self.cfg.lock().unwrap();
            c.cover = cs;
            c.save();
            drop(c);
            reload = true;
        }
        if pick_dir {
            #[cfg(not(mobile_ui))]
            self.spawn_dialog(|| rfd::FileDialog::new().set_title("Pasta com as capas").pick_folder().map(Dlg::CoversDir));
        }
        if pick_fb {
            #[cfg(not(mobile_ui))]
            self.spawn_dialog(|| rfd::FileDialog::new().add_filter("Imagens", cover::IMAGE_EXT).pick_file().map(Dlg::Fallback));
            #[cfg(mobile_ui)]
            self.open_image_picker(PickMode::Fallback);
        }
        if wipe {
            let lib = self.dir.join("covers");
            let mut c = self.cfg.lock().unwrap();
            for (_, f) in c.custom_covers.drain() {
                let p = PathBuf::from(f);
                if p.starts_with(&lib) {
                    let _ = std::fs::remove_file(p);
                }
            }
            c.save();
            drop(c);
            self.wipe_armed = false;
            reload = true;
        }
        if reload {
            self.reload_cover();
        }
        self.show_cover = open;
        if !open {
            self.wipe_armed = false;
        }
    }
}
