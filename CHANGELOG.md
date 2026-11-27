# 更新日志

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

这一版把「求原图」（hiyufan/shizhen）0.4.0 之后的视频解析修复移植过来，新增今日头条，
并升级有安全公告的依赖。今日头条（微头条短链、头条视频）和抖音的新链接形态用真实链接验证过，
直链能打开；实网冒烟新增两条头条用例。微博 App 分享链接、rednote、tt.site 只有离线样本测试。

### 破坏性变更
- `Source` 新增 `TouTiao`。对 `Source` 做穷尽 `match` 的下游代码需要补上这一项

### 新增
- 今日头条（第 36 个平台）：微头条 `m.toutiao.com/w/<id>` 取帖子里的全部图片、正文和作者；
  头条视频 `/video/<id>` 和西瓜一样已并入抖音，作品 ID 就是 aweme_id，走抖音解析器，不用 yt-dlp；
  App「复制链接」的 `m.toutiao.com/is/` 短链跟到上面两种。头条文章直接说明暂不支持
- 抖音搜索结果页 `so.douyin.com/video/detail?aweme_id=`、不带 www 的 `douyin.com` / `m.douyin.com` 作品链接
- 微博 App 分享的 `mapp.api.weibo.cn/fx/<编号>.html`（跟跳转到对应微博）、`t.cn` 落地的
  `weibo.com/p/230444<mid>` 卡片页
- 微博图集的实况照片：图片带上那段视频（`live_photo_url`）
- 小红书国际版 `rednote.com` 的笔记链接，换成国内站同一篇笔记解析
- TikTok App 分享的 `tt.site` 短链

### 修复
- 小红书跳到 `/404/sec_…` 安全页时报"已删除"并停止重试。同样的跳转多半是临时拦截，现在归为
  `restricted`：带 xsec_token 的提示隔段时间再试，不带的长链接提示在 App 里重新复制完整分享链接；
  300012 / 300013 安全限制归为 `blocked` 并给出错误码；`/website-login/` 登录页归为 `login`
- 小红书拼错误信息时按字节截断页面，中文页面可能切在字符中间而 panic
- 抖音视频封面用的是 300×400 的居中裁切小图，改用同一张封面按原比例缩放的 `cover_original_scale`
- 抖音直播间 `live.douyin.com` 的房间号被当成作品 ID 拿去查接口；直播间和汽水音乐链接现在直接说明
- 抖音 `modal_id` / `aweme_id` 不是数字时不再拿去查接口
- 微博单条链接的 ID 后面粘着别的字符时整段被当成 ID；直播与直播回放链接明确说明不支持，
  直播卡片的播放页地址不再当成视频文件返回
- 好看视频转载作品没有 `mth` 字段时作者为空，改用顶层的作者名与头像

### 依赖
- 升级有 RustSec 公告的依赖：ctutils 0.4.8（RUSTSEC-2026-0385）、zstd 0.14.5 / zstd-safe 8.4.0 /
  zstd-sys 2.2.0（RUSTSEC-2026-0391 / 0398 / 0403 / 0410 等）、url 2.5.9（RUSTSEC-2026-0592）、tokio 1.54.1
- 去掉没有用到的 `regex` 和 `thiserror`

## [0.4.0] - 2026-10-22

这一版主要修 YouTube：机房 / WARP 出口下解析被拦、下载被限速、默认档存成 `.m3u8` 文本，
以及备用客户端交出下不完的地址。都按同作者的「求原图」（hiyufan/shizhen）线上排查结果改，
并逐个完整下载验证。

### 新增
- YouTube 请求带上游客身份（visitorData，取自 `sw.js_data`，进程内缓存、过期后台刷新）。
  不带时机房 / WARP 出口一律报"确认不是机器人"；2026-10-22 经 WARP 实测 3 条受限视频，
  不带全被拦，带上全部解析成功、直链可读。所有客户端都报机器人校验时丢掉这份身份，下次重新领
- `download` 支持 HLS：解析主清单 / 分片清单，8 路并发拉分片按顺序拼接，音视频分开的用 ffmpeg 合并成 mp4；
  默认挑 1080p 以内的 H.264。点播、不加密、整段分片之外的清单（直播、加密、按字节区间切）直接说明不支持
- `download` 的分轨直链下载失败、结果里又有 HLS 时，改下同清晰度的 HLS

### 修复
- `download` / `serve /media` 拉 YouTube 媒体被限速：googlevideo 对一次拉完整个文件的请求限速
  （实测 0.24–0.7 MB/s），改为按 10 MB 分段请求再拼成一条流。直连下载 84 MB 的 1080p
  从 150 秒还没下完变为 2 秒；`/media` 不带 Range 回 200、`bytes=N-` 回 206，一段以内的请求照旧透传
- 配了 `ALCEDO_PROXY` 时 YouTube 媒体下载 403：直链绑定解析时的出口 IP，媒体请求却直连。
  googlevideo 的地址现在和解析走同一个代理，其他 CDN 仍直连
- YouTube 默认档下载存下来的是 `.m3u8` 文本：visionOS 客户端不给音视频合一的直链，默认地址是 HLS 清单。
  现在按 HLS 下载，直连 1080p 约 2 秒
- 媒体请求遇到 302 不跟：googlevideo 的分轨、HLS 分片常先跳到另一台节点。现在手动跟最多 5 跳，每一跳都过地址检查
- YouTube 备用客户端会交出下不完的地址：android_vr 的直链开头能读、完整下载时 403（缺 PO Token），
  却被当成功结果返回；TV 不带播放器签名版本一律 `UNPLAYABLE`。两者移出名单，iOS 只取 HLS
  （它的直链全是加密签名）。2026-10-22 带游客身份逐个完整下载验证
- YouTube 全部客户端失败时报的是最后一个客户端的原因（常常是 TV 的"内容受限"），把"出口被判成机器人"
  盖掉了。现在报优先级最高的客户端的原因
- `download` 合并音视频时成品文件名多出 ` (2)`：文件名掐到 60 个字符，标题一长，临时轨的 `[视频轨]` 后缀被掐掉、
  和成品同名。临时轨改用独立的文件名，合并下载也不再打印临时轨的路径
- `download` 的进度在输出重定向到文件时也照打，日志里全是 `\r`；结束后进度行停在 90%。
  现在只在终端里显示，结束时清掉，结果行接在原处
- `alcedo --list | head` 这类接管道的用法，下游提前关管道时 panic 并打出堆栈。现在和其它 Unix 工具一样
  安静退出，退出码 141

## [0.3.0] - 2026-10-19

这一版主要从同作者的「求原图」（hiyufan/shizhen）线上反馈里移植修复，并把实网冒烟从
7 个平台扩到 18 条用例、开始校验直链真的能打开。

### 破坏性变更
- `VideoInfo` 新增公开字段 `notice`。用结构体字面量构造 `VideoInfo` 的下游代码需要补上
  `..Default::default()`；JSON 输出向后兼容（空时不输出）

### 新增
- `VideoInfo.notice`：平台没给的部分是什么、该怎么办。快手实况（只有照片和配乐）先用上，CLI 打印为"提示"
- 快手网页版 / 落地页链接：`www.kuaishou.com/short-video/<id>`、`live.kuaishou.com/u/<用户>/<id>`、
  `/fw/photo/<id>`、`m.gifshow.com`，以及网页版分享的 `www.kuaishou.com/f/` 短链
- B 站动态（`bilibili.com/opus/<id>`、`t.bilibili.com/<id>`），含实况照片
- B 站多 P：按链接里的 `?p=` 取那一 P，时长、标题、`page_url` 跟着分 P（#30）
- Instagram 多图帖：读帖子页内嵌的整条帖子，每张原图都拿到
- 贴进来的是平台 CDN 上的图片 / 视频文件地址时直接给出结果，不再报"不支持这个站点"
- 微博手机站 `m.weibo.cn`、梨视频列表页 `/video_<id>`、搜狐首页 `<日期>/n<vid>.shtml` 链接
- 全民K歌纯音频翻唱：给音频地址，标题用歌名
- `Req::idempotent()`：只读查询类 POST 声明后，网络错误时和 GET 一样重试一次
- 实网冒烟新增 TikTok、X、Instagram、微博、微博手机站、西瓜、B 站多 P；
  每条都带上解析给的请求头去拉直链前 1KB，必须 2xx

### 修复
- TikTok 直链一律 403：带上作品页种下的 `tt_chain_token` 等 cookie
- `serve /media` 按解析时记下的请求头转发（Referer / UA / Cookie），不再只按 CDN 域名补 Referer；
  名单外的 CDN 补作品页所在站点的 Referer
- Instagram 链接全被报成"内容已删除"：接口 404 时先读帖子页、再退 OG 标签；
  只对部分用户可见的内容归为平台限制
- 西瓜视频全部拿不到：改走抖音接口（西瓜已并入抖音，ID 就是 aweme_id）；抖音短链跳到西瓜时接着解析
- 快手单图作品报"没拿到内容"；图集给 jpg 原图并带配乐；同清晰度的 H.265 档位标出来；落地页偶发空页面重试一次
- fake-ip 代理开了 IPv6 时整个域名被 DNS 检查拒掉：只丢掉内网记录、连剩下的公网地址，全是内网才拒绝（#31）
- 网络错误的说明带上最底层的原因（以前"解析到内网地址 …，已拒绝"会被截掉，只剩"连不上"）
- 只读 POST（微博视频、最右、皮皮搞笑、Twitch、YouTube）遇网络抖动不再直接失败
- B 站番剧 / 电影 / 课程、抖音电脑版主页 / 搜索页链接给出明确的"不支持"，不再报误导的错
- 缓存键保留 B 站分 P 的 `p` 和梨视频的 `contId`，不同分 P 不再共用一条缓存

### 依赖
- rustls 0.23.46（RUSTSEC-2026-0346 / 0347 / 0348 / 0349）
- hmac 0.13 + sha2 0.11

## [0.2.0] 及更早 - 2026-10-02

### 新增
- 35 个平台的解析器，纯 Rust，无外部进程依赖
- 按平台隔离的限流 + 熔断；只有平台侧故障计入熔断，用户贴失效链接不会误伤
- 字节系匿名 `ttwid` 游客身份，自动获取与失效重领
- 外部签名器框架（`ALCEDO_SIGNER_<平台>`），把会季度轮换的算法挡在仓库外
- CLI `alcedo`：摘要 / `--json` / `--list`
- `alcedo serve` 常驻 HTTP 服务（`/parse`、`/health`），给其他语言的程序调用；
  启动预热并每 4 分钟保温，可选 `ALCEDO_SERVE_TOKEN` 鉴权
- npm 分发：`npm install -g alcedo-cli` 安装预编译二进制（Linux musl / macOS / Windows，
  x64 与 arm64）；推 `v*` tag 自动发布 npm 和 GitHub Release
- CI：每次 push 跑 fmt + clippy + 测试 + 三平台构建 + 依赖审计；
  每日定时对着线上平台跑真实解析，红了自动开 issue

### 从旧版迁移时的行为变化
- 平台识别改为**域名后缀**匹配。原先的子串匹配会把
  `https://evil.com/?r=www.douyin.com` 认成抖音
- B 站直接取 DASH 分离轨，高清档位不再需要外部进程
- YouTube 走 InnerTube 的 visionos / ios / android_vr / tv 客户端，
  零 JS、零 PO Token
- 错误带结构化 `reason`，只有 `parse` 意味着解析器需要更新
- 响应按 `Content-Type` 的 charset 解码（六间房、美拍这类老站点返回 GBK）

### 性能
- B 站两个 playurl 并发、cid 改从 pagelist 取，热连接 p50 ~250ms → ~145ms
- 抖音 slidesinfo 优先走 `www.douyin.com`（`www.iesdouyin.com` 作退路），
  p90 357ms → 231ms；短链跳转与领 ttwid 并发；图文笔记直接带 `request_source=200`
- 游客身份（ttwid / buvid）过期后在宽限期内先用旧值、后台刷新

### 修复
- B 站播放地址改走 `x/player/wbi/playurl`：老接口在同一出口高频调用后稳定 412
- B 站未登录只能拿到 360p / 480p：DASH 请求补上 `try_look=1` 和浏览器指纹参数（`dm_*`），
  游客也能拿到 720p / 1080p
- B 站 `av` 号链接一直返回 -400（av 号被当成 bvid 传给接口）
- 西瓜视频预热的是 `www.iesdouyin.com`，与实际请求的主机不一致，预热从未生效
- AcFun 页面结构已改为 `window.videoInfo` + `ksPlayJson`，老解析器取不到数据
- 数字型 uid（微博、逗拍等）用取字符串的方式会静默得到空串
- HTML 里的 JSON 改为括号配对提取，转义过的 `</script>` 不再截断数据
