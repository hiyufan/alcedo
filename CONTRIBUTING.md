# 参与开发

## 本地跑起来

```bash
cargo test --workspace                 # 单元 + 契约测试，全部离线
cargo clippy --workspace --all-targets # lint 基线在 Cargo.toml 的 [workspace.lints]
cargo fmt --all
```

提 PR 之前把上面三条跑干净就行，CI 跑的是同一套。

**Windows 上需要一个链接器。** 装了 Visual Studio 生成工具就用默认的 MSVC；
没装的话 GNU 工具链更省事，具体见 README 的「Windows 上的构建」。

## 加一个平台

1. `crates/alcedo/src/model.rs` 的 `Source` 加一项，补 `as_str` / `display_name` /
   `is_cn`，并加进 `Source::ALL`
2. `crates/alcedo/src/registry.rs` 的 `DOMAINS` 登记域名
3. 写 `crates/alcedo/src/parsers/<name>.rs`，导出
   `pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo>`
4. `parsers/mod.rs` 里 `pub mod` + `dispatch` 加分支
5. 写单元测试，用真实响应造一份**最小**样本

单请求就能搞定的平台直接加进 `parsers/simple.rs`，别单开文件。

## 几条硬规矩

**单元测试里不要发网络请求。** 联网的用例全部放 `tests/smoke.rs` 并标
`#[ignore]`。理由是这两类测试回答的问题不一样：单元测试验证"给定这份响应，
解析逻辑对不对"，冒烟测试验证"平台今天还是不是这么返回的"。混在一起的结果是
CI 会因为对方限流而随机变红，最后没人再看它。

**别 panic。** 解析器吃的是不可信输入，`unwrap()` / 数组越界在这里等于
线上崩溃。用 `?` 和 `Error::*`。全项目 `unsafe_code = "forbid"`。

**错误要归对类。** `Reason::Parse` 的含义是"平台改版了，解析器需要更新"——
只有它需要有人动手。内容不存在是 `Deleted`，被风控是 `Blocked`，要登录是
`Login`。归错类的代价是站长去查一个根本不存在的问题，或者反过来，真出问题时
淹没在噪音里。

**平台自己的内部代号不要直接展示。** 比如 TikTok 的 `GearName` 是
`normal_720` / `lowest_1080_1`，算得出短边就用 `720p`，那个只当兜底。

**注释写"为什么"，不写"做了什么"。** 代码本身说明做了什么；值钱的是
"这里为什么绕了一下"——哪个接口会返回什么怪东西、哪条路踩过坑。

## 提交信息

用 [Conventional Commits](https://www.conventionalcommits.org/)：

```
feat(douyin): 支持图文帖的实况照片
fix(acfun): 页面结构改成 window.videoInfo + ksPlayJson
refactor: 收敛重复的清晰度标签逻辑
```

正文写清楚**为什么**这么改。解析器这类代码半年后回看，"当时那个平台返回了
什么"是唯一还有价值的信息。

## 平台挂了怎么办

先跑冒烟测试确认是真挂了还是自己网络的问题：

```bash
cargo test -p alcedo --test smoke -- --ignored <平台名> --nocapture
```

再看报错的 `reason`：

| reason | 多半是 |
| --- | --- |
| `parse` | **真改版了**，要改解析器 |
| `blocked` | 出口 IP 被风控，配代理或换出口 |
| `login` | 平台开始要登录，配对应的 `ALCEDO_*_COOKIE` |
| `deleted` | 就是这条内容没了，换个链接再试 |
