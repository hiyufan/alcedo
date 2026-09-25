<div align="center">

# alcedo

**A Rust library and CLI for video platform extraction**

Give it a share link, get back media URLs, image galleries, covers, author info
and available quality options.

[![CI](https://github.com/hiyufan/alcedo/actions/workflows/ci.yml/badge.svg)](https://github.com/hiyufan/alcedo/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/alcedo-cli.svg)](https://www.npmjs.com/package/alcedo-cli)
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

*Alcedo* is a genus of kingfishers.

## Features

- **35 platforms**: supports the Chinese and international sites listed below.
- **Native Rust**: built-in extraction paths do not require Python, Node or a headless browser.
- **Consistent output**: shared data structures for media URLs, galleries, Live Photos and author information.
- **Multiple qualities**: returns available formats, including 4K, AV1, H.265 and audio-only where provided; identifies separate audio and video tracks.
- **Structured errors**: distinguishes unavailable content, sign-in requirements, rate limits and network failures.
- **Request controls**: reuses HTTP connections, limits requests per platform and enables private-address checks by default.

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

Run `alcedo --list` for the full list and supported domains. Available content and
quality depend on sign-in status, region, network egress and platform changes;
see [limitations](#limitations).

## Quick start

### Install

**npm** (recommended): prebuilt binaries for Linux / macOS / Windows on x64 and arm64. No Rust
toolchain needed.

```bash
npm install -g alcedo-cli     # installs the `alcedo` command
npx alcedo-cli --list         # or run without installing
```

**Prebuilt archives**: download the archive for your platform from
[GitHub Releases](https://github.com/hiyufan/alcedo/releases), extract it and put `alcedo` on
your `PATH`. Linux builds are statically linked against musl and work on any distribution.

**From source**: requires Rust 1.85+. On Windows, MSVC with the C++ build tools from Visual
Studio Build Tools is recommended. See the [Windows build notes](CONTRIBUTING.en.md#windows-builds)
for other toolchains and troubleshooting.

```bash
cargo install --git https://github.com/hiyufan/alcedo alcedo-cli
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
    // Read configuration from the environment; this Client can be reused
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

| Reason | Meaning | Suggested action |
| --- | --- | --- |
| `parse` | Response data does not match the extractor's expectations | Maintainers check API changes and parsing logic |
| `deleted` | Content removed / private / link expired | User tries another link |
| `login` | The platform now requires sign-in | Operator supplies a cookie |
| `blocked` | Throttled or rate-limited | Reduce request frequency; check egress or proxy settings |
| `restricted` | The platform restricts access to this content | Check account, region or content permissions |
| `unsupported` | Link not recognised | User tries another link |
| `network` / `timeout` | Network trouble | Retry later |
| `empty` | No media was extracted | Check the link; report persistent failures to maintainers |

## Output format

The main fields are shown below. Some fields are omitted when they contain empty
strings, empty collections or zero values.

```jsonc
{
  "video_url": "https://...",     // default media URL; may be empty for galleries or separate tracks
  "cover_url": "https://...",
  "title": "...",
  "music_url": "https://...",     // background music / gallery audio
  "images": [                     // gallery fields; depend on the content type
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

    // separate tracks use video_url/audio_url; merge for combined playback
    { "label": "2160p AV1", "url": "", "ext": "mp4", "height": 2160, "codec": "AV1",
      "video_url": "https://...", "audio_url": "https://..." }
  ],
  "video_headers": { "Referer": "https://www.douyin.com/" }
}
```

> [!IMPORTANT]
> Pass the returned `video_headers` through when downloading media. Some CDNs
> validate headers such as `Referer`; omitting them may result in a 403.

## Configuration

The CLI and `Client::new()` read configuration from environment variables without
a config file. Library users can also supply a `Config` to `Client::with_config`:

```rust
use std::time::Duration;
use alcedo::{Client, Config};

let client = Client::with_config(Config {
    total_timeout: Duration::from_secs(60),
    ..Config::default()
});
```

`Config::default()` does not read the environment. Use `Config::from_env()` to
start with environment settings before changing individual fields.

| Variable | What it does | Default |
| --- | --- | --- |
| `ALCEDO_PROXY` | Proxy for every platform, e.g. `http://127.0.0.1:7890`, `socks5://...` | none |
| `ALCEDO_PROXY_CN` | Preferred proxy for Chinese platforms; falls back to `ALCEDO_PROXY` when unset | none |
| `ALCEDO_RELAY_CN` | Mainland-China egress relay (a forwarding function on a Chinese edge platform, e.g. `https://example.com/relay`); when set, parse requests for Chinese platforms go through it, taking precedence over `ALCEDO_PROXY_CN` | none |
| `ALCEDO_RELAY_TOKEN` | Auth token for the relay; must match `TOKEN` in the relay function | none |
| `ALCEDO_BILI_COOKIE` | Bilibili login cookie; available quality depends on account and content permissions | none |
| `ALCEDO_XHS_COOKIE` | Xiaohongshu cookie for requests that require sign-in | none |
| `ALCEDO_DOUYIN_COOKIE` | Douyin cookie; when set, no anonymous identity is fetched | none |
| `ALCEDO_YOUTUBE_COOKIE` | YouTube cookie for requests that require sign-in; does not guarantee passing bot checks | none |
| `ALCEDO_SIGNER_DOUYIN` | Optional Douyin signer; see the [signer protocol](docs/signers.en.md) | none |
| `ALCEDO_CONNECT_TIMEOUT` | Connection timeout in seconds | `8` |
| `ALCEDO_REQUEST_TIMEOUT` | Per-request timeout in seconds | `20` |
| `ALCEDO_TOTAL_TIMEOUT` | Whole-parse timeout in seconds | `45` |
| `ALCEDO_MAX_REDIRECTS` | Maximum redirect count | `8` |
| `ALCEDO_MAX_BODY_BYTES` | Response body cap | `16777216` |
| `ALCEDO_SSRF_DNS` | Set to `0` to disable private-address blocking at the DNS layer | `1` |
| `ALCEDO_CACHE_TTL` | Result cache TTL in seconds; `0` disables. See [result cache](#result-cache) | `300` |
| `ALCEDO_CACHE_CAPACITY` | Maximum cached entries | `512` |
| `ALCEDO_HEDGE_AFTER_MS` | Hedged-request threshold; `0` disables. See [hedged requests](#hedged-requests) | `0` |

Proxy, cookie and `ALCEDO_SSRF_DNS` settings accept their corresponding legacy
`PARSE_VIDEO_*` names. Timeout, body size, redirect and signer settings use the
`ALCEDO_*` names above. Signers read the environment separately and are not part
of `Config`.

## Request controls and access restrictions

Rate limiting and circuit breaking are enabled per platform. Consecutive
platform-side failures temporarily pause requests with increasing cooldowns;
errors such as deleted content do not count toward the breaker. Some extraction
paths obtain and cache guest identities.

Douyin can use an external signer; see the [configuration and JSON protocol](docs/signers.en.md).
When a signer is unset or fails, extraction continues without its signature.
Whether the request succeeds still depends on the platform response.

See the [design notes](docs/design.en.md) for connection reuse, platform dispatch
and address checks.

## Result cache

On by default with a 5-minute TTL. Popular content gets parsed repeatedly in short
windows; a cache hit takes **0 ms**.

The critical constraint is **never serving an already-dead URL** — that's worse than
being slow, because the user gets a 403 and no reason to retry. So the TTL isn't a
guess; it's read off the direct URL itself:

| Platform | Where the expiry lives | Measured lifetime |
| --- | --- | --- |
| Douyin | A hex segment in the path | ~80 minutes |
| Bilibili | `deadline` parameter | 2 hours |
| Others | `x-expires` / `expire` / `oe` parameters | varies |

We take the **earliest** expiry across every URL in the result, subtract a 2-minute
safety margin, then clamp to the configured cap. URLs close to expiry aren't cached
at all.

The cap is short (5 minutes by default) because a live URL doesn't mean the
**content** is still there — it may have been deleted or made private. And the
returns diminish fast: at 100 parses per hour for one link, 5 minutes already
absorbs about 92% of them.

Set `ALCEDO_CACHE_TTL=0` when you need a fresh result every time. As a library you
can reach the cache directly:

```rust
let cache = alcedo::result_cache();
println!("{} entries", cache.len());
cache.clear();   // after a platform change or a cookie swap
```

## Hedged requests

If the primary request hasn't returned within a threshold, send a duplicate and take
whichever answers first. **Off by default.**

Hedging only helps when the slowness comes from backend instance variance. When it
comes from a shared path (a local proxy, a cross-border link), the duplicate hits the
same bottleneck. Measured here through a proxy: about 10% off p90, within noise —
while the slower fraction of requests doubles. On a rate-limited platform, extra
requests are themselves a risk.

It may pay off on a direct connection, but **measure before enabling**:

```bash
ALCEDO_HEDGE_AFTER_MS=0   cargo run --release -p alcedo --example bench -- <url> 30
ALCEDO_HEDGE_AFTER_MS=300 cargo run --release -p alcedo --example bench -- <url> 30
```

Compare p90 between the two. Put the threshold between p50 and p90.

## Limitations

- Platform support does not imply support for every content type. Sign-in status, region and API changes affect results.
- The library returns media information and URLs. Callers handle downloading, HLS processing and merging separate audio and video tracks.
- Watermarks, quality and codecs depend on the media URLs the platform provides. Some content has only galleries, audio or separate tracks.
- Rate limiting, guest identities and external signers do not guarantee access through platform restrictions.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

See the [contributing guide](CONTRIBUTING.en.md) for test strategy, networked smoke
tests and Windows builds. For timing extraction, see [performance measurement](docs/design.en.md#performance-measurement).

## Contributing

PRs are welcome. See the [contributing guide](CONTRIBUTING.en.md) for adding
platforms, code requirements and commit conventions.

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
