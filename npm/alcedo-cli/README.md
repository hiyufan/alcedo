# alcedo-cli

[alcedo](https://github.com/hiyufan/alcedo) 的命令行，预编译二进制，无需 Rust 工具链。
给一条分享链接，返回媒体地址、图集、封面、作者和可用清晰度，支持抖音、快手、小红书、
B 站、微博、YouTube、TikTok 等 35 个平台。

The command-line tool for [alcedo](https://github.com/hiyufan/alcedo), shipped as a
prebuilt binary. No Rust toolchain needed.

```bash
npm install -g alcedo-cli
# 或者不安装直接跑 / or run without installing
npx alcedo-cli --list
```

```bash
alcedo "https://v.douyin.com/xxxxxx/"      # 人类可读摘要 / human-readable summary
alcedo --json "https://..."                # JSON
alcedo --list                              # 支持的平台 / supported platforms
```

预编译平台 / Prebuilt for: Linux x64/arm64（静态链接 musl / static musl）、
macOS x64/arm64、Windows x64/arm64。

文档、配置和支持的平台列表见 [GitHub](https://github.com/hiyufan/alcedo)。
See [GitHub](https://github.com/hiyufan/alcedo) for docs and configuration.

MIT License
