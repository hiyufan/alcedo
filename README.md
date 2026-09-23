<div align="center">

# alcedo

**Rust 视频平台解析库与命令行工具**

给一条分享链接，返回媒体地址、图集、封面、作者和可用清晰度。

[![CI](https://github.com/hiyufan/alcedo/actions/workflows/ci.yml/badge.svg)](https://github.com/hiyufan/alcedo/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Platforms](https://img.shields.io/badge/platforms-35-green.svg)](#支持的平台)

[English](README.en.md) · **简体中文**

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

*Alcedo* 是翠鸟属的学名。

## 特性

- **35 个平台**：支持下列国内与海外站点。
- **原生 Rust**：内置解析路径不依赖 Python、Node 或无头浏览器。
- **统一输出**：视频地址、图集、实况照片和作者信息使用统一数据结构。
- **多清晰度**：返回平台可用的档位，包括部分内容的 4K、AV1、H.265 和纯音频；标明分离的音视频轨。
- **结构化错误**：区分内容不可用、需要登录、限流、网络故障等原因。
- **请求控制**：复用 HTTP 连接，按平台限流与熔断，默认启用内网地址检查。

## 支持的平台

<table>
<tr><th>国内</th><th>海外</th></tr>
<tr valign="top"><td>

抖音 · 快手 · 小红书 · 哔哩哔哩<br>
微博 · 微视 · 绿洲 · 最右<br>
西瓜视频 · 皮皮虾 · 皮皮搞笑<br>
虎牙 · AcFun · 度小视 · 梨视频<br>
腾讯视频 · 搜狐视频 · 央视网<br>
逗拍 · 美拍 · 好看视频<br>
新片场 · 六间房 · 全民K歌

</td><td>

YouTube · TikTok · X (Twitter)<br>
Instagram · Threads · Facebook<br>
Vimeo · Twitch · Reddit<br>
Pinterest · Dailymotion

</td></tr>
</table>

`alcedo --list` 打印完整清单和每个平台支持的域名。可用内容和清晰度受登录状态、
地区、网络出口及平台接口变化影响，详见[使用限制](#使用限制)。

## 快速开始

### 安装

需要 Rust 工具链。Windows 推荐使用 MSVC 工具链并安装 Visual Studio Build Tools
的 C++ 构建工具；其他工具链和排障见[Windows 构建](CONTRIBUTING.md#windows-构建)。

```bash
git clone https://github.com/hiyufan/alcedo
cd alcedo
cargo install --path crates/alcedo-cli
```

或只构建不安装，产物在 `target/release/alcedo`（Windows 为 `alcedo.exe`）：

```bash
cargo build --release
```

### 命令行

```bash
alcedo "https://v.douyin.com/xxxxxx/"      # 人类可读摘要
alcedo --json "https://..."                # JSON
alcedo --list                              # 支持的平台
```

也可以传入整段分享文案，程序会提取其中的链接：

```bash
alcedo "7.99 复制打开抖音，看看【作者】的作品 https://v.douyin.com/iRNBho6u/ 很好看"
```

### 作为库使用

```toml
[dependencies]
alcedo = { git = "https://github.com/hiyufan/alcedo" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use alcedo::Client;

#[tokio::main]
async fn main() -> alcedo::Result<()> {
    // 从环境变量读取配置，可复用这个 Client
    let client = Client::new()?;
    let info = client.parse("https://v.douyin.com/xxxxxx/").await?;

    println!("{} / {}", info.title, info.author.name);
    println!("直链: {}", info.video_url);
    for f in &info.formats {
        println!("  {} {}", f.label, if f.needs_merge() { "(需合并)" } else { "" });
    }
    Ok(())
}
```

### 错误处理

失败时拿到的是带原因的结构化错误：

```rust
use alcedo::Reason;

match client.parse(url).await {
    Ok(info) => { /* ... */ }
    Err(e) if e.reason == Reason::Deleted => { /* 内容没了 */ }
    Err(e) if e.reason == Reason::Login   => { /* 提示站长配 cookie */ }
    Err(e) => eprintln!("[{}] {}", e.reason, e),
}
```

| 原因 | 含义 | 建议处理 |
| --- | --- | --- |
| `parse` | 返回数据不符合解析器预期 | 维护者检查接口变化和解析逻辑 |
| `deleted` | 内容已删除 / 私密 / 链接过期 | 用户换个链接 |
| `login` | 平台要求登录 | 站长配 cookie |
| `blocked` | 被风控 / 限流 | 降低请求频率，检查网络出口或代理 |
| `restricted` | 平台限制访问这条内容 | 检查账号、地区或内容权限 |
| `unsupported` | 不认识这个链接 | 用户换个链接 |
| `network` / `timeout` | 网络问题 | 稍后重试 |
| `empty` | 未提取到媒体内容 | 检查链接；持续出现时反馈给维护者 |

## 输出格式

以下展示主要字段；空字符串、空集合和零值的部分字段会省略。

```jsonc
{
  "video_url": "https://...",     // 默认媒体地址；图集或只有分离轨时可能为空
  "cover_url": "https://...",
  "title": "...",
  "music_url": "https://...",     // 背景音乐 / 图集音轨
  "images": [                     // 图集字段示例，按内容类型返回
    { "url": "https://...", "live_photo_url": "https://..." }   // 实况照片带短视频
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

    // 分离轨使用 video_url/audio_url；需要合并后播放音视频
    { "label": "2160p AV1", "url": "", "ext": "mp4", "height": 2160, "codec": "AV1",
      "video_url": "https://...", "audio_url": "https://..." }
  ],
  "video_headers": { "Referer": "https://www.douyin.com/" }
}
```

> [!IMPORTANT]
> 下载媒体时应透传返回的 `video_headers`。部分 CDN 会校验 `Referer` 等请求头，
> 缺少这些头可能导致 403。

## 配置

CLI 和 `Client::new()` 从环境变量读取配置，不需要配置文件。作为库使用时，也可以
通过 `Client::with_config(Config)` 指定配置：

```rust
use std::time::Duration;
use alcedo::{Client, Config};

let client = Client::with_config(Config {
    total_timeout: Duration::from_secs(60),
    ..Config::default()
});
```

`Config::default()` 不读取环境变量；需要在环境配置上修改时，使用 `Config::from_env()`。

| 变量 | 说明 | 默认 |
| --- | --- | --- |
| `ALCEDO_PROXY` | 所有平台的代理，如 `http://127.0.0.1:7890`、`socks5://...` | 无 |
| `ALCEDO_PROXY_CN` | 国内平台优先使用的代理，未设置时使用 `ALCEDO_PROXY` | 无 |
| `ALCEDO_BILI_COOKIE` | B 站登录 cookie，可用清晰度取决于账号和内容权限 | 无 |
| `ALCEDO_XHS_COOKIE` | 小红书登录 cookie，用于需要登录的访问 | 无 |
| `ALCEDO_DOUYIN_COOKIE` | 抖音 cookie；配了就不再去领匿名身份 | 无 |
| `ALCEDO_YOUTUBE_COOKIE` | YouTube cookie，用于需要登录的访问；不保证消除机器人校验 | 无 |
| `ALCEDO_SIGNER_DOUYIN` | 可选的抖音签名器，见[签名器协议](docs/signers.md) | 无 |
| `ALCEDO_CONNECT_TIMEOUT` | 建立连接超时秒数 | `8` |
| `ALCEDO_REQUEST_TIMEOUT` | 单请求超时秒数 | `20` |
| `ALCEDO_TOTAL_TIMEOUT` | 整次解析超时秒数 | `45` |
| `ALCEDO_MAX_REDIRECTS` | 最大重定向次数 | `8` |
| `ALCEDO_MAX_BODY_BYTES` | 响应体上限 | `16777216` |
| `ALCEDO_SSRF_DNS` | 设为 `0` 关掉 DNS 层内网地址拦截 | `1` |

代理、cookie 和 `ALCEDO_SSRF_DNS` 兼容对应的 `PARSE_VIDEO_*` 旧名称；超时、
响应大小、重定向和签名器设置使用表中的 `ALCEDO_*` 名称。签名器单独读取环境变量，
不属于 `Config`。

## 请求控制与访问限制

默认按平台限流与熔断。连续的平台侧失败会暂时暂停该平台的请求，并逐步延长冷却时间；
内容已删除等错误不计入熔断。部分解析路径会获取并缓存游客身份。

抖音可接入外部签名器，配置和 JSON 协议见[签名器文档](docs/signers.md)。未配置或调用
失败时会继续尝试不带签名的请求，最终是否成功仍取决于平台响应。

连接复用、平台分发和地址检查的实现见[设计说明](docs/design.md)。

## 使用限制

- 支持某个平台不代表支持其全部内容类型；登录状态、地区和平台接口变化会影响解析结果。
- 本项目返回媒体信息和地址，下载、HLS 处理及分离音视频轨的合并由调用方完成。
- 水印、清晰度和编码取决于平台提供的媒体地址；部分内容只有图集、音频或分离的音视频轨。
- 限流、游客身份和外部签名器不能保证通过平台的访问限制。

## 开发

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

测试策略、联网冒烟测试和 Windows 构建说明见[贡献指南](CONTRIBUTING.md)。
测量解析耗时的方法见[性能测量](docs/design.md#性能测量)。

## 贡献

欢迎 PR。添加平台的步骤、代码要求和提交约定见[贡献指南](CONTRIBUTING.md)。

发现漏洞请走[私密报告](https://github.com/hiyufan/alcedo/security/advisories/new)，
别开公开 issue，详见 [SECURITY.md](SECURITY.md)。

## 免责声明

本项目只负责从平台公开下发的数据里取出媒体地址，不破解加密内容、不绕过付费墙。
取到的内容版权归原作者所有，请遵守各平台的服务条款，仅在合理使用范围内使用。

## 许可

[MIT](LICENSE)
