<div align="center">

# alcedo

**高性能视频平台解析核心**

给一条分享链接，返回无水印直链、图集、封面、作者和各档清晰度。

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

> *Alcedo* 是翠鸟属的学名。翠鸟不撒网——它停住、看准、垂直扎进去，
> 带着要的那一样东西出来。这个项目做的是同一件事。

## 特性

- **35 个平台**，国内与海外主流站点全覆盖
- **单个二进制，零运行时依赖** —— 不需要 Python、Node 或无头浏览器，解析阶段也不需要 ffmpeg
- **快** —— 进程级连接复用，热路径上 B 站约 250 ms、YouTube 约 1 s
- **无水印** —— 取的是平台自己下发的干净直链
- **图集与实况照片** —— 多图帖、Live Photo 的短视频一并取出
- **完整清晰度阶梯** —— 含 4K / AV1 / H.265 / 纯音频，音视频分离的档位会标明
- **说人话的错误** —— 九类结构化原因，前端可直接照着提示用户
- **内建抗风控** —— 按平台隔离的限流与熔断，默认开启
- **安全** —— 逐跳 SSRF 防护，全项目禁用 `unsafe`

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

`alcedo --list` 打印完整清单和每个平台认的域名。

## 快速开始

### 安装

```bash
git clone https://github.com/hiyufan/alcedo
cd alcedo
cargo install --path crates/alcedo-cli
```

或只构建不安装，产物在 `target/release/alcedo`：

```bash
cargo build --release
```

### 命令行

```bash
alcedo "https://v.douyin.com/xxxxxx/"      # 人类可读摘要
alcedo --json "https://..."                # JSON
alcedo --list                              # 支持的平台
```

App 的分享文案可以整段粘进来，链接会自动被抠出来：

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
    // 构造一次，全程复用——它持有连接池
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

| 原因 | 含义 | 该谁处理 |
| --- | --- | --- |
| `parse` | 平台页面结构变了 | **维护者**，解析器需要更新 |
| `deleted` | 内容已删除 / 私密 / 链接过期 | 用户换个链接 |
| `login` | 平台要求登录 | 站长配 cookie |
| `blocked` | 被风控 / 限流 | 站长换出口或配代理 |
| `restricted` | 平台不对外提供这条数据 | 无解，用 App 打开一般能看 |
| `unsupported` | 不认识这个链接 | 用户换个链接 |
| `network` / `timeout` | 网络问题 | 稍后重试 |
| `empty` | 解析成功但内容为空 | 少见，多半是平台异常 |

这个区分是刻意的：**只有 `parse` 需要有人动手**，其余都是外部状况。

## 输出格式

```jsonc
{
  "video_url": "https://...",     // 浏览器能直接播的那一档
  "cover_url": "https://...",
  "title": "...",
  "music_url": "https://...",     // 背景音乐 / 图集音轨
  "images": [                     // 图集；有视频时为空
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

    // url 为空 + video_url/audio_url 非空 = 音视频分离，需要 ffmpeg 合并
    { "label": "2160p AV1", "url": "", "ext": "mp4", "height": 2160, "codec": "AV1",
      "video_url": "https://...", "audio_url": "https://..." }
  ],
  "video_headers": { "Referer": "https://www.douyin.com/" }
}
```

> [!IMPORTANT]
> `video_headers` 不是可选项。抖音、B 站、小红书、TikTok 的 CDN 都校验 `Referer`，
> 下载直链时必须原样带上，否则一律 403。

## 配置

全部走环境变量，不需要配置文件。

| 变量 | 说明 | 默认 |
| --- | --- | --- |
| `ALCEDO_PROXY` | 所有平台的代理，如 `http://127.0.0.1:7890`、`socks5://...` | 无 |
| `ALCEDO_PROXY_CN` | **只**给国内平台用的代理。境外部署时抖音 / 小红书这些必须走它 | 无 |
| `ALCEDO_BILI_COOKIE` | B 站登录 cookie，不配只能拿到 720p | 无 |
| `ALCEDO_XHS_COOKIE` | 小红书登录 cookie，机房 IP 基本必配 | 无 |
| `ALCEDO_DOUYIN_COOKIE` | 抖音 cookie；配了就不再去领匿名身份 | 无 |
| `ALCEDO_YOUTUBE_COOKIE` | YouTube cookie，撞上机器人校验时需要 | 无 |
| `ALCEDO_SIGNER_<平台>` | 外部签名器，见[下文](#外部签名器) | 无 |
| `ALCEDO_REQUEST_TIMEOUT` | 单请求超时秒数 | `20` |
| `ALCEDO_TOTAL_TIMEOUT` | 整次解析超时秒数 | `45` |
| `ALCEDO_MAX_BODY_BYTES` | 响应体上限 | `16777216` |
| `ALCEDO_SSRF_DNS` | 设为 `0` 关掉 DNS 层内网地址拦截 | `1` |

`PARSE_VIDEO_*` 前缀同样认，用于从旧部署脚本平滑迁移。

## 抗风控

三层，默认开启、不需要配置。

**按平台隔离的限流与熔断。** 每个平台一个令牌桶和一个熔断器。抖音被风控不会影响
正在解析的 YouTube；连续 5 次平台侧失败就暂停该平台，冷却时间指数退避
（15s → 30s → … 封顶 5 分钟），期间快速失败而不是继续去撞。

这里有一个刻意的设计：**只有平台侧的故障才计入熔断**。用户连贴 5 条失效链接会得到
5 个 `deleted`，那是内容没了、不是平台挂了——拿它开熔断器等于让用户的手滑把整个
平台停掉几分钟。只有 `blocked` / `network` / `timeout` / `login` 这类「对面不让我们
进」的信号才算数。

**游客身份。** 部分平台对完全没有 cookie 的请求越来越不客气，同时又提供了免签名的
注册端点。按浏览器该有的样子去领一份匿名身份并缓存，被挡了就丢掉重领。

### 外部签名器

少数平台用了会**定期轮换**的请求签名算法。把这类算法硬编进来，意味着每次轮换都要
重新逆向并发版——它会成为整个仓库维护成本最高、同时最容易失修的一块。

所以这里做成可插拔的提供者：算法放在外部程序或服务里，变了换那个，不用碰也不用
重新编译本仓库。

```bash
ALCEDO_SIGNER_DOUYIN=http://127.0.0.1:9000/sign   # 常驻服务（推荐）
ALCEDO_SIGNER_DOUYIN=cmd:/opt/alcedo/signer       # 子进程，stdin/stdout 走 JSON
```

协议（两种传输一致）：

```jsonc
// 请求
{ "platform": "douyin", "url": "https://...", "query": "aweme_ids=%5B123%5D",
  "user_agent": "Mozilla/5.0 ...", "body": "" }

// 响应，三个字段都可选
{ "query": { "a_bogus": "..." }, "headers": {}, "cookies": { "msToken": "..." } }
```

**没配就是没配**——解析器走免签名路径，不会因此报错。签名器挂了、超时（5 秒）、
吐了非法 JSON，也一律退回免签名路径：它是增强项，不该把本来能走通的路一起带走。

## 设计

**连接池是进程级的。** 一次解析要发 3–4 个请求（短链跳转、接口、页面）。按代理配置
各留一个 HTTP 客户端，TCP 连接、TLS 会话、DNS 结果全程复用。

**分发是静态的。** 平台是编译期已知的闭集，`match` 直接调到具体函数，没有 trait
object，解析路径上一次虚调用都没有。

**HTML 里的 JSON 按括号配对取，不用正则。** `marker\s*=\s*(.*?)</script>` 这种写法有
两个坑：JSON 字符串里出现转义过的 `</script>` 会被提前截断；后面跟着别的 JS 语句时
会把 `;var x=1` 一起吞进去。配对扫描没有这些问题，而且在几百 KB 的页面上快一个量级。

**平台识别按域名后缀，不按子串。** 子串匹配会把 `https://evil.com/?r=www.douyin.com`
认成抖音，然后把抖音的请求头发给攻击者的服务器。

**SSRF 拦在 DNS 解析层。** 解析器会跟着用户给的短链一路跳转，每一跳都是用户控制的
地址。在自定义 resolver 里拒绝内网 IP，既省掉一次 `getaddrinfo`，又没有「检查完到
真正连接之间 DNS 记录被换掉」的时间差。

**YouTube 不跑 JS。** 网页客户端返回的地址带 `signatureCipher`，要执行下发的混淆 JS
才能还原，还额外需要 PO Token。而 VisionOS / iOS / Android VR / TV 这些客户端返回的
是**已经签好名的直链**——同样的内容，零 JS、零 token。哪天某个客户端被加了限制，
挪一下客户端顺序就行。

**B 站直接取 DASH。** `playurl` 带上 `fnval=4048` 就会返回分离的音视频轨，自己列出
档位交给上层合并，一次请求拿到 4K / AV1 / H.265。

## 性能

同一台机器、同一条链接、同一个进程内连续解析 6 次：

| 平台 | 首次（含 DNS + TLS 握手） | 热连接中位 |
| --- | --- | --- |
| 哔哩哔哩 | 412 ms | **256 ms** |
| 梨视频 | — | **216 ms** |
| AcFun | — | **412 ms** |
| YouTube | — | **982 ms** |

连接复用的收益在首次与后续的差距上看得很清楚。自己复现：

```bash
cargo run --release -p alcedo --example bench -- <url> 10
```

> 网络抖动会盖过不少差异，这几个数字是同一时段连续跑出来的，换时间或换网络会浮动。

## 开发

```bash
cargo test --workspace                  # 单元 + 契约测试，全部离线
cargo clippy --workspace --all-targets  # lint 基线见 Cargo.toml 的 [workspace.lints]
cargo fmt --all

cargo test --workspace -- --ignored     # 联网的冒烟测试（会真的打平台接口）
```

### 测试策略

两类测试回答的问题不一样，所以分开：

- **单元 + 契约测试**（188 个，全部离线）：给定这份响应，解析逻辑对不对
- **冒烟测试**（标了 `#[ignore]`，每天定时跑）：平台今天还是不是这么返回的

单元测试用的是固定样本，平台改了页面结构它们照样全绿——只有真去打一次线上接口才
知道解析器是不是还管用。所以定时冒烟是这个项目的**平台改版早期预警**：红了自动开
issue，恢复了自动关。

### lint 基线

规则写在根 `Cargo.toml` 的 `[workspace.lints]` 里，不是靠 CI 脚本传参数——本地和 CI
跑的是同一套，不会出现「我这儿是绿的」。

当前状态：`clippy::all` 为 **deny**，外加一组经过筛选的 pedantic 规则，**零 warning**。
`unsafe_code = "forbid"`，`missing_docs = "warn"`。

选进来的规则各有出处，都是在这个项目里真抓到过问题的：

| 规则 | 它抓到过什么 |
| --- | --- |
| `case_sensitive_file_extension_comparisons` | `.ends_with(".webp")` 漏掉 CDN 返回的 `.WEBP` |
| `cast_possible_truncation` | 十几处绕 f64 转整数，大数丢精度 |
| `unnecessary_wraps` | 永远返回 `Ok` 的函数，逼调用方多写一层 `?` |
| `too_many_lines` | 一个 137 行的函数（已拆） |

### Windows 上构建

需要一个可用的链接器。装了 Visual Studio 生成工具就用默认的 MSVC 工具链；
没装的话走 GNU 工具链更省事：

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup set default-host x86_64-pc-windows-gnu
winget install -e --id BrechtSanders.WinLibs.POSIX.MSVCRT   # 提供 dlltool / as
# 把它的 mingw64\bin 加进 PATH
```

rustup 自带的 mingw 是精简版，缺 `dlltool` 依赖的 `as`，部分 crate 会链接失败。

## 贡献

欢迎 PR。加新平台的步骤、几条硬规矩和提交信息规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。

发现漏洞请走[私密报告](https://github.com/hiyufan/alcedo/security/advisories/new)，
别开公开 issue，详见 [SECURITY.md](SECURITY.md)。

## 免责声明

本项目只负责从平台公开下发的数据里取出媒体地址，不破解加密内容、不绕过付费墙。
取到的内容版权归原作者所有，请遵守各平台的服务条款，仅在合理使用范围内使用。

## 许可

[MIT](LICENSE)
