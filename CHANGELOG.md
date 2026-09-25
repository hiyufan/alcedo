# 更新日志

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [未发布]

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

### 修复
- AcFun 页面结构已改为 `window.videoInfo` + `ksPlayJson`，老解析器取不到数据
- 数字型 uid（微博、逗拍等）用取字符串的方式会静默得到空串
- HTML 里的 JSON 改为括号配对提取，转义过的 `</script>` 不再截断数据
