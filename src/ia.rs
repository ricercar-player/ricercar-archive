//! A small client for the public Internet Archive APIs: advanced search
//! (https://archive.org/advancedsearch.php), item metadata
//! (https://archive.org/metadata/<identifier>) and file downloads. None of
//! them needs an account.

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

pub const BASE: &str = "https://archive.org";
/// Advanced search refuses to page past this many results.
pub const DEEP_LIMIT: u64 = 10_000;

#[derive(Debug)]
pub enum Error {
    /// No such item, or a dark (withdrawn) one.
    NotFound,
    /// HTTP 429 / 503: come back after that many seconds.
    RateLimited(u64),
    /// The Archive said no for another reason.
    Status(u16, String),
    /// DNS, TCP, TLS, timeouts, answers that are not JSON.
    Network(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::RateLimited(s) => write!(f, "the Archive asks to slow down ({s} s)"),
            Error::Status(code, msg) if msg.is_empty() => write!(f, "archive.org answered {code}"),
            Error::Status(code, msg) => write!(f, "archive.org answered {code}: {msg}"),
            Error::Network(e) => write!(f, "{e}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Percent-encode a path segment or query value (RFC 3986 unreserved
/// characters stay).
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `https://archive.org/download/<id>/<file>`, each path segment of the
/// file name encoded. The Archive redirects it to a storage node.
pub fn download_url(id: &str, file: &str) -> String {
    let path: Vec<String> = file.split('/').map(encode).collect();
    format!("{BASE}/download/{id}/{}", path.join("/"))
}

/// The item's or collection's thumbnail.
pub fn image_url(id: &str) -> String {
    format!("{BASE}/services/img/{id}")
}

/// Words of a user query, safe to put in a Lucene query: letters, digits
/// and a few joiners only, so that nothing the user types is an operator.
pub fn words(query: &str) -> Vec<String> {
    query
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '.' | '-')))
        .map(|w| w.trim_matches(|c| matches!(c, '\'' | '.' | '-')))
        .filter(|w| !w.is_empty())
        .filter(|w| !matches!(*w, "AND" | "OR" | "NOT" | "TO"))
        .take(12)
        .map(str::to_string)
        .collect()
}

pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    pub fn new() -> Client {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(9))
            .user_agent(concat!(
                "ricercar-archive/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/ricercar-player/ricercar-archive)"
            ))
            .build();
        Client { agent }
    }

    fn json(&self, req: ureq::Request) -> Result<Value> {
        match req.call() {
            Ok(r) => {
                let text = r.into_string().map_err(|e| Error::Network(e.to_string()))?;
                serde_json::from_str(&text)
                    .map_err(|_| Error::Network("archive.org sent something else than JSON".into()))
            }
            Err(ureq::Error::Status(code, r)) => Err(status(code, r)),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }

    /// One page of an advanced search: the documents and how many there
    /// are in all (capped at what the service lets us page through).
    pub fn search(
        &self,
        q: &str,
        sort: &str,
        fields: &[&str],
        start: u64,
        rows: u64,
    ) -> Result<(Vec<Value>, u64)> {
        // With two sort keys the service ignores `start` and only honours
        // `page`: an offset between pages reads from the page start and
        // drops the head.
        let (page, rows, skip) = pages(start, rows);
        let mut req = self
            .agent
            .get(&format!("{BASE}/advancedsearch.php"))
            .query("q", q)
            .query("sort[]", sort)
            // Ties (same date, same count) would shuffle between pages.
            .query("sort[]", "identifier asc")
            .query("rows", &rows.to_string())
            .query("page", &page.to_string())
            .query("output", "json");
        for f in fields {
            req = req.query("fl[]", f);
        }
        let v = self.json(req)?;
        if let Some(e) = v["error"].as_str() {
            return Err(Error::Status(400, e.chars().take(200).collect()));
        }
        let r = &v["response"];
        let docs: Vec<Value> = r["docs"]
            .as_array()
            .map(|d| d.iter().skip(skip as usize).cloned().collect())
            .unwrap_or_default();
        let total = r["numFound"].as_u64().unwrap_or(0).min(DEEP_LIMIT);
        Ok((docs, total))
    }

    /// The whole metadata record of an item: `metadata`, `files`…
    pub fn metadata(&self, id: &str) -> Result<Value> {
        let v = self.json(self.agent.get(&format!("{BASE}/metadata/{}", encode(id))))?;
        // Unknown identifiers answer `{}`; withdrawn ones `is_dark`.
        if v["metadata"].is_null() || v["is_dark"] == true {
            return Err(Error::NotFound);
        }
        Ok(v)
    }

    /// The first `n` bytes of a file, after redirects.
    pub fn head_bytes(&self, url: &str, n: usize) -> Result<Vec<u8>> {
        let r = self
            .agent
            .get(url)
            .set("Range", &format!("bytes=0-{}", n - 1))
            .call();
        match r {
            Ok(r) => {
                let mut buf = Vec::with_capacity(n);
                r.into_reader()
                    .take(n as u64)
                    .read_to_end(&mut buf)
                    .map_err(|e| Error::Network(e.to_string()))?;
                Ok(buf)
            }
            Err(ureq::Error::Status(code, r)) => Err(status(code, r)),
            Err(e) => Err(Error::Network(e.to_string())),
        }
    }
}

/// `(page, rows, skip)` that cover `[start, start + limit)`: pages of
/// `limit` when `start` falls on one, else one page from 0 (the host pages
/// in steps of its limit, so that is rare).
fn pages(start: u64, limit: u64) -> (u64, u64, u64) {
    if start % limit == 0 {
        (start / limit + 1, limit, 0)
    } else {
        (1, start + limit, start)
    }
}

fn status(code: u16, r: ureq::Response) -> Error {
    let retry = r
        .header("Retry-After")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(30);
    match code {
        404 | 410 => Error::NotFound,
        429 | 503 => Error::RateLimited(retry),
        _ => {
            let text: String = r
                .into_string()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            Error::Status(code, text.trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            download_url("gd77-05-08", "sub dir/t01 (#1).flac"),
            "https://archive.org/download/gd77-05-08/sub%20dir/t01%20%28%231%29.flac"
        );
        assert_eq!(image_url("etree"), "https://archive.org/services/img/etree");
    }

    #[test]
    fn paging() {
        assert_eq!(pages(0, 50), (1, 50, 0));
        assert_eq!(pages(100, 50), (3, 50, 0));
        assert_eq!(pages(30, 20), (1, 50, 30));
    }

    #[test]
    fn query_words() {
        assert_eq!(words("Grateful Dead"), ["Grateful", "Dead"]);
        assert_eq!(words("title:(x) AND \"moe.\" OR *"), ["title", "x", "moe"]);
        assert_eq!(
            words("Guns N' Roses 1987-06-19"),
            ["Guns", "N", "Roses", "1987-06-19"]
        );
        assert!(words("  ()[]{}  ").is_empty());
    }
}
