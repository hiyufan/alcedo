// 把 CI 构建出的各平台二进制组装成可发布的 npm 包。
//
//   node npm/pack.mjs <版本号> <二进制目录> <输出目录> [--scope @组织]
//
// 二进制目录按 <平台 id>/<可执行文件> 摆放（即 release 工作流下载 artifact 后的样子）。
// 输出目录里每个子目录是一个包：先发各平台包，最后发 alcedo-cli 主包，
// 否则主包的 optionalDependencies 会短暂指向不存在的版本。
//
// --scope 给包名加组织前缀（@hiyufan/alcedo-cli），发 GitHub Packages 用——
// 那边的 npm 源只收和 owner 同名的 scope。目录名始终用不带 scope 的基础名，
// 发布方从每个目录的 package.json 里读真实包名。
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const [version, binDir, outDir] = argv;
let scope = "";
for (let i = 3; i < argv.length; i++) {
  if (argv[i] === "--scope") {
    scope = argv[++i] ?? "";
  }
}
if (!version || !binDir || !outDir) {
  console.error("用法: node npm/pack.mjs <版本号> <二进制目录> <输出目录> [--scope @组织]");
  process.exit(2);
}
if (scope && !/^@[a-z0-9-]+$/.test(scope)) {
  console.error(`scope 必须形如 @小写组织名: ${scope}`);
  process.exit(2);
}
const fullName = (base) => (scope ? `${scope}/${base}` : base);

const platforms = JSON.parse(readFileSync(join(here, "platforms.json"), "utf8"));
const main = JSON.parse(readFileSync(join(here, "alcedo-cli", "package.json"), "utf8"));
const mainBase = main.name;

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
  const base = `${main.name}-${p.id}`;
  const name = fullName(base);
  const dir = join(outDir, base);
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

const mainDir = join(outDir, mainBase);
main.name = fullName(mainBase);
cpSync(join(here, "alcedo-cli"), mainDir, { recursive: true });
copyFileSync(join(here, "..", "LICENSE"), join(mainDir, "LICENSE"));
writeFileSync(join(mainDir, "package.json"), `${JSON.stringify(main, null, 2)}\n`);

// 发布顺序：平台包在前，主包在最后。写目录名（= 不带 scope 的基础名），
// 包名由发布方从各目录的 package.json 读
const order = [...platforms.map((p) => `${mainBase}-${p.id}`), mainBase];
writeFileSync(join(outDir, "publish-order.txt"), `${order.join("\n")}\n`);
console.log(`已生成 ${order.length} 个包 @ ${version}${scope ? `（scope ${scope}）` : ""} -> ${outDir}`);
