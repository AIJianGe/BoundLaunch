/**
 * useErrorClassifier — 智能错误分类
 *
 * 设计目的：
 * - 把 ComfyUI 启动失败 / 进程崩溃的错误进行结构化分类
 * - 给出针对性的诊断建议（指导用户操作）
 * - 用于 CrashModal / PortConflictModal 展示
 *
 * 分类维度：
 * 1. 错误类型（ErrorKind）
 * 2. 严重程度（Severity）
 * 3. 可自动恢复（AutoRecoverable）
 * 4. 推荐的修复动作（RecommendedAction）
 *
 * 分类依据：
 * - exit_code：1 = 一般错误，-1/SIGKILL = 强杀
 * - stderr_tail 关键字匹配（按优先级）
 * - 错误消息关键字匹配
 */

export type ErrorKind =
  | "port_in_use"        // 端口被占用
  | "module_not_found"   // ModuleNotFoundError
  | "cuda_unavailable"   // CUDA 不可用
  | "cuda_assertion"     // AssertionError (torch not compiled with CUDA)
  | "oom"                // Out of memory
  | "out_of_memory"      // 同上
  | "torchvision_missing" // torchvision.ops 缺失
  | "python_error"       // Python 语法/运行错误
  | "main_not_found"     // main.py 不存在
  | "venv_corrupted"     // venv 损坏
  | "permission_denied"  // 权限不足
  | "killed_manually"    // 手动停止
  | "killed_oom"         // OOM killer
  | "unknown";           // 未识别

export type Severity = "low" | "medium" | "high" | "critical";

export type RecommendedAction = {
  /** 操作 ID（前端按钮调用） */
  id:
    | "open_settings"
    | "open_torch_settings"
    | "open_path_settings"
    | "kill_occupying_process"
    | "kill_all_python"
    | "reinstall_torch"
    | "reinstall_requirements"
    | "rebuild_venv"
    | "view_logs"
    | "change_port"
    | "restart"
    | "check_installation"
    | "run_as_admin"
    | "reduce_workload"
    | "none";
  /** 操作标签（中文） */
  label: string;
  /** 是否主操作（高亮） */
  primary?: boolean;
};

export interface ErrorClassification {
  kind: ErrorKind;
  kind_label: string;          // 错误类型中文
  severity: Severity;          // 严重程度
  title: string;               // 短标题（一行）
  description: string;         // 详细描述
  root_cause: string;          // 根因说明
  recommended_actions: RecommendedAction[];
  /** 匹配的关键词（用于调试） */
  matched_keywords: string[];
}

/** 输入：crashed 事件或 start_failed 事件的关键字段 */
export interface ClassifyInput {
  exit_code?: number | null;
  stderr_tail?: string[];
  /** 完整错误消息 */
  error_message?: string;
  /** 错误发生时间（用于判断是否被手动 kill） */
  reason?: string;
}

// ============================================================================
// 错误分类规则
// ============================================================================

/** 错误类型 + 严重程度 + 标题模板 + 根因模板 */
const ERROR_RULES: Array<{
  kind: ErrorKind;
  severity: Severity;
  title: string;
  description: string;
  root_cause: string;
  /** 关键字匹配（任一命中即匹配） */
  keywords: string[];
  /** 推荐动作生成器 */
  buildActions: (ctx: { has_port: boolean; module?: string }) => RecommendedAction[];
}> = [
  {
    kind: "port_in_use",
    severity: "medium",
    title: "端口被占用",
    description: "ComfyUI 启动时尝试绑定端口失败，端口已被其他进程占用。",
    root_cause: "目标端口已被另一个进程 LISTEN 占用，ComfyUI 无法绑定。",
    keywords: [
      "Address already in use",
      "Errno 10048",
      "Errno 48",
      "OSError: [Errno 98]",
      "bind: address already in use",
      "port is already in use",
      "PortInUse",
    ],
    buildActions: ({ has_port }) => [
      {
        id: "kill_occupying_process",
        label: "结束占用进程",
        primary: true,
      },
      ...(has_port ? [{ id: "change_port" as const, label: "修改 ComfyUI 端口" }] : []),
      { id: "kill_all_python", label: "清理所有 Python 进程（兜底）" },
      { id: "view_logs", label: "查看完整日志" },
    ],
  },
  {
    kind: "torchvision_missing",
    severity: "high",
    title: "torchvision 依赖不完整",
    description: "检测到 venv 中的 torchvision 缺少 ops/io 子模块，无法运行 ComfyUI。",
    root_cause: "torchvision 安装残缺（与 torch 不匹配或部分子模块未安装）。",
    keywords: [
      "No module named 'torchvision.ops'",
      "No module named 'torchvision.io'",
      "torchvision.transforms.functional_tensor",
    ],
    buildActions: () => [
      {
        id: "open_torch_settings",
        label: "前往 torch 设置页重装",
        primary: true,
      },
      {
        id: "reinstall_torch",
        label: "强制一致重装 torch",
      },
      { id: "view_logs", label: "查看完整日志" },
    ],
  },
  {
    kind: "module_not_found",
    severity: "high",
    title: "Python 依赖缺失",
    description: "ComfyUI 需要的某个 Python 模块未安装。",
    root_cause: "venv 中的依赖缺失或不完整（可能安装阶段被中断）。",
    keywords: [
      "ModuleNotFoundError",
      "ImportError: No module named",
      "ImportError: cannot import name",
    ],
    buildActions: ({ module }) => [
      {
        id: "open_path_settings",
        label: "前往依赖管理",
        primary: true,
      },
      {
        id: "reinstall_requirements",
        label: "重装 ComfyUI 依赖",
      },
      ...(module
        ? [{ id: "view_logs" as const, label: `查看 ${module} 详情` }]
        : [{ id: "view_logs" as const, label: "查看完整日志" }]),
    ],
  },
  {
    kind: "cuda_assertion",
    severity: "high",
    title: "PyTorch 不支持 CUDA",
    description: "当前 torch 安装不包含 CUDA 支持，但启动模式需要 GPU。",
    root_cause: "torch+cpu 启动了 GPU 模式；或在 venv 中 torch/torchvision/torchaudio 来自不同源。",
    keywords: [
      "AssertionError: Torch not compiled with CUDA enabled",
      "Torch not compiled with CUDA",
      "torch.cuda.is_available() == False",
    ],
    buildActions: () => [
      {
        id: "open_torch_settings",
        label: "前往 torch 设置页",
        primary: true,
      },
      {
        id: "reinstall_torch",
        label: "强制一致重装 torch",
      },
      {
        id: "change_port",  // 用作：切换到 CPU 模式
        label: "切换到 CPU 模式",
      },
    ],
  },
  {
    kind: "cuda_unavailable",
    severity: "high",
    title: "CUDA 不可用",
    description: "系统检测不到可用的 CUDA 设备。",
    root_cause: "GPU 驱动未安装 / CUDA 版本不匹配 / 显卡不受支持。",
    keywords: [
      "CUDA not available",
      "cuda runtime error",
      "no CUDA-capable device is detected",
      "NVIDIA driver on your system is too old",
    ],
    buildActions: () => [
      {
        id: "open_torch_settings",
        label: "前往 torch 设置页",
        primary: true,
      },
      {
        id: "check_installation",
        label: "检查 GPU 驱动",
      },
    ],
  },
  {
    kind: "out_of_memory",
    severity: "high",
    title: "显存/内存不足",
    description: "ComfyUI 在加载模型时耗尽显存或内存。",
    root_cause: "工作流过大 / 模型超过显存 / 其他进程占用大量 GPU 内存。",
    keywords: [
      "OutOfMemoryError",
      "CUDA out of memory",
      "cuDNN error: Out of memory",
      "std::bad_alloc",
      "Killed",
      "exit code 137",  // OOM killer 在 Linux
    ],
    buildActions: () => [
      {
        id: "reduce_workload",
        label: "切换到低显存模式",
        primary: true,
      },
      { id: "view_logs", label: "查看完整日志" },
    ],
  },
  {
    kind: "main_not_found",
    severity: "critical",
    title: "ComfyUI 仓库不完整",
    description: "main.py 不存在，ComfyUI 仓库可能未克隆或被删除。",
    root_cause: "ComfyUI 目录缺失或被损坏。",
    keywords: [
      "FileNotFoundError: main.py",
      "No such file or directory: 'main.py'",
      "main.py: No such file or directory",
    ],
    buildActions: () => [
      {
        id: "check_installation",
        label: "检查 ComfyUI 安装",
        primary: true,
      },
    ],
  },
  {
    kind: "venv_corrupted",
    severity: "high",
    title: "Python 虚拟环境损坏",
    description: "venv 目录损坏或 Python 解释器不可用。",
    root_cause: "venv 创建过程被中断 / venv 文件被外部修改。",
    keywords: [
      "venv",
      "pyvenv.cfg",
      "Python interpreter not found",
      "no such file or directory",
    ],
    buildActions: () => [
      {
        id: "rebuild_venv",
        label: "重建 venv",
        primary: true,
      },
      {
        id: "open_path_settings",
        label: "检查 venv 路径",
      },
    ],
  },
  {
    kind: "permission_denied",
    severity: "high",
    title: "权限不足",
    description: "无法启动 ComfyUI，因为缺少必要的文件或网络权限。",
    root_cause: "文件权限被修改 / Windows 上未以管理员身份运行。",
    keywords: [
      "Permission denied",
      "Access is denied",
      "Operation not permitted",
    ],
    buildActions: () => [
      {
        id: "run_as_admin",
        label: "以管理员身份运行",
        primary: true,
      },
    ],
  },
  {
    kind: "python_error",
    severity: "medium",
    title: "Python 错误",
    description: "ComfyUI 启动时遇到 Python 运行时错误。",
    root_cause: "可能是依赖版本冲突或代码兼容性问题。",
    keywords: [
      "SyntaxError",
      "IndentationError",
      "TypeError",
      "AttributeError",
      "ValueError",
    ],
    buildActions: () => [
      { id: "view_logs", label: "查看完整日志", primary: true },
      { id: "open_path_settings", label: "检查 Python 版本" },
    ],
  },
];

/** 兜底：未匹配任何规则 */
const UNKNOWN_RULE = {
  kind: "unknown" as ErrorKind,
  severity: "medium" as Severity,
  title: "未知错误",
  description: "ComfyUI 退出，但未能识别具体原因。",
  root_cause: "可能需要查看完整日志才能定位问题。",
  buildActions: () => [
    { id: "view_logs" as const, label: "查看完整日志", primary: true },
    { id: "restart" as const, label: "重新启动" },
  ],
};

/** 手动 kill 的识别（不视为错误） */
const KILLED_RULES: Array<{
  kind: ErrorKind;
  keywords: string[];
  exit_codes: (number | null)[];
}> = [
  {
    kind: "killed_manually",
    keywords: ["Killed by user", "user stop", "graceful stop"],
    exit_codes: [0, 143], // 143 = 128 + 15 (SIGTERM)
  },
  {
    kind: "killed_oom",
    keywords: ["OOM", "out of memory killer"],
    exit_codes: [137], // 128 + 9 (SIGKILL)
  },
];

// ============================================================================
// 分类函数
// ============================================================================

/** 合并文本用于匹配 */
function buildSearchText(input: ClassifyInput): string {
  const parts: string[] = [];
  if (input.error_message) parts.push(input.error_message);
  if (input.stderr_tail && input.stderr_tail.length > 0) {
    parts.push(input.stderr_tail.join("\n"));
  }
  return parts.join("\n");
}

/** 提取 ModuleNotFoundError 中的模块名 */
function extractModuleName(text: string): string | undefined {
  const m = text.match(/(?:ModuleNotFoundError|ImportError)[^\n]*?['"]([^'"]+)['"]/);
  return m?.[1];
}

/** 主分类函数 */
export function classifyError(input: ClassifyInput): ErrorClassification {
  const searchText = buildSearchText(input);
  const lowerText = searchText.toLowerCase();
  const matchedKeywords: string[] = [];

  // 1. 优先识别手动 kill（不算错误）
  for (const rule of KILLED_RULES) {
    if (input.exit_code !== undefined && rule.exit_codes.includes(input.exit_code)) {
      if (rule.kind === "killed_oom") {
        matchedKeywords.push(`exit_code=${input.exit_code}`);
        return {
        kind: "killed_oom",
        kind_label: "系统 OOM Killer",
        severity: "high",
        title: "系统 OOM 终止",
        description: "Linux OOM Killer 杀掉了 ComfyUI 进程（系统内存不足）。",
        root_cause: "系统总内存不足，OOM Killer 选择杀掉 ComfyUI 释放内存。",
        recommended_actions: [
          { id: "reduce_workload", label: "减小工作流规模", primary: true },
          { id: "view_logs", label: "查看完整日志" },
        ],
        matched_keywords: matchedKeywords,
      };
    }
  }
  if (matchedKeywords.length === 0) {
    // exit_code 命中但没匹配到关键字（罕见的边界情况）
  }
  for (const kw of rule.keywords) {
      if (lowerText.includes(kw.toLowerCase())) {
        matchedKeywords.push(kw);
        if (rule.kind === "killed_manually") {
          return {
            kind: "killed_manually",
            kind_label: "用户主动停止",
            severity: "low",
            title: "用户停止",
            description: "ComfyUI 已被用户手动停止。",
            root_cause: "用户通过 UI 发送了停止命令。",
            recommended_actions: [
              { id: "restart", label: "重新启动", primary: true },
            ],
            matched_keywords: matchedKeywords,
          };
        }
      }
    }
  }

  // 2. 匹配错误规则
  for (const rule of ERROR_RULES) {
    for (const kw of rule.keywords) {
      if (lowerText.includes(kw.toLowerCase())) {
        matchedKeywords.push(kw);
        const module = rule.kind === "module_not_found"
          ? extractModuleName(searchText)
          : undefined;
        return {
          kind: rule.kind,
          kind_label: getErrorKindLabel(rule.kind),
          severity: rule.severity,
          title: rule.title,
          description: rule.description,
          root_cause: rule.root_cause,
          recommended_actions: rule.buildActions({ has_port: true, module }),
          matched_keywords: matchedKeywords,
        };
      }
    }
  }

  // 3. 兜底：未识别
  return {
    kind: UNKNOWN_RULE.kind,
    kind_label: "未知错误",
    severity: UNKNOWN_RULE.severity,
    title: UNKNOWN_RULE.title,
    description: UNKNOWN_RULE.description,
    root_cause: UNKNOWN_RULE.root_cause,
    recommended_actions: UNKNOWN_RULE.buildActions(),
    matched_keywords: matchedKeywords,
  };
}

/** 错误类型 → 中文标签 */
function getErrorKindLabel(kind: ErrorKind): string {
  const map: Record<ErrorKind, string> = {
    port_in_use: "端口冲突",
    module_not_found: "依赖缺失",
    cuda_unavailable: "CUDA 不可用",
    cuda_assertion: "PyTorch 不支持 CUDA",
    oom: "显存不足",
    out_of_memory: "内存不足",
    torchvision_missing: "torchvision 损坏",
    python_error: "Python 错误",
    main_not_found: "ComfyUI 缺失",
    venv_corrupted: "venv 损坏",
    permission_denied: "权限不足",
    killed_manually: "用户停止",
    killed_oom: "系统 OOM",
    unknown: "未知错误",
  };
  return map[kind];
}

/** Composable 包装 */
export function useErrorClassifier() {
  return {
    classify: classifyError,
  };
}
