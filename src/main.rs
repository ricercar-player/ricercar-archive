//! Live Music Archive source plugin for ricercar (plugin protocol 1).
//!
//! Speaks JSON-RPC over stdin/stdout with the player, and the public
//! Internet Archive APIs with archive.org: the etree collection's concert
//! recordings, free to share, most of them lossless. No account: favourites
//! stay in the plugin's data directory and make up its library.
//!
//! Tracks play from the original FLAC files, bit for bit, whose STREAMINFO
//! the plugin reads before answering so that the reported format is the
//! real one. Recordings whose originals are private (stream-only artists)
//! play from the Archive's MP3 copies.

mod favorites;
mod ia;
mod items;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use favorites::Favorites;
use ia::{Client, Error};
use items::{Output, Ref};

const PROTOCOL: u64 = 1;
const PAGE: u64 = 200;
/// How long an item's metadata record is reused.
const RECORD_TTL: Duration = Duration::from_secs(600);
const RECORD_CACHE: usize = 64;
const DOC_FIELDS: &[&str] = &[
    "identifier",
    "title",
    "creator",
    "date",
    "venue",
    "coverage",
];

struct RpcError {
    code: i64,
    message: String,
    retry_after: Option<u64>,
}

fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        retry_after: None,
    }
}

impl From<Error> for RpcError {
    fn from(e: Error) -> RpcError {
        match e {
            Error::NotFound => rpc_err(-32002, "not found on archive.org"),
            Error::RateLimited(s) => RpcError {
                code: -32004,
                message: e.to_string(),
                retry_after: Some(s),
            },
            Error::Status(code, _) if code >= 500 => rpc_err(-32005, e.to_string()),
            Error::Status(..) => rpc_err(-32603, e.to_string()),
            Error::Network(m) => rpc_err(-32005, m),
        }
    }
}

type Reply = Result<Value, RpcError>;

struct Out(Mutex<std::io::Stdout>);

impl Out {
    fn send(&self, v: Value) {
        let mut out = self.0.lock().unwrap();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }
}

/// Today's month and day (UTC), for "On this day".
fn month_day() -> (u32, u32) {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (m, d)
}

fn this_year() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    1970 + (secs / 31_556_952) as i64
}

/// Concerts played on this month and day, any year since 1950.
fn on_this_day(m: u32, d: u32) -> String {
    let dates: Vec<String> = (1950..=this_year())
        .map(|y| format!("{y}-{m:02}-{d:02}"))
        .collect();
    format!("mediatype:etree AND date:({})", dates.join(" OR "))
}

struct Plugin {
    fr: Mutex<bool>,
    output: Mutex<Output>,
    client: Client,
    favorites: Mutex<Favorites>,
    records: Mutex<HashMap<String, (Instant, Arc<Value>)>>,
}

impl Plugin {
    fn fr(&self) -> bool {
        *self.fr.lock().unwrap()
    }

    fn initialize(&self, p: &Value) -> Reply {
        let data_dir = p["data_dir"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        *self.fr.lock().unwrap() = p["locale"].as_str().is_some_and(|l| l.starts_with("fr"));
        *self.output.lock().unwrap() = Output::from_json(&p["output"]);
        *self.favorites.lock().unwrap() = Favorites::load(&data_dir);
        let proto = p["protocol"].as_u64().unwrap_or(0);
        if proto != PROTOCOL {
            eprintln!("host speaks protocol {proto}, this plugin {PROTOCOL}");
        }
        Ok(json!({
            "protocol": PROTOCOL,
            "plugin": {"id": "archive", "name": "Live Music Archive", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {
                "auth": false, "browse": true, "search": true, "resolve": true,
                "favorites": true, "reporting": false, "remote_control": false,
                "library": true
            }
        }))
    }

    /// An item's metadata record, from the cache when fresh.
    fn record(&self, id: &str) -> Result<Arc<Value>, RpcError> {
        if let Some((at, v)) = self.records.lock().unwrap().get(id)
            && at.elapsed() < RECORD_TTL
        {
            return Ok(v.clone());
        }
        let v = Arc::new(self.client.metadata(id)?);
        let mut cache = self.records.lock().unwrap();
        if cache.len() >= RECORD_CACHE {
            let oldest = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                cache.remove(&k);
            }
        }
        cache.insert(id.to_string(), (Instant::now(), v.clone()));
        Ok(v)
    }

    // --------------------------------------------------------------- browse

    fn root(&self) -> Reply {
        let fr = self.fr();
        let t = |en: &'static str, f: &'static str| if fr { f } else { en };
        let (m, d) = month_day();
        let today = if fr {
            format!("Ce jour-là ({d:02}/{m:02})")
        } else {
            format!("On this day ({m:02}-{d:02})")
        };
        let sections = [
            (
                "popular",
                t("Popular this week", "Populaires cette semaine").to_string(),
            ),
            ("recent", t("Recently added", "Ajouts récents").to_string()),
            ("today", today),
            ("artists", t("Artists", "Artistes").to_string()),
            ("favorites", t("Favourites", "Favoris").to_string()),
        ];
        let entry = |(r, title): &(&str, String)| json!({"ref": r, "kind": "folder", "title": title, "browsable": true});
        // Home shelves, for hosts that show the library instead of the
        // sections: the lists of recordings, not the directory of artists
        // nor the favourites (already in the library).
        let home: Vec<Value> = sections
            .iter()
            .filter(|(r, _)| matches!(*r, "popular" | "recent" | "today"))
            .map(entry)
            .collect();
        let sections: Vec<Value> = sections.iter().map(entry).collect();
        Ok(json!({ "sections": sections, "home": home }))
    }

    /// One page of recordings matching `q`.
    fn recordings(&self, q: &str, sort: &str, offset: u64, limit: u64) -> Reply {
        let (docs, total) = self.client.search(q, sort, DOC_FIELDS, offset, limit)?;
        let list: Vec<Value> = docs.iter().filter_map(items::recording).collect();
        let has_more = offset + (docs.len() as u64) < total && !docs.is_empty();
        Ok(json!({"items": list, "total": total, "has_more": has_more}))
    }

    /// One page of artists (etree collections) matching `q`.
    fn artists(&self, q: &str, offset: u64, limit: u64) -> Reply {
        let fields = ["identifier", "title", "item_count"];
        let (docs, total) = self
            .client
            .search(q, "downloads desc", &fields, offset, limit)?;
        let fr = self.fr();
        let list: Vec<Value> = docs.iter().filter_map(|d| items::artist(d, fr)).collect();
        let has_more = offset + (docs.len() as u64) < total && !docs.is_empty();
        Ok(json!({"items": list, "total": total, "has_more": has_more}))
    }

    fn list(&self, p: &Value) -> Reply {
        let r = p["ref"].as_str().unwrap_or("");
        let offset = p["offset"].as_u64().unwrap_or(0);
        let limit = p["limit"].as_u64().unwrap_or(PAGE).clamp(1, PAGE);
        match items::parse_ref(r).ok_or_else(|| rpc_err(-32002, "no such list"))? {
            Ref::Section("popular") => {
                self.recordings("mediatype:etree", "week desc", offset, limit)
            }
            Ref::Section("recent") => {
                self.recordings("mediatype:etree", "publicdate desc", offset, limit)
            }
            Ref::Section("today") => {
                let (m, d) = month_day();
                self.recordings(&on_this_day(m, d), "downloads desc", offset, limit)
            }
            Ref::Section("artists") => {
                self.artists("mediatype:collection AND collection:etree", offset, limit)
            }
            Ref::Section("favorites") => Ok(page(
                self.favorites.lock().unwrap().all().to_vec(),
                offset,
                limit,
            )),
            Ref::Artist(c) => self.recordings(
                &format!("mediatype:etree AND collection:{c}"),
                "date desc",
                offset,
                limit,
            ),
            Ref::Recording(id) => {
                let rec = self.record(id)?;
                Ok(page(items::tracks(id, &rec), offset, limit))
            }
            _ => Err(rpc_err(-32002, "no such list")),
        }
    }

    fn search(&self, p: &Value) -> Reply {
        let words = ia::words(p["query"].as_str().unwrap_or(""));
        let offset = p["offset"].as_u64().unwrap_or(0);
        let limit = p["limit"].as_u64().unwrap_or(50).clamp(1, PAGE);
        let wanted: Vec<&str> = p["kinds"]
            .as_array()
            .map(|k| k.iter().filter_map(Value::as_str).collect())
            .unwrap_or_else(|| vec!["artist", "album"]);
        if words.is_empty() {
            return Ok(json!({ "groups": [] }));
        }
        let terms = words
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut groups = Vec::new();
        // Tracks are not indexed one by one: artists and recordings only.
        if wanted.contains(&"artist") {
            let q = format!("mediatype:collection AND collection:etree AND title:({terms})");
            let mut g = self.artists(&q, offset, limit)?;
            g["kind"] = "artist".into();
            groups.push(g);
        }
        if wanted.contains(&"album") {
            let q = format!("mediatype:etree AND ({terms})");
            let mut g = self.recordings(&q, "downloads desc", offset, limit)?;
            g["kind"] = "album".into();
            groups.push(g);
        }
        Ok(json!({ "groups": groups }))
    }

    fn item(&self, r: &str) -> Reply {
        match items::parse_ref(r).ok_or_else(|| rpc_err(-32002, "no such item"))? {
            Ref::Artist(c) => {
                let rec = self.record(c)?;
                let mut meta = rec["metadata"].clone();
                meta["item_count"] = rec["item_count"].clone();
                items::artist(&meta, self.fr())
            }
            Ref::Recording(id) => items::recording(&self.record(id)?["metadata"]),
            Ref::Track(id, file) => {
                items::find_track(id, &*self.record(id)?, file).map(|(_, it)| it)
            }
            Ref::Section(_) => None,
        }
        .ok_or_else(|| rpc_err(-32002, "not a music item"))
    }

    fn favorite(&self, p: &Value) -> Reply {
        let r = p["ref"].as_str().unwrap_or("");
        let saved = if p["on"].as_bool().unwrap_or(false) {
            let it = self.item(r)?;
            self.favorites.lock().unwrap().add(it)
        } else {
            self.favorites.lock().unwrap().remove(r)
        };
        saved.map_err(|e| rpc_err(-32603, format!("cannot save the favourites: {e}")))?;
        Ok(Value::Null)
    }

    fn library(&self, method: &str, p: &Value) -> Reply {
        let offset = p["offset"].as_u64().unwrap_or(0);
        let limit = p["limit"].as_u64().unwrap_or(PAGE).clamp(1, PAGE);
        let kind = match method {
            "library.albums" => "album",
            "library.artists" => "artist",
            _ => "track",
        };
        Ok(page(
            self.favorites.lock().unwrap().of_kind(kind),
            offset,
            limit,
        ))
    }

    // -------------------------------------------------------------- resolve

    fn resolve(&self, p: &Value) -> Reply {
        let r = p["ref"].as_str().unwrap_or("");
        let Some(Ref::Track(id, name)) = items::parse_ref(r) else {
            return Err(rpc_err(-32002, "not a track"));
        };
        let rec = self.record(id)?;
        let (file, _) = items::find_track(id, &rec, name)
            .ok_or_else(|| rpc_err(-32002, "this file is gone or no longer public"))?;
        let url = ia::download_url(id, name);
        let mut format = items::file_format(&file);
        if format["codec"] == "flac" {
            // The listing does not give the sample rate: read it.
            let head = self.client.head_bytes(&url, 8192)?;
            if let Some((rate, bits, channels)) = items::streaminfo(&head) {
                format = json!({"sample_rate": rate, "bits": bits, "channels": channels, "codec": "flac"});
                if !self.output.lock().unwrap().takes(rate, bits) {
                    return self.lossy(id, &rec, name, rate, bits);
                }
            }
        }
        Ok(json!({
            "url": url,
            "duration_ms": items::length_ms(&file),
            "format": format,
            "live": false,
        }))
        .map(|mut v| {
            if let Some(o) = v.as_object_mut() {
                o.retain(|_, v| !v.is_null());
            }
            v
        })
    }

    /// The DAC cannot take the original: the Archive's MP3 copy, or
    /// `unavailable`.
    fn lossy(&self, id: &str, rec: &Value, name: &str, rate: u32, bits: u8) -> Reply {
        let what = format!("{rate} Hz / {bits} bits");
        let Some(copy) = items::lossy_copy(rec, name) else {
            eprintln!("{id}/{name}: {what} does not fit this output, no MP3 copy");
            return Err(rpc_err(-32003, format!("the DAC cannot take {what}")));
        };
        let copy_name = copy["name"].as_str().unwrap_or(name);
        eprintln!("{id}/{name}: {what} does not fit this output, playing the MP3 copy");
        let mut v = json!({
            "url": ia::download_url(id, copy_name),
            "format": {"codec": "mp3"},
            "live": false,
        });
        if let Some(ms) = items::length_ms(copy) {
            v["duration_ms"] = ms.into();
        }
        Ok(v)
    }

    // ------------------------------------------------------------- dispatch

    fn handle(&self, method: &str, p: &Value) -> Reply {
        match method {
            "initialize" => self.initialize(p),
            "browse.root" => self.root(),
            "browse.list" => self.list(p),
            "search" => self.search(p),
            "item.get" => self.item(p["ref"].as_str().unwrap_or("")),
            "favorites.set" => self.favorite(p),
            "library.albums" | "library.artists" | "library.tracks" => self.library(method, p),
            // No `library.playlists`: the Archive has no user playlists.
            "track.resolve" => self.resolve(p),
            _ => Err(rpc_err(-32601, format!("method not found: {method}"))),
        }
    }
}

fn page(all: Vec<Value>, offset: u64, limit: u64) -> Value {
    let total = all.len() as u64;
    let items: Vec<Value> = all
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    json!({"items": items, "total": total, "has_more": offset + limit < total})
}

fn main() {
    if let Some(a) = std::env::args().nth(1) {
        if a == "--version" {
            println!("ricercar-archive {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        eprintln!("unknown option {a}");
    }
    let out = Arc::new(Out(Mutex::new(std::io::stdout())));
    let plugin = Arc::new(Plugin {
        fr: Mutex::new(false),
        output: Mutex::new(Output::default()),
        client: Client::new(),
        favorites: Mutex::new(Favorites::default()),
        records: Mutex::new(HashMap::new()),
    });

    for line in BufReader::new(std::io::stdin()).lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(method) = msg["method"].as_str().map(str::to_string) else {
            continue; // an answer; this plugin sends no requests
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = msg.get("id").cloned() else {
            if method == "output.changed" {
                *plugin.output.lock().unwrap() = Output::from_json(&params["output"]);
            }
            continue;
        };
        if method == "shutdown" {
            out.send(json!({"jsonrpc": "2.0", "id": id, "result": null}));
            return;
        }
        let first = method == "initialize";
        let run = {
            let plugin = plugin.clone();
            let out = out.clone();
            move || {
                let reply = match plugin.handle(&method, &params) {
                    Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
                    Err(e) => {
                        let mut err = json!({"code": e.code, "message": e.message});
                        if let Some(s) = e.retry_after {
                            err["data"] = json!({ "retry_after": s });
                        }
                        json!({"jsonrpc": "2.0", "id": id, "error": err})
                    }
                };
                out.send(reply);
            }
        };
        // The handshake first, in order; everything else may overlap.
        if first {
            run();
        } else {
            std::thread::spawn(run);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_query() {
        let q = on_this_day(9, 29);
        assert!(q.starts_with("mediatype:etree AND date:(1950-09-29 OR 1951-09-29"));
        assert!(q.ends_with(&format!("{}-09-29)", this_year())));
        let (m, d) = month_day();
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
    }

    #[test]
    fn root_and_home() {
        let plugin = Plugin {
            fr: Mutex::new(false),
            output: Mutex::new(Output::default()),
            client: Client::new(),
            favorites: Mutex::new(Favorites::default()),
            records: Mutex::new(HashMap::new()),
        };
        let root = plugin.root().ok().unwrap();
        let refs = |k: &str| -> Vec<String> {
            root[k]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["ref"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            refs("sections"),
            ["popular", "recent", "today", "artists", "favorites"]
        );
        assert_eq!(refs("home"), ["popular", "recent", "today"]);
        assert!(
            root["home"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["browsable"] == true)
        );
        assert_eq!(
            plugin
                .handle("library.playlists", &Value::Null)
                .err()
                .unwrap()
                .code,
            -32601
        );
    }

    #[test]
    fn paging() {
        let all: Vec<Value> = (0..5).map(|i| json!(i)).collect();
        let p = page(all, 3, 2);
        assert_eq!(p["items"], json!([3, 4]));
        assert_eq!(p["has_more"], false);
        assert_eq!(p["total"], 5);
    }
}
