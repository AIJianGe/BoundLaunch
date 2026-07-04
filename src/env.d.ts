/// <reference types="vite/client" />

/**
 * 全局环境类型声明
 *
 * - `__APP_VERSION__`：由 vite.config.ts `define` 注入，构建时从 package.json 读取
 *   用于 AboutPage 显示 launcher 版本号
 * - `__APP_BUILD_TIME__`：构建时间戳（可选，预留）
 *
 * 详见 vite.config.ts 中 `define` 配置
 */

declare const __APP_VERSION__: string;
