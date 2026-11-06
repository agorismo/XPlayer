//! Leitura de tags (lofty): título/artista/álbum, ReplayGain e capa embutida.

use crate::playlist::Track;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::tag::{Accessor, ItemKey};
use std::path::Path;

pub const AUDIO_EXT: &[&str] = &["mp3", "flac", "ogg", "oga", "wav", "m4a", "aac", "mp4", "aif", "aiff", "caf"];

pub fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXT.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn parse_db(s: &str) -> Option<f32> {
    s.trim().trim_end_matches(|c: char| c.is_alphabetic()).trim().replace(',', ".").parse().ok()
}

fn parse_f(s: &str) -> Option<f32> {
    s.trim().replace(',', ".").parse().ok()
}

pub fn read_meta(path: &str) -> Track {
    let mut t = Track::from_path(path);
    t.meta_loaded = true;
    let Ok(tagged) = lofty::read_from_path(path) else { return t };
    t.duration = tagged.properties().duration().as_secs_f64();
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        if let Some(v) = tag.title() {
            if !v.trim().is_empty() {
                t.title = v.to_string();
            }
        }
        t.artist = tag.artist().map(|s| s.to_string()).unwrap_or_default();
        t.album = tag.album().map(|s| s.to_string()).unwrap_or_default();
        t.year = tag.year();
        t.track_no = tag.track();
        t.rg_track_gain = tag.get_string(&ItemKey::ReplayGainTrackGain).and_then(parse_db);
        t.rg_track_peak = tag.get_string(&ItemKey::ReplayGainTrackPeak).and_then(parse_f);
        t.rg_album_gain = tag.get_string(&ItemKey::ReplayGainAlbumGain).and_then(parse_db);
        t.rg_album_peak = tag.get_string(&ItemKey::ReplayGainAlbumPeak).and_then(parse_f);
    }
    t
}

/// Bytes da capa embutida (prefere "capa frontal").
pub fn embedded_cover(path: &str) -> Option<Vec<u8>> {
    let tagged = lofty::read_from_path(path).ok()?;
    let mut best: Option<&lofty::picture::Picture> = None;
    for tag in tagged.tags() {
        for p in tag.pictures() {
            if p.pic_type() == PictureType::CoverFront {
                return Some(p.data().to_vec());
            }
            best.get_or_insert(p);
        }
    }
    best.map(|p| p.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rg_parsing() {
        assert_eq!(parse_db("-6.50 dB"), Some(-6.5));
        assert_eq!(parse_db("+3,2 dB"), Some(3.2));
        assert_eq!(parse_db("-4.0"), Some(-4.0));
        assert_eq!(parse_f("0.987654"), Some(0.987654));
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    const MEDIA: &str = "/tmp/claude-0/-home-claude/4d4526df-e6cf-5301-a01d-05b1322b6bac/scratchpad/media";

    #[test]
    fn reads_tags_replaygain_and_embedded_cover() {
        let p = format!("{MEDIA}/emb.mp3");
        if !std::path::Path::new(&p).exists() {
            return;
        }
        let t = read_meta(&p);
        assert_eq!(t.title, "Teste Música");
        assert_eq!(t.artist, "Banda X");
        assert_eq!(t.album, "Disco Y");
        assert_eq!(t.year, Some(2020));
        assert_eq!(t.track_no, Some(3));
        assert!((t.duration - 3.0).abs() < 0.2, "dur {}", t.duration);
        assert_eq!(t.rg_track_gain, Some(-6.5));
        assert_eq!(t.rg_track_peak, Some(0.9));
        let cover = embedded_cover(&p).expect("capa embutida");
        assert!(image::load_from_memory(&cover).is_ok());
        // sem tags: cai no nome do arquivo
        let f = read_meta(&format!("{MEDIA}/mono.ogg"));
        assert_eq!(f.title, "mono");
        assert!(embedded_cover(&format!("{MEDIA}/plain.flac")).is_none());
    }
}
