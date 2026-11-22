//! Tag reading (lofty in place of 1.x mutagen) and embedded cover extraction.
//!
//! - `metadata(path) -> Metadata`: the fields `library.db` stores. Starts from
//!   the 1.x file-name fallbacks (title = file stem, album = parent folder,
//!   codec = suffix) and overlays tags and stream properties when lofty can
//!   read the file. `codec` uses the 1.x mutagen class names (`mp3`, `flac`,
//!   `oggvorbis`, `oggopus`, `mp4`, `wave`, `aiff`, `monkeysaudio`, ...).
//! - `embedded_cover(path, data_dir) -> String`: writes the first embedded
//!   picture (ID3 prefers the front cover) to
//!   `<data>/artwork/embedded/<sha256(resolved path)>-<st_mtime_ns>.jpg|.png`
//!   and removes older generations for the same file; "" when none. The audio
//!   file is never modified.

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::ogg::OggPictureStorage;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag, TagType};
use sha2::{Digest, Sha256};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_no: i64,
    pub disc_no: i64,
    pub year: i64,
    pub genre: String,
    pub duration: f64,
    pub codec: String,
    pub sample_rate: i64,
    pub bit_depth: i64,
    pub channels: i64,
    pub bitrate: i64,
}

/// Python `Path.suffix.lower().lstrip('.')`.
pub fn suffix(p: &Path) -> String {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Python `Path.stem`.
pub fn stem(p: &Path) -> String {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[..i].to_string(),
        _ => name,
    }
}

/// Python `int(str(v).split('/')[0].split('-')[0])`, 0 on failure.
pub fn num(v: &str) -> i64 {
    let a = v.split('/').next().unwrap_or("");
    let b = a.split('-').next().unwrap_or("");
    b.trim().parse().unwrap_or(0)
}

fn codec_name(ft: FileType) -> Option<&'static str> {
    Some(match ft {
        FileType::Aac => "aac",
        FileType::Aiff => "aiff",
        FileType::Ape => "monkeysaudio",
        FileType::Flac => "flac",
        FileType::Mpeg => "mp3",
        FileType::Mp4 => "mp4",
        FileType::Mpc => "musepack",
        FileType::Opus => "oggopus",
        FileType::Vorbis => "oggvorbis",
        FileType::Speex => "oggspeex",
        FileType::Wav => "wave",
        FileType::WavPack => "wavpack",
        _ => return None,
    })
}

fn tag_str(tag: Option<&Tag>, keys: &[ItemKey]) -> Option<String> {
    let tag = tag?;
    keys.iter()
        .find_map(|k| tag.get_string(k))
        .map(|s| s.to_string())
}

pub fn metadata(f: &Path) -> Metadata {
    let parent = f
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = Metadata {
        title: stem(f),
        artist: String::new(),
        album: parent,
        album_artist: String::new(),
        track_no: 0,
        disc_no: 0,
        year: 0,
        genre: String::new(),
        duration: 0.0,
        codec: suffix(f),
        sample_rate: 0,
        bit_depth: 0,
        channels: 0,
        bitrate: 0,
    };
    let opts = ParseOptions::new().read_cover_art(false);
    let Ok(probe) = Probe::open(f) else {
        return out;
    };
    let Ok(probe) = probe.options(opts).guess_file_type() else {
        return out;
    };
    let Ok(tf) = probe.read() else {
        return out;
    };
    let tag = tf.primary_tag().or_else(|| tf.first_tag());
    if let Some(v) = tag_str(tag, &[ItemKey::TrackTitle]) {
        out.title = v;
    }
    out.artist = tag_str(tag, &[ItemKey::TrackArtist]).unwrap_or_default();
    if let Some(v) = tag_str(tag, &[ItemKey::AlbumTitle]) {
        out.album = v;
    }
    out.album_artist = tag_str(tag, &[ItemKey::AlbumArtist]).unwrap_or_default();
    out.track_no = num(&tag_str(tag, &[ItemKey::TrackNumber]).unwrap_or_default());
    out.disc_no = num(&tag_str(tag, &[ItemKey::DiscNumber]).unwrap_or_default());
    out.year = num(&tag_str(tag, &[ItemKey::RecordingDate, ItemKey::Year]).unwrap_or_default());
    out.genre = tag_str(tag, &[ItemKey::Genre]).unwrap_or_default();
    let p = tf.properties();
    out.duration = p.duration().as_secs_f64();
    out.sample_rate = p.sample_rate().unwrap_or(0) as i64;
    out.channels = p.channels().unwrap_or(0) as i64;
    out.bitrate = p.audio_bitrate().unwrap_or(0) as i64;
    out.bit_depth = p.bit_depth().unwrap_or(0) as i64;
    if let Some(c) = codec_name(tf.file_type()) {
        out.codec = c.to_string();
    }
    out
}

fn pick(pics: &[Picture], prefer_front: bool) -> Option<&Picture> {
    if pics.is_empty() {
        return None;
    }
    if prefer_front {
        if let Some(p) = pics
            .iter()
            .find(|p| p.pic_type() == PictureType::CoverFront)
        {
            return Some(p);
        }
    }
    pics.first()
}

/// The raw bytes and mime of the cover 1.x would extract, or None.
fn embedded_picture(f: &Path) -> Option<(Vec<u8>, bool)> {
    let probe = Probe::open(f).ok()?.guess_file_type().ok()?;
    let ft = probe.file_type();
    // FLAC: METADATA_BLOCK_PICTURE blocks win over pictures inside the comment.
    if ft == Some(FileType::Flac) {
        let mut file = std::fs::File::open(f).ok()?;
        if let Ok(flac) = lofty::flac::FlacFile::read_from(&mut file, ParseOptions::new()) {
            if let Some((p, _)) = flac.pictures().first() {
                return Some((p.data().to_vec(), is_png(p)));
            }
            if let Some(vc) = flac.vorbis_comments() {
                if let Some((p, _)) = vc.pictures().first() {
                    return Some((p.data().to_vec(), is_png(p)));
                }
            }
        }
        return None;
    }
    let tf = probe.read().ok()?;
    for tag in tf.tags() {
        if let Some(p) = pick(tag.pictures(), tag.tag_type() == TagType::Id3v2) {
            return Some((p.data().to_vec(), is_png(p)));
        }
    }
    None
}

fn is_png(p: &Picture) -> bool {
    match p.mime_type() {
        Some(MimeType::Png) => true,
        Some(MimeType::Unknown(s)) => s.to_lowercase().contains("png"),
        _ => false,
    }
}

pub(crate) fn sha256_hex(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Python `Path.resolve()` (non-strict): the canonical path, else the
/// absolute path as given.
fn resolve_lenient(f: &Path) -> PathBuf {
    std::fs::canonicalize(f).unwrap_or_else(|_| {
        if f.is_absolute() {
            f.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|d| d.join(f))
                .unwrap_or_else(|_| f.to_path_buf())
        }
    })
}

pub fn embedded_cover(f: &Path, data_dir: &Path) -> String {
    let cache = data_dir.join("artwork").join("embedded");
    let stamp: i128 = std::fs::metadata(f)
        .map(|m| m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128)
        .unwrap_or(0);
    let base = sha256_hex(&resolve_lenient(f).to_string_lossy());
    // Fast path: this generation was already extracted (the name carries
    // the source mtime, so a changed file gets a new name).
    for ext in [".jpg", ".png"] {
        let out = cache.join(format!("{base}-{stamp}{ext}"));
        if std::fs::metadata(&out)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            return out.to_string_lossy().to_string();
        }
    }
    let Some((data, png)) = embedded_picture(f) else {
        return String::new();
    };
    if data.is_empty() {
        return String::new();
    }
    if std::fs::create_dir_all(&cache).is_err() {
        return String::new();
    }
    let ext = if png { ".png" } else { ".jpg" };
    let name = format!("{base}-{stamp}{ext}");
    let out = cache.join(&name);
    let same = std::fs::metadata(&out)
        .map(|m| m.len() == data.len() as u64)
        .unwrap_or(false);
    if !same {
        let tmp = cache.join(format!("{name}.tmp"));
        if std::fs::write(&tmp, &data).is_err() || std::fs::rename(&tmp, &out).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return String::new();
        }
    }
    // One extracted generation per source file.
    if let Ok(rd) = std::fs::read_dir(&cache) {
        let prefix = format!("{base}-");
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(&prefix) && n != name {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    out.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallbacks_from_file_names() {
        let d = std::env::temp_dir().join(format!("nplay-tags-{}", fastrand::u64(..)));
        std::fs::create_dir_all(d.join("My Album")).unwrap();
        let f = d.join("My Album").join("01 Song.mp3");
        std::fs::write(&f, b"not audio").unwrap();
        let m = metadata(&f);
        assert_eq!(m.title, "01 Song");
        assert_eq!(m.album, "My Album");
        assert_eq!(m.codec, "mp3");
        assert_eq!(m.duration, 0.0);
        assert_eq!(embedded_cover(&f, &d), "");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn helpers() {
        assert_eq!(num("3/12"), 3);
        assert_eq!(num("2001-05-03"), 2001);
        assert_eq!(num("x"), 0);
        assert_eq!(suffix(Path::new("/a/B.FLAC")), "flac");
        assert_eq!(suffix(Path::new("/a/.mp3")), "");
        assert_eq!(stem(Path::new("/a/b.c.mp3")), "b.c");
    }
}
