use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("nplay-db-test-{:016x}", fastrand::u64(..)));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(t: &Tmp) -> DB {
    DB::open_in(&t.0.join("library.db"), &t.0.join("data")).unwrap()
}

/// A tiny but valid FLAC header with Vorbis comments and an optional picture.
fn flac_bytes(comments: &[&str], picture: Option<(&str, &[u8])>) -> Vec<u8> {
    let mut out = b"fLaC".to_vec();
    let block = |out: &mut Vec<u8>, kind: u8, last: bool, body: &[u8]| {
        out.push(kind | if last { 0x80 } else { 0 });
        let n = body.len() as u32;
        out.extend_from_slice(&n.to_be_bytes()[1..]);
        out.extend_from_slice(body);
    };
    let mut si = Vec::new();
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let total: u64 = 44100 * 3;
    let v: u64 = (44100u64 << 44) | (1u64 << 41) | (15u64 << 36) | total;
    si.extend_from_slice(&v.to_be_bytes());
    si.extend_from_slice(&[0u8; 16]);
    block(&mut out, 0, false, &si);
    let mut vc = Vec::new();
    let vendor = b"nplay-test";
    vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    vc.extend_from_slice(vendor);
    vc.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
        vc.extend_from_slice(c.as_bytes());
    }
    block(&mut out, 4, picture.is_none(), &vc);
    if let Some((mime, data)) = picture {
        let mut p = Vec::new();
        p.extend_from_slice(&3u32.to_be_bytes());
        p.extend_from_slice(&(mime.len() as u32).to_be_bytes());
        p.extend_from_slice(mime.as_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for _ in 0..4 {
            p.extend_from_slice(&0u32.to_be_bytes());
        }
        p.extend_from_slice(&(data.len() as u32).to_be_bytes());
        p.extend_from_slice(data);
        block(&mut out, 6, true, &p);
    }
    // A few bytes of "audio" so the stream is not empty.
    out.extend_from_slice(&[0xff, 0xf8, 0, 0, 0, 0, 0, 0]);
    out
}

fn tr(source: &str, id: &str, title: &str) -> Track {
    let mut t = Track::new(source, id, title);
    t.artist = "Artist".into();
    t
}

#[test]
fn fresh_library_schema_and_no_backups() {
    let t = Tmp::new();
    let db = open(&t);
    assert_eq!(db.schema_version().unwrap(), 5);
    assert_eq!(db.count().unwrap(), 0);
    for b in [
        "library.pre-1.2.0.db",
        "library.pre-1.1.0.db",
        "library.pre-0.4.3.db",
    ] {
        assert!(!t.0.join(b).exists(), "{b}");
    }
    let mode: String = db
        .read(|c| c.query_row("PRAGMA journal_mode", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(mode, "wal");
    // Reopen is idempotent.
    drop(db);
    let db = open(&t);
    assert_eq!(db.schema_version().unwrap(), 5);
}

/// A library exactly as Python NPLAY 1.8.2 creates it (statements from db.py).
fn python_182_library(path: &Path) {
    let c = Connection::open(path).unwrap();
    c.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .unwrap();
    c.execute_batch(SCHEMA).unwrap();
    for (col, typ) in NEW_COLUMNS {
        c.execute_batch(&format!("ALTER TABLE tracks ADD COLUMN {col} {typ}"))
            .unwrap();
    }
    c.execute_batch(PLAY_EVENTS).unwrap();
    c.execute_batch("PRAGMA user_version=5").unwrap();
    // Rows in Python's json.dumps format (", " and ": " separators).
    let tj = r#"{"source": "navidrome", "id": "nav1", "title": "Havenless", "artist": "Enslaved", "album": "Below the Lights", "duration": 300.5, "url": "", "cover": "", "path": "", "kind": "track", "seekable": true, "meta": {"x": 1}}"#;
    c.execute(
        "INSERT INTO tracks(id,path,title,artist,album,mtime,added,album_artist,track_no,disc_no,year,genre,duration,codec,sample_rate,bit_depth,channels,bitrate) VALUES('abc','/nowhere/Album/01 a.flac','A','Art','Album',1700000000.5,1700000001.0,'AA',1,1,1999,'Metal',201.5,'flac',44100,16,2,900)",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO favorites VALUES('navidrome:nav1',?,1.0)", [tj])
        .unwrap();
    c.execute(
        "INSERT INTO favorites VALUES('legacyid',?,0.5)",
        [tj.replace("nav1", "legacyid")],
    )
    .unwrap();
    c.execute(
        "INSERT INTO history(item_id,data,played) VALUES('navidrome:nav1',?,2.0)",
        [tj],
    )
    .unwrap();
    c.execute("INSERT INTO resume VALUES('legacy',42.5,1.0)", [])
        .unwrap();
    c.execute("INSERT INTO playlists(name,created) VALUES('Road',1.0)", [])
        .unwrap();
    c.execute(
        "INSERT INTO playlist_items(playlist_id,data,position,added) VALUES(1,?,0,1.0)",
        [tj],
    )
    .unwrap();
    c.execute(
        "INSERT INTO radio_stations(name,url,homepage,artwork,added) VALUES('P2','https://x/p2',NULL,NULL,1.0)",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO ratings VALUES('local:abc',5,1.0)", [])
        .unwrap();
    c.execute("INSERT INTO discoveries(title,artist,source,data,added) VALUES('Song','Band','radio','{\"station\": \"P2\"}',3.0)", []).unwrap();
    c.execute(
        "INSERT INTO bookmarks(item_key,data,pos,added) VALUES('navidrome:nav1',?,12.0,4.0)",
        [tj],
    )
    .unwrap();
}

#[test]
fn python_182_library_opens_unchanged() {
    let t = Tmp::new();
    let path = t.0.join("library.db");
    python_182_library(&path);
    let db = open(&t);
    assert_eq!(db.schema_version().unwrap(), 5);
    assert!(!t.0.join("library.pre-1.2.0.db").exists());
    let all = db.search("", 500).unwrap();
    assert_eq!(all.len(), 1);
    let a = &all[0];
    assert_eq!(
        (a.source.as_str(), a.id.as_str(), a.title.as_str()),
        ("local", "abc", "A")
    );
    assert_eq!(a.duration, 201.5);
    assert_eq!(a.url, "/nowhere/Album/01 a.flac");
    let keys: Vec<&str> = a.meta.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "suffix",
            "album_artist",
            "track_no",
            "disc_no",
            "year",
            "genre",
            "codec",
            "sample_rate",
            "bit_depth",
            "channels",
            "bitrate"
        ]
    );
    assert_eq!(a.meta["year"], json!(1999));
    assert_eq!(a.meta["suffix"], json!("flac"));
    assert_eq!(db.favorites().unwrap().len(), 2);
    // Legacy favorite id without source prefix toggles off.
    let mut legacy = tr("navidrome", "legacyid", "x");
    legacy.artist.clear();
    assert!(!db.favorite(&legacy).unwrap());
    assert_eq!(db.favorites().unwrap().len(), 1);
    assert_eq!(db.history(10).unwrap()[0].title, "Havenless");
    assert_eq!(db.resume_get("local:new", Some("legacy")).unwrap(), 42.5);
    assert_eq!(
        db.playlists().unwrap(),
        vec![("Road".into(), "playlist:1".into(), "1 tracks".into())]
    );
    assert_eq!(db.playlist_tracks(1).unwrap()[0].meta["x"], json!(1));
    let radios = db.radios().unwrap();
    assert_eq!(radios[0].id, "custom:1");
    assert_eq!(radios[0].meta["homepage"], json!(""));
    assert_eq!(db.rating_get("abc", "local").unwrap(), 5);
    assert_eq!(db.rated(4, 10).unwrap().len(), 1);
    let d = db.discoveries(10).unwrap();
    assert_eq!(d[0].meta["origin"], json!("radio"));
    assert_eq!(d[0].meta["station"], json!("P2"));
    assert_eq!(d[0].id, "discovery:1");
    let b = db.bookmarks(10).unwrap();
    assert_eq!(b[0].meta["bookmark_pos"], json!(12.0));
    assert_eq!(db.most_played(10).unwrap()[0].meta["play_count"], json!(1));
    // Python can still read what Rust writes (plain JSON in the same columns).
    db.history_add(&tr("spotify", "s1", "New")).unwrap();
    let raw: String = db
        .read(|c| {
            c.query_row(
                "SELECT data FROM history ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    let v: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["source"], json!("spotify"));
}

#[test]
fn old_schema_migrates_additively_with_backups() {
    let t = Tmp::new();
    let path = t.0.join("library.db");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE tracks(id TEXT PRIMARY KEY,path TEXT UNIQUE,title TEXT,artist TEXT,album TEXT,mtime REAL); CREATE TABLE favorites(id TEXT PRIMARY KEY,data TEXT,added REAL);").unwrap();
        c.execute(
            "INSERT INTO tracks VALUES('x','/m/a.mp3','T','A','Al',123.0)",
            [],
        )
        .unwrap();
    }
    let db = open(&t);
    assert_eq!(db.schema_version().unwrap(), 5);
    for b in [
        "library.pre-1.2.0.db",
        "library.pre-1.1.0.db",
        "library.pre-0.4.3.db",
    ] {
        assert!(t.0.join(b).exists(), "{b}");
    }
    let (added, codec): (f64, Option<String>) = db
        .read(|c| {
            c.query_row("SELECT added,codec FROM tracks", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        })
        .unwrap();
    assert_eq!(added, 123.0);
    assert_eq!(codec, None);
    let tr = &db.search("T", 10).unwrap()[0];
    assert_eq!(tr.meta["codec"], json!(""));
    assert_eq!(tr.meta["track_no"], json!(0));
    // The backup is the untouched old library.
    let old = Connection::open(t.0.join("library.pre-0.4.3.db")).unwrap();
    let n: i64 = old
        .query_row(
            "SELECT count(*) FROM pragma_table_info('tracks')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 6);
}

#[test]
fn scan_incremental_offline_and_excludes() {
    let t = Tmp::new();
    let music = t.0.join("Music");
    let sub = music.join("Band").join("Album");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(music.join("loose.mp3"), b"junk").unwrap();
    std::fs::write(
        sub.join("01 x.flac"),
        flac_bytes(
            &[
                "TITLE=Song One",
                "ARTIST=Band",
                "ALBUM=Album",
                "TRACKNUMBER=1/9",
                "DATE=2004-01-02",
                "ALBUMARTIST=The Band",
                "GENRE=Rock",
            ],
            None,
        ),
    )
    .unwrap();
    std::fs::write(sub.join("notes.txt"), b"x").unwrap();
    for d in [".hidden", "node_modules", "Reaper-projects"] {
        std::fs::create_dir_all(music.join(d)).unwrap();
        std::fs::write(music.join(d).join("skip.mp3"), b"x").unwrap();
    }
    std::fs::write(music.join("skip-me.ogg"), b"x").unwrap();
    // Nested dir symlink is not followed; file symlink duplicates are deduped.
    let outside = t.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("o.mp3"), b"x").unwrap();
    std::os::unix::fs::symlink(&outside, music.join("linkdir")).unwrap();
    std::os::unix::fs::symlink(music.join("loose.mp3"), music.join("dup.mp3")).unwrap();
    // A configured root that is itself a symlink is resolved and followed.
    let rootlink = t.0.join("rootlink");
    std::os::unix::fs::symlink(&music, &rootlink).unwrap();

    let db = open(&t);
    let calls = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(Mutex::new(Map::new()));
    let (c2, d2) = (calls.clone(), done.clone());
    let progress: ProgressFn = Arc::new(move |m: &Map<String, Value>| {
        c2.fetch_add(1, Ordering::SeqCst);
        if m.get("done").is_some() {
            *d2.lock().unwrap() = m.clone();
        }
    });
    let roots = vec![rootlink.to_string_lossy().to_string()];
    let ex = vec!["Reaper-projects".to_string(), "skip-*".to_string()];
    let r = db.scan(&roots, Some(progress), &ex, 250, false).unwrap();
    assert_eq!(
        (r.checked, r.new, r.updated, r.removed, r.total),
        (2, 2, 0, 0, 2)
    );
    assert_eq!(
        r.roots,
        vec![std::fs::canonicalize(&music)
            .unwrap()
            .to_string_lossy()
            .to_string()]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(done.lock().unwrap()["done"], json!(true));
    assert_eq!(db.last_scan()["total"], json!(2));
    let keys: Vec<String> = r.to_map().keys().cloned().collect();
    assert_eq!(
        keys,
        ["checked", "new", "updated", "removed", "total", "offline", "roots"]
    );

    let songs = db.search("Song", 10).unwrap();
    assert_eq!(songs.len(), 1);
    let s = &songs[0];
    assert_eq!(s.artist, "Band");
    assert_eq!(s.meta["album_artist"], json!("The Band"));
    assert_eq!(s.meta["track_no"], json!(1));
    assert_eq!(s.meta["year"], json!(2004));
    assert_eq!(s.meta["genre"], json!("Rock"));
    assert_eq!(s.meta["codec"], json!("flac"));
    assert_eq!(s.meta["sample_rate"], json!(44100));
    assert_eq!(s.meta["bit_depth"], json!(16));
    assert_eq!(s.meta["channels"], json!(2));
    assert!((s.duration - 3.0).abs() < 0.01);
    let real = std::fs::canonicalize(sub.join("01 x.flac")).unwrap();
    assert_eq!(s.id, sha1::sha1_hex(real.to_string_lossy().as_bytes()));
    // loose.mp3 and its symlink dup.mp3 are one track (named after whichever came first).
    let loose = &db.search("Music", 10).unwrap()[0];
    assert_eq!(loose.album, "Music");
    assert_eq!(loose.meta["codec"], json!("mp3"));

    // Unchanged: nothing new or updated.
    let r = db.scan(&roots, None, &ex, 250, false).unwrap();
    assert_eq!((r.checked, r.new, r.updated, r.removed), (2, 0, 0, 0));
    // Force: full rescan keeps ids and the added stamp.
    let added_before: f64 = db
        .read(|c| {
            c.query_row("SELECT added FROM tracks WHERE title='Song One'", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    let r = db.scan(&roots, None, &ex, 250, true).unwrap();
    assert_eq!((r.new, r.updated, r.total), (0, 2, 2));
    let added_after: f64 = db
        .read(|c| {
            c.query_row("SELECT added FROM tracks WHERE title='Song One'", [], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(added_before, added_after);
    // Changed mtime -> updated.
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(music.join("loose.mp3"))
        .unwrap();
    f.set_modified(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000),
    )
    .unwrap();
    let r = db.scan(&roots, None, &ex, 250, false).unwrap();
    assert_eq!((r.new, r.updated), (0, 1));
    // Offline root never removes anything.
    let missing = vec![t.0.join("unplugged").to_string_lossy().to_string()];
    let r = db.scan(&missing, None, &[], 250, false).unwrap();
    assert_eq!((r.removed, r.total), (0, 2));
    assert_eq!(r.offline, missing);
    // A deleted file inside an online root is removed.
    std::fs::remove_file(music.join("dup.mp3")).unwrap();
    std::fs::remove_file(music.join("loose.mp3")).unwrap();
    let r = db.scan(&roots, None, &ex, 250, false).unwrap();
    assert_eq!((r.removed, r.total), (1, 1));
}

#[test]
fn covers_folder_wins_then_embedded_generation() {
    let t = Tmp::new();
    let album = t.0.join("Music").join("A");
    std::fs::create_dir_all(&album).unwrap();
    let f = album.join("t.flac");
    let png = b"\x89PNG\r\n\x1a\nfakepng";
    std::fs::write(&f, flac_bytes(&["TITLE=T"], Some(("image/png", png)))).unwrap();
    let db = open(&t);
    db.scan(
        &[t.0.join("Music").to_string_lossy().to_string()],
        None,
        &[],
        250,
        false,
    )
    .unwrap();
    let cover = db.search("", 10).unwrap()[0].cover.clone();
    let cache = t.0.join("data").join("artwork").join("embedded");
    assert!(
        cover.starts_with(&cache.to_string_lossy().to_string()),
        "{cover}"
    );
    assert!(cover.ends_with(".png"));
    assert_eq!(std::fs::read(&cover).unwrap(), png);
    let real = std::fs::canonicalize(&f).unwrap();
    let base = tags::sha256_hex(&real.to_string_lossy());
    let m = std::fs::metadata(&f).unwrap();
    let stamp = m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128;
    assert_eq!(
        Path::new(&cover).file_name().unwrap().to_string_lossy(),
        format!("{base}-{stamp}.png")
    );
    // A new generation replaces the old file.
    let fh = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
    fh.set_modified(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000),
    )
    .unwrap();
    let cover2 = db.search("", 10).unwrap()[0].cover.clone();
    assert_ne!(cover, cover2);
    assert!(!Path::new(&cover).exists());
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
    // An explicit folder cover wins.
    std::fs::write(album.join("folder.jpg"), b"jpg").unwrap();
    let cover3 = db.search("", 10).unwrap()[0].cover.clone();
    assert_eq!(cover3, album.join("folder.jpg").to_string_lossy());
}

#[allow(clippy::too_many_arguments)]
fn insert(
    db: &DB,
    id: &str,
    path: &str,
    title: &str,
    artist: &str,
    album: &str,
    aa: &str,
    year: i64,
    no: i64,
) {
    db.write(|c| {
        c.execute(
            "INSERT INTO tracks(id,path,title,artist,album,mtime,added,album_artist,track_no,disc_no,year) VALUES(?,?,?,?,?,1,?,?,?,1,?)",
            params![id, path, title, artist, album, paths::now(), aa, no, year],
        )
    })
    .unwrap();
}

#[test]
fn browse_queries() {
    let t = Tmp::new();
    let db = open(&t);
    insert(&db, "1", "/m/b/x/2.mp3", "Two", "B", "X", "", 2001, 2);
    insert(&db, "2", "/m/b/x/1.mp3", "One", "B", "X", "", 2001, 1);
    insert(
        &db,
        "3",
        "/m/a/y/1.mp3",
        "Feat",
        "Guest",
        "Y",
        "a band",
        1990,
        1,
    );
    insert(&db, "4", "/m/c/z.mp3", "Loose", "", "", "", 0, 0);
    let s: Vec<String> = db
        .search("", 10)
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(s, ["Loose", "Feat", "One", "Two"]);
    assert_eq!(db.search("on", 10).unwrap().len(), 1);
    assert_eq!(db.search("o", 10).unwrap().len(), 3);
    assert_eq!(
        db.find_artists("", 10).unwrap(),
        vec![("a band".into(), 1), ("B".into(), 2)]
    );
    assert_eq!(
        db.find_artists("BAN", 10).unwrap(),
        vec![("a band".into(), 1)]
    );
    assert_eq!(
        db.local_artists().unwrap()[1],
        ("B".into(), "local:artist:B".into(), "2 tracks".into())
    );
    assert_eq!(
        db.local_albums().unwrap(),
        vec![
            (
                "Y".into(),
                "local:album:a band\tY".into(),
                "a band · 1 tracks".into()
            ),
            ("X".into(), "local:album:B\tX".into(), "B · 2 tracks".into()),
        ]
    );
    let folders = db.local_folders().unwrap();
    assert_eq!(
        folders[0],
        (
            "y".into(),
            "local:folder:/m/a/y".into(),
            "1 tracks · /m/a/y".into()
        )
    );
    assert_eq!(folders.len(), 3);
    let by: Vec<String> = db
        .tracks_by_artist("B")
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(by, ["One", "Two"]);
    assert_eq!(db.tracks_by_album("a band", "Y").unwrap().len(), 1);
    assert_eq!(db.tracks_by_folder("/m/b/").unwrap().len(), 2);
    assert_eq!(db.random_tracks(2).unwrap().len(), 2);
    let (a, ts) = db.random_artist_tracks().unwrap();
    assert!(!a.is_empty() && !ts.is_empty());
    assert!(!db.random_album_tracks().unwrap().is_empty());
    assert_eq!(db.shuffled_artist_tracks("B").unwrap().len(), 2);
    assert_eq!(db.recently_added(1).unwrap().len(), 1);
    // Played tracks drop out of never_played/unplayed_albums (both id forms).
    let mut one = Track::new("local", "2", "One");
    db.history_add(&one).unwrap();
    one.id = "1".into();
    db.write(|c| {
        c.execute(
            "INSERT INTO history(item_id,data,played) VALUES('1',?,1)",
            [track_json(&one)],
        )
    })
    .unwrap();
    let never: Vec<String> = db
        .never_played(10)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(never, ["4", "3"]);
    let un = db.unplayed_albums(10).unwrap();
    assert_eq!(
        un,
        vec![(
            "Y".into(),
            "local:album:Guest\tY".into(),
            "Guest · 1 tracks · unplayed".into()
        )]
    );
}

#[test]
fn playlists_ratings_resume_bookmarks_discoveries_radio() {
    let t = Tmp::new();
    let db = open(&t);
    assert_eq!(db.playlist_create("  ").unwrap(), None);
    let pid = db.playlist_create(" Mix ").unwrap().unwrap();
    assert_eq!(db.playlist_create("mix").unwrap(), Some(pid));
    for i in 0..3 {
        db.playlist_add(pid, &tr("local", &i.to_string(), &format!("T{i}")))
            .unwrap();
    }
    let titles = |db: &DB| -> Vec<String> {
        db.playlist_tracks(pid)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
    };
    assert_eq!(titles(&db), ["T0", "T1", "T2"]);
    assert_eq!(db.playlist_move(pid, 0, 1).unwrap(), 1);
    assert_eq!(titles(&db), ["T1", "T0", "T2"]);
    assert_eq!(db.playlist_move(pid, 2, 5).unwrap(), 2);
    assert_eq!(db.playlist_move(pid, 9, 1).unwrap(), 9);
    db.playlist_remove(pid, 0).unwrap();
    db.playlist_remove(pid, 7).unwrap();
    assert_eq!(titles(&db), ["T0", "T2"]);
    db.playlist_rename(pid, "Road").unwrap();
    assert_eq!(db.playlist_name(pid).unwrap(), "Road");
    let other = db.playlist_create("Other").unwrap().unwrap();
    assert!(db.playlist_rename(other, "road").is_err());
    assert_eq!(db.playlist_name(999).unwrap(), "Playlist");
    db.playlist_delete(pid).unwrap();
    assert_eq!(db.playlists().unwrap().len(), 1);

    db.rating_set("abc", 9, "local").unwrap();
    assert_eq!(db.rating_get("abc", "local").unwrap(), 5);
    assert_eq!(db.rating_get("local:abc", "x").unwrap(), 5);
    db.rating_set("abc", 0, "local").unwrap();
    assert_eq!(db.rating_get("abc", "local").unwrap(), 0);

    db.resume_set("", 3.0).unwrap();
    db.resume_set("k", -5.0).unwrap();
    assert_eq!(db.resume_get("k", None).unwrap(), 0.0);
    db.resume_set("k", 7.5).unwrap();
    assert_eq!(db.resume_get("k", None).unwrap(), 7.5);
    assert_eq!(db.resume_get("", None).unwrap(), 0.0);

    let tk = tr("podcast", "ep", "Ep");
    let bid = db.bookmark_add(&tk, 33.0).unwrap();
    let b = db.bookmarks(10).unwrap();
    assert_eq!(b[0].meta["bookmark_id"], json!(bid));
    db.bookmark_delete(bid).unwrap();
    assert!(db.bookmarks(10).unwrap().is_empty());

    assert!(!db.discovery_add("  ", "", "radio", None).unwrap());
    let mut extra = Map::new();
    extra.insert("station".into(), json!("Ö"));
    assert!(db
        .discovery_add(" Song ", " A ", "radio", Some(&extra))
        .unwrap());
    let d = &db.discoveries(10).unwrap()[0];
    assert_eq!(
        (d.title.as_str(), d.artist.as_str(), d.kind.as_str()),
        ("Song", "A", "discovery")
    );
    assert!(!d.seekable);
    assert_eq!(d.meta["station"], json!("Ö"));

    assert!(db.radio_add("x", "ftp://bad", "", "").is_err());
    assert_eq!(
        db.radio_add(" ", "https://ok", "", "").unwrap_err().message,
        "Station needs a name and an http(s) stream URL"
    );
    db.radio_add(" Jazz ", "https://jazz/stream ", " https://jazz ", "")
        .unwrap();
    let r = &db.radios().unwrap()[0];
    assert_eq!(
        (r.title.as_str(), r.url.as_str(), r.artist.as_str()),
        ("Jazz", "https://jazz/stream", "Custom radio")
    );
    assert_eq!(r.meta["homepage"], json!("https://jazz"));
    let id: i64 = r.id.trim_start_matches("custom:").parse().unwrap();
    db.radio_delete(id).unwrap();
    assert!(db.radios().unwrap().is_empty());

    // Favorites toggle.
    let f = tr("youtube", "v1", "Vid");
    assert!(db.favorite(&f).unwrap());
    assert_eq!(db.favorites().unwrap()[0].title, "Vid");
    assert!(!db.favorite(&f).unwrap());
}

#[test]
fn play_events_and_listening_stats() {
    let t = Tmp::new();
    let db = open(&t);
    let mut a = tr("spotify", "1", "Long");
    a.duration = 300.0;
    let e = db.play_started(&a).unwrap().unwrap();
    db.play_finished(e, 200.0, true).unwrap();
    db.play_finished(e, 10.0, false).unwrap(); // max(seconds) keeps 200
    let mut b = tr("local", "2", "Short");
    b.artist = "Zed".into();
    b.duration = 20.0;
    let e2 = db.play_started(&b).unwrap().unwrap();
    db.play_finished(e2, 11.0, false).unwrap();
    let e3 = db.play_started(&b).unwrap().unwrap();
    db.play_finished(e3, 1.0, false).unwrap();
    db.play_finished(0, 5.0, true).unwrap();
    let s = db.listening_stats(7.0).unwrap();
    assert_eq!(
        (s.plays, s.substantial, s.completed, s.minutes),
        (3, 2, 0, 3)
    );
    assert_eq!(s.artists, vec![("Artist".into(), 1), ("Zed".into(), 1)]);
    assert_eq!(s.sources.len(), 2);
    assert_eq!(s.to_value()["artists"][0], json!(["Artist", 1]));
}

#[test]
fn handle_is_shared_across_threads() {
    fn assert_send_sync<T: Send + Sync + Clone>() {}
    assert_send_sync::<DB>();
    let t = Tmp::new();
    let db = open(&t);
    let hs: Vec<_> = (0..4)
        .map(|i| {
            let db = db.clone();
            std::thread::spawn(move || {
                for j in 0..25 {
                    db.history_add(&tr("local", &format!("{i}-{j}"), "x"))
                        .unwrap();
                    db.resume_set(&format!("{i}"), j as f64).unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(db.history(1000).unwrap().len(), 100);
    // A second handle (like the service and the TUI) sees the same data.
    let db2 = open(&t);
    assert_eq!(db2.resume_get("3", None).unwrap(), 24.0);
}

#[test]
fn config_scan_uses_music_dirs_and_batch() {
    let t = Tmp::new();
    assert_eq!(norm_path("/a//b/"), "/a/b");
    assert_eq!(expanduser("~/Music"), paths::home_dir().join("Music"));
    let db = open(&t);
    // Batch below 25 still works (clamped).
    let music = t.0.join("M");
    std::fs::create_dir_all(&music).unwrap();
    for i in 0..30 {
        std::fs::write(music.join(format!("{i}.mp3")), b"x").unwrap();
    }
    let r = db
        .scan(&[music.to_string_lossy().to_string()], None, &[], 1, false)
        .unwrap();
    assert_eq!((r.new, r.total), (30, 30));
}

/// Cross-check against files created by Python NPLAY itself:
/// `NPLAY_PY_DATA=<dir with library.db> cargo test -- --ignored python_made`.
#[test]
#[ignore]
fn python_made_library_opens() {
    let Some(dir) = std::env::var_os("NPLAY_PY_DATA") else { return };
    let dir = PathBuf::from(dir);
    let db = DB::open_in(&dir.join("library.db"), &dir).unwrap();
    assert_eq!(db.schema_version().unwrap(), 5);
    assert_eq!(db.playlists().unwrap()[0].0, "PyList");
    assert_eq!(db.playlist_tracks(1).unwrap()[0].title, "Py Song");
    assert_eq!(db.favorites().unwrap().len(), 1);
    assert_eq!(db.radios().unwrap()[0].title, "PyRadio");
    assert_eq!(db.resume_get("local:x", None).unwrap(), 12.5);
    assert!(db.count().unwrap() >= 1);
    let st = crate::statistics::StatisticsStore::open(&dir.join("statistics.sqlite3")).unwrap();
    assert_eq!(st.pending_events(10).unwrap().len(), 1);
    db.history_add(&tr("local", "rust", "From Rust")).unwrap();
}

/// Print `tags::metadata` for every file in `NPLAY_TAG_DIR` (manual
/// comparison against 1.x mutagen output).
#[test]
#[ignore]
fn print_metadata_for_dir() {
    let Some(dir) = std::env::var_os("NPLAY_TAG_DIR") else { return };
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        println!("TAGS {:?} {:?}", p.file_name().unwrap(), tags::metadata(&p));
    }
}
