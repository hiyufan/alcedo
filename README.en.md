<div align="center">

# alcedo

**A fast video platform extractor**

Give it a share link, get back watermark-free direct URLs, image galleries,
covers, author info and the full quality ladder.

[![CI](https://github.com/hiyufan/alcedo/actions/workflows/ci.yml/badge.svg)](https://github.com/hiyufan/alcedo/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platforms-35-green.svg)](#supported-platforms)

**English** · [简体中文](README.md)

</div>

---

```console
$ alcedo https://www.youtube.com/watch?v=dQw4w9WgXcQ
平台  : YouTube
标题  : Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)
作者  : Rick Astley
时长  : 213.0s
清晰度:
  - 2160p AV1  243.8 MB  (需合并音视频)
  - 2160p VP9  362.0 MB  (需合并音视频)
  - 1080p       84.3 MB  (需合并音视频)
  ...
  - 仅音频       3.4 MB
```

> *Alcedo* is the genus of kingfishers. A kingfisher does not cast a net — it
> perches, takes aim, dives straight down and comes back with exactly the one
> thing it went for. This project does the same.

## Features

- **35 platforms**, covering the major Chinese and international sites
- **Single binary, no runtime dependencies** — no Python, no Node, no headless
  browser; extraction doesn't need ffmpeg either
- **Fast** — process-wide connection reuse; ~250 ms for Bilibili and ~1 s for
  YouTube on a warm connection
- **Watermark-free** — uses the clean direct URLs the platforms serve themselves
- **Galleries and Live Photos** — multi-image posts and the short clip behind a
  Live Photo both come out
- **Full quality ladder** — 4K / AV1 / H.265 / audio-only, with separated
  audio-video tracks clearly flagged
- **Errors humans can act on** — nine structured reasons a frontend can surface
  directly
- **Anti-throttling built in** — per-platform rate limiting and circuit breaking,
  on by default
- **Safe** — per-hop SSRF protection, `unsafe` forbidden workspace-wide

## Supported platforms

<table>
<tr><th>China</th><th>International</th></tr>
<tr valign="top"><td>

Douyin · Kuaishou · Xiaohongshu · Bilibili<br>
Weibo · Weishi · Oasis · Zuiyou<br>
Xigua · Pipixia · Pipigaoxiao<br>
Huya · AcFun · Duxiaoshi · PearVideo<br>
Tencent Video · Sohu Video · CCTV<br>
Doupai · Meipai · Haokan<br>
Xinpianchang · 6.cn · WeSing

</td><td>

YouTube · TikTok · X (Twitter)<br>
Instagram · Threads · Facebook<br>
Vimeo · Twitch · Reddit<br>
Pinterest · Dailymotion

</td></tr>
</table>

Run `alcedo --list` for the full list with the domains each platform matches.

## Quick start

### Install

```bash
git clone https://github.com/hiyufan/alcedo
cd alcedo
cargo install --path crates/alcedo-cli
```

Or just build it — the binary lands in `target/release/alcedo`:

```bash
cargo build --release
```

### Command line

```bash
alcedo "https://v.douyin.com/xxxxxx/"      # human-readable summary
alcedo --json "https://..."                # JSON
alcedo --list                              # supported platforms
```

You can paste a whole share blurb from an app — the link is pulled out for you:

```bash
alcedo "7.99 复制打开抖音，看看【作者】的作品 https://v.douyin.com/iRNBho6u/ 很好看"
```

### As a library

```toml
[dependencies]
alcedo = { git = "https://github.com/hiyufan/alcedo" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use alcedo::Client;

#[tokio::main]
async fn main() -> alcedo::Result<()> {
    // Build once and reuse it — it owns the connection pool
    let client = Client::new()?;
    let info = client.parse("https://v.douyin.com/xxxxxx/").await?;

    println!("{} / {}", info.title, info.author.name);
    println!("direct url: {}", info.video_url);
    for f in &info.formats {
        println!("  {} {}", f.label, if f.needs_merge() { "(needs merge)" } else { "" });
    }
    Ok(())
}
```

### Error handling

Failures come back as structured errors carrying a reason:

```rust
use alcedo::Reason;

match client.parse(url).await {
    Ok(info) => { /* ... */ }
    Err(e) if e.reason == Reason::Deleted => { /* the content is gone */ }
    Err(e) if e.reason == Reason::Login   => { /* tell the operator to add a cookie */ }
    Err(e) => eprintln!("[{}] {}", e.reason, e),
}
```

| Reason | Meaning | Who acts on it |
| --- | --- | --- |
| `parse` | The platform changed its page structure | **Maintainers** — the extractor needs updating |
| `deleted` | Content removed / private / link expired | User tries another link |
| `login` | The platform now requires sign-in | Operator supplies a cookie |
| `blocked` | Throttled or rate-limited | Operator changes egress or adds a proxy |
| `restricted` | The platform won't serve this item's data | Nothing to do; the app usually still shows it |
| `unsupported` | Link not recognised | User tries another link |
| `network` / `timeout` | Network trouble | Retry later |
| `empty` | Parsed fine but nothing in it | Rare, usually a platform hiccup |

This split is deliberate: **only `parse` needs a human**. Everything else is an
external condition.

## Output format

```jsonc
{
  "video_url": "https://...",     // the track a browser can play directly
  "cover_url": "https://...",
  "title": "...",
  "music_url": "https://...",     // background music / gallery audio
  "images": [                     // gallery; empty when there's a video
    { "url": "https://...", "live_photo_url": "https://..." }   // Live Photo clip
  ],
  "author": { "uid": "...", "name": "...", "avatar": "https://..." },
  "source": "douyin",
  "page_url": "https://...",
  "duration": 15.0,
  "width": 1080,
  "height": 1920,
  "formats": [
    { "label": "1080p", "url": "https://...", "ext": "mp4",
      "height": 1080, "filesize": 8400000, "codec": "" },

    // empty url + non-empty video_url/audio_url = separate tracks, needs ffmpeg
    { "label": "2160p AV1", "url": "", "ext": "mp4", "height": 2160, "codec": "AV1",
      "video_url": "https://...", "audio_url": "https://..." }
  ],
  "video_headers": { "Referer": "https://www.douyin.com/" }
}
```

> [!IMPORTANT]
> `video_headers` is not optional. The CDNs behind Douyin, Bilibili, Xiaohongshu
> and TikTok all check `Referer`. Pass these headers through verbatim when
> fetching a direct URL, or you get a 403.

## Configuration

Everything is environment variables — no config file.

| Variable | What it does | Default |
| --- | --- | --- |
| `ALCEDO_PROXY` | Proxy for every platform, e.g. `http://127.0.0.1:7890`, `socks5://...` | none |
| `ALCEDO_PROXY_CN` | Proxy used **only** for Chinese platforms. Required for Douyin / Xiaohongshu when deploying outside China | none |
| `ALCEDO_BILI_COOKIE` | Bilibili login cookie; without it you only get 720p | none |
| `ALCEDO_XHS_COOKIE` | Xiaohongshu login cookie; effectively required from datacenter IPs | none |
| `ALCEDO_DOUYIN_COOKIE` | Douyin cookie; when set, no anonymous identity is fetched | none |
| `ALCEDO_YOUTUBE_COOKIE` | YouTube cookie, for when you hit the bot check | none |
| `ALCEDO_SIGNER_<PLATFORM>` | External signer, see [below](#external-signers) | none |
| `ALCEDO_REQUEST_TIMEOUT` | Per-request timeout in seconds | `20` |
| `ALCEDO_TOTAL_TIMEOUT` | Whole-parse timeout in seconds | `45` |
| `ALCEDO_MAX_BODY_BYTES` | Response body cap | `16777216` |
| `ALCEDO_SSRF_DNS` | Set to `0` to disable private-address blocking at the DNS layer | `1` |

The `PARSE_VIDEO_*` prefix is also accepted, to ease migration from older
deployment scripts.

## Resilience

Three layers, all on by default and requiring no configuration.

**Per-platform rate limiting and circuit breaking.** Each platform gets its own
token bucket and circuit breaker. Douyin getting throttled does not affect a
YouTube parse in flight. Five consecutive platform-side failures trip the
breaker for that platform, with exponential backoff (15s → 30s → … capped at
5 minutes); during that window requests fail fast instead of hammering further.

One deliberate design point: **only platform-side failures count toward
tripping**. A user pasting five dead links gets five `deleted` errors — the
content is gone, the platform is fine. Counting those would let a user's slip of
the hand take a whole platform offline for minutes. Only `blocked` / `network` /
`timeout` / `login` — the "they won't let us in" signals — are counted.

**Guest identities.** Some platforms have grown hostile toward requests carrying
no cookies at all, while still offering an unsigned registration endpoint. We
fetch an anonymous identity the way a browser would, cache it, and drop it for a
fresh one when it gets rejected.

### External signers

A few platforms use request-signing algorithms that **rotate on a schedule**.
Hard-coding one of those here would mean re-reversing and re-releasing on every
rotation — it would become the highest-maintenance, most rot-prone part of the
whole repository.

So it's a pluggable provider instead: the algorithm lives in an external program
or service. When it changes you swap that out, without touching or rebuilding
this repository.

```bash
ALCEDO_SIGNER_DOUYIN=http://127.0.0.1:9000/sign   # long-running service (preferred)
ALCEDO_SIGNER_DOUYIN=cmd:/opt/alcedo/signer       # subprocess, JSON over stdin/stdout
```

The protocol is identical over both transports:

```jsonc
// request
{ "platform": "douyin", "url": "https://...", "query": "aweme_ids=%5B123%5D",
  "user_agent": "Mozilla/5.0 ...", "body": "" }

// response — all three fields optional
{ "query": { "a_bogus": "..." }, "headers": {}, "cookies": { "msToken": "..." } }
```

**Not configuring one is fine** — extractors take the unsigned path and don't
error. If the signer crashes, times out (5 s), or returns invalid JSON, the
unsigned path is used anyway: a signer is an enhancement and must never take
down a route that already worked.

## Design

**The connection pool is process-wide.** One parse issues 3–4 requests (short
link redirect, API, page). One HTTP client per proxy configuration means TCP
connections, TLS sessions and DNS results are reused throughout.

**Dispatch is static.** Platforms are a closed set known at compile time, so a
`match` calls straight into the concrete function. No trait objects, no virtual
calls anywhere on the parse path.

**JSON embedded in HTML is extracted by brace matching, not regex.** The usual
`marker\s*=\s*(.*?)</script>` has two failure modes: an escaped `</script>`
inside a JSON string truncates the payload early, and a trailing `;var x=1`
gets swallowed when other JS follows. Brace matching has neither problem, and
it's an order of magnitude faster on pages of a few hundred KB.

**Platforms are matched by domain suffix, not substring.** Substring matching
would classify `https://evil.com/?r=www.douyin.com` as Douyin and then send
Douyin's request headers to the attacker's server.

**SSRF is blocked at DNS resolution.** Extractors follow user-supplied short
links, so every hop is a user-controlled address. Rejecting private IPs inside a
custom resolver saves a `getaddrinfo` call *and* closes the window where a DNS
record could change between the check and the actual connection.

**YouTube needs no JavaScript.** The web client returns URLs carrying a
`signatureCipher` that requires running obfuscated JS to unwrap, plus a PO
token. The VisionOS / iOS / Android VR / TV clients return **already-signed
direct URLs** — same content, zero JS, zero tokens. If one client gets
restricted, reordering the client list is the whole fix.

**Bilibili goes straight to DASH.** Passing `fnval=4048` to `playurl` returns
separated audio and video tracks, so the quality ladder — 4K, AV1, H.265 — comes
out of a single request and is handed upward for merging.

## Performance

Same machine, same link, six consecutive parses in one process:

| Platform | First call (incl. DNS + TLS) | Warm median |
| --- | --- | --- |
| Bilibili | 412 ms | **256 ms** |
| PearVideo | — | **216 ms** |
| AcFun | — | **412 ms** |
| YouTube | — | **982 ms** |

The gap between the first call and the rest is connection reuse paying off.
Reproduce it yourself:

```bash
cargo run --release -p alcedo --example bench -- <url> 10
```

> Network jitter covers a lot of ground. These numbers were taken back-to-back
> in one sitting; a different time or network will shift them.

## Development

```bash
cargo test --workspace                  # unit + contract tests, all offline
cargo clippy --workspace --all-targets  # lint baseline lives in [workspace.lints]
cargo fmt --all

cargo test --workspace -- --ignored     # networked smoke tests (hits real platforms)
```

### Testing strategy

The two kinds of tests answer different questions, so they're kept apart:

- **Unit + contract tests** (188, all offline): given this response, is the
  parsing logic correct?
- **Smoke tests** (marked `#[ignore]`, run on a daily schedule): is the platform
  still returning it this way today?

Unit tests run against fixed samples, so they stay green even after a platform
rewrites its page. Only actually hitting the live endpoint tells you whether an
extractor still works. That makes the scheduled smoke run this project's **early
warning for platform changes**: it opens an issue when it goes red and closes it
when things recover.

### Lint baseline

The rules live in `[workspace.lints]` in the root `Cargo.toml` rather than being
passed by a CI script, so local and CI runs share one standard — no "it's green
on my machine".

Current state: `clippy::all` at **deny**, plus a curated set of pedantic lints,
with **zero warnings**. `unsafe_code = "forbid"` and `missing_docs = "warn"`.

Each selected lint earned its place by catching something real here:

| Lint | What it caught |
| --- | --- |
| `case_sensitive_file_extension_comparisons` | `.ends_with(".webp")` missing a CDN's `.WEBP` |
| `cast_possible_truncation` | A dozen integer conversions routed through f64, losing precision on large values |
| `unnecessary_wraps` | Functions that always returned `Ok`, forcing callers into a pointless `?` |
| `too_many_lines` | A 137-line function (since split) |

### Building on Windows

You need a working linker. With Visual Studio Build Tools installed, the default
MSVC toolchain just works. Without them, the GNU toolchain is less hassle:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup set default-host x86_64-pc-windows-gnu
winget install -e --id BrechtSanders.WinLibs.POSIX.MSVCRT   # provides dlltool / as
# then add its mingw64\bin to PATH
```

The mingw bundled with rustup is trimmed down and lacks the `as` that `dlltool`
depends on, so some crates fail to link.

## Contributing

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to add a platform,
the house rules, and commit message conventions.

For vulnerabilities, please use
[private reporting](https://github.com/hiyufan/alcedo/security/advisories/new)
rather than a public issue — see [SECURITY.md](SECURITY.md).

## Disclaimer

This project only extracts media URLs from data the platforms serve publicly. It
does not break encryption or bypass paywalls. Extracted content remains the
property of its original authors; please respect each platform's terms of
service and keep your use within fair-use bounds.

## License

[MIT](LICENSE)
