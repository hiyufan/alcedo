#!/usr/bin/env node
// npm 入口：找到当前平台对应的预编译二进制，原样转发参数、标准流和退出码。
//
// 二进制放在按平台拆分的可选依赖里（alcedo-cli-<os>-<arch>），npm 只会装上
// 与当前 os/cpu 匹配的那一个，用户不需要 Rust 工具链。
"use strict";

const { spawnSync } = require("child_process");
const path = require("path");

// Windows on ARM 用 x64 二进制（系统自带 x64 仿真），平台包里 cpu 同时声明了两者
const arch = process.platform === "win32" && process.arch === "arm64" ? "x64" : process.arch;
// Windows 的平台包叫 windows-x64 而不是 win32-x64：后者被 npm 的反垃圾检测拦截，发布不了
const os = process.platform === "win32" ? "windows" : process.platform;
const pkg = `alcedo-cli-${os}-${arch}`;
const exe = process.platform === "win32" ? "alcedo.exe" : "alcedo";

let binary;
try {
  binary = path.join(path.dirname(require.resolve(`${pkg}/package.json`)), "bin", exe);
} catch {
  console.error(
    `alcedo: 没有找到当前平台的二进制包 ${pkg}。\n` +
      "  - 安装时用了 --no-optional / --omit=optional？去掉后重装\n" +
      "  - 平台不在支持列表里？可以从源码安装：cargo install --git https://github.com/hiyufan/alcedo alcedo-cli",
  );
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit", windowsHide: false });
if (result.error) {
  console.error(`alcedo: 启动 ${binary} 失败: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.status ?? 1);
}
