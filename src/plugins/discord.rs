//! Discord Rich Presence — port do foo_discord_rich (TheQwertiest, MIT).
//! Mostra "Listening to" no Discord, com capa buscada no MusicBrainz / Cover Art Archive.

use super::*;
use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use serde_json::json;
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const UA: &str = "MeuPlayer/0.2 ( https://github.com/ )"; // MusicBrainz exige User-Agent
const MIN_INTERVAL: Duration = Duration::from_secs(5); // o Discord limita atualizações
const RETRY: Duration = Duration::from_secs(15);

pub fn meta() -> Meta {
    Meta {
        id: "discord-rich",
        name: "Discord Rich Presence",
        version: "0.2.0",
        description: "Mostra no Discord o que você está ouvindo (\"Listening to\"). Portado do foo_discord_rich.",
        default_enabled: false,
        settings: vec![
            SettingDef { key: "clientId", label: "Application ID do Discord", kind: Kind::Text, default: json!("") },
            SettingDef { key: "details", label: "Linha 1 (details)", kind: Kind::Text, default: json!("%title%") },
            SettingDef { key: "state", label: "Linha 2 (state)", kind: Kind::Text, default: json!("%artist%") },
            SettingDef { key: "largeText", label: "Texto da capa", kind: Kind::Text, default: json!("%album%") },
            SettingDef { key: "fetchArt", label: "Buscar capa (MusicBrainz)", kind: Kind::Bool, default: json!(true) },
            SettingDef { key: "clearOnPause", label: "Limpar ao pausar", kind: Kind::Bool, default: json!(false) },
        ],
    }
}

#[derive(Default)]
pub struct Discord {
    client: Option<DiscordIpcClient>,
    ready: bool,
    update_at: Option<Instant>,
    retry_at: Option<Instant>,
    last_update: Option<Instant>,
    art_cache: HashMap<String, Option<String>>,
}

fn template(tpl: &str, t: &TrackInfo) -> String {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        let after = &rest[a + 1..];
        match after.find('%') {
            Some(b) if after[..b].chars().all(|c| c.is_alphanumeric() || c == '_') && b > 0 => {
                let val = match &after[..b] {
                    "title" => t.title.clone(),
                    "artist" => t.artist.clone(),
                    "album" => t.album.clone(),
                    "year" => t.year.map(|y| y.to_string()).unwrap_or_default(),
                    "trackNo" => t.track_no.map(|y| y.to_string()).unwrap_or_default(),
                    "duration" => (t.duration as u64).to_string(),
                    _ => String::new(),
                };
                out.push_str(&val);
                rest = &after[b + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// O Discord exige campos com 2–128 caracteres.
fn clamp(s: String) -> Option<String> {
    let mut s = s;
    if s.is_empty() {
        return None;
    }
    if s.chars().count() == 1 {
        s.push(' ');
    }
    if s.chars().count() > 128 {
        s = s.chars().take(125).collect::<String>() + "…";
    }
    Some(s)
}

fn fetch_json(url: &str) -> Result<serde_json::Value, String> {
    ureq::get(url)
        .set("User-Agent", UA)
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl Discord {
    fn find_art(&mut self, api: &Api, artist: &str, album: &str) -> Option<String> {
        if album.is_empty() {
            return None;
        }
        let key = format!("{artist}|{album}").to_lowercase();
        if let Some(v) = self.art_cache.get(&key) {
            return v.clone();
        }
        let mut url = None;
        let mut q = format!("release:\"{}\"", album.replace('"', ""));
        if !artist.is_empty() {
            q.push_str(&format!(" AND artist:\"{}\"", artist.replace('"', "")));
        }
        match fetch_json(&format!("https://musicbrainz.org/ws/2/release/?query={}&fmt=json&limit=3", enc(&q))) {
            Ok(mb) => {
                for rel in mb["releases"].as_array().cloned().unwrap_or_default() {
                    let Some(id) = rel["id"].as_str() else { continue };
                    if let Ok(caa) = fetch_json(&format!("https://coverartarchive.org/release/{id}")) {
                        let images = caa["images"].as_array().cloned().unwrap_or_default();
                        let img = images.iter().find(|i| i["front"].as_bool() == Some(true)).or(images.first());
                        if let Some(img) = img {
                            url = img["thumbnails"]["500"]
                                .as_str()
                                .or(img["thumbnails"]["large"].as_str())
                                .or(img["image"].as_str())
                                .map(String::from);
                            if url.is_some() {
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => api.log(format!("falha ao buscar capa: {e}")),
        }
        self.art_cache.insert(key, url.clone());
        url
    }

    fn schedule(&mut self, immediate: bool) {
        let wait = if immediate {
            self.last_update.map(|t| MIN_INTERVAL.saturating_sub(t.elapsed())).unwrap_or_default()
        } else {
            MIN_INTERVAL
        };
        self.update_at = Some(Instant::now() + wait);
    }

    fn push(&mut self, api: &Api) {
        if !self.ready || self.client.is_none() {
            return;
        }
        self.last_update = Some(Instant::now());
        let st = api.state();
        let result = match st.track.as_ref().filter(|_| st.playing || !api.get_bool("clearOnPause")) {
            None => self.client.as_mut().unwrap().clear_activity().map_err(|e| e.to_string()),
            Some(t) => {
                let details = clamp(template(&api.get_str("details"), t)).or_else(|| clamp(t.title.clone()));
                let state = clamp(template(&api.get_str("state"), t));
                let large_text = clamp(template(&api.get_str("largeText"), t));
                let art = if api.get_bool("fetchArt") { self.find_art(api, &t.artist, &t.album) } else { None };
                let mut ts = activity::Timestamps::new();
                let timed = st.playing && st.duration > 0.0;
                if timed {
                    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
                    let start = now_ms - (st.position * 1000.0) as i64;
                    ts = ts.start(start / 1000).end((start + (st.duration * 1000.0) as i64) / 1000);
                }
                let mut assets = activity::Assets::new();
                if let Some(a) = art.as_deref() {
                    assets = assets.large_image(a);
                }
                if let Some(l) = large_text.as_deref() {
                    assets = assets.large_text(l);
                }
                if !st.playing {
                    assets = assets.small_text("Pausado");
                }
                let mut act = activity::Activity::new().activity_type(activity::ActivityType::Listening).assets(assets);
                if let Some(d) = details.as_deref() {
                    act = act.details(d);
                }
                if let Some(s) = state.as_deref() {
                    act = act.state(s);
                }
                if timed {
                    act = act.timestamps(ts);
                }
                self.client.as_mut().unwrap().set_activity(act).map_err(|e| e.to_string())
            }
        };
        match result {
            Ok(()) => api.set_status("Conectado ao Discord"),
            Err(e) => {
                api.log(format!("erro ao atualizar presença: {e}"));
                api.set_status(format!("Erro: {e} — reconectando…"));
                self.drop_client();
                self.retry_at = Some(Instant::now() + RETRY);
            }
        }
    }

    fn drop_client(&mut self) {
        self.ready = false;
        if let Some(mut c) = self.client.take() {
            let _ = c.clear_activity();
            let _ = c.close();
        }
    }

    fn connect(&mut self, api: &Api) {
        self.drop_client();
        self.retry_at = None;
        let id = api.get_str("clientId").trim().to_string();
        if id.is_empty() {
            api.set_status("Informe o Application ID do Discord nas configurações.");
            return;
        }
        match DiscordIpcClient::new(&id).and_then(|mut c| c.connect().map(|_| c)) {
            Ok(c) => {
                self.client = Some(c);
                self.ready = true;
                api.set_status("Conectado ao Discord");
                self.schedule(true);
            }
            Err(e) => {
                api.set_status(format!("Discord não encontrado ({e}). Tentando de novo…"));
                self.retry_at = Some(Instant::now() + RETRY);
            }
        }
    }
}

impl Plugin for Discord {
    fn start(&mut self, api: &Api) {
        self.connect(api);
    }
    fn stop(&mut self, _api: &Api) {
        self.drop_client();
    }
    fn on_event(&mut self, _api: &Api, ev: Event, _st: &PlayerState) {
        if matches!(ev, Event::Track | Event::Play | Event::Pause | Event::Stop) {
            self.schedule(true);
        }
    }
    fn on_setting(&mut self, api: &Api, key: &str) {
        if key == "clientId" {
            self.connect(api);
        } else {
            self.schedule(true);
        }
    }
    fn tick(&mut self, api: &Api) {
        if let Some(t) = self.retry_at {
            if Instant::now() >= t {
                self.connect(api);
            }
        }
        if let Some(t) = self.update_at {
            if Instant::now() >= t {
                self.update_at = None;
                self.push(api);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_works() {
        let t = TrackInfo { title: "Música".into(), artist: "Banda".into(), album: "Disco".into(), ..Default::default() };
        assert_eq!(template("%title% - %artist%", &t), "Música - Banda");
        assert_eq!(template("100% puro", &t), "100% puro");
        assert_eq!(template("%desconhecido%x", &t), "x");
    }

    #[test]
    fn clamp_rules() {
        assert_eq!(clamp(String::new()), None);
        assert_eq!(clamp("a".into()).unwrap(), "a ");
        assert_eq!(clamp("x".repeat(200)).unwrap().chars().count(), 126);
    }
}
