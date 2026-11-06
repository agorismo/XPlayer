//! Capas / arte do álbum, com suporte a imagens locais.
//!
//! Fontes (ordem e ativação configuráveis):
//!  - Manual:   imagem que o usuário escolheu para a faixa ou para o álbum
//!  - Embutida: imagem dentro das tags do arquivo
//!  - Pasta:    cover.jpg, folder.png, front.jpg... na pasta da faixa (nomes configuráveis)
//!  - Biblioteca de capas: uma pasta central com "Artista - Álbum.jpg" ou "Álbum.jpg"
//! Se nada for encontrado, usa a imagem padrão (opcional).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];

pub fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXT.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum CoverSource {
    Manual,
    Embedded,
    Folder,
    CoversDir,
}

impl CoverSource {
    pub fn label(self) -> &'static str {
        match self {
            CoverSource::Manual => "Capa escolhida por mim (faixa/álbum)",
            CoverSource::Embedded => "Capa embutida no arquivo",
            CoverSource::Folder => "Imagem na pasta da música (cover.jpg, folder.png…)",
            CoverSource::CoversDir => "Biblioteca de capas (pasta central)",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SourceEntry {
    pub source: CoverSource,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct CoverSettings {
    pub sources: Vec<SourceEntry>,
    /// Nomes (sem extensão) procurados na pasta da faixa, separados por vírgula, em ordem de prioridade.
    pub folder_names: String,
    pub covers_dir: String,
    /// Imagem usada quando nenhuma fonte tem capa.
    pub fallback_image: String,
    /// Ao escolher uma imagem, copia para a pasta do player (continua valendo se o original for movido).
    pub copy_into_library: bool,
    /// Escopo padrão ao definir uma capa: true = álbum inteiro, false = só a faixa.
    pub default_scope_album: bool,
    /// Lado máximo (px) ao carregar a imagem; imagens maiores são reduzidas.
    pub max_size: u32,
}

impl Default for CoverSettings {
    fn default() -> Self {
        Self {
            sources: vec![
                SourceEntry { source: CoverSource::Manual, enabled: true },
                SourceEntry { source: CoverSource::Embedded, enabled: true },
                SourceEntry { source: CoverSource::Folder, enabled: true },
                SourceEntry { source: CoverSource::CoversDir, enabled: false },
            ],
            folder_names: "cover, folder, front, albumart, album, art".into(),
            covers_dir: String::new(),
            fallback_image: String::new(),
            copy_into_library: true,
            default_scope_album: true,
            max_size: 1024,
        }
    }
}

impl CoverSettings {
    /// Garante que cada fonte apareça exatamente uma vez e que os valores sejam sensatos.
    pub fn normalize(&mut self) {
        let mut seen = Vec::new();
        self.sources.retain(|e| {
            if seen.contains(&e.source) {
                false
            } else {
                seen.push(e.source);
                true
            }
        });
        for d in CoverSettings::default().sources {
            if !seen.contains(&d.source) {
                self.sources.push(d);
            }
        }
        self.max_size = self.max_size.clamp(128, 4096);
    }
}

#[derive(Clone, Debug)]
pub struct CoverQuery {
    pub path: String,
    pub artist: String,
    pub album: String,
}

pub struct Resolved {
    pub bytes: Vec<u8>,
    /// Texto curto mostrado ao usuário: de onde veio a capa.
    pub origin: String,
}

pub fn track_key(path: &str) -> String {
    format!("track|{path}")
}

pub fn album_key(artist: &str, album: &str) -> Option<String> {
    if album.trim().is_empty() {
        None
    } else {
        Some(format!("album|{}|{}", artist.trim().to_lowercase(), album.trim().to_lowercase()))
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect::<String>().trim().to_string()
}

/// Lista imagens de uma pasta: (nome sem extensão em minúsculas, caminho).
fn images_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().to_lowercase(), p)))
        .collect()
}

fn read_image(p: &Path) -> Option<Vec<u8>> {
    std::fs::read(p).ok().filter(|b| !b.is_empty())
}

pub fn find_in_folder(track_path: &str, names: &str) -> Option<PathBuf> {
    let dir = Path::new(track_path).parent()?;
    let imgs = images_in(dir);
    for n in names.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) {
        if let Some((_, p)) = imgs.iter().find(|(stem, _)| *stem == n) {
            return Some(p.clone());
        }
    }
    None
}

pub fn find_in_covers_dir(dir: &str, artist: &str, album: &str) -> Option<PathBuf> {
    if dir.trim().is_empty() || album.trim().is_empty() {
        return None;
    }
    let imgs = images_in(Path::new(dir));
    let mut names = Vec::new();
    if !artist.trim().is_empty() {
        names.push(format!("{} - {}", artist.trim(), album.trim()));
        names.push(sanitize(&format!("{} - {}", artist.trim(), album.trim())));
    }
    names.push(album.trim().to_string());
    names.push(sanitize(album));
    for n in names.iter().map(|s| s.to_lowercase()) {
        if let Some((_, p)) = imgs.iter().find(|(stem, _)| *stem == n) {
            return Some(p.clone());
        }
    }
    None
}

pub fn resolve(q: &CoverQuery, s: &CoverSettings, custom: &HashMap<String, String>) -> Option<Resolved> {
    for entry in s.sources.iter().filter(|e| e.enabled) {
        let found = match entry.source {
            CoverSource::Manual => {
                let keys = [Some(track_key(&q.path)), album_key(&q.artist, &q.album)];
                keys.into_iter()
                    .flatten()
                    .filter_map(|k| custom.get(&k))
                    .find_map(|f| read_image(Path::new(f)).map(|b| (b, "Capa escolhida por você".to_string())))
            }
            CoverSource::Embedded => crate::meta::embedded_cover(&q.path).map(|b| (b, "Capa embutida".to_string())),
            CoverSource::Folder => find_in_folder(&q.path, &s.folder_names).and_then(|p| {
                let name = p.file_name()?.to_string_lossy().to_string();
                read_image(&p).map(|b| (b, format!("Pasta: {name}")))
            }),
            CoverSource::CoversDir => find_in_covers_dir(&s.covers_dir, &q.artist, &q.album).and_then(|p| {
                let name = p.file_name()?.to_string_lossy().to_string();
                read_image(&p).map(|b| (b, format!("Biblioteca: {name}")))
            }),
        };
        if let Some((bytes, origin)) = found {
            return Some(Resolved { bytes, origin });
        }
    }
    if !s.fallback_image.trim().is_empty() {
        if let Some(b) = read_image(Path::new(&s.fallback_image)) {
            return Some(Resolved { bytes: b, origin: "Imagem padrão".into() });
        }
    }
    None
}

/// Decodifica e reduz a imagem para exibição.
pub fn decode(bytes: &[u8], max: u32) -> Option<egui::ColorImage> {
    let mut img = image::load_from_memory(bytes).ok()?;
    if img.width() > max || img.height() > max {
        img = img.thumbnail(max, max);
    }
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Valida que o arquivo é uma imagem legível e (opcionalmente) copia para a biblioteca do player.
/// Retorna o caminho que deve ser guardado.
pub fn import_image(src: &Path, covers_dir: &Path, key: &str, copy: bool) -> Result<PathBuf, String> {
    let bytes = std::fs::read(src).map_err(|e| format!("não consegui ler o arquivo: {e}"))?;
    image::guess_format(&bytes).map_err(|_| "o arquivo não parece ser uma imagem".to_string())?;
    image::load_from_memory(&bytes).map_err(|e| format!("imagem inválida: {e}"))?;
    if !copy {
        return Ok(src.to_path_buf());
    }
    std::fs::create_dir_all(covers_dir).map_err(|e| e.to_string())?;
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("img").to_lowercase();
    let dest = covers_dir.join(format!("{:x}.{ext}", md5::compute(key.as_bytes())));
    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Salva os bytes de uma capa como `cover.<ext>` na pasta da faixa.
pub fn save_beside(track_path: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = Path::new(track_path).parent().ok_or("faixa sem pasta")?;
    let ext = match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::WebP) => "webp",
        Ok(image::ImageFormat::Bmp) => "bmp",
        Ok(image::ImageFormat::Gif) => "gif",
        _ => return Err("formato de imagem desconhecido".into()),
    };
    let dest = dir.join(format!("cover.{ext}"));
    if dest.exists() {
        return Err(format!("{} já existe", dest.display()));
    }
    std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("meu-player-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png(color: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb(color));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn query(dir: &Path) -> CoverQuery {
        CoverQuery { path: dir.join("faixa.mp3").to_string_lossy().into(), artist: "Banda".into(), album: "Disco".into() }
    }

    #[test]
    fn folder_cover_is_found_case_insensitive() {
        let d = tmpdir("folder");
        std::fs::write(d.join("Folder.PNG"), png([1, 2, 3])).unwrap();
        let r = resolve(&query(&d), &CoverSettings::default(), &HashMap::new()).unwrap();
        assert!(r.origin.contains("Folder.PNG"));
    }

    #[test]
    fn manual_beats_folder_and_track_beats_album() {
        let d = tmpdir("manual");
        std::fs::write(d.join("cover.png"), png([9, 9, 9])).unwrap();
        let album_img = d.join("album.png.bak.png");
        std::fs::write(&album_img, png([1, 1, 1])).unwrap();
        let track_img = d.join("track_custom.png");
        std::fs::write(&track_img, png([2, 2, 2])).unwrap();
        let q = query(&d);
        let mut custom = HashMap::new();
        custom.insert(album_key("Banda", "Disco").unwrap(), album_img.to_string_lossy().into());
        let r = resolve(&q, &CoverSettings::default(), &custom).unwrap();
        assert_eq!(r.origin, "Capa escolhida por você");
        assert_eq!(r.bytes, png([1, 1, 1]));
        custom.insert(track_key(&q.path), track_img.to_string_lossy().into());
        assert_eq!(resolve(&q, &CoverSettings::default(), &custom).unwrap().bytes, png([2, 2, 2]));
    }

    #[test]
    fn disabled_source_is_skipped_and_fallback_used() {
        let d = tmpdir("fallback");
        std::fs::write(d.join("cover.png"), png([5, 5, 5])).unwrap();
        let fb = d.join("padrao.png");
        std::fs::write(&fb, png([7, 7, 7])).unwrap();
        let mut s = CoverSettings::default();
        for e in &mut s.sources {
            if e.source == CoverSource::Folder {
                e.enabled = false;
            }
        }
        s.fallback_image = fb.to_string_lossy().into();
        let r = resolve(&query(&d), &s, &HashMap::new()).unwrap();
        assert_eq!(r.origin, "Imagem padrão");
    }

    #[test]
    fn covers_dir_lookup() {
        let lib = tmpdir("lib");
        std::fs::write(lib.join("banda - disco.jpg"), png([4, 4, 4])).unwrap();
        let song_dir = tmpdir("song");
        let mut s = CoverSettings::default();
        s.covers_dir = lib.to_string_lossy().into();
        s.sources.iter_mut().for_each(|e| e.enabled = e.source == CoverSource::CoversDir);
        let r = resolve(&query(&song_dir), &s, &HashMap::new()).unwrap();
        assert!(r.origin.starts_with("Biblioteca"));
    }

    #[test]
    fn import_copies_and_rejects_non_images() {
        let d = tmpdir("import");
        let src = d.join("x.png");
        std::fs::write(&src, png([3, 3, 3])).unwrap();
        let dest = import_image(&src, &d.join("covers"), "track|a", true).unwrap();
        assert!(dest.starts_with(d.join("covers")) && dest.exists());
        let txt = d.join("a.png");
        std::fs::write(&txt, b"nao sou imagem").unwrap();
        assert!(import_image(&txt, &d.join("covers"), "k", true).is_err());
    }

    #[test]
    fn decode_downsizes() {
        let img = image::RgbImage::from_pixel(400, 200, image::Rgb([1, 2, 3]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        let c = decode(&out.into_inner(), 128).unwrap();
        assert!(c.size[0] <= 128 && c.size[1] <= 128);
    }

    #[test]
    fn normalize_restores_missing_sources() {
        let mut s = CoverSettings { sources: vec![], ..Default::default() };
        s.normalize();
        assert_eq!(s.sources.len(), 4);
    }
}
