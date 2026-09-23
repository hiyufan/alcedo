# pvcore

视频平台解析核心。给一条分享链接，返回无水印直链、图集、封面、作者和各档清晰度。

**35 个平台，纯 Rust，零外部进程依赖。** 不调 yt-dlp、不跑 JS 引擎、不需要 ffmpeg
就能完成解析（合并音视频轨时才需要 ffmpeg，那是下载阶段的事）。

```console
$ pv https://www.youtube.com/watch?v=dQw4w9WgXcQ
平台  : YouTube
标题  : Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)
作者  : Rick Astley
时长  : 213.0s
清晰度:
  - 2160p AV1  243.8 MB  (需合并音视频)
  - 2160p VP9  362.0 MB  (需合并音视频)
  - 1080p      84.3 MB   (需合并音视频)
  ...
  - 仅音频     3.4 MB
```

---

## 支持的平台

| 国内 | | 海外 |
| --- | --- | --- |
| 抖音 · 快手 · 小红书 · 哔哩哔哩 | 微博 · 微视 · 绿洲 · 最右 | YouTube · TikTok · X (Twitter) |
| 西瓜视频 · 皮皮虾 · 皮皮搞笑 | 虎牙 · AcFun · 度小视 | Instagram · Threads · Facebook |
| 腾讯视频 · 搜狐视频 · 央视网 | 梨视频 · 逗拍 · 美拍 | Vimeo · Twitch · Reddit |
| 好看视频 · 新片场 · 六间房 | 全民K歌 | Pinterest · Dailymotion |

`pv --list` 打印完整清单和每个平台认的域名。

## 用起来

### 命令行

```bash
cargo install --path crates/pvcli      # 装出 `pv`

pv "https://v.douyin.com/xxxxxx/"      # 摘要
pv --json "https://..."                # JSON
pv --list                              # 支持的平台
```

分享文案可以整段粘进来，链接会自己被抠出来：

```bash
pv "7.99 复制打开抖音，看看【作者】的作品 https://v.douyin.com/iRNBho6u/ 很好看"
```

### 当库用

```rust
let client = pvcore::Client::new()?;          // 构造一次，全程复用（它持有连接池）
let info = client.parse("https://v.douyin.com/xxxxxx/").await?;

println!("{} / {}", info.title, info.author.name);
println!("直链: {}", info.video_url);
for f in &info.formats {
    println!("  {} {}", f.label, if f.needs_merge() { "(需合并)" } else { "" });
}
```

失败时拿到的是带原因的结构化错误，前端可以直接照着说人话：

```rust
match client.parse(url).await {
    Err(e) if e.reason == pvcore::Reason::Login => { /* 提示站长配 cookie */ }
    Err(e) if e.reason == pvcore::Reason::Deleted => { /* 内容没了 */ }
    Err(e) => eprintln!("[{}] {}", e.reason, e),
    Ok(info) => { /* ... */ }
}
```

原因取值：`deleted` `login` `blocked` `unsupported` `network` `timeout` `empty`
`restricted` `parse`。其中只有 **`parse`** 意味着解析器需要更新，其余都是外部状况——
这个区分是刻意的：站长只需要盯 `parse`。

## 配置

全部走环境变量。`PV_*` 是正式名字，`PARSE_VIDEO_*` 同样认（兼容 Python 版的部署脚本）。

| 变量 | 作用 |
| --- | --- |
| `PV_PROXY` | 所有平台的代理，如 `http://127.0.0.1:7890`、`socks5://...` |
| `PV_PROXY_CN` | **只**给国内平台用的代理。境外部署时抖音 / 小红书这些必须走它 |
| `PV_BILI_COOKIE` | B 站登录 cookie，不配只能拿到 720p |
| `PV_XHS_COOKIE` | 小红书登录 cookie，机房 IP 基本必配 |
| `PV_DOUYIN_COOKIE` / `PV_YOUTUBE_COOKIE` | 同上，按需 |
| `PV_REQUEST_TIMEOUT` | 单请求超时秒数，默认 20 |
| `PV_TOTAL_TIMEOUT` | 整次解析超时秒数，默认 45 |
| `PV_MAX_BODY_BYTES` | 响应体上限，默认 16 MiB |
| `PV_SSRF_DNS=0` | 关掉 DNS 层的内网地址拦截（只有自建内网镜像时才需要） |

## 结果长什么样

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
  "width": 1080, "height": 1920,
  "formats": [
    { "label": "1080p",     "url": "https://...", "ext": "mp4", "height": 1080,
      "filesize": 8400000,  "codec": "" },
    // url 为空 + video_url/audio_url 非空 = 音视频分离，需要 ffmpeg 合并
    { "label": "2160p AV1", "url": "", "ext": "mp4", "height": 2160, "codec": "AV1",
      "video_url": "https://...", "audio_url": "https://..." }
  ],
  "video_headers": { "Referer": "https://www.douyin.com/" }   // 下载直链时必须原样带上
}
```

> `video_headers` 不是可选项。抖音、B 站、小红书、TikTok 的 CDN 都校验 Referer，
> 缺了一律 403。

## 设计上的取舍

**连接池是进程级的。** 一次抖音解析要发 3–4 个请求（短链跳转、接口、页面）。按代理
配置各留一个 `reqwest::Client`，TCP 连接、TLS 会话、DNS 结果全程复用。

**分发是静态的。** 平台是编译期已知的闭集，`match` 直接调到具体函数，没有 trait
object，解析路径上一次虚调用都没有。

**HTML 里的 JSON 按括号配对取，不用正则。** `marker\s*=\s*(.*?)</script>` 这种写法
有两个坑：JSON 字符串里出现转义过的 `</script>` 会被提前截断；后面跟着别的 JS 语句时
会把 `;var x=1` 一起吞进去。配对扫描没有这些问题，而且在几百 KB 的页面上快一个量级。

**平台识别按域名后缀，不按子串。** `domain in url` 这种判断会把
`https://evil.com/?r=www.douyin.com` 认成抖音，然后把抖音的请求头发给攻击者的服务器。

**SSRF 拦在 DNS 解析层。** 解析器会跟着用户给的短链一路跳转，每一跳都是用户控制的
地址。在自定义 resolver 里拒绝内网 IP，既省掉一次 `getaddrinfo`，又没有"检查完到真正
连接之间 DNS 记录被换掉"的时间差。

**YouTube 不跑 JS。** web 客户端返回的地址带 `signatureCipher`，要执行 YouTube 下发的
混淆 JS 才能还原，2024 年之后还额外要 PO Token。而 VisionOS / iOS / Android VR / TV
这些客户端返回的是**已经签好名的直链**——同样的内容，零 JS、零 token。哪天某个客户端
被加了限制，挪一下 `CLIENTS` 的顺序就行。

**B 站不外挂 yt-dlp。** `playurl` 带上 `fnval=4048` 直接给 DASH 分离轨，自己列档位就行，
不需要为了高清晰度另起一个进程。

## 实测延迟

同一台机器、同一条链接、同一个进程内连续解析 6 次，对比原 Python 版
（`cargo run --release -p pvcore --example bench -- <url> 6`）：

| 链接 | Python 中位 | pvcore 中位 | |
| --- | --- | --- | --- |
| B 站 `BV1GJ411x7h7` | 1139 ms | **256 ms** | 快 4.4×，档位 3 → 6 |
| 梨视频 `detail_1742158` | 120 ms | 216 ms | **更慢**，见下 |

B 站那条差距主要来自架构：Python 版要另起一个 yt-dlp 进程才能列出高清档位，
pvcore 直接向 `playurl` 要 DASH。连接复用也看得见——首次 412 ms（含 DNS + TLS
握手），之后稳定在 243–275 ms。

梨视频这条我们**更慢，而且是故意的**：平台的 `videoStatus` 接口不返回标题，
pvcore 额外并发拉一次详情页（只读前 24 KB）把标题补上，Python 版则直接给一个
空标题。多出来的时间换的是一个能用的结果。

> 网络抖动会盖过不少差异，这几个数字是同一时段连续跑出来的，换时间/换网络会浮动。
> 想自己复现：`cargo run --release -p pvcore --example bench -- <url> 10`。

## 开发

```bash
cargo test --workspace          # 单元测试，全部离线
cargo clippy --workspace --all-targets
cargo fmt --all

cargo test --workspace -- --ignored    # 联网的冒烟测试（会真的打平台接口）
```

### lint 基线

规则写在根 `Cargo.toml` 的 `[workspace.lints]` 里，不是靠 CI 脚本传参数——本地和
CI 跑的是同一套，不会出现"我这儿是绿的"。当前状态：**`clippy::all` 为 deny，
外加一组在这个项目里真抓到过 bug 的 pedantic 规则，零 warning。**

选进来的几条各有出处：

| 规则 | 它抓到过什么 |
| --- | --- |
| `case_sensitive_file_extension_comparisons` | `.ends_with(".webp")` 漏掉 CDN 返回的 `.WEBP` |
| `cast_possible_truncation` | 十几处 `num_at(..) as i64` 绕 f64 转整数，大数丢精度 |
| `unnecessary_wraps` | 两个永远返回 `Ok` 的函数，逼调用方多写一层 `?` |
| `too_many_lines` | 唯一一个 137 行的函数（已拆） |
| `unsafe_code = "forbid"` | 全项目零 unsafe，锁死；解析器吃的是不可信输入 |

明确关掉的是纯风格偏好（`module_name_repetitions`、`missing_errors_doc` 等），
它们会产生 200 多条噪音把真信号盖掉。

### Windows 上的构建

需要一个能用的链接器。装了 Visual Studio 生成工具就用默认的 MSVC 工具链；没装的话
走 GNU 工具链更省事：

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup set default-host x86_64-pc-windows-gnu
winget install -e --id BrechtSanders.WinLibs.POSIX.MSVCRT   # 提供 dlltool / as
# 把它的 mingw64\bin 加进 PATH
```

（rustup 自带的 mingw 是精简版，缺 `dlltool` 依赖的 `as`，`getrandom` 这类用
raw-dylib 的 crate 会链接失败。）

### 加一个平台

1. `crates/pvcore/src/model.rs` 的 `Source` 加一项，补 `as_str` / `display_name` /
   `is_cn`
2. `crates/pvcore/src/registry.rs` 的 `DOMAINS` 登记域名
3. 写 `crates/pvcore/src/parsers/<name>.rs`，导出
   `pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo>`
4. `parsers/mod.rs` 里 `pub mod` + `dispatch` 加分支
5. 用真实响应造一份最小 JSON/HTML 写单元测试——**不要**在单测里发网络请求

单请求就能搞定的平台直接加进 `parsers/simple.rs`，别单开文件。

## 许可

MIT
