# 参与开发

[English](CONTRIBUTING.en.md) · **简体中文** · [返回 README](README.md)

## 本地检查

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

提交 PR 前请通过这些检查。格式不符时运行 `cargo fmt --all`。

Rust 和 Clippy 规则统一配置在根目录 [Cargo.toml](Cargo.toml) 的 `[workspace.lints]`，
两个 crate 均继承这些规则。CI 将警告视为错误；规则的选择理由见配置旁的注释。

## 测试策略

- 单元和契约测试使用固定样本，验证解析逻辑，默认运行且不访问外部平台。
- [冒烟测试](crates/alcedo/tests/smoke.rs) 访问真实平台，标记为 `#[ignore]`，默认跳过。
  定时工作流每天运行这些测试，用于发现平台接口和访问条件的变化。

手动运行全部冒烟测试：

```bash
cargo test -p alcedo --test smoke -- --ignored --nocapture
```

平台访问可能需要代理或 cookie，配置见 [README](README.md#配置)。
网络、限流和地区限制也会使冒烟测试失败，不能仅凭失败就认定解析器有缺陷。

## Windows 构建

推荐使用 MSVC 工具链，并安装 Visual Studio Build Tools 的 C++ 构建工具。
如果提示找不到 `link.exe`，请检查 C++ 工具和 Windows SDK 是否安装。

使用 GNU 工具链时，可以只为当前命令指定工具链。以下以 x86-64 Windows 为例：

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu --component rustfmt --component clippy
cargo +stable-x86_64-pc-windows-gnu build --workspace
```

若链接阶段提示缺少 `dlltool` 或 `as`，请安装完整的 MinGW-w64 工具链，并把对应的
`bin` 目录加入 `PATH`。工具链要求见 [rustup Windows 文档](https://rust-lang.github.io/rustup/installation/windows.html)；
`cargo +工具链` 的用法见 [rustup 工具链选择](https://rust-lang.github.io/rustup/overrides.html#toolchain-override-shorthand)。

## 添加平台

1. `crates/alcedo/src/model.rs` 的 `Source` 加一项，补 `as_str` / `display_name` /
   `is_cn`，并加进 `Source::ALL`
2. `crates/alcedo/src/registry.rs` 的 `DOMAINS` 登记域名
3. 写 `crates/alcedo/src/parsers/<name>.rs`，导出
   `pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo>`
4. `parsers/mod.rs` 里 `pub mod` + `dispatch` 加分支
5. 写单元测试，用真实响应造一份**最小**样本
6. 同步更新中英文 README 的平台清单和数量

简单的单请求解析器可以放在 `parsers/simple.rs`。模块职责和公共处理流程见
[设计说明](docs/design.md)。

## 代码要求

外部平台请求放在冒烟测试中；单元测试和契约测试应可离线重复运行。

对不可信输入使用 `Result` 和结构化错误，避免 `unwrap()` 或未检查的数组索引。
工作区设置了 `unsafe_code = "forbid"`。

根据故障原因选择错误类型：数据结构不符用 `Parse`，内容不可用用 `Deleted`，
限流用 `Blocked`，需要登录用 `Login`。操作建议见 [README 错误表](README.md#错误处理)。

输出可读的清晰度标签，例如 `720p`；平台内部代号只作为兜底。

注释解释接口约束、兼容处理和选择理由，避免重复代码本身已经表达的行为。

## 提交约定

用 [Conventional Commits](https://www.conventionalcommits.org/)：

```text
feat(douyin): 支持图文帖的实况照片
fix(acfun): 适配新的页面数据结构
refactor: 合并清晰度标签逻辑
```

PR 中说明问题、行为变化和验证方式。涉及平台接口变化时，附上复现条件和脱敏样本；
修改用户文档时同步更新中英文版本。

## 排查平台失败

先针对相应平台运行冒烟测试，将 `<平台名>` 替换为测试名称中的筛选词：

```bash
cargo test -p alcedo --test smoke -- --ignored "<平台名>" --nocapture
```

结合错误原因、网络出口和登录状态判断问题。持续出现 `parse` 时检查响应结构；
出现 `blocked` 或 `login` 时先检查访问条件。记录完整的错误信息和复现步骤，
移除 cookie 等敏感信息后再提交 issue。
