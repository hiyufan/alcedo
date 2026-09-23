# 设计说明

[English](design.en.md) · **简体中文** · [返回 README](../README.md)

## 解析流程

`Client::parse` 从输入中提取 URL，识别平台，通过限流检查后调用相应解析器。
解析结果统一为 `VideoInfo`，补充来源、页面地址及需要的媒体请求头，并整理清晰度列表。
公开入口见 [lib.rs](../crates/alcedo/src/lib.rs)。

平台识别检查 URL 的主机名及域名边界，避免把查询参数里的平台名称当作目标站点。
已识别的平台通过 `match` 分发到具体解析函数；新增平台需更新
[registry.rs](../crates/alcedo/src/registry.rs) 和 [parsers/mod.rs](../crates/alcedo/src/parsers/mod.rs)。

## HTTP 与请求控制

[HTTP 层](../crates/alcedo/src/http/mod.rs) 按代理配置在进程内共享客户端，复用连接、
TLS 会话及 DNS 缓存。短链重定向由 HTTP 层逐跳处理，并检查目标地址。
自定义 [DNS resolver](../crates/alcedo/src/http/ssrf.rs) 默认拒绝解析结果中的内网地址。

[限流与熔断](../crates/alcedo/src/http/resilience.rs) 按平台维护，并在进程内共享状态。
连续 5 次计入熔断的失败会暂停请求，冷却从 15 秒开始，指数增长至最多 5 分钟。
`blocked`、`network`、`timeout` 和 `login` 计入失败；内容已删除等错误不计入。

部分解析路径使用[游客身份缓存](../crates/alcedo/src/http/identity.rs)。
这些机制用于减少重复请求和处理暂时故障，不保证平台始终接受请求。

## 页面数据与媒体格式

[json_after](../crates/alcedo/src/util.rs) 从指定标记后扫描配对的括号，并处理 JSON
字符串和转义，避免把后续 JavaScript 语句混入 JSON。平台解析器负责将提取的数据
转换为公共模型。

[YouTube 解析器](../crates/alcedo/src/parsers/youtube.rs) 尝试多个客户端，使用响应中
可直接获取的媒体地址，并跳过带 `signatureCipher` 的条目；没有可用地址时仍可能失败。
[B 站解析器](../crates/alcedo/src/parsers/bilibili.rs) 请求 DASH 数据并列出可用音视频轨。
具体档位受内容和访问权限影响，下载与合并由调用方负责。

抖音的可选签名服务与解析器分离，便于独立更新签名实现。接入方式见
[外部签名器](signers.md)。

## 性能测量

在同一个进程内重复解析一条链接，将 `<url>` 替换为可访问的分享链接：

```bash
cargo run --release -p alcedo --example bench -- "<url>" 10
```

[测量程序](../crates/alcedo/examples/bench.rs) 输出每次解析耗时、成功次数、最快值和
排序后的中间样本值。汇总包含首次成功解析，偶数样本取靠后的中间值；失败请求不进入
耗时汇总，因此该值不能直接当作热连接中位数。

分享测量结果时应记录提交版本、日期、硬件、测试链接、代理或登录条件和成功比例。
如需比较首次与后续解析，应从逐次输出中分别统计。端到端耗时包含网络和平台响应时间。
