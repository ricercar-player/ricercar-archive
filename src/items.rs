//! Archive records → ricercar items, which files of a recording to play,
//! and whether the DAC takes them.
//!
//! Refs: `c/<collection>` an artist (an etree collection), `i/<identifier>`
//! a recording (shown as an album), `t/<identifier>/<file name>` one of its
//! tracks. Top-level sections use bare words (`popular`, `artists`…).

use serde_json::{Value, json};

use crate::ia;

#[derive(Debug, PartialEq)]
pub enum Ref<'a> {
    Section(&'a str),
    Artist(&'a str),
    Recording(&'a str),
    Track(&'a str, &'a str),
}

/// Archive identifiers: letters, digits, `.`, `-`, `_`, at most 100.
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

pub fn parse_ref(r: &str) -> Option<Ref<'_>> {
    match r.split_once('/') {
        None => {
            (!r.is_empty() && r.bytes().all(|b| b.is_ascii_lowercase())).then_some(Ref::Section(r))
        }
        Some(("c", id)) => identifier(id).then_some(Ref::Artist(id)),
        Some(("i", id)) => identifier(id).then_some(Ref::Recording(id)),
        Some(("t", rest)) => {
            let (id, file) = rest.split_once('/')?;
            let ok = identifier(id)
                && !file.is_empty()
                && file.len() <= 512
                && !file.chars().any(char::is_control)
                && !file.split('/').any(|p| p == ".." || p.is_empty());
            ok.then_some(Ref::Track(id, file))
        }
        _ => None,
    }
}

/// A trimmed string, or the entries of an array joined with ", ".
pub fn text(v: &Value, k: &str) -> Option<String> {
    let s = match v.get(k)? {
        Value::String(s) => s.trim().to_string(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

fn join(parts: &[Option<String>]) -> Option<String> {
    let v: Vec<&str> = parts.iter().flatten().map(String::as_str).collect();
    (!v.is_empty()).then(|| v.join(" · "))
}

/// `1995-07-09`, from `1995-07-09T00:00:00Z` or `1995-07-09`.
fn date(v: &Value) -> Option<String> {
    let d = text(v, "date")?;
    Some(d.split('T').next().unwrap_or(&d).to_string())
}

fn year(v: &Value) -> Option<i64> {
    let y = text(v, "year")
        .or_else(|| date(v))
        .and_then(|d| d.get(..4)?.parse().ok())?;
    (1000..3000).contains(&y).then_some(y)
}

/// "Soldier Field, Chicago, IL".
fn place(v: &Value) -> Option<String> {
    let p: Vec<String> = [text(v, "venue"), text(v, "coverage")]
        .into_iter()
        .flatten()
        .collect();
    (!p.is_empty()).then(|| p.join(", "))
}

fn finish(mut it: Value) -> Value {
    if let Some(o) = it.as_object_mut() {
        o.retain(|_, v| !v.is_null());
    }
    it
}

/// A recording, from a search document or an item's `metadata`.
pub fn recording(v: &Value) -> Option<Value> {
    let id = text(v, "identifier").filter(|i| identifier(i))?;
    let title = text(v, "title").unwrap_or_else(|| id.clone());
    let artist = text(v, "creator");
    Some(finish(json!({
        "ref": format!("i/{id}"),
        "kind": "album",
        "title": title,
        "subtitle": join(&[artist.clone(), date(v), place(v)]),
        "artist": artist,
        "album": title,
        "year": year(v),
        "art": ia::image_url(&id),
        "browsable": true,
    })))
}

/// An artist: an etree collection.
pub fn artist(v: &Value, fr: bool) -> Option<Value> {
    let id = text(v, "identifier").filter(|i| identifier(i))?;
    let name = text(v, "title").unwrap_or_else(|| id.clone());
    let count = v["item_count"]
        .as_u64()
        .filter(|n| *n > 0)
        .map(|n| match (fr, n) {
            (false, 1) => "1 recording".to_string(),
            (false, n) => format!("{n} recordings"),
            (true, 1) => "1 enregistrement".to_string(),
            (true, n) => format!("{n} enregistrements"),
        });
    Some(finish(json!({
        "ref": format!("c/{id}"),
        "kind": "artist",
        "title": name,
        "subtitle": count,
        "artist": name,
        "art": ia::image_url(&id),
        "browsable": true,
    })))
}

/// How a recording's files come: the originals when they are lossless and
/// public, else the Archive's lossy copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Lossless,
    Mp3,
    Ogg,
}

fn tier(f: &Value) -> Option<Tier> {
    if f["private"] == "true" || f["private"] == true {
        return None; // stream-only recordings keep their originals private
    }
    match f["format"].as_str()? {
        "Flac" | "24bit Flac" => Some(Tier::Lossless),
        "VBR MP3" | "320Kbps MP3" | "256Kbps MP3" | "128Kbps MP3" => Some(Tier::Mp3),
        "Ogg Vorbis" => Some(Tier::Ogg),
        _ => None,
    }
}

/// `462.4` or `07:42` or `1:02:03`, in milliseconds.
pub fn length_ms(v: &Value) -> Option<i64> {
    let s = text(v, "length")?;
    let secs = if s.contains(':') {
        s.split(':').try_fold(0.0, |acc, p| {
            p.trim().parse::<f64>().ok().map(|x| acc * 60.0 + x)
        })?
    } else {
        s.parse::<f64>().ok()?
    };
    (secs > 0.0).then(|| (secs * 1000.0).round() as i64)
}

fn track_number(f: &Value) -> Option<u32> {
    // "01", "3", "3/12".
    text(f, "track")?.split('/').next()?.trim().parse().ok()
}

/// Order by digit runs as numbers: `t2` before `t10`.
fn natural_key(s: &str) -> Vec<(u64, String)> {
    let mut key = Vec::new();
    let mut digits = String::new();
    let mut other = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !other.is_empty() {
                key.push((u64::MAX, std::mem::take(&mut other)));
            }
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push((digits.parse().unwrap_or(u64::MAX - 1), String::new()));
                digits.clear();
            }
            other.push(c);
        }
    }
    if !digits.is_empty() {
        key.push((digits.parse().unwrap_or(u64::MAX - 1), String::new()));
    }
    if !other.is_empty() {
        key.push((u64::MAX, other));
    }
    key
}

/// The files to play for a recording, in order: every file of the best
/// tier available.
pub fn playlist(files: &[Value]) -> Vec<&Value> {
    let Some(best) = files.iter().filter_map(tier).min() else {
        return Vec::new();
    };
    let mut list: Vec<&Value> = files
        .iter()
        .filter(|f| tier(f) == Some(best) && f["name"].is_string())
        .collect();
    let mut numbers: Vec<Option<u32>> = list.iter().map(|f| track_number(f)).collect();
    numbers.sort();
    numbers.dedup();
    // Track numbers restart for each set or disc on many recordings; then
    // the file names (gd77-05-08d1t01…) carry the order.
    if numbers.len() == list.len() && numbers.iter().all(Option::is_some) {
        list.sort_by_key(|f| track_number(f));
    } else {
        list.sort_by_key(|f| natural_key(f["name"].as_str().unwrap_or("")));
    }
    list
}

/// What a file is, as far as its listing says.
pub fn file_format(f: &Value) -> Value {
    match f["format"].as_str() {
        Some("24bit Flac") => json!({"bits": 24, "codec": "flac"}),
        Some("Flac") => json!({"codec": "flac"}),
        Some("Ogg Vorbis") => json!({"codec": "vorbis"}),
        _ => json!({"codec": "mp3"}),
    }
}

/// One track of a recording (`meta` is the item's `metadata`).
pub fn track(id: &str, meta: &Value, f: &Value, number: usize) -> Option<Value> {
    let name = f["name"].as_str()?;
    let stem = name.rsplit('/').next().unwrap_or(name);
    let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
    let title = text(f, "title").unwrap_or_else(|| stem.to_string());
    let album_artist = text(meta, "creator");
    let artist = text(f, "creator").or_else(|| album_artist.clone());
    let album = text(meta, "title");
    Some(finish(json!({
        "ref": format!("t/{id}/{name}"),
        "kind": "track",
        "title": title,
        "subtitle": join(&[artist.clone(), date(meta)]),
        "artist": artist,
        "album": album,
        "album_artist": album_artist,
        "track_no": number,
        "year": year(meta),
        "genre": text(meta, "genre"),
        "duration_ms": length_ms(f),
        "art": ia::image_url(id),
        "format": file_format(f),
        "playable": true,
    })))
}

/// Every track of a recording, from its metadata record.
pub fn tracks(id: &str, record: &Value) -> Vec<Value> {
    let files = record["files"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    playlist(files)
        .into_iter()
        .enumerate()
        .filter_map(|(i, f)| track(id, &record["metadata"], f, i + 1))
        .collect()
}

/// The playable file of a recording named `name`, with its track item.
pub fn find_track(id: &str, record: &Value, name: &str) -> Option<(Value, Value)> {
    let files = record["files"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let list = playlist(files);
    let (i, f) = list
        .iter()
        .enumerate()
        .find(|(_, f)| f["name"] == name)
        .map(|(i, f)| (i, (*f).clone()))
        .or_else(|| {
            // A file of another tier (a favourite saved before the originals
            // went private, say): play it if it is still public.
            files
                .iter()
                .find(|f| f["name"] == name && tier(f).is_some())
                .map(|f| (0, f.clone()))
        })?;
    let it = track(id, &record["metadata"], &f, i + 1)?;
    Some((f, it))
}

/// The public MP3 copy the Archive made of `original`, if any.
pub fn lossy_copy<'a>(record: &'a Value, original: &str) -> Option<&'a Value> {
    record["files"]
        .as_array()?
        .iter()
        .find(|f| f["original"] == original && tier(f) == Some(Tier::Mp3))
}

/// `(sample_rate, bits, channels)` from a FLAC file's first bytes (its
/// STREAMINFO block), skipping an ID3v2 tag in front if there is one.
pub fn streaminfo(b: &[u8]) -> Option<(u32, u8, u8)> {
    let mut at = 0;
    if b.get(..3)? == b"ID3" {
        let h = b.get(6..10)?;
        let size = h
            .iter()
            .fold(0usize, |s, x| (s << 7) | (*x as usize & 0x7f));
        at = 10 + size;
    }
    if b.get(at..at + 4)? != b"fLaC" || b.get(at + 4)? & 0x7f != 0 {
        return None;
    }
    let s = b.get(at + 8..at + 8 + 34)?;
    let rate = (u32::from(s[10]) << 12) | (u32::from(s[11]) << 4) | (u32::from(s[12]) >> 4);
    let channels = ((s[12] >> 1) & 0x07) + 1;
    let bits = (((s[12] & 0x01) << 4) | (s[13] >> 4)) + 1;
    (rate > 0).then_some((rate, bits, channels))
}

/// What the DAC takes natively, from `initialize` / `output.changed`.
#[derive(Clone, Debug, Default)]
pub struct Output {
    pub bit_perfect: bool,
    pub max_rate: Option<u32>,
    pub max_bits: Option<u8>,
    pub rates: Vec<u32>,
}

impl Output {
    pub fn from_json(v: &Value) -> Output {
        Output {
            bit_perfect: v["bit_perfect"].as_bool().unwrap_or(false),
            max_rate: v["max_rate"].as_u64().map(|r| r as u32),
            max_bits: v["max_bits"].as_u64().map(|b| b as u8),
            rates: v["rates"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_u64)
                        .map(|r| r as u32)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Whether the engine can play `rate` / `bits` without converting.
    /// Without a bit-perfect device (the null sink, a sound server) the
    /// engine has the last word.
    pub fn takes(&self, rate: u32, bits: u8) -> bool {
        if !self.bit_perfect {
            return true;
        }
        let rate_ok = if self.rates.is_empty() {
            self.max_rate.is_none_or(|m| rate <= m)
        } else {
            self.rates.contains(&rate)
        };
        rate_ok && self.max_bits.is_none_or(|m| bits <= m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        assert_eq!(parse_ref("popular"), Some(Ref::Section("popular")));
        assert_eq!(
            parse_ref("c/GratefulDead"),
            Some(Ref::Artist("GratefulDead"))
        );
        assert_eq!(
            parse_ref("i/gd77-05-08.sbd.hicks.4982.sbeok.shnf"),
            Some(Ref::Recording("gd77-05-08.sbd.hicks.4982.sbeok.shnf"))
        );
        assert_eq!(
            parse_ref("t/gd77/sub/d1 t01.flac"),
            Some(Ref::Track("gd77", "sub/d1 t01.flac"))
        );
        assert_eq!(parse_ref("t/gd77/../x.flac"), None);
        assert_eq!(parse_ref("t/gd77/"), None);
        assert_eq!(parse_ref("i/a b"), None);
        assert_eq!(parse_ref("i/a/b"), None);
        assert_eq!(parse_ref("x/abc"), None);
        assert_eq!(parse_ref("Popular"), None);
    }

    fn files() -> Vec<Value> {
        serde_json::from_value(json!([
            {"name": "gd77d2t01.flac", "format": "Flac", "source": "original", "title": "Scarlet Begonias", "track": "01", "length": "620.5"},
            {"name": "gd77d1t10.flac", "format": "Flac", "source": "original", "title": "Loser", "track": "10", "length": "07:42"},
            {"name": "gd77d1t02.flac", "format": "Flac", "source": "original", "track": "02"},
            {"name": "gd77d1t01.flac", "format": "Flac", "source": "original", "title": "Minglewood", "track": "01"},
            {"name": "gd77d1t01.mp3", "format": "VBR MP3", "source": "derivative", "original": "gd77d1t01.flac"},
            {"name": "gd77.ffp", "format": "Flac FingerPrint"},
            {"name": "gd77.jpg", "format": "JPEG"}
        ]))
        .unwrap()
    }

    #[test]
    fn picks_originals_in_file_order() {
        let f = files();
        let names: Vec<&str> = playlist(&f)
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        // Track numbers repeat across discs: file names decide.
        assert_eq!(
            names,
            [
                "gd77d1t01.flac",
                "gd77d1t02.flac",
                "gd77d1t10.flac",
                "gd77d2t01.flac"
            ]
        );
    }

    #[test]
    fn stream_only_falls_back_to_mp3() {
        let f: Vec<Value> = serde_json::from_value(json!([
            {"name": "b.shn", "format": "Shorten", "private": "true"},
            {"name": "a2.flac", "format": "Flac", "private": "true", "track": "2"},
            {"name": "a1.flac", "format": "Flac", "private": "true", "track": "1"},
            {"name": "a2.mp3", "format": "VBR MP3", "original": "a2.flac", "track": "2"},
            {"name": "a1.mp3", "format": "VBR MP3", "original": "a1.flac", "track": "1"},
            {"name": "a1.ogg", "format": "Ogg Vorbis", "original": "a1.flac"}
        ]))
        .unwrap();
        let names: Vec<&str> = playlist(&f)
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["a1.mp3", "a2.mp3"]);
        assert!(playlist(&[json!({"name": "x.shn", "format": "Shorten"})]).is_empty());
    }

    #[test]
    fn track_mapping() {
        let record = json!({
            "metadata": {"identifier": "gd77", "title": "Grateful Dead Live at Barton Hall on 1977-05-08",
                         "creator": "Grateful Dead", "date": "1977-05-08", "venue": "Barton Hall",
                         "coverage": "Ithaca, NY"},
            "files": files(),
        });
        let t = tracks("gd77", &record);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0]["ref"], "t/gd77/gd77d1t01.flac");
        assert_eq!(t[0]["title"], "Minglewood");
        assert_eq!(t[0]["artist"], "Grateful Dead");
        assert_eq!(
            t[0]["album"],
            "Grateful Dead Live at Barton Hall on 1977-05-08"
        );
        assert_eq!(t[0]["year"], 1977);
        assert_eq!(t[0]["format"], json!({"codec": "flac"}));
        assert_eq!(t[1]["title"], "gd77d1t02"); // no title: the file name
        assert_eq!(t[2]["duration_ms"], 462_000);
        assert_eq!(t[3]["track_no"], 4);
        assert_eq!(t[3]["duration_ms"], 620_500);
        let (f, it) = find_track("gd77", &record, "gd77d1t10.flac").unwrap();
        assert_eq!(f["title"], "Loser");
        assert_eq!(it["track_no"], 3);
        assert!(find_track("gd77", &record, "gd77.jpg").is_none());
        assert_eq!(
            lossy_copy(&record, "gd77d1t01.flac").unwrap()["name"],
            "gd77d1t01.mp3"
        );
        assert!(lossy_copy(&record, "gd77d1t02.flac").is_none());

        let r = recording(&record["metadata"]).unwrap();
        assert_eq!(r["ref"], "i/gd77");
        assert_eq!(
            r["subtitle"],
            "Grateful Dead · 1977-05-08 · Barton Hall, Ithaca, NY"
        );
        assert_eq!(r["art"], "https://archive.org/services/img/gd77");
    }

    #[test]
    fn artist_mapping() {
        let a = artist(
            &json!({"identifier": "GratefulDead", "title": "Grateful Dead", "item_count": 18377}),
            true,
        )
        .unwrap();
        assert_eq!(a["ref"], "c/GratefulDead");
        assert_eq!(a["subtitle"], "18377 enregistrements");
        let doc = json!({"identifier": "x", "creator": ["A", "B"], "date": "2001-02-03T00:00:00Z"});
        let r = recording(&doc).unwrap();
        assert_eq!(r["artist"], "A, B");
        assert_eq!(r["year"], 2001);
        assert_eq!(r["title"], "x");
        assert!(recording(&json!({"identifier": "bad id"})).is_none());
    }

    #[test]
    fn lengths() {
        assert_eq!(length_ms(&json!({"length": "1:02:03"})), Some(3_723_000));
        assert_eq!(length_ms(&json!({"length": "0"})), None);
        assert_eq!(length_ms(&json!({"length": "n/a"})), None);
    }

    fn flac_head(rate: u32, bits: u8, channels: u8) -> Vec<u8> {
        let mut s = vec![0u8; 34];
        s[10] = (rate >> 12) as u8;
        s[11] = (rate >> 4) as u8;
        s[12] = ((rate & 0x0f) << 4) as u8 | ((channels - 1) << 1) | ((bits - 1) >> 4);
        s[13] = ((bits - 1) & 0x0f) << 4;
        let mut b = b"fLaC".to_vec();
        b.extend([0x00, 0x00, 0x00, 34]);
        b.extend(s);
        b
    }

    #[test]
    fn flac_streaminfo() {
        assert_eq!(streaminfo(&flac_head(96_000, 24, 2)), Some((96_000, 24, 2)));
        assert_eq!(streaminfo(&flac_head(44_100, 16, 1)), Some((44_100, 16, 1)));
        let mut tagged = b"ID3\x04\x00\x00\x00\x00\x00\x05hello".to_vec();
        tagged.extend(flac_head(48_000, 16, 2));
        assert_eq!(streaminfo(&tagged), Some((48_000, 16, 2)));
        assert_eq!(streaminfo(b"OggS...."), None);
        assert_eq!(streaminfo(&flac_head(48_000, 16, 2)[..20]), None);
    }

    #[test]
    fn output_fits() {
        let usb = Output {
            bit_perfect: true,
            max_rate: Some(96_000),
            max_bits: Some(24),
            rates: vec![44_100, 48_000, 88_200, 96_000],
        };
        assert!(usb.takes(48_000, 24));
        assert!(!usb.takes(192_000, 24));
        assert!(!usb.takes(44_100, 32));
        assert!(Output::default().takes(384_000, 32));
    }
}
