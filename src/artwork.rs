//! Cover artwork in the terminal (port of `nplay/artwork.py`).
//!
//! 1.x spawned `kitten icat`; this version speaks the kitty graphics protocol
//! itself, so it works in kitty and in NIRUTERM (which has no kitten). The
//! image is decoded with the `image` crate (PNG/JPEG), scaled to fit the cell
//! box keeping its aspect ratio (scaled up too, like `--scale-up`), centred
//! horizontally, PNG-encoded and sent in 4096-byte chunks
//! (`a=T,f=100,t=d,c=..,r=..,C=1,q=2`) under one fixed image id; the cursor
//! is saved and restored around it. Clearing deletes that image (`a=d,d=I`).
//!
//! Public API (`Artwork` is `Clone + Send + Sync`):
//! - `Artwork::new(on_ready, cache)`: `on_ready(url)` runs on the download
//!   thread after a prefetch finished (hit or miss).
//! - `kitty_supported()`: `KITTY_WINDOW_ID` set, `TERM=xterm-kitty` or
//!   `TERM_PROGRAM=niruterm`. `kitty()` is the same for this handle (can be
//!   forced with `set_kitty(Some(bool))`).
//! - `path(url)`, `cached(url)`: cache lookups (local file paths are used
//!   directly, e.g. embedded covers extracted to a file).
//! - `prefetch(url)`: background download into the cache (deduplicated).
//! - `draw(url, row, col, cols, rows) -> Drawn`: `Drawn.shown` says whether
//!   the image is (now) on screen; `Drawn.bytes` must be written to the
//!   terminal (empty when nothing changed: same url/position/size as the last
//!   draw). Nothing is drawn below 8x4 cells or without protocol support; a
//!   missing download is prefetched and `shown=false`.
//!   `draw_to(writer, ...) -> bool` writes the bytes itself and flushes.
//! - `clear() -> Vec<u8>`: bytes deleting the image; forgets the last key.
//! - `set_cell_size(w, h)`: cell size in pixels (default: TIOCGWINSZ on the
//!   tty, else 8x16) used for aspect-correct fitting.
//! - `encode(path, cols, rows, cell) -> Option<(Vec<u8>, fit_cols, fit_rows)>`:
//!   the kitty escape payload for a file (pure, for tests).

use crate::artwork_cache::ArtworkCache;
use base64::Engine;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type ReadyCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// The image id NPLAY uses for its cover ("NPL").
pub const IMAGE_ID: u32 = 0x004e_504c;
const CHUNK: usize = 4096;

/// Result of `draw`: write `bytes`, `shown` is the 1.x boolean.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Drawn {
    pub shown: bool,
    pub bytes: Vec<u8>,
}

struct State {
    last: String,
    pending: HashSet<String>,
    cell: Option<(u32, u32)>,
    kitty: Option<bool>,
}

struct Inner {
    cache: ArtworkCache,
    on_ready: Option<ReadyCallback>,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct Artwork {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Terminal supports the kitty graphics protocol (kitty or NIRUTERM).
pub fn kitty_supported() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    !env("KITTY_WINDOW_ID").is_empty()
        || env("TERM") == "xterm-kitty"
        || env("TERM_PROGRAM").eq_ignore_ascii_case("niruterm")
}

/// Cell size in pixels from the terminal (TIOCGWINSZ), if it reports one.
pub fn terminal_cell_size() -> Option<(u32, u32)> {
    let probe = |fd: i32| -> Option<(u32, u32)> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
            return None;
        }
        if ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
            return None;
        }
        Some((
            (ws.ws_xpixel / ws.ws_col).max(1) as u32,
            (ws.ws_ypixel / ws.ws_row).max(1) as u32,
        ))
    };
    probe(1).or_else(|| probe(0)).or_else(|| {
        use std::os::fd::AsRawFd;
        let f = std::fs::File::open("/dev/tty").ok()?;
        probe(f.as_raw_fd())
    })
}

/// Escape bytes deleting NPLAY's image.
pub fn delete_bytes() -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={IMAGE_ID},q=2\x1b\\").into_bytes()
}

/// Kitty graphics escape bytes for the image at `path`, fitted to
/// `cols` x `rows` cells of `cell` pixels; also the fitted cell size.
/// The sequence assumes the cursor already sits at the box's top-left cell;
/// horizontal centring is done by the caller (`draw`).
pub fn encode(path: &Path, cols: u32, rows: u32, cell: (u32, u32)) -> Option<(Vec<u8>, u32, u32)> {
    let data = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&data).ok()?;
    let (iw, ih) = (img.width().max(1) as f64, img.height().max(1) as f64);
    let (cw, ch) = (cell.0.max(1) as f64, cell.1.max(1) as f64);
    let (bw, bh) = (cols as f64 * cw, rows as f64 * ch);
    let scale = (bw / iw).min(bh / ih);
    let (pw, ph) = ((iw * scale).max(1.0), (ih * scale).max(1.0));
    let fit_cols = ((pw / cw).round() as u32).clamp(1, cols);
    let fit_rows = ((ph / ch).round() as u32).clamp(1, rows);
    // Never send more pixels than the box shows (bounded memory and payload).
    let px_w = (pw.round() as u32).clamp(1, 4096);
    let px_h = (ph.round() as u32).clamp(1, 4096);
    let scaled = img
        .resize_exact(px_w, px_h, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let mut png = Vec::new();
    {
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                scaled.as_raw(),
                scaled.width(),
                scaled.height(),
                image::ExtendedColorType::Rgba8,
            )
            .ok()?;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let bytes = b64.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / CHUNK * 24 + 96);
    let mut chunks = bytes.chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(c) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(
                out,
                "\x1b_Ga=T,f=100,t=d,i={IMAGE_ID},q=2,C=1,c={fit_cols},r={fit_rows},m={more};"
            )
            .ok()?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};").ok()?;
        }
        out.extend_from_slice(c);
        out.extend_from_slice(b"\x1b\\");
    }
    Some((out, fit_cols, fit_rows))
}

impl Artwork {
    pub fn new(on_ready: Option<ReadyCallback>, cache: Option<ArtworkCache>) -> Artwork {
        Artwork {
            inner: Arc::new(Inner {
                cache: cache.unwrap_or_else(|| ArtworkCache::new(500)),
                on_ready,
                state: Mutex::new(State {
                    last: String::new(),
                    pending: HashSet::new(),
                    cell: None,
                    kitty: None,
                }),
            }),
        }
    }

    pub fn cache(&self) -> &ArtworkCache {
        &self.inner.cache
    }

    pub fn kitty(&self) -> bool {
        lock(&self.inner.state)
            .kitty
            .unwrap_or_else(kitty_supported)
    }

    pub fn set_kitty(&self, forced: Option<bool>) {
        lock(&self.inner.state).kitty = forced;
    }

    pub fn set_cell_size(&self, w: u32, h: u32) {
        lock(&self.inner.state).cell = Some((w.max(1), h.max(1)));
    }

    fn cell(&self) -> (u32, u32) {
        let c = lock(&self.inner.state).cell;
        c.or_else(terminal_cell_size).unwrap_or((8, 16))
    }

    pub fn path(&self, url: &str) -> Option<PathBuf> {
        self.inner.cache.path(url)
    }

    pub fn cached(&self, url: &str) -> Option<PathBuf> {
        self.inner.cache.cached(url)
    }

    pub fn last_key(&self) -> String {
        lock(&self.inner.state).last.clone()
    }

    pub fn prefetch(&self, url: &str) {
        if url.is_empty()
            || !(url.starts_with("http://") || url.starts_with("https://"))
            || self.cached(url).is_some()
        {
            return;
        }
        if !lock(&self.inner.state).pending.insert(url.to_string()) {
            return;
        }
        let me = self.clone();
        let url = url.to_string();
        let spawned = std::thread::Builder::new()
            .name("nplay-artwork".into())
            .spawn(move || {
                me.inner.cache.fetch_default(&url);
                lock(&me.inner.state).pending.remove(&url);
                if let Some(cb) = &me.inner.on_ready {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(&url)));
                }
            });
        if let Err(e) = spawned {
            // Forget it so a later draw can retry.
            warn!("artwork prefetch thread failed: {e}");
            lock(&self.inner.state).pending.clear();
        }
    }

    pub fn clear(&self) -> Vec<u8> {
        let kitty = self.kitty();
        lock(&self.inner.state).last.clear();
        if kitty {
            delete_bytes()
        } else {
            Vec::new()
        }
    }

    pub fn draw(&self, url: &str, row: u32, col: u32, cols: u32, rows: u32) -> Drawn {
        if !self.kitty() || url.is_empty() || cols < 8 || rows < 4 {
            return Drawn::default();
        }
        let Some(p) = self.cached(url) else {
            self.prefetch(url);
            return Drawn::default();
        };
        let key = format!("{url}:{row}:{col}:{cols}:{rows}");
        let last = self.last_key();
        if key == last {
            return Drawn {
                shown: true,
                bytes: Vec::new(),
            };
        }
        let mut out = Vec::new();
        if !last.is_empty() {
            out.extend(self.clear());
        }
        let Some((img, fit_cols, _)) = encode(&p, cols, rows, self.cell()) else {
            return Drawn {
                shown: false,
                bytes: out,
            };
        };
        let x = col + (cols - fit_cols) / 2;
        out.extend_from_slice(b"\x1b7");
        let _ = write!(out, "\x1b[{};{}H", row + 1, x + 1);
        out.extend_from_slice(&img);
        out.extend_from_slice(b"\x1b8");
        lock(&self.inner.state).last = key;
        Drawn {
            shown: true,
            bytes: out,
        }
    }

    pub fn draw_to(
        &self,
        w: &mut dyn Write,
        url: &str,
        row: u32,
        col: u32,
        cols: u32,
        rows: u32,
    ) -> bool {
        let d = self.draw(url, row, col, cols, rows);
        if !d.bytes.is_empty() && (w.write_all(&d.bytes).is_err() || w.flush().is_err()) {
            lock(&self.inner.state).last.clear();
            return false;
        }
        d.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "nplay-art-{name}-{}-{:x}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png(path: &Path, w: u32, h: u32) {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 99]));
        img.save(path).unwrap();
    }

    /// Parse our own output with the same rules NIRUTERM applies.
    fn commands(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 2 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'_' && bytes[i + 2] == b'G' {
                let start = i + 3;
                let end = (start..bytes.len() - 1)
                    .find(|&j| bytes[j] == 0x1b && bytes[j + 1] == b'\\')
                    .unwrap();
                let raw = &bytes[start..end];
                let semi = raw.iter().position(|b| *b == b';').unwrap_or(raw.len());
                out.push((
                    String::from_utf8_lossy(&raw[..semi]).into_owned(),
                    raw.get(semi + 1..).unwrap_or(&[]).to_vec(),
                ));
                i = end + 2;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn encode_fits_aspect_and_chunks() {
        let d = tmpdir("enc");
        let p = d.join("wide.png");
        png(&p, 400, 200);
        let (bytes, c, r) = encode(&p, 20, 20, (10, 20)).unwrap();
        // 200x400 px box, 2:1 image -> 200x100 px = 20 cols x 5 rows.
        assert_eq!((c, r), (20, 5));
        let cmds = commands(&bytes);
        assert!(cmds[0].0.contains("a=T,f=100,t=d"));
        assert!(cmds[0].0.contains("c=20,r=5"));
        assert!(cmds[0].0.contains(&format!("i={IMAGE_ID}")));
        assert!(cmds[0].0.contains("q=2"));
        assert!(cmds.last().unwrap().0.ends_with("m=0"));
        let mut all = Vec::new();
        for (i, (_, payload)) in cmds.iter().enumerate() {
            assert!(payload.len() <= CHUNK);
            if i + 1 < cmds.len() {
                assert_eq!(payload.len() % 4, 0);
            }
            all.extend_from_slice(payload);
        }
        let png_bytes = base64::engine::general_purpose::STANDARD
            .decode(&all)
            .unwrap();
        let back = image::load_from_memory(&png_bytes).unwrap();
        assert_eq!((back.width(), back.height()), (200, 100));
        // Tall image in a wide box: limited by rows, scaled up.
        let t = d.join("tall.jpg");
        image::RgbImage::from_pixel(10, 40, image::Rgb([1, 2, 3]))
            .save(&t)
            .unwrap();
        let (_, c, r) = encode(&t, 30, 8, (8, 16)).unwrap();
        assert_eq!((c, r), (4, 8));
        assert!(encode(&d.join("missing.png"), 10, 10, (8, 16)).is_none());
        let junk = d.join("junk.png");
        std::fs::write(&junk, b"not an image").unwrap();
        assert!(encode(&junk, 10, 10, (8, 16)).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn draw_skips_unchanged_and_clears_on_change() {
        let d = tmpdir("draw");
        let cover = d.join("cover.png");
        png(&cover, 64, 64);
        let art = Artwork::new(None, Some(ArtworkCache::with_root(d.join("cache"), 500)));
        art.set_kitty(Some(true));
        art.set_cell_size(8, 16);
        let url = cover.to_str().unwrap();
        assert_eq!(art.draw(url, 2, 4, 7, 10), Drawn::default(), "too narrow");
        assert_eq!(art.draw(url, 2, 4, 10, 3), Drawn::default(), "too short");
        let first = art.draw(url, 2, 4, 20, 10);
        assert!(first.shown);
        assert!(first.bytes.starts_with(b"\x1b7\x1b["));
        assert!(first.bytes.ends_with(b"\x1b8"));
        // 160x160 px box: square image -> 20 cols x 10 rows; no offset.
        assert!(String::from_utf8_lossy(&first.bytes).starts_with("\x1b7\x1b[3;5H"));
        let again = art.draw(url, 2, 4, 20, 10);
        assert!(again.shown && again.bytes.is_empty());
        let moved = art.draw(url, 2, 4, 40, 10);
        assert!(moved.shown);
        assert!(moved.bytes.starts_with(&delete_bytes()));
        // Square image, 320x160 box -> 20 cols centred in 40: column 4+10.
        assert!(String::from_utf8_lossy(&moved.bytes).contains("\x1b[3;15H"));
        assert_eq!(art.clear(), delete_bytes());
        assert!(art.last_key().is_empty());
        let mut sink = Vec::new();
        assert!(art.draw_to(&mut sink, url, 0, 0, 20, 10));
        assert!(!sink.is_empty());
        art.set_kitty(Some(false));
        assert_eq!(art.draw(url, 0, 0, 30, 10), Drawn::default());
        assert!(art.clear().is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn uncached_url_prefetches_and_reports_ready() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let tx = Mutex::new(tx);
        let d = tmpdir("pre");
        let art = Artwork::new(
            Some(Arc::new(move |u: &str| {
                let _ = lock(&tx).send(u.to_string());
            })),
            Some(ArtworkCache::with_root(d.clone(), 500)),
        );
        art.set_kitty(Some(true));
        let url = "http://127.0.0.1:9/nothing.png";
        assert!(!art.draw(url, 0, 0, 20, 10).shown);
        let got = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(got, url);
        assert!(art.cached(url).is_none());
        let _ = std::fs::remove_dir_all(d);
    }
}
