# 外部签名器

[English](signers.en.md) · **简体中文** · [返回 README](../README.md)

签名器为平台请求补充查询参数、请求头和 cookie。当前抖音解析器接入了这个机制，
签名算法由外部程序或 HTTP 服务提供，本仓库不附带签名实现。

## 配置

本机程序使用子进程方式，将路径替换为实际可执行文件：

```bash
export ALCEDO_SIGNER_DOUYIN="cmd:/opt/alcedo/signer"
```

也可以指定 HTTP 服务，将地址替换为实际端点：

```bash
export ALCEDO_SIGNER_DOUYIN="https://signer.example.com/sign"
```

PowerShell 使用 `$env:ALCEDO_SIGNER_DOUYIN = "..."` 设置环境变量。
配置由签名器直接从环境变量读取，不属于 `Config`。

HTTP 服务复用解析器的 HTTP 层和代理设置，也受地址检查限制。当前不能使用
`127.0.0.1` 或内网 IP 作为 HTTP 端点；本机服务请使用子进程方式。

## 传输与协议

- 子进程每次调用接收 stdin 中的一份 JSON，读到 EOF 后处理；stdout 只输出一份
  响应 JSON，并以状态码 0 退出。命令按空白拆分，不经 shell，不能使用带空格的路径或参数。
- HTTP 方式向端点 POST 请求 JSON，并从成功响应中读取 JSON。

两种方式使用相同字段。请求示例：

```json
{
  "platform": "douyin",
  "url": "https://www.douyin.com/aweme/v1/web/aweme/detail/",
  "query": "aweme_ids=%5B123%5D",
  "user_agent": "Mozilla/5.0 ...",
  "body": ""
}
```

`url`、`query` 和 `body` 由解析器传入；签名器应使用收到的值计算签名。
`user_agent` 是调用方提供的 UA。

响应示例，三个字段均可省略，字段内的键和值均为字符串：

```json
{
  "query": { "a_bogus": "..." },
  "headers": {},
  "cookies": { "msToken": "..." }
}
```

`query` 追加到请求 URL，参数值由调用方编码；`headers` 和 `cookies` 合并到平台请求。
返回 `{}` 表示不添加签名。

## 超时与失败处理

子进程收到输入后，等待其退出和输出的上限为 5 秒。HTTP 方式使用普通请求超时
（`ALCEDO_REQUEST_TIMEOUT`，默认 20 秒），整次解析还受 `ALCEDO_TOTAL_TIMEOUT` 约束。

签名器未配置、调用失败、退出码非零、HTTP 状态失败或响应 JSON 无效时，返回空签名；
调用失败会记录警告。解析器继续尝试不带签名的请求，但平台仍可能拒绝，整次解析超时
也会中止流程。

实现和协议测试见 [signer.rs](../crates/alcedo/src/parsers/signer.rs)。
