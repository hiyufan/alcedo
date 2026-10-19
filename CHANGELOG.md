# 更新日志

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

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
