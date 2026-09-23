# Contributing

**English** · [简体中文](CONTRIBUTING.md) · [Back to README](README.en.md)

## Local checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Run these checks before opening a PR. Use `cargo fmt --all` to fix formatting.
Both crates inherit the Rust and Clippy rules from `[workspace.lints]` in the root
[Cargo.toml](Cargo.toml). CI treats warnings as errors; comments alongside the
configuration explain why individual rules are enabled.

## Test strategy

- Unit and contract tests use fixed samples to check parsing logic. They run by
  default without accessing external platforms.
- [Smoke tests](crates/alcedo/tests/smoke.rs) access real platforms. They are marked
  `#[ignore]` and skipped by default. A daily workflow runs them to detect changes
  in platform APIs and access conditions.

Run all smoke tests manually:

```bash
cargo test -p alcedo --test smoke -- --ignored --nocapture
```

Platform access may require proxies or cookies; see [configuration](README.en.md#configuration).
Network failures, rate limits and regional restrictions can also fail a smoke
test, so a failure alone does not establish an extractor defect.

## Windows builds

MSVC with the C++ build tools from Visual Studio Build Tools is recommended. If
`link.exe` cannot be found, check that the C++ tools and Windows SDK are installed.

For GNU, select the toolchain for each command. This example targets x86-64 Windows:

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu --component rustfmt --component clippy
cargo +stable-x86_64-pc-windows-gnu build --workspace
```

If linking fails because `dlltool` or `as` is missing, install a full MinGW-w64
toolchain and add its `bin` directory to `PATH`. See the [rustup Windows documentation](https://rust-lang.github.io/rustup/installation/windows.html)
for prerequisites and [toolchain selection](https://rust-lang.github.io/rustup/overrides.html#toolchain-override-shorthand)
for the `cargo +toolchain` syntax.

## Adding a platform

1. Add the platform to `Source` in `crates/alcedo/src/model.rs`, implement
   `as_str`, `display_name` and `is_cn`, and include it in `Source::ALL`.
2. Register its domains in `DOMAINS` in `crates/alcedo/src/registry.rs`.
3. Implement `pub async fn parse(http: &Http, url: &str) -> Result<VideoInfo>`
   in `crates/alcedo/src/parsers/<name>.rs`.
4. Export the module and add a `dispatch` branch in `parsers/mod.rs`.
5. Add offline tests using minimal samples from real responses, and separate
   smoke tests where appropriate.
6. Update the platform lists and counts in both READMEs.

Simple extractors that need one request can live in `parsers/simple.rs`. See the
[design notes](docs/design.en.md) for module responsibilities and shared processing.

## Code requirements

Keep external platform requests in smoke tests. Unit and contract tests should
run reproducibly offline.

Handle untrusted input with `Result` and structured errors. Avoid `unwrap()`
and unchecked array indexing. The workspace sets `unsafe_code = "forbid"`.

Match errors to their cause: `Parse` for unexpected data structure, `Deleted`
for unavailable content, `Blocked` for rate limits and `Login` for sign-in
requirements. See the [error table](README.en.md#error-handling) for suggested actions.

Return readable quality labels such as `720p`; use platform-specific internal
identifiers only as a fallback.

Explain API constraints, compatibility handling and design choices in
comments; avoid restating behavior already clear from the code.

## Commit conventions

Use [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat(douyin): support Live Photos in image posts
fix(acfun): adapt to the new page data structure
refactor: consolidate quality label handling
```

Describe the problem, behavior changes and validation in the PR. For platform
API changes, include reproduction conditions and sanitized samples. Keep the
Chinese and English user documentation in sync.

## Investigating platform failures

Run smoke tests for the affected platform, replacing `<platform>` with a filter
that matches the relevant test names:

```bash
cargo test -p alcedo --test smoke -- --ignored "<platform>" --nocapture
```

Consider the error reason, network egress and sign-in status together. For
persistent `parse` errors, inspect the response structure. For `blocked` or
`login`, check access conditions first. Include complete errors and reproduction
steps in issues, after removing cookies and other sensitive information.
