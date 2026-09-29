#!/usr/bin/env python3
"""End-to-end test of the plugin against the real archive.org: JSON-RPC over
stdio, then the resolved streams with ffprobe. Needs network and ffprobe.

    cargo build --release && tests/live.py
"""
import json, os, queue, subprocess, sys, tempfile, threading

BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "ricercar-archive")
DATA = tempfile.mkdtemp(prefix="ricercar-archive-")
USB = {"device": "hw:9,0", "bit_perfect": True, "max_rate": 96000, "max_bits": 24,
       "rates": [44100, 48000, 88200, 96000]}
CD = {"device": "hw:9,0", "bit_perfect": True, "max_rate": 44100, "max_bits": 16, "rates": [44100]}
# A 48 kHz audience FLAC recording, and a stream-only soundboard.
FLAC48 = "gd1995-07-09.schoeps.wklitz.95444.flac1648"
STREAM_ONLY = "gd73-06-10.sbd.hollister.174.sbeok.shnf"


class Plugin:
    def __init__(self):
        self.p = subprocess.Popen([BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=open(DATA + "/plugin.log", "a"), text=True)
        self.q, self.n = {}, 0
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.p.stdout:
            m = json.loads(line)
            if "id" in m:
                self.q[m["id"]].put(m)

    def call(self, method, params=None):
        self.n += 1
        i = self.n
        self.q[i] = queue.Queue()
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}}) + "\n")
        self.p.stdin.flush()
        m = self.q[i].get(timeout=40)
        return m.get("result", m.get("error"))

    def notify(self, method, params):
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n")
        self.p.stdin.flush()


failed = 0


def check(cond, msg):
    global failed
    print(("PASS " if cond else "FAIL ") + msg)
    failed += 0 if cond else 1


def probe(url):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries",
                          "stream=codec_name,sample_rate,bits_per_raw_sample,channels",
                          "-of", "json", url], capture_output=True, text=True, timeout=60)
    return json.loads(out.stdout or "{}").get("streams", [{}])[0]


p = Plugin()
init = p.call("initialize", {"protocol": 1, "data_dir": DATA, "locale": "fr-FR", "output": USB})
caps = init["capabilities"]
check(init["plugin"]["id"] == "archive" and not caps["auth"] and caps["library"], "initialize")

root = p.call("browse.root")
check([s["ref"] for s in root["sections"]] == ["popular", "recent", "today", "artists", "favorites"],
      "root: " + ", ".join(s["title"] for s in root["sections"]))

check([s["ref"] for s in root.get("home", [])] == ["popular", "recent", "today"]
      and all(s["browsable"] for s in root["home"]),
      "home: " + ", ".join(s["title"] for s in root.get("home", [])))

for sec in ("popular", "recent", "today"):
    r = p.call("browse.list", {"ref": sec, "offset": 0, "limit": 20})
    check(len(r["items"]) > 0 and r["items"][0]["kind"] == "album" and r["has_more"],
          "%s: %d items, first %r" % (sec, len(r["items"]), r["items"][0]["title"] if r["items"] else None))

ar = p.call("browse.list", {"ref": "artists", "offset": 0, "limit": 5})
check(ar["items"][0]["ref"] == "c/GratefulDead" and ar["total"] > 1000,
      "artists: %s (%s)" % (ar["items"][0]["title"], ar["items"][0].get("subtitle")))
gd = p.call("browse.list", {"ref": "c/GratefulDead", "offset": 0, "limit": 10})
check(len(gd["items"]) == 10 and gd["total"] == 10000, "artist recordings, capped total %s" % gd["total"])
gd2 = p.call("browse.list", {"ref": "c/GratefulDead", "offset": 10, "limit": 10})
check(not {i["ref"] for i in gd["items"]} & {i["ref"] for i in gd2["items"]}, "artist paging")

tr = p.call("browse.list", {"ref": "i/" + FLAC48, "offset": 0, "limit": 200})
t0 = tr["items"][0]
check(tr["total"] > 10 and t0["title"] == "Touch Of Grey" and t0["format"]["codec"] == "flac"
      and t0["track_no"] == 1 and t0["duration_ms"] > 400000,
      "recording tracks: %d, first %r" % (tr["total"], t0["title"]))

res = p.call("track.resolve", {"ref": t0["ref"], "purpose": "play"})
check(res.get("format") == {"sample_rate": 48000, "bits": 16, "channels": 2, "codec": "flac"},
      "resolve FLAC: " + json.dumps(res.get("format")))
s = probe(res["url"])
check(s.get("codec_name") == "flac" and s.get("sample_rate") == "48000", "stream probe: " + json.dumps(s))

p.notify("output.changed", {"output": CD})
res = p.call("track.resolve", {"ref": t0["ref"], "purpose": "play"})
check(res.get("format") == {"codec": "mp3"} and res["url"].endswith(".mp3"),
      "44.1-only DAC -> MP3 copy: " + res.get("url", json.dumps(res)))
p.notify("output.changed", {"output": USB})

so = p.call("browse.list", {"ref": "i/" + STREAM_ONLY, "offset": 0, "limit": 200})
check(so["total"] > 5 and all(i["format"]["codec"] == "mp3" for i in so["items"]),
      "stream-only recording: %d MP3 tracks" % so["total"])
res = p.call("track.resolve", {"ref": so["items"][1]["ref"], "purpose": "preload"})
s = probe(res["url"])
check(s.get("codec_name") == "mp3", "stream-only resolve + probe: " + json.dumps(s))

sr = p.call("search", {"query": "grateful dead", "offset": 0, "limit": 5})
g = {x["kind"]: x for x in sr["groups"]}
check(g["artist"]["items"][0]["ref"] == "c/GratefulDead" and len(g["album"]["items"]) == 5,
      "search: artist + recordings")
check(p.call("search", {"query": "title:(x) OR *", "offset": 0, "limit": 5}).get("groups") is not None,
      "search with operators is sanitized")
check(p.call("search", {"query": "  ", "offset": 0, "limit": 5}) == {"groups": []}, "empty search")

for r in ("i/" + FLAC48, "c/GratefulDead", t0["ref"]):
    check(p.call("favorites.set", {"ref": r, "on": True}) is None, "favourite " + r)
lib = {m: p.call(m, {"offset": 0, "limit": 200})["items"] for m in ("library.albums", "library.artists", "library.tracks")}
check([len(v) for v in lib.values()] == [1, 1, 1] and lib["library.tracks"][0]["title"] == "Touch Of Grey",
      "library from favourites")
al, ar1 = lib["library.albums"][0], lib["library.artists"][0]
check(al["kind"] == "album" and al["browsable"] and al.get("artist") and al.get("year") and al.get("art"),
      "library album: artist %r, year %s" % (al.get("artist"), al.get("year")))
check(ar1["kind"] == "artist" and ar1["browsable"] and ar1.get("art"), "library artist: " + ar1["title"])
check(p.call("browse.list", {"ref": al["ref"], "offset": 0, "limit": 5})["items"][0]["kind"] == "track",
      "library album -> tracks")
check(p.call("browse.list", {"ref": ar1["ref"], "offset": 0, "limit": 5})["items"][0]["kind"] == "album",
      "library artist -> recordings")
check(p.call("library.playlists", {"offset": 0, "limit": 200})["code"] == -32601, "no library.playlists")
check(len(p.call("browse.list", {"ref": "favorites", "offset": 0, "limit": 50})["items"]) == 3, "favourites section")
p.call("favorites.set", {"ref": "c/GratefulDead", "on": False})
check(len(json.load(open(DATA + "/favorites.json"))["items"]) == 2, "unfavourite, saved")

check(p.call("item.get", {"ref": t0["ref"]})["title"] == "Touch Of Grey", "item.get track")
check(p.call("item.get", {"ref": "i/no-such-item-ricercar-xyz"})["code"] == -32002, "missing item -> not_found")
check(p.call("track.resolve", {"ref": "t/%s/nope.flac" % FLAC48})["code"] == -32002, "missing file -> not_found")
check(p.call("browse.list", {"ref": "../x"})["code"] == -32002, "bad ref -> not_found")
check(p.call("nope")["code"] == -32601, "unknown method")

p.call("shutdown")
print("\n%d failed" % failed)
sys.exit(1 if failed else 0)
