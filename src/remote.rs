//! Biblioteca remota: o desktop compartilha uma pasta de músicas pela rede local (servidor HTTP
//! simples, com código de acesso) e o app (iOS ou desktop) lista e toca essas músicas em streaming.
//!
//! Faixas remotas têm caminho `pc://<caminho relativo>`; o endereço/código ficam na conexão atual,
//! então continuam valendo se o IP do PC mudar (basta reconectar).

use crate::cover::{self, CoverQuery, CoverSettings, CoverSource};
use crate::meta;
use crate::playlist::Track;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use symphonia::core::io::MediaSource;

pub const PREFIX: &str = "pc://";
pub const DEFAULT_PORT: u16 = 7788;

// ------------------------------------------------------------------ conexão atual (cliente)

#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub addr: String,
    pub code: String,
}

static CONN: RwLock<Option<Conn>> = RwLock::new(None);

pub fn set_conn(c: Option<Conn>) {
    *CONN.write().unwrap() = c;
}

pub fn is_remote(path: &str) -> bool {
    path.starts_with(PREFIX)
}

/// "192.168.0.10" -> "192.168.0.10:7788"; remove "http://" e barras finais.
pub fn norm_addr(a: &str) -> String {
    let a = a.trim().trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/');
    if a.contains(':') || a.is_empty() {
        a.to_string()
    } else {
        format!("{a}:{DEFAULT_PORT}")
    }
}

pub fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|x| u8::from_str_radix(x, 16).ok());
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// URL de um endpoint ("file" ou "cover") para uma faixa `pc://...`, usando a conexão atual.
pub fn url_for(path: &str, endpoint: &str) -> Option<String> {
    let c = CONN.read().unwrap().clone()?;
    let rel = path.strip_prefix(PREFIX)?;
    Some(format!("http://{}/api/{endpoint}?p={}&token={}", c.addr, pct_encode(rel), pct_encode(&c.code)))
}

fn agent(read_secs: u64) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout_read(Duration::from_secs(read_secs)).build()
}

fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => "código incorreto".into(),
        ureq::Error::Status(c, _) => format!("o PC respondeu com erro {c}"),
        ureq::Error::Transport(t) => format!("não consegui falar com o PC ({t}). Mesma rede Wi-Fi? Compartilhamento ligado?"),
    }
}

#[derive(Deserialize)]
struct LibResp {
    scanning: bool,
    tracks: Vec<Track>,
}

/// Baixa a lista de músicas do PC. Retorna (faixas, ainda_indexando).
pub fn fetch_library(addr: &str, code: &str) -> Result<(Vec<Track>, bool), String> {
    let url = format!("http://{}/api/library?token={}", norm_addr(addr), pct_encode(code));
    let r: LibResp = agent(60).get(&url).call().map_err(http_err)?.into_json().map_err(|e| format!("resposta inválida: {e}"))?;
    Ok((r.tracks, r.scanning))
}

/// Capa (já reduzida pelo PC) de uma faixa remota.
pub fn fetch_cover(path: &str) -> Option<Vec<u8>> {
    let url = url_for(path, "cover")?;
    let resp = agent(15).get(&url).call().ok()?;
    let mut data = Vec::new();
    resp.into_reader().take(8 * 1024 * 1024).read_to_end(&mut data).ok()?;
    (!data.is_empty()).then_some(data)
}

// ------------------------------------------------------------------ leitor com Range (streaming)

const CHUNK: u64 = 512 * 1024;

pub struct RangeReader {
    agent: ureq::Agent,
    url: String,
    pos: u64,
    len: u64,
    buf: Vec<u8>,
    buf_start: u64,
}

fn io_err(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg)
}

impl RangeReader {
    pub fn open(url: &str) -> Result<Self, String> {
        let mut r = RangeReader { agent: agent(20), url: url.to_string(), pos: 0, len: 0, buf: Vec::new(), buf_start: 0 };
        r.fetch(0).map_err(|e| e.to_string())?;
        if r.len == 0 {
            return Err("arquivo vazio".into());
        }
        Ok(r)
    }

    fn fetch(&mut self, start: u64) -> io::Result<()> {
        let resp = self
            .agent
            .get(&self.url)
            .set("Range", &format!("bytes={}-{}", start, start + CHUNK - 1))
            .call()
            .map_err(|e| io_err(http_err(e)))?;
        if let Some(total) = resp.header("Content-Range").and_then(|cr| cr.rsplit('/').next()).and_then(|t| t.parse::<u64>().ok()) {
            self.len = total;
        } else if let Some(l) = resp.header("Content-Length").and_then(|l| l.parse::<u64>().ok()) {
            self.len = l; // servidor sem Range: só funciona do início
        }
        let mut data = Vec::new();
        resp.into_reader().take(CHUNK).read_to_end(&mut data)?;
        self.buf = data;
        self.buf_start = start;
        Ok(())
    }
}

impl Read for RangeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        let in_buf = self.pos >= self.buf_start && self.pos < self.buf_start + self.buf.len() as u64;
        if !in_buf {
            self.fetch(self.pos)?;
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let off = (self.pos - self.buf_start) as usize;
        let n = out.len().min(self.buf.len() - off);
        out[..n].copy_from_slice(&self.buf[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let np = match from {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
        };
        if np < 0 {
            return Err(io_err("seek antes do início".into()));
        }
        self.pos = np as u64;
        Ok(self.pos)
    }
}

impl MediaSource for RangeReader {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

// ------------------------------------------------------------------ servidor (desktop)

#[derive(Default)]
pub struct Library {
    pub scanning: bool,
    pub tracks: Vec<Track>,
}

pub struct Share {
    stop: Arc<AtomicBool>,
    pub lib: Arc<Mutex<Library>>,
    pub port: u16,
}

impl Share {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn status(&self) -> (usize, bool) {
        let l = self.lib.lock().unwrap();
        (l.tracks.len(), l.scanning)
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn local_ip() -> Option<String> {
    // "conecta" um socket UDP só para descobrir qual interface sairia para a rede (nada é enviado)
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

pub fn random_code() -> String {
    const ALPHA: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7) | 1;
    (0..6)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ALPHA[(x % ALPHA.len() as u64) as usize] as char
        })
        .collect()
}

fn rel_of(root: &Path, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/"))
}

pub fn start(root: PathBuf, port: u16, token: String) -> Result<Share, String> {
    let root = root.canonicalize().map_err(|e| format!("pasta inválida: {e}"))?;
    if !root.is_dir() {
        return Err("escolha uma pasta".into());
    }
    let mut listener = None;
    for p in (port..port.saturating_add(10)).chain(if port == 0 { 0..1 } else { 0..0 }) {
        if let Ok(l) = TcpListener::bind(("0.0.0.0", p)) {
            listener = Some(l);
            break;
        }
    }
    let listener = listener.ok_or("não consegui abrir a porta (já está em uso?)")?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    let lib = Arc::new(Mutex::new(Library { scanning: true, tracks: Vec::new() }));

    // indexação em segundo plano (4 threads lendo as tags)
    {
        let (root, lib, stop) = (root.clone(), lib.clone(), stop.clone());
        std::thread::spawn(move || {
            let files: Vec<PathBuf> = walkdir::WalkDir::new(&root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file() && meta::is_audio(e.path()))
                .map(|e| e.into_path())
                .collect();
            let next = AtomicUsize::new(0);
            std::thread::scope(|sc| {
                for _ in 0..4 {
                    sc.spawn(|| loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(f) = files.get(i) else { break };
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(rel) = rel_of(&root, f) else { continue };
                        let mut t = meta::read_meta(&f.to_string_lossy());
                        t.path = format!("{PREFIX}{rel}");
                        lib.lock().unwrap().tracks.push(t);
                    });
                }
            });
            lib.lock().unwrap().scanning = false;
        });
    }

    // aceita conexões
    {
        let (root, lib, stop) = (root.clone(), lib.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, _)) => {
                        let (root, lib, token) = (root.clone(), lib.clone(), token.clone());
                        std::thread::spawn(move || {
                            let _ = s.set_nonblocking(false);
                            let _ = handle(s, &root, &token, &lib);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(40)),
                    Err(_) => std::thread::sleep(Duration::from_millis(200)),
                }
            }
        });
    }
    Ok(Share { stop, lib, port })
}

fn respond(s: &mut TcpStream, code: u16, reason: &str, headers: &[(&str, String)], body: &[u8], head_only: bool) -> io::Result<()> {
    let mut h = format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes())?;
    if !head_only {
        s.write_all(body)?;
    }
    Ok(())
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "aif" | "aiff" => "audio/aiff",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

/// "bytes=a-b" | "bytes=a-" | "bytes=-n"  ->  (início, fim inclusivo)
fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        return Some((len.saturating_sub(n), len.checked_sub(1)?));
    }
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() { len.checked_sub(1)? } else { b.parse::<u64>().ok()?.min(len.checked_sub(1)?) };
    (start <= end && start < len).then_some((start, end))
}

fn resolve_rel(root: &Path, rel: &str) -> Option<PathBuf> {
    let rp = Path::new(rel);
    if rel.is_empty() || !rp.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let canon = root.join(rp).canonicalize().ok()?;
    (canon.starts_with(root) && canon.is_file()).then_some(canon)
}

fn handle(mut s: TcpStream, root: &Path, token: &str, lib: &Mutex<Library>) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut head = Vec::new();
    let mut b = [0u8; 1024];
    loop {
        let n = s.read(&mut b)?;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&b[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") || head.len() > 16 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let req = lines.next().unwrap_or("");
    let mut parts = req.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let range = lines.find_map(|l| l.to_lowercase().starts_with("range:").then(|| l[6..].trim().to_string()));
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return respond(&mut s, 405, "Method Not Allowed", &[], b"", false);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let params: HashMap<String, String> =
        query.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (pct_decode(k), pct_decode(v))).collect();
    if !ct_eq(params.get("token").map(|s| s.as_str()).unwrap_or(""), token) {
        return respond(&mut s, 401, "Unauthorized", &[], "código incorreto".as_bytes(), head_only);
    }
    match path {
        "/api/ping" => respond(&mut s, 200, "OK", &[], b"ok", head_only),
        "/api/library" => {
            let body = {
                let l = lib.lock().unwrap();
                serde_json::json!({ "scanning": l.scanning, "tracks": l.tracks }).to_string()
            };
            respond(&mut s, 200, "OK", &[("Content-Type", "application/json; charset=utf-8".into())], body.as_bytes(), head_only)
        }
        "/api/file" => {
            let Some(abs) = resolve_rel(root, params.get("p").map(|s| s.as_str()).unwrap_or("")) else {
                return respond(&mut s, 404, "Not Found", &[], b"", head_only);
            };
            // só serve áudio e imagens (nunca outros arquivos da pasta)
            if !meta::is_audio(&abs) && !cover::is_image(&abs) {
                return respond(&mut s, 403, "Forbidden", &[], b"", head_only);
            }
            let mut f = std::fs::File::open(&abs)?;
            let len = f.metadata()?.len();
            let ct = content_type(&abs).to_string();
            let (code, reason, start, end) = match range.as_deref().map(|r| parse_range(r, len)) {
                Some(Some((a, b))) => (206, "Partial Content", a, b),
                Some(None) => {
                    return respond(&mut s, 416, "Range Not Satisfiable", &[("Content-Range", format!("bytes */{len}"))], b"", head_only)
                }
                None => (200, "OK", 0, len.saturating_sub(1)),
            };
            let n = if len == 0 { 0 } else { end - start + 1 };
            let mut h = format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nAccept-Ranges: bytes\r\nContent-Type: {ct}\r\nContent-Length: {n}\r\n");
            if code == 206 {
                h.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
            }
            h.push_str("\r\n");
            s.write_all(h.as_bytes())?;
            if head_only || n == 0 {
                return Ok(());
            }
            f.seek(SeekFrom::Start(start))?;
            let mut left = n;
            let mut buf = vec![0u8; 64 * 1024];
            while left > 0 {
                let want = buf.len().min(left as usize);
                let r = f.read(&mut buf[..want])?;
                if r == 0 {
                    break;
                }
                s.write_all(&buf[..r])?;
                left -= r as u64;
            }
            Ok(())
        }
        "/api/cover" => {
            let Some(abs) = resolve_rel(root, params.get("p").map(|s| s.as_str()).unwrap_or("")) else {
                return respond(&mut s, 404, "Not Found", &[], b"", head_only);
            };
            let mut cs = CoverSettings::default();
            for e in &mut cs.sources {
                e.enabled = matches!(e.source, CoverSource::Embedded | CoverSource::Folder);
            }
            cs.fallback_image.clear();
            let q = CoverQuery { path: abs.to_string_lossy().to_string(), artist: String::new(), album: String::new() };
            let jpeg = cover::resolve(&q, &cs, &HashMap::new()).and_then(|r| {
                // reduz para economizar rede (a capa aparece pequena no celular)
                let img = image::load_from_memory(&r.bytes).ok()?.thumbnail(600, 600).to_rgb8();
                let mut out = io::Cursor::new(Vec::new());
                img.write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
                Some(out.into_inner())
            });
            match jpeg {
                Some(b) => respond(&mut s, 200, "OK", &[("Content-Type", "image/jpeg".into())], &b, head_only),
                None => respond(&mut s, 404, "Not Found", &[], b"", head_only),
            }
        }
        _ => respond(&mut s, 404, "Not Found", &[], b"", head_only),
    }
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    pub const MEDIA: &str = "/tmp/claude-0/-home-claude/4d4526df-e6cf-5301-a01d-05b1322b6bac/scratchpad/media";

    #[test]
    fn helpers() {
        assert_eq!(pct_decode(&pct_encode("Álbum x/y é+z.mp3")), "Álbum x/y é+z.mp3");
        assert_eq!(norm_addr(" http://192.168.0.2/ "), "192.168.0.2:7788");
        assert_eq!(norm_addr("pc.local:9000"), "pc.local:9000");
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=5000-", 1000), None);
        assert_eq!(random_code().len(), 6);
    }

    #[test]
    fn share_and_stream_end_to_end() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !Path::new(MEDIA).join("emb.mp3").exists() {
            return;
        }
        let share = start(PathBuf::from(MEDIA), 0, "SEGREDO".into()).expect("start");
        let addr = format!("127.0.0.1:{}", share.port);
        // biblioteca
        let mut lib = (vec![], true);
        for _ in 0..100 {
            lib = fetch_library(&addr, "SEGREDO").expect("biblioteca");
            if !lib.1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!lib.1, "indexação não terminou");
        let t = lib.0.iter().find(|t| t.path == "pc://emb.mp3").expect("emb.mp3 na biblioteca");
        assert_eq!(t.title, "Teste Música");
        assert_eq!(t.artist, "Banda X");
        // código errado
        assert!(fetch_library(&addr, "ERRADO").unwrap_err().contains("código"));
        // streaming com Range == arquivo original
        set_conn(Some(Conn { addr: addr.clone(), code: "SEGREDO".into() }));
        let url = url_for("pc://emb.mp3", "file").unwrap();
        let mut r = RangeReader::open(&url).unwrap();
        let orig = std::fs::read(Path::new(MEDIA).join("emb.mp3")).unwrap();
        assert_eq!(r.byte_len(), Some(orig.len() as u64));
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert!(got == orig, "conteúdo difere");
        r.seek(SeekFrom::End(-100)).unwrap();
        let mut tail = Vec::new();
        r.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, orig[orig.len() - 100..]);
        // capa embutida via rede
        let cv = fetch_cover("pc://emb.mp3").expect("capa");
        assert!(image::load_from_memory(&cv).is_ok());
        // segurança: sem token, saída da pasta e arquivo que não é mídia
        let raw = |req: &str| {
            let mut s = TcpStream::connect(&addr).unwrap();
            s.write_all(req.as_bytes()).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        };
        assert!(raw("GET /api/file?p=emb.mp3 HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 401"));
        assert!(raw("GET /api/file?p=..%2F..%2Fetc%2Fpasswd&token=SEGREDO HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
        assert!(raw("GET /api/file?p=%2Fetc%2Fpasswd&token=SEGREDO HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
        assert!(raw("GET /api/file?p=emb.mp3&token=SEGREDO HTTP/1.1\r\nRange: bytes=999999999-\r\n\r\n").starts_with("HTTP/1.1 416"));
        share.stop();
        set_conn(None);
    }
}
