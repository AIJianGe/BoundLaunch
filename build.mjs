#!/usr/bin/env node
/**
 * 无界启动器 (BoundLaunch) 跨平台打包脚本
 *
 * 详见 `PR/08-验证清单.md §4 跨平台打包`
 *
 * 5 阶段流水线：
 *   1. Pre-check       环境检查（Node / Rust / Tauri CLI / 系统依赖提示）
 *   2. TypeCheck       前端 TypeScript 类型检查
 *   3. Test            Rust 单元测试（可选，--skip-tests 跳过）
 *   4. Build           前端构建 + Tauri 打包
 *   5. Report          产物清单（路径 / 大小 / SHA256）
 *
 * 用法：
 *   node build.mjs                          # 默认全流程，按当前平台决定 bundle targets
 *   node build.mjs --skip-tests              # 跳过 Rust 单元测试
 *   node build.mjs --target nsis             # 仅打包指定 target
 *   node build.mjs --debug                  # 调试模式（不优化）
 *   node build.mjs --no-typecheck           # 跳过类型检查
 *   node build.mjs --help                   # 显示帮助
 *
 * 退出码：
 *   0 - 成功
 *   1 - 环境检查失败
 *   2 - 类型检查失败
 *   3 - 测试失败
 *   4 - 构建失败
 *   5 - 产物未找到
 *
 * 设计模式：
 * - **Template Method**：BuildPipeline.run 定义阶段顺序，子类可重写各阶段
 * - **Strategy**：EnvironmentCheckStrategy 按平台不同检查不同依赖
 * - **Command**：每个阶段封装为独立 Command 类，便于单测和复用
 * - **Facade**：build.mjs 作为对外统一入口
 *
 * @author BoundLaunch Team
 * @version 1.0.0
 */

import { spawnSync } from "node:child_process";
import {
  existsSync,
  statSync,
  readdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { join, resolve, dirname, relative, basename, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { platform, arch, cpus, totalmem } from "node:os";

// ============================================================================
// 配置常量
// ============================================================================

const CONFIG = {
  /** 项目根目录 */
  rootDir: resolve(dirname(fileURLToPath(import.meta.url))),
  /** src-tauri 子目录 */
  srcTauriDir: "src-tauri",
  /** Rust target 输出目录 */
  targetDir: "src-tauri/target",
  /** bundle 产物目录 */
  bundleDir: "src-tauri/target/release/bundle",
  /** package.json 路径 */
  packageJsonPath: "package.json",
  /** 各平台默认 bundle targets */
  defaultTargets: {
    win32: ["msi", "nsis"],
    linux: ["deb", "appimage"],
    darwin: ["dmg"],
  },
  /** 各平台必需命令 */
  requiredCommands: {
    win32: ["node", "npm", "cargo", "rustc"],
    linux: ["node", "npm", "cargo", "rustc"],
    darwin: ["node", "npm", "cargo", "rustc"],
  },
  /** 各平台必需的系统库（仅检查提示，不强制） */
  systemLibHints: {
    linux: [
      "libwebkit2gtk-4.1",
      "libgtk-3",
      "libayatana-appindicator3",
      "librsvg-2",
    ],
  },
  /** 编译并行度硬上限（自动检测时不超过此值） */
  maxBuildJobs: 16,
  /** 编译并行度有效取值范围 */
  buildJobsRange: { min: 1, max: 32 },
  /** build.config.json 文件名 */
  buildConfigFile: "build.config.json",
};

// ============================================================================
// 颜色输出工具（不依赖第三方库）
// ============================================================================

const Colors = {
  reset: "\x1b[0m",
  bold: "\x1b[1m",
  dim: "\x1b[2m",
  red: "\x1b[31m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  blue: "\x1b[34m",
  magenta: "\x1b[35m",
  cyan: "\x1b[36m",
  gray: "\x1b[90m",
};

const colorEnabled = process.stdout.isTTY && !process.env.NO_COLOR;

function paint(color, text) {
  return colorEnabled ? `${color}${text}${Colors.reset}` : text;
}

const log = {
  info: (msg) => console.log(paint(Colors.cyan, "ℹ"), msg),
  success: (msg) => console.log(paint(Colors.green, "✓"), msg),
  warn: (msg) => console.log(paint(Colors.yellow, "⚠"), msg),
  error: (msg) => console.error(paint(Colors.red, "✗"), msg),
  debug: (msg) => {
    if (process.env.DEBUG) console.log(paint(Colors.gray, "•"), paint(Colors.dim, msg));
  },
  stage: (title) => {
    const line = "─".repeat(Math.max(8, 60 - title.length));
    console.log();
    console.log(paint(Colors.blue, `${line} ${title} ${line}`));
  },
  raw: (msg) => console.log(msg),
};

// ============================================================================
// 命令行参数解析
// ============================================================================

class ArgParser {
  constructor(argv) {
    this.args = argv;
    this.options = {
      skipTests: false,
      skipTypecheck: false,
      debug: false,
      target: null,
      help: false,
    };
    this._parse();
  }

  _parse() {
    for (const arg of this.args) {
      switch (arg) {
        case "--help":
        case "-h":
          this.options.help = true;
          break;
        case "--skip-tests":
          this.options.skipTests = true;
          break;
        case "--no-typecheck":
          this.options.skipTypecheck = true;
          break;
        case "--debug":
          this.options.debug = true;
          break;
        case "--target":
          // 下一个参数作为 target 值
          break;
        default:
          if (arg.startsWith("--target=")) {
            this.options.target = arg.slice("--target=".length);
          } else if (this._prevArg === "--target") {
            this.options.target = arg;
          }
          break;
      }
      this._prevArg = arg;
    }
  }

  showHelp() {
    console.log(`
${paint(Colors.bold, "无界启动器 打包脚本")}

${paint(Colors.cyan, "用法:")}
  node build.mjs [options]

${paint(Colors.cyan, "选项:")}
  --skip-tests          跳过 Rust 单元测试
  --no-typecheck        跳过前端 TypeScript 类型检查
  --target <name>       指定 bundle target（如 msi / nsis / deb / appimage / dmg）
  --debug               调试模式（cargo build --debug，不优化）
  --help, -h            显示此帮助信息

${paint(Colors.cyan, "环境变量:")}
  DEBUG=1               启用详细日志
  NO_COLOR=1            禁用颜色输出
  TAURI_BUNDLE_TARGETS  逗号分隔的 bundle targets（覆盖默认）
  BUILD_JOBS=N          编译并行度（1-32），覆盖 build.config.json；
                        传给 cargo test -j N 与 npx tauri build 内部 cargo build
                        详见 pr/05-依赖与启动参数.md §5 / pr/08-验证清单.md §4.4

${paint(Colors.cyan, "配置文件:")}
  build.config.json     项目根目录的 JSON 配置文件
                        示例: { "build_jobs": 8 }
                        完整规范见 build.config.README.md

${paint(Colors.cyan, "示例:")}
  node build.mjs                              # 默认全流程
  node build.mjs --skip-tests                 # 跳过测试
  node build.mjs --target nsis                # 仅打包 NSIS
  node build.mjs --debug --skip-tests        # 调试构建，跳过测试
  BUILD_JOBS=4 node build.mjs --skip-tests    # 限制为 4 路并行（低内存机器）

${paint(Colors.cyan, "退出码:")}
  0 - 成功
  1 - 环境检查失败
  2 - 类型检查失败
  3 - 测试失败
  4 - 构建失败
  5 - 产物未找到
`);
  }
}

// ============================================================================
// 编译配置（build.config.json + 环境变量 + 自动检测）
// ============================================================================

/**
 * BuildConfig - 编译并行度（build_jobs）配置解析
 *
 * 设计模式：
 * - **Strategy**：4 个来源按优先级链式查找
 * - **Value Object**：jobs + source 一起传递
 * - **Single Source of Truth**：build_jobs 全局唯一读取入口
 *
 * 优先级（高→低）：
 *   1. 环境变量 BUILD_JOBS（临时）
 *   2. build.config.json 的 build_jobs 字段（项目级）
 *   3. os.cpus().length（自动检测，硬上限 16）
 *
 * 完整规范见 pr/05-依赖与启动参数.md §5 / pr/08-验证清单.md §4.4
 */
class BuildConfig {
  constructor() {
    this._loadFromFile();
    this._resolve();
  }

  /** 读取 build.config.json（不存在则静默忽略） */
  _loadFromFile() {
    this._fileConfig = {};
    const configPath = join(CONFIG.rootDir, CONFIG.buildConfigFile);
    if (!existsSync(configPath)) {
      log.debug(`${CONFIG.buildConfigFile} not found, skip file config`);
      return;
    }
    try {
      const raw = readFileSync(configPath, "utf-8");
      const parsed = JSON.parse(raw);
      // 忽略 _ 开头的注释字段（_comment / _schema_version 等）
      this._fileConfig = {};
      for (const [k, v] of Object.entries(parsed)) {
        if (!k.startsWith("_")) this._fileConfig[k] = v;
      }
      log.debug(`loaded ${CONFIG.buildConfigFile}: ${JSON.stringify(this._fileConfig)}`);
    } catch (e) {
      log.warn(`无法解析 ${CONFIG.buildConfigFile}: ${e.message}，已忽略`);
      this._fileConfig = {};
    }
  }

  /**
   * 按优先级解析 build_jobs
   * @returns {{ jobs: number, source: string, cpuCount: number, maxAuto: number }}
   */
  _resolve() {
    const cpuCount = cpus().length;
    const maxAuto = CONFIG.maxBuildJobs;
    const { min, max } = CONFIG.buildJobsRange;

    // 1. 环境变量 BUILD_JOBS
    if (process.env.BUILD_JOBS !== undefined && process.env.BUILD_JOBS !== "") {
      const parsed = parseInt(process.env.BUILD_JOBS, 10);
      if (Number.isFinite(parsed) && parsed >= min && parsed <= max) {
        this.jobs = parsed;
        this.source = `环境变量 BUILD_JOBS=${parsed}`;
        this.cpuCount = cpuCount;
        this.maxAuto = maxAuto;
        return;
      }
      log.warn(
        `BUILD_JOBS=${process.env.BUILD_JOBS} 越界（有效 ${min}-${max}），已忽略`,
      );
    }

    // 2. build.config.json
    const fileVal = this._fileConfig.build_jobs;
    if (fileVal !== undefined && fileVal !== null && fileVal !== 0) {
      const parsed = parseInt(fileVal, 10);
      if (Number.isFinite(parsed) && parsed >= min && parsed <= max) {
        this.jobs = parsed;
        this.source = `${CONFIG.buildConfigFile} = ${parsed}`;
        this.cpuCount = cpuCount;
        this.maxAuto = maxAuto;
        return;
      }
      log.warn(
        `${CONFIG.buildConfigFile}.build_jobs=${fileVal} 越界（有效 ${min}-${max}），已忽略`,
      );
    }

    // 3. 自动检测（限制在 maxAuto 以内）
    const auto = Math.min(cpuCount, maxAuto);
    this.jobs = auto;
    this.source =
      auto < cpuCount
        ? `自动检测 ${cpuCount} 核 → 限制 ${auto}`
        : `自动检测 ${cpuCount} 核`;
    this.cpuCount = cpuCount;
    this.maxAuto = maxAuto;
  }

  /** 输出配置摘要（启动时打印） */
  printSummary() {
    log.info(`Build Jobs: ${this.jobs} (来源: ${this.source})`);
  }
}

// ============================================================================
// 命令执行器
// ============================================================================

class CommandExecutor {
  /**
   * 同步执行命令，继承 stdio
   * @param {string} command 命令名
   * @param {string[]} args 参数列表
   * @param {object} options spawnSync 选项
   * @returns {{status: number, stdout: string, stderr: string}}
   */
  static run(command, args = [], options = {}) {
    log.debug(`exec: ${command} ${args.join(" ")}`);
    const result = spawnSync(command, args, {
      stdio: options.silent ? ["ignore", "pipe", "pipe"] : "inherit",
      cwd: options.cwd || CONFIG.rootDir,
      shell: process.platform === "win32" && !options.noShell,
      encoding: "utf-8",
      env: { ...process.env, ...(options.env || {}) },
    });

    if (result.error) {
      // ENOENT 等错误
      throw new Error(`command not found: ${command} (${result.error.message})`);
    }

    return {
      status: result.status,
      stdout: result.stdout || "",
      stderr: result.stderr || "",
    };
  }

  /**
   * 检查命令是否可用
   */
  static which(command) {
    const checkCmd = process.platform === "win32" ? "where" : "which";
    const result = spawnSync(checkCmd, [command], {
      stdio: ["ignore", "pipe", "pipe"],
      shell: process.platform === "win32",
      encoding: "utf-8",
    });
    return result.status === 0;
  }

  /**
   * 获取命令版本号
   */
  static version(command, versionFlag = "--version") {
    const result = spawnSync(command, [versionFlag], {
      stdio: ["ignore", "pipe", "pipe"],
      shell: process.platform === "win32",
      encoding: "utf-8",
    });
    if (result.status !== 0) return null;
    return (result.stdout || "").trim().split("\n")[0];
  }
}

// ============================================================================
// 环境检查器（Strategy 模式：按平台不同检查策略）
// ============================================================================

class EnvironmentChecker {
  constructor(options, buildConfig) {
    this.options = options;
    this.buildConfig = buildConfig;
    this.errors = [];
    this.warnings = [];
    this.info = {};
  }

  async check() {
    log.stage("Stage 1: Pre-check");

    this._checkPlatform();
    this._checkNodeVersion();
    this._checkRequiredCommands();
    this._checkProjectStructure();
    this._checkRustToolchain();
    this._checkTauriCli();
    this._checkSystemDependencies();
    this._checkMemory();
    this._readPackageVersion();
    this.info.bundleTargets = this.resolveBundleTargets();

    // 输出汇总
    console.log();
    log.info(`平台: ${this.info.platform}/${this.info.arch}`);
    log.info(`Node: ${this.info.nodeVersion}`);
    log.info(`Rust: ${this.info.rustVersion}`);
    log.info(`Cargo: ${this.info.cargoVersion}`);
    log.info(`Tauri CLI: ${this.info.tauriCliVersion}`);
    log.info(`项目版本: ${paint(Colors.bold, this.info.packageVersion)}`);
    log.info(`Bundle Targets: ${this.info.bundleTargets.join(", ")}`);
    if (this.buildConfig) this.buildConfig.printSummary();

    this.warnings.forEach((w) => log.warn(w));

    if (this.errors.length > 0) {
      console.log();
      this.errors.forEach((e) => log.error(e));
      console.log();
      log.error("环境检查失败，请修复上述问题后重试");
      console.log(paint(Colors.gray, "详见 PR/08-验证清单.md §1 本机环境前置检查"));
      process.exit(1);
    }

    log.success("环境检查通过");
  }

  _checkPlatform() {
    this.info.platform = platform();
    this.info.arch = arch();
    if (!["win32", "linux", "darwin"].includes(this.info.platform)) {
      this.errors.push(`不支持的平台: ${this.info.platform}`);
    }
  }

  _checkNodeVersion() {
    const raw = process.versions.node;
    const major = parseInt(raw.split(".")[0], 10);
    this.info.nodeVersion = `v${raw}`;
    if (major < 18) {
      this.errors.push(`Node.js 版本过低: ${raw}（需 18+）`);
    }
  }

  _checkRequiredCommands() {
    const required = CONFIG.requiredCommands[this.info.platform] || [];
    for (const cmd of required) {
      if (!CommandExecutor.which(cmd)) {
        this.errors.push(`未找到命令: ${cmd}`);
      }
    }
  }

  _checkProjectStructure() {
    const requiredPaths = [
      "package.json",
      "vite.config.ts",
      "tsconfig.json",
      "src-tauri/Cargo.toml",
      "src-tauri/tauri.conf.json",
      "src-tauri/src/lib.rs",
    ];
    for (const p of requiredPaths) {
      const full = join(CONFIG.rootDir, p);
      if (!existsSync(full)) {
        this.errors.push(`项目文件缺失: ${p}`);
      }
    }
  }

  _checkRustToolchain() {
    this.info.rustVersion = CommandExecutor.version("rustc", "--version");
    this.info.cargoVersion = CommandExecutor.version("cargo", "--version");
    if (!this.info.rustVersion || !this.info.cargoVersion) {
      this.errors.push("Rust 工具链未安装（rustc / cargo）");
    }
  }

  _checkTauriCli() {
    // Tauri CLI 通过 @tauri-apps/cli 安装到 node_modules/.bin
    const localTauri = join(CONFIG.rootDir, "node_modules", ".bin", "tauri");
    const ext = process.platform === "win32" ? ".cmd" : "";
    if (existsSync(localTauri + ext)) {
      const result = CommandExecutor.run("npx", ["tauri", "--version"], { silent: true });
      this.info.tauriCliVersion = result.stdout.trim().split("\n")[0];
    } else {
      this.warnings.push("未检测到本地 Tauri CLI（将自动通过 npx 调用）");
      this.info.tauriCliVersion = "(via npx)";
    }
  }

  _checkSystemDependencies() {
    if (this.info.platform === "linux") {
      const hints = CONFIG.systemLibHints.linux || [];
      log.info(`Linux 系统依赖提示（不强制检查）: ${hints.join(", ")}`);
      log.info(paint(Colors.gray, "  若缺库请执行: sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf"));
    }
    if (this.info.platform === "win32") {
      log.info(paint(Colors.gray, "  Windows 需要: Visual Studio Build Tools 2022 (C++ 工作负载) + WebView2 Runtime"));
    }
  }

  _checkMemory() {
    const memBytes = totalmem();
    const memGB = Math.round(memBytes / 1024 / 1024 / 1024);
    this.info.memoryGB = memGB;
    if (memGB < 4) {
      this.warnings.push(`内存较低: ${memGB} GB（建议 8GB+ 以加速 Rust 编译）`);
    }
  }

  _readPackageVersion() {
    try {
      const pkg = JSON.parse(
        readFileSync(join(CONFIG.rootDir, CONFIG.packageJsonPath), "utf-8"),
      );
      this.info.packageVersion = pkg.version;
    } catch {
      this.errors.push("无法读取 package.json");
    }
  }

  /**
   * 决定本次打包的 bundle targets
   */
  resolveBundleTargets() {
    // 优先级：--target > TAURI_BUNDLE_TARGETS > 默认
    if (this.options.target) {
      return [this.options.target];
    }
    if (process.env.TAURI_BUNDLE_TARGETS) {
      return process.env.TAURI_BUNDLE_TARGETS.split(",").map((s) => s.trim()).filter(Boolean);
    }
    return CONFIG.defaultTargets[this.info.platform] || [];
  }
}

// ============================================================================
// 构建流水线（Template Method 模式）
// ============================================================================

class BuildPipeline {
  constructor(options, envInfo, buildConfig) {
    this.options = options;
    this.envInfo = envInfo;
    this.buildConfig = buildConfig;
    this.bundleTargets = envInfo.resolveBundleTargets();
    envInfo.info.bundleTargets = this.bundleTargets;
  }

  async run() {
    await this.typecheck();
    await this.test();
    await this.build();
  }

  /** 阶段 2：前端 TypeScript 类型检查 */
  async typecheck() {
    if (this.options.skipTypecheck) {
      log.warn("已跳过类型检查（--no-typecheck）");
      return;
    }

    log.stage("Stage 2: TypeCheck");
    log.info("运行 vue-tsc --noEmit ...");

    const result = CommandExecutor.run("npx", ["vue-tsc", "--noEmit"], { silent: true });

    if (result.status !== 0) {
      console.log(result.stdout);
      console.log(result.stderr);
      log.error("类型检查失败");
      process.exit(2);
    }

    log.success("类型检查通过");
  }

  /** 阶段 3：Rust 单元测试 */
  async test() {
    if (this.options.skipTests) {
      log.warn("已跳过单元测试（--skip-tests）");
      return;
    }

    log.stage("Stage 3: Test");
    const jobs = this.buildConfig ? this.buildConfig.jobs : null;
    const jobsSuffix = jobs ? ` -- -j ${jobs}` : "";
    log.info(`运行 cargo test --all${jobsSuffix} ...`);

    const result = CommandExecutor.run("cargo", ["test", "--all", "-j", String(jobs || cpus().length)], {
      cwd: join(CONFIG.rootDir, CONFIG.srcTauriDir),
      silent: true,
    });

    if (result.status !== 0) {
      console.log(result.stdout);
      console.log(result.stderr);
      log.error("单元测试失败");
      process.exit(3);
    }

    // 解析测试数量
    const match = result.stdout.match(/test result: ok\. (\d+) passed; (\d+) failed/);
    if (match) {
      log.success(`单元测试通过（${match[1]} 个测试）`);
    } else {
      log.success("单元测试通过");
    }
  }

  /** 阶段 4：Tauri 构建 + 打包 */
  async build() {
    log.stage("Stage 4: Build");

    // 4.1 前端构建
    log.info("构建前端 (vite build) ...");
    const feResult = CommandExecutor.run("npm", ["run", "build"]);
    if (feResult.status !== 0) {
      log.error("前端构建失败");
      process.exit(4);
    }
    log.success("前端构建完成");

    // 4.2 Rust 编译 + Tauri 打包
    const profile = this.options.debug ? "debug" : "release";
    const tauriArgs = ["tauri", "build"];

    if (this.options.debug) {
      tauriArgs.push("--debug");
    }

    // 仅打包指定 bundle target
    if (this.options.target) {
      tauriArgs.push("--bundles", this.options.target);
    } else if (process.env.TAURI_BUNDLE_TARGETS) {
      tauriArgs.push("--bundles", process.env.TAURI_BUNDLE_TARGETS);
    } else {
      // 用默认全部（每个平台不同）
    }

    log.info(`运行 npx ${tauriArgs.join(" ")} ...`);
    log.info(paint(Colors.gray, `  profile: ${profile}`));
    if (this.buildConfig) {
      log.info(paint(Colors.gray, `  CARGO_BUILD_JOBS=${this.buildConfig.jobs}（传给内部 cargo build）`));
    }
    log.info(paint(Colors.gray, `  首次编译约 5-10 分钟，请耐心等待 ...`));

    // 把 CARGO_BUILD_JOBS 注入到子进程环境（Cargo 原生支持）
    const tauriEnv = this.buildConfig
      ? { CARGO_BUILD_JOBS: String(this.buildConfig.jobs) }
      : {};

    const tauriResult = CommandExecutor.run("npx", tauriArgs, { env: tauriEnv });

    if (tauriResult.status !== 0) {
      log.error("Tauri 构建失败");
      process.exit(4);
    }

    log.success("Tauri 构建完成");
  }
}

// ============================================================================
// 产物报告器
// ============================================================================

class ArtifactReporter {
  constructor(envInfo) {
    this.envInfo = envInfo;
    this.artifacts = [];
  }

  report() {
    log.stage("Stage 5: Report");

    const bundleRoot = join(CONFIG.rootDir, CONFIG.bundleDir);
    if (!existsSync(bundleRoot)) {
      log.warn(`bundle 目录不存在: ${bundleRoot}`);
      log.warn("可能是构建被跳过或失败");
      return;
    }

    this._scanArtifacts(bundleRoot);

    if (this.artifacts.length === 0) {
      log.warn("未找到任何产物文件");
      return;
    }

    // 输出产物清单
    console.log();
    console.log(paint(Colors.bold, "📦 打包产物清单:"));
    console.log(paint(Colors.gray, "─".repeat(80)));

    let totalSize = 0;
    for (const art of this.artifacts) {
      const sizeStr = this._formatSize(art.size);
      const relPath = relative(CONFIG.rootDir, art.path);
      console.log(
        `  ${paint(Colors.green, art.type.padEnd(10))}  ${sizeStr.padStart(12)}  ${relPath}`,
      );
      console.log(paint(Colors.gray, `  ${" ".repeat(10)}  SHA256: ${art.sha256.slice(0, 16)}...`));
      totalSize += art.size;
    }

    console.log(paint(Colors.gray, "─".repeat(80)));
    console.log(`  ${paint(Colors.bold, "总计")}: ${this.artifacts.length} 个文件, ${this._formatSize(totalSize)}`);
    console.log();

    // 写入产物清单文件
    this._writeManifest();

    log.success(`打包完成！产物位于: ${relative(CONFIG.rootDir, bundleRoot)}`);
  }

  _scanArtifacts(dir) {
    // 各平台合法产物扩展名（统一小写匹配，避免大小写敏感导致漏报）
    const platformExt = {
      win32: [".msi", ".exe"],
      linux: [".deb", ".appimage"],
      darwin: [".dmg", ".app"],
    };
    const validExts = new Set(platformExt[this.envInfo.info.platform] || []);

    const walk = (d) => {
      let entries;
      try {
        entries = readdirSync(d, { withFileTypes: true });
      } catch (e) {
        log.warn(`无法读取目录: ${relative(CONFIG.rootDir, d)} (${e.message})`);
        return;
      }
      log.debug(`scanning: ${relative(CONFIG.rootDir, d)} (${entries.length} entries)`);
      for (const entry of entries) {
        const fullPath = join(d, entry.name);
        if (entry.isDirectory()) {
          walk(fullPath);
        } else if (entry.isFile()) {
          // 使用 path.extname 替代手动 lastIndexOf，更健壮（处理无扩展名/多点文件名）
          const ext = extname(entry.name).toLowerCase();
          if (validExts.has(ext)) {
            try {
              const stat = statSync(fullPath);
              const sha256 = this._computeSha256(fullPath);
              this.artifacts.push({
                type: ext.replace(".", "").toLowerCase(),
                path: fullPath,
                size: stat.size,
                sha256,
                mtime: stat.mtime,
              });
            } catch (e) {
              // 单个文件处理失败不中断整个扫描（可能文件被占用/权限不足）
              log.warn(`无法处理文件: ${relative(CONFIG.rootDir, fullPath)} (${e.message})`);
            }
          }
        }
      }
    };
    walk(dir);

    // 按 mtime 倒序（最新优先）
    this.artifacts.sort((a, b) => b.mtime - a.mtime);
  }

  _computeSha256(filePath) {
    const hash = createHash("sha256");
    // 大文件分块读取
    const fd = readFileSync(filePath);
    hash.update(fd);
    return hash.digest("hex");
  }

  _formatSize(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
    return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }

  _writeManifest() {
    const manifest = {
      version: this.envInfo.packageVersion,
      platform: this.envInfo.info.platform,
      arch: this.envInfo.arch,
      buildTime: new Date().toISOString(),
      artifacts: this.artifacts.map((a) => ({
        type: a.type,
        path: relative(CONFIG.rootDir, a.path).replace(/\\/g, "/"),
        size: a.size,
        sha256: a.sha256,
      })),
    };

    const manifestPath = join(CONFIG.rootDir, CONFIG.bundleDir, "manifest.json");
    try {
      writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));
      log.info(`产物清单: ${relative(CONFIG.rootDir, manifestPath)}`);
    } catch (e) {
      log.warn(`无法写入清单文件: ${e.message}`);
    }
  }
}

// ============================================================================
// 主入口
// ============================================================================

async function main() {
  const startTime = Date.now();
  console.log(paint(Colors.bold + Colors.cyan, "\n🚀 无界启动器 打包脚本 v1.0\n"));

  const argParser = new ArgParser(process.argv.slice(2));
  if (argParser.options.help) {
    argParser.showHelp();
    process.exit(0);
  }

  try {
    // 解析编译配置（在所有阶段前一次性确定，确保一致性）
    const buildConfig = new BuildConfig();

    const checker = new EnvironmentChecker(argParser.options, buildConfig);
    await checker.check();

    const pipeline = new BuildPipeline(argParser.options, checker, buildConfig);
    await pipeline.run();

    const reporter = new ArtifactReporter(checker);
    reporter.report();

    const elapsed = Math.round((Date.now() - startTime) / 1000);
    console.log();
    log.success(paint(Colors.bold, `全部完成！耗时 ${elapsed}s`));
    console.log();
    process.exit(0);
  } catch (e) {
    console.log();
    log.error(`打包失败: ${e.message}`);
    if (process.env.DEBUG) {
      console.error(e.stack);
    }
    console.log();
    log.info(paint(Colors.gray, "如需排查请参考 PR/08-验证清单.md §7 故障排查"));
    process.exit(4);
  }
}

main();
