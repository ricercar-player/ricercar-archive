# ricercar-archive

A [ricercar](https://github.com/ricercar-player/ricercar) source plugin for
the Internet Archive's [Live Music Archive](https://archive.org/details/etree):
hundreds of thousands of concert recordings by artists who allow their
live shows to be shared, most of them lossless.

- **Browse:** popular this week, recently added, *on this day* (shows
  played on today's date, any year), and every artist's recordings, newest
  first.
  In ricercar's Home page, the first three show as shelves.
- **Search:** artists and recordings.
- **Favourites:** star recordings, artists and tracks. The Archive needs no
  account, so the plugin keeps them itself; they make up its library in
  ricercar's Albums, Artists and Tracks pages, marked *Live Music Archive*.
- **Bit-perfect:** tracks play from the original FLAC files, byte for byte
  (seekable). The plugin reads each file's FLAC header before playing, so
  ricercar knows the real sample rate and depth. When your DAC cannot take
  them, it plays the Archive's MP3 copy instead, or reports the track as
  unavailable if there is none.
- **Stream-only recordings** (some artists keep their originals private)
  play from the Archive's public MP3 copies.

The plugin uses the Archive's public, documented APIs only
([advanced search](https://archive.org/advancedsearch.php),
[metadata](https://archive.org/developers/md-read.html), downloads). It
needs no account and sends no personal data.

## Install

**From ricercar (0.4.0 and later):** open **Plugins** in the sidebar and
install *Live Music Archive*.

**By hand:** download `archive-x86_64` or `archive-aarch64` from the
[releases](https://github.com/ricercar-player/ricercar-archive/releases),
check it against its `.sha256` file, make it executable, and declare it in
`~/.config/ricercar/config.toml`:

```toml
[[plugins]]
id = "archive"
command = "/home/you/.local/bin/archive-x86_64"
```

**From source:**

```sh
cargo build --release
# target/release/ricercar-archive
```

## Notes

- Favourites live in `~/.local/share/ricercar/plugins/archive/favorites.json`.
  ricercar reads the library when the plugin starts; after starring
  something, refresh the library (or restart) to see it in the Albums,
  Artists and Tracks pages. The plugin's *Favourites* section is always up
  to date.
- Metadata is whatever the tapers and uploaders wrote, and it varies a lot
  from one recording to the next. Tracks without a title show their file
  name; the order follows the track numbers when they are unique, else the
  file names (`d1t01`, `d1t02`, `d2t01`…).
- The Archive's search pages through the first 10 000 results only. For
  artists with more recordings (the Grateful Dead have over 18 000), search
  for a year or a venue to reach older shows.
- Shorten (`.shn`) originals are not offered, as ricercar cannot decode them;
  their FLAC or MP3 copies are, when the Archive has them.
- Covers are the Archive's item thumbnails, often the artist's or the
  collection's picture.
- Please support the [Internet Archive](https://archive.org/donate) and the
  artists and tapers who share these recordings.

## Protocol

Plugin protocol 1, as described in ricercar's
[docs/plugins.md](https://github.com/ricercar-player/ricercar/blob/main/docs/plugins.md),
with the `favorites` and `library` capabilities and without `auth`.
`browse.root` gives the `sections` (for hosts without the library) and the
`home` shelves (`popular`, `recent`, `today`). There is no
`library.playlists`: the Archive has no user playlists.

| Ref | Meaning |
|---|---|
| `popular`, `recent`, `today`, `artists`, `favorites` | Top-level sections |
| `c/<collection>` | Artist (an etree collection): its recordings |
| `i/<identifier>` | Recording (shown as an album): its tracks |
| `t/<identifier>/<file>` | Track |

Error codes follow the protocol: missing items and files answer
`not_found`; files the output cannot take, without an MP3 copy, answer
`unavailable`; the Archive asking to slow down answers `rate_limited` with
`retry_after`; unreachable servers answer `network`.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo build --release && tests/live.py   # end to end against archive.org (network, ffprobe)
```

The CI builds static binaries (musl) for x86_64 and aarch64 on every tag
`v*` and attaches them, with their SHA-256, to a GitHub release.
`contrib/hub-entry.toml` is the entry for the
[ricercar plugin hub](https://github.com/ricercar-player/ricercar-plugins).

## Licence

MIT. The recordings belong to their artists and are shared under the
Live Music Archive's terms; this plugin is not affiliated with the Internet
Archive.
