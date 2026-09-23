// 把 CI 构建出的各平台二进制组装成可发布的 npm 包。
//
//   node npm/pack.mjs <版本号> <二进制目录> <输出目录>
//
// 二进制目录按 <平台 id>/<可执行文件> 摆放（即 release 工作流下载 artifact 后的样子）。
// 输出目录里每个子目录是一个包：先发各平台包，最后发 alcedo-cli 主包，
// 否则主包的 optionalDependencies 会短暂指向不存在的版本。
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const [version, binDir, outDir] = process.argv.slice(2);
if (!version || !binDir || !outDir) {
  console.error("用法: node npm/pack.mjs <版本号> <二进制目录> <输出目录>");
  process.exit(2);
}

const platforms = JSON.parse(readFileSync(join(here, "platforms.json"), "utf8"));
const main = JSON.parse(readFileSync(join(here, "alcedo-cli", "package.json"), "utf8"));

rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

// 缺任何一个平台都不发：半套包发上去，用户装到的就是一个跑不起来的命令
const missing = platforms.filter((p) => !existsSync(join(binDir, p.id, p.exe)));
if (missing.length > 0) {
  console.error(`缺少二进制: ${missing.map((p) => `${p.id}/${p.exe}`).join(", ")}`);
  process.exit(1);
}

main.version = version;
main.optionalDependencies = {};

for (const p of platforms) {
  const name = `${main.name}-${p.id}`;
  const dir = join(outDir, name);
  mkdirSync(join(dir, "bin"), { recursive: true });

  const dest = join(dir, "bin", p.exe);
  copyFileSync(join(binDir, p.id, p.exe), dest);
  chmodSync(dest, 0o755);

  const pkg = {
    name,
    version,
    description: `${main.name} 的 ${p.id} 预编译二进制，由 ${main.name} 自动选用，不要直接安装`,
    homepage: main.homepage,
    repository: main.repository,
    license: main.license,
    os: [p.os],
    cpu: p.cpu,
    files: ["bin"],
    preferUnplugged: true,
  };
  writeFileSync(join(dir, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
  copyFileSync(join(here, "..", "LICENSE"), join(dir, "LICENSE"));
  writeFileSync(
    join(dir, "README.md"),
    `# ${name}\n\n[${main.name}](https://www.npmjs.com/package/${main.name}) 的 \`${p.target}\` 二进制。` +
      `请安装 \`${main.name}\`，不要直接依赖这个包。\n`,
  );
  main.optionalDependencies[name] = version;
}

const mainDir = join(outDir, main.name);
cpSync(join(here, "alcedo-cli"), mainDir, { recursive: true });
copyFileSync(join(here, "..", "LICENSE"), join(mainDir, "LICENSE"));
writeFileSync(join(mainDir, "package.json"), `${JSON.stringify(main, null, 2)}\n`);

// 发布顺序：平台包在前，主包在最后
const order = [...Object.keys(main.optionalDependencies), main.name];
writeFileSync(join(outDir, "publish-order.txt"), `${order.join("\n")}\n`);
console.log(`已生成 ${order.length} 个包 @ ${version} -> ${outDir}`);
