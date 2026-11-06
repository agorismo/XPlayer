//! Last.fm Scrobbler — API 2.0 (o protocolo 1.2 do foo_audioscrobbler original está obsoleto).
//! Regras mantidas: faixa > 30 s; scrobble após 50% ou 4 min; fila offline persistente.

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const ENDPOINT: &str = "https://ws.audioscrobbler.com/2.0/";
const FLUSH_EVERY: Duration = Duration::from_secs(60);
const BATCH: usize = 50;

pub fn meta() -> Meta {
    Meta {
        id: "lastfm",
        name: "Last.fm Scrobbler",
        version: "0.2.0",
        description: "Envia o que você ouve para o Last.fm (now playing + scrobble) usando a API atual 2.0. Inspirado no foo_audioscrobbler.",
        default_enabled: false,
        settings: vec![
            SettingDef { key: "apiKey", label: "API key", kind: Kind::Text, default: json!("") },
            SettingDef { key: "apiSecret", label: "API secret", kind: Kind::Password, default: json!("") },
            SettingDef { key: "username", label: "Usuário Last.fm", kind: Kind::Text, default: json!("") },
            SettingDef { key: "password", label: "Senha (só para logar)", kind: Kind::Password, default: json!("") },
            SettingDef { key: "nowPlaying", label: "Enviar \"ouvindo agora\"", kind: Kind::Bool, default: json!(true) },
        ],
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Scrobble {
    artist: String,
    title: String,
    album: String,
    duration: u64,
    timestamp: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedSession {
    user: String,
    key: String,
}

struct Current {
    info: TrackInfo,
    started_at: u64, // ms
    played: f64,
    last_pos: f64,
    scrobbled: bool,
}

#[derive(Default)]
pub struct LastFm {
    queue: Vec<Scrobble>,
    session_key: String,
    cur: Option<Current>,
    last_flush: Option<Instant>,
    flush_now: bool,
    login_now: bool,
}

struct CallError {
    code: i64,
    msg: String,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn sign(params: &[(String, String)], secret: &str) -> String {
    let mut p: Vec<&(String, String)> = params.iter().filter(|(k, _)| k != "format" && k != "callback").collect();
    p.sort_by(|a, b| a.0.cmp(&b.0));
    let base: String = p.iter().map(|(k, v)| format!("{k}{v}")).collect::<String>() + secret;
    format!("{:x}", md5::compute(base.as_bytes()))
}

fn call(api: &Api, params: Vec<(String, String)>) -> Result<serde_json::Value, CallError> {
    let mut body = params;
    body.push(("api_key".into(), api.get_str("apiKey")));
    let sig = sign(&body, &api.get_str("apiSecret"));
    body.push(("api_sig".into(), sig));
    body.push(("format".into(), "json".into()));
    let form: Vec<(&str, &str)> = body.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let res = ureq::post(ENDPOINT).timeout(Duration::from_secs(15)).send_form(&form);
    let json: serde_json::Value = match res {
        Ok(r) => r.into_json().unwrap_or_default(),
        // o Last.fm devolve o erro da API em JSON com status HTTP 4xx
        Err(ureq::Error::Status(_, r)) => r.into_json().unwrap_or_default(),
        Err(e) => return Err(CallError { code: -1, msg: e.to_string() }),
    };
    if let Some(code) = json["error"].as_i64() {
        return Err(CallError { code, msg: json["message"].as_str().unwrap_or("erro").to_string() });
    }
    Ok(json)
}

fn valid(t: &TrackInfo) -> bool {
    !t.artist.is_empty() && !t.title.is_empty() && t.duration > 30.0
}

impl LastFm {
    fn file(api: &Api, name: &str) -> PathBuf {
        api.data_dir.join(name)
    }

    fn save_queue(&self, api: &Api) {
        if let Ok(s) = serde_json::to_string(&self.queue) {
            if let Err(e) = std::fs::write(Self::file(api, "queue.json"), s) {
                api.log(format!("erro ao salvar fila: {e}"));
            }
        }
    }

    fn has_creds(api: &Api) -> bool {
        ["apiKey", "apiSecret", "username"].iter().all(|k| !api.get_str(k).trim().is_empty())
    }

    fn ensure_session(&mut self, api: &Api) -> bool {
        if !self.session_key.is_empty() {
            return true;
        }
        if !Self::has_creds(api) {
            api.set_status("Preencha API key, secret e usuário.");
            return false;
        }
        let password = api.get_str("password");
        if password.is_empty() {
            api.set_status("Informe a senha uma vez para logar (ela é apagada depois do login).");
            return false;
        }
        let r = call(
            api,
            vec![
                ("method".into(), "auth.getMobileSession".into()),
                ("username".into(), api.get_str("username")),
                ("password".into(), password),
            ],
        );
        match r {
            Ok(j) => {
                self.session_key = j["session"]["key"].as_str().unwrap_or_default().to_string();
                let user = j["session"]["name"].as_str().unwrap_or_default().to_string();
                let saved = SavedSession { user: user.clone(), key: self.session_key.clone() };
                let _ = std::fs::write(Self::file(api, "session.json"), serde_json::to_string(&saved).unwrap_or_default());
                api.set("password", json!("")); // só a session key fica guardada
                api.set_status(format!("Logado como {user}."));
                !self.session_key.is_empty()
            }
            Err(e) => {
                api.set_status(format!("Falha no login: {}", e.msg));
                false
            }
        }
    }

    fn status(&self, api: &Api, extra: &str) {
        let q = if self.queue.is_empty() { String::new() } else { format!(" — {} na fila", self.queue.len()) };
        let x = if extra.is_empty() { String::new() } else { format!(" — {extra}") };
        api.set_status(format!("Logado{q}{x}"));
    }

    fn flush(&mut self, api: &Api) {
        self.last_flush = Some(Instant::now());
        if self.queue.is_empty() || !self.ensure_session(api) {
            return;
        }
        while !self.queue.is_empty() {
            let n = self.queue.len().min(BATCH);
            let mut p: Vec<(String, String)> =
                vec![("method".into(), "track.scrobble".into()), ("sk".into(), self.session_key.clone())];
            for (i, s) in self.queue[..n].iter().enumerate() {
                p.push((format!("artist[{i}]"), s.artist.clone()));
                p.push((format!("track[{i}]"), s.title.clone()));
                p.push((format!("timestamp[{i}]"), s.timestamp.to_string()));
                if !s.album.is_empty() {
                    p.push((format!("album[{i}]"), s.album.clone()));
                }
                if s.duration > 0 {
                    p.push((format!("duration[{i}]"), s.duration.to_string()));
                }
            }
            match call(api, p) {
                Ok(_) => {
                    self.queue.drain(..n);
                    self.save_queue(api);
                }
                Err(e) => {
                    if e.code == 9 {
                        // sessão inválida: loga de novo
                        self.session_key.clear();
                        let _ = std::fs::write(Self::file(api, "session.json"), "{}");
                    }
                    self.status(api, &format!("erro ao enviar ({}); tentará de novo", e.msg));
                    return;
                }
            }
        }
        self.status(api, "scrobbles enviados");
    }

    fn now_playing(&mut self, api: &Api, t: &TrackInfo) {
        if !api.get_bool("nowPlaying") || !self.ensure_session(api) {
            return;
        }
        let mut p: Vec<(String, String)> = vec![
            ("method".into(), "track.updateNowPlaying".into()),
            ("sk".into(), self.session_key.clone()),
            ("artist".into(), t.artist.clone()),
            ("track".into(), t.title.clone()),
        ];
        if !t.album.is_empty() {
            p.push(("album".into(), t.album.clone()));
        }
        if t.duration > 0.0 {
            p.push(("duration".into(), (t.duration.round() as u64).to_string()));
        }
        if let Err(e) = call(api, p) {
            if e.code == 9 {
                self.session_key.clear();
            }
            api.log(format!("now playing falhou: {}", e.msg));
        }
    }

    fn start_track(&mut self, api: &Api, st: &PlayerState) {
        let Some(t) = st.track.clone() else {
            self.cur = None;
            return;
        };
        if let Some(c) = self.cur.as_mut() {
            let same = c.info.path == t.path;
            if same && (now_ms() - c.started_at) as f64 / 1000.0 < t.duration.max(1.0) && !c.scrobbled {
                c.info = t; // mesma faixa ainda em andamento (play/pause): mantém a contagem
                return;
            }
        }
        self.cur = Some(Current { info: t.clone(), started_at: now_ms(), played: 0.0, last_pos: st.position, scrobbled: false });
        if valid(&t) {
            self.now_playing(api, &t);
        } else {
            api.log(format!("faixa ignorada (sem artista/título ou duração ≤ 30s): {}", t.title));
        }
    }

    fn on_time(&mut self, api: &Api, st: &PlayerState) {
        let same = matches!((&self.cur, &st.track), (Some(c), Some(t)) if c.info.path == t.path);
        if !same {
            return self.start_track(api, st);
        }
        let c = self.cur.as_mut().unwrap();
        let delta = st.position - c.last_pos;
        c.last_pos = st.position;
        // só conta reprodução contínua (ignora seeks grandes)
        if st.playing && delta > 0.0 && delta < 3.0 {
            c.played += delta;
        }
        let t = c.info.clone();
        if !c.scrobbled && valid(&t) && c.played >= (t.duration / 2.0).min(240.0) {
            c.scrobbled = true;
            let started = c.started_at;
            self.queue.push(Scrobble {
                artist: t.artist,
                title: t.title,
                album: t.album,
                duration: t.duration.round() as u64,
                timestamp: started / 1000,
            });
            self.save_queue(api);
            self.flush_now = true;
        }
    }
}

impl Plugin for LastFm {
    fn start(&mut self, api: &Api) {
        self.last_flush = Some(Instant::now());
        self.queue = std::fs::read_to_string(Self::file(api, "queue.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let saved: SavedSession = std::fs::read_to_string(Self::file(api, "session.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !saved.key.is_empty() && saved.user == api.get_str("username") {
            self.session_key = saved.key;
        }
        if Self::has_creds(api) && (!self.session_key.is_empty() || !api.get_str("password").is_empty()) {
            if self.ensure_session(api) {
                self.status(api, "");
                self.flush_now = true;
            }
        } else {
            api.set_status("Preencha API key, secret, usuário e senha.");
        }
    }

    fn stop(&mut self, _api: &Api) {
        self.cur = None;
    }

    fn on_event(&mut self, api: &Api, ev: Event, st: &PlayerState) {
        match ev {
            Event::Track => self.start_track(api, st),
            Event::Play => {
                match &mut self.cur {
                    Some(c) if st.track.as_ref().map(|t| t.path == c.info.path).unwrap_or(false) => c.last_pos = st.position,
                    _ => self.start_track(api, st),
                }
            }
            Event::Time => self.on_time(api, st),
            Event::Stop => self.cur = None,
            Event::Ended => {
                if self.cur.is_some() {
                    let mut s = st.clone();
                    s.position = st.duration;
                    s.playing = true;
                    if let Some(c) = self.cur.as_mut() {
                        c.last_pos = st.duration;
                    }
                    self.on_time(api, &s);
                }
                self.cur = None;
            }
            Event::Pause => {}
        }
    }

    fn on_setting(&mut self, _api: &Api, key: &str) {
        if ["apiKey", "apiSecret", "username", "password"].contains(&key) {
            self.session_key.clear();
            self.login_now = true;
        }
    }

    fn tick(&mut self, api: &Api) {
        if self.login_now {
            self.login_now = false;
            if self.ensure_session(api) {
                self.flush_now = true;
            }
        }
        let due = self.last_flush.map(|t| t.elapsed() >= FLUSH_EVERY).unwrap_or(false);
        if self.flush_now || due {
            self.flush_now = false;
            self.flush(api);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_sorted_and_ignores_format() {
        let p = vec![
            ("method".to_string(), "auth.getMobileSession".to_string()),
            ("api_key".to_string(), "k".to_string()),
            ("format".to_string(), "json".to_string()),
        ];
        let expect = format!("{:x}", md5::compute(b"api_keykmethodauth.getMobileSessionSEGREDO"));
        assert_eq!(sign(&p, "SEGREDO"), expect);
    }
}
