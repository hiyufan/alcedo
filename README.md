# alcedo

视频平台解析核心。给一条分享链接，返回无水印直链、图集、封面、作者和各档清晰度。

> *Alcedo* 是翠鸟属的学名。翠鸟不撒网——它停住、看准、垂直扎进去，
> 带着要的那一样东西出来。这个项目做的是同一件事。

**35 个平台，纯 Rust，零外部进程依赖。** 不调 yt-dlp、不跑 JS 引擎、不需要 ffmpeg
就能完成解析（合并音视频轨时才需要 ffmpeg，那是下载阶段的事）。

```console
$ alcedo https://www.youtube.com/watch?v=dQw4w9WgXcQ
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

`alcedo --list` 打印完整清单和每个平台认的域名。

## 用起来

### 命令行

```bash
cargo install --path crates/alcedo-cli      # 装出 `alcedo`

alcedo "https://v.douyin.com/xxxxxx/"      # 摘要
alcedo --json "https://..."                # JSON
alcedo --list                              # 支持的平台
```

分享文案可以整段粘进来，链接会自己被抠出来：

```bash
alcedo "7.99 复制打开抖音，看看【作者】的作品 https://v.douyin.com/iRNBho6u/ 很好看"
```

### 当库用

```rust
let client = alcedo::Client::new()?;          // 构造一次，全程复用（它持有连接池）
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
    Err(e) if e.reason == alcedo::Reason::Login => { /* 提示站长配 cookie */ }
    Err(e) if e.reason == alcedo::Reason::Deleted => { /* 内容没了 */ }
    Err(e) => eprintln!("[{}] {}", e.reason, e),
    Ok(info) => { /* ... */ }
}
```

原因取值：`deleted` `login` `blocked` `unsupported` `network` `timeout` `empty`
`restricted` `parse`。其中只有 **`parse`** 意味着解析器需要更新，其余都是外部状况——
这个区分是刻意的：站长只需要盯 `parse`。

## 配置

全部走环境变量。`ALCEDO_*` 是正式名字，`PARSE_VIDEO_*` 同样认（兼容 Python 版的部署脚本）。

| 变量 | 作用 |
| --- | --- |
| `ALCEDO_PROXY` | 所有平台的代理，如 `http://127.0.0.1:7890`、`socks5://...` |
| `ALCEDO_PROXY_CN` | **只**给国内平台用的代理。境外部署时抖音 / 小红书这些必须走它 |
| `ALCEDO_BILI_COOKIE` | B 站登录 cookie，不配只能拿到 720p |
| `ALCEDO_XHS_COOKIE` | 小红书登录 cookie，机房 IP 基本必配 |
| `ALCEDO_DOUYIN_COOKIE` / `ALCEDO_YOUTUBE_COOKIE` | 同上，按需 |
| `ALCEDO_REQUEST_TIMEOUT` | 单请求超时秒数，默认 20 |
| `ALCEDO_TOTAL_TIMEOUT` | 整次解析超时秒数，默认 45 |
| `ALCEDO_MAX_BODY_BYTES` | 响应体上限，默认 16 MiB |
| `ALCEDO_SSRF_DNS=0` | 关掉 DNS 层的内网地址拦截（只有自建内网镜像时才需要） |
| `ALCEDO_SIGNER_<平台>` | 外部签名器，见下文。如 `ALCEDO_SIGNER_DOUYIN=http://127.0.0.1:9000/sign` |

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

## 抗风控

三层，都是默认开启、不需要配置的：

**按平台隔离的限流 + 熔断。** 每个平台一个令牌桶和一个熔断器。抖音被风控不会
影响正在解析的 YouTube；连续 5 次平台侧失败就暂停该平台，冷却时间指数退避
（15s → 30s → … 封顶 5 分钟），期间快速失败而不是继续去撞。

这里有一个刻意的设计：**只有平台侧的故障才计入熔断**。用户连贴 5 条失效链接
会得到 5 个 `deleted`，那是内容没了、不是平台挂了——拿它开熔断器等于让用户的
手滑把整个平台停掉几分钟。只有 `blocked` / `network` / `timeout` / `login`
这类"对面不让我们进"的信号才算数。

**游客身份。** 字节系接口对完全没有 cookie 的请求越来越不客气，同时又提供了
一个免签名的注册端点。我们按浏览器该有的样子去领一份匿名 `ttwid` 并缓存半小时，
被挡了（401/403/412）就丢掉重领。

**外部签名器（可选）。** 见下。

## 关于 a_bogus：为什么不写进来

抖音的 `a_bogus` 是 SM3 管线 + VM 混淆 + 熵字节的产物，而且**大约每季度轮换
一次算法**。把它硬编进 Rust 的后果很具体：每三个月要重新逆向一遍、重新发版，
否则抖音解析直接死。对一个没有全职维护者的项目，这会是整个仓库维护成本最高、
同时最容易失修的一块。

所以这里用 yt-dlp 应对 PO Token 的同一个思路——**提供者框架**。签名算法放在
外部程序或服务里，算法变了换那个，不用碰也不用重新编译本仓库：

```bash
ALCEDO_SIGNER_DOUYIN=http://127.0.0.1:9000/sign   # 常驻服务（推荐）
ALCEDO_SIGNER_DOUYIN=cmd:/opt/alcedo/douyin-signer    # 子进程，stdin/stdout 走 JSON
```

协议（两种传输一样）：

```jsonc
// 请求
{ "platform": "douyin", "url": "https://...", "query": "aweme_ids=%5B123%5D",
  "user_agent": "Mozilla/5.0 ...", "body": "" }
// 响应，三个字段都可选
{ "query": { "a_bogus": "..." }, "headers": {}, "cookies": { "msToken": "..." } }
```

**没配就是没配**——解析器走免签名路径，不会因此报错。签名器挂了、超时了（5 秒）、
吐了非法 JSON，也一律退回免签名路径：它是增强项，不该把本来能走通的路一起带走。

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
（`cargo run --release -p alcedo --example bench -- <url> 6`）：

| 链接 | Python 中位 | alcedo 中位 | |
| --- | --- | --- | --- |
| B 站 `BV1GJ411x7h7` | 1139 ms | **256 ms** | 快 4.4×，档位 3 → 6 |
| 梨视频 `detail_1742158` | 120 ms | 216 ms | **更慢**，见下 |

B 站那条差距主要来自架构：Python 版要另起一个 yt-dlp 进程才能列出高清档位，
alcedo 直接向 `playurl` 要 DASH。连接复用也看得见——首次 412 ms（含 DNS + TLS
握手），之后稳定在 243–275 ms。

梨视频这条我们**更慢，而且是故意的**：平台的 `videoStatus` 接口不返回标题，
alcedo 额外并发拉一次详情页（只读前 24 KB）把标题补上，Python 版则直接给一个
空标题。多出来的时间换的是一个能用的结果。

> 网络抖动会盖过不少差异，这几个数字是同一时段连续跑出来的，换时间/换网络会浮动。
> 想自己复现：`cargo run --release -p alcedo --example bench -- <url> 10`。

## 和同类项目比

同一台机器、同一条链接，编译型对编译型（CLI 冷启动 × 4，取中位）：

| | B站 | YouTube | AcFun | Vimeo |
| --- | --- | --- | --- | --- |
| **alcedo** | **430 ms** | **982 ms** | **412 ms** | 1485 ms |
| yt-dlp (Python) | 1770 ms | 4124 ms | — | — |
| lux (Go, v0.24.1) | 4910 ms | ✗ 崩溃 | ✗ panic | ✗ 崩溃 |

lux 那三个失败是真的：AcFun 直接 `panic: index out of range`（就是本项目修掉
的那次页面结构变更），YouTube 还在抓网页 player JS。原因不在代码质量，在维护
节奏——lux 最后一次提交是 2025-12，最后一次发布是 2024-05。

**但要说清楚我们输在哪：**

- **覆盖面差两个数量级**。yt-dlp 有 1752 个提取器，我们 35 个平台。不在一个量级。
- **抖音 / TikTok 的对抗能力不是最强的**。`Douyin_TikTok_Download_API` 实现了
  完整的 a_bogus 签名和无头浏览器身份池；我们走的是免签名公开接口 + 可插拔签名器。
- **零运行记录**。yt-dlp 每月 18 次提交不是因为代码差，是因为平台每月都在变。

定位是"这 35 个平台上最快、错误信息最清楚、不会崩"，不是"取代 yt-dlp"。

## 开发

```bash
cargo test --workspace          # 单元测试，全部离线
cargo clippy --workspace --all-targets
cargo fmt --all

cargo test --workspace -- --ignored    # 联网的冒烟测试（会真的打平台接口）
```

### CI

- `ci.yml`：每次 push / PR 跑 fmt + clippy + 测试 + 三平台构建 + 依赖漏洞审计
- `smoke.yml`：**每天定时**对着线上平台跑一遍真实解析，红了自动开 issue、恢复了自动关

第二个是这个项目的平台改版早期预警。单元测试用的是固定样本，平台改了页面结构
它们照样全绿——只有真去打一次线上接口才知道解析器是不是还管用。lux 就是活例子。

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

1. `crates/alcedo/src/model.rs` 的 `Source` 加一项，补 `as_str` / `display_name` /
   `is_cn`
2. `crates/alcedo/src/registry.rs` 的 `DOMAINS` 登记域名
3. 写 `crates/alcedo/src/parsers/<name>.rs`，导出
   `pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo>`
4. `parsers/mod.rs` 里 `pub mod` + `dispatch` 加分支
5. 用真实响应造一份最小 JSON/HTML 写单元测试——**不要**在单测里发网络请求

单请求就能搞定的平台直接加进 `parsers/simple.rs`，别单开文件。

## 许可

MIT
