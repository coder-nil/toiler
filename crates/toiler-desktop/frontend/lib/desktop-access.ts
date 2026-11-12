import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { domainToASCII, domainToUnicode } from "node:url";
import { getAgentDir } from "@earendil-works/pi-coding-agent";

/**
 * 手机访问（局域网）配置。
 *
 * 这份文件是 Node 侧与桌面壳（Rust 代理）之间的唯一契约：
 *   - 设置页通过 /api/mobile/access 写入；
 *   - Rust 代理每次请求检查文件 mtime，变化即热加载，所以改密码不需要重启任何进程；
 *   - `lanPort` 由 Rust 在代理真正监听成功后回写，Node 侧不再自己猜端口。
 */
export interface DesktopLanAccess {
  enabled: boolean;
  password: string | null;
  /**
   * 公网入口（Cloudflare Tunnel）。与 enabled 相互独立：局域网关掉了也可以单独
   * 开公网，反之亦然（只是公网那条链路必须先有密码，代理才肯监听）。
   */
  public: DesktopPublicAccess;
}

export type DesktopTunnelMode = "quick" | "named";

export interface DesktopPublicAccess {
  enabled: boolean;
  /** quick = 临时 trycloudflare 地址；named = 固定域名（需要先 cloudflared login）。 */
  mode: DesktopTunnelMode;
  /** **永远是 punycode**（`xn--1xa.works`）；界面上的 Unicode 形式只存在于 UI 层。 */
  hostname: string | null;
}

/** 桌面壳回写的运行时状态：代理真正监听的端口。 */
export interface DesktopLanRuntime {
  lanPort: number | null;
}

/** 桌面壳回写的公网隧道状态（写在与代理不同的文件里，避免互相覆盖）。 */
export interface DesktopTunnelRuntime {
  state: DesktopTunnelState;
  mode: DesktopTunnelMode;
  url: string | null;
  hostname: string | null;
  pid: number | null;
  restarts: number;
  lastError: string | null;
  logTail: string[];
  updatedAtMs: number | null;
}

export type DesktopTunnelState =
  | "disabled"
  /** 手机访问开关没打开，入口代理还没监听。 */
  | "needs-proxy"
  /** 找不到 cloudflared 可执行文件。 */
  | "missing-binary"
  /** named 模式但还没登录 Cloudflare。 */
  | "needs-login"
  | "starting"
  | "running"
  | "error";

export const MIN_DESKTOP_ACCESS_PASSWORD_LENGTH = 8;

const EMPTY_PUBLIC_ACCESS: DesktopPublicAccess = { enabled: false, mode: "quick", hostname: null };

function emptyAccess(): DesktopLanAccess {
  return { enabled: false, password: null, public: { ...EMPTY_PUBLIC_ACCESS } };
}

const EMPTY_RUNTIME: DesktopLanRuntime = { lanPort: null };
const EMPTY_TUNNEL_RUNTIME: DesktopTunnelRuntime = {
  state: "disabled",
  mode: "quick",
  url: null,
  hostname: null,
  pid: null,
  restarts: 0,
  lastError: null,
  logTail: [],
  updatedAtMs: null,
};

export function desktopAccessPath(agentDir = getAgentDir()): string {
  return join(agentDir, "desktop-access.json");
}

/**
 * 运行时状态单独存放，避免设置页与代理同时写同一个文件。
 */
export function desktopLanRuntimePath(agentDir = getAgentDir()): string {
  return join(agentDir, "desktop-access.runtime.json");
}

/**
 * 隧道的运行态单独一份文件：`desktop-access.runtime.json` 由代理的管理器线程
 * 反复重写，两个线程各写一份会互相覆盖（lanPort 会被挤掉）。
 */
export function desktopTunnelRuntimePath(agentDir = getAgentDir()): string {
  return join(agentDir, "desktop-access.tunnel.json");
}

function normalizePort(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > 65535) return null;
  return value;
}

/** 进程号：macOS 的 PID 上限是 99998，不能复用端口那套 16 位校验。 */
function normalizePid(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1) return null;
  return value;
}

function normalizePassword(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * 把界面上输入的域名归一化成 punycode。
 *
 * 契约：落盘的 hostname 永远是 punycode。原因有两个：
 *   1. 浏览器发出去的 Host 头就是 punycode，cloudflared 的 ingress 按它匹配；
 *   2. 桌面壳（Rust）那边拒绝一切非 ASCII 主机名，等于拒绝了「没转换过的输入」。
 */
export function normalizeAccessHostname(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  if (!trimmed) return null;
  const ascii = domainToASCII(trimmed).toLowerCase();
  if (!ascii || !ascii.includes(".") || ascii.length > 253) return null;
  const valid = ascii.split(".").every((label) =>
    label.length > 0
    && label.length <= 63
    && /^[a-z0-9-]+$/.test(label)
    && !label.startsWith("-")
    && !label.endsWith("-"));
  return valid ? ascii : null;
}

function normalizeTunnelMode(value: unknown): DesktopTunnelMode {
  return value === "named" ? "named" : "quick";
}

/** punycode → 界面展示用的 Unicode 形式（`xn--1xa.works` → `π.works`）。 */
export function displayHostname(hostname: string | null): string | null {
  if (!hostname) return null;
  return domainToUnicode(hostname);
}

function normalizePublicAccess(value: unknown): DesktopPublicAccess {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { ...EMPTY_PUBLIC_ACCESS };
  }
  const record = value as Record<string, unknown>;
  const mode = normalizeTunnelMode(record.mode);
  const hostname = normalizeAccessHostname(record.hostname);
  // named 模式没有合法域名就不能算启用：否则会起一个必然 404 的隧道。
  const enabled = record.enabled === true && (mode === "quick" || hostname !== null);
  return { enabled, mode, hostname };
}

/** 读取配置；文件缺失或损坏时都退回到「未启用」，绝不让坏配置把局域网开出来。 */
export function readDesktopLanAccess(databasePath = desktopAccessPath()): DesktopLanAccess {
  try {
    const parsed = JSON.parse(readFileSync(databasePath, "utf-8")) as unknown;
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return emptyAccess();
    const record = parsed as Record<string, unknown>;
    const password = normalizePassword(record.password);
    return {
      enabled: record.enabled === true && password !== null,
      password,
      public: normalizePublicAccess(record.public),
    };
  } catch {
    return emptyAccess();
  }
}

/** 读取桌面壳回写的监听端口。 */
export function readDesktopLanRuntime(databasePath = desktopLanRuntimePath()): DesktopLanRuntime {
  try {
    const parsed = JSON.parse(readFileSync(databasePath, "utf-8")) as unknown;
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return { ...EMPTY_RUNTIME };
    const record = parsed as Record<string, unknown>;
    // 这份文件跨重启复用，而端口是随机分配的：只认本实例（同一个外壳进程）写下的记录，
    // 否则二维码/局域网跳转会把手机指向一个已经属于别的进程（例如 Next dev server）
    // 的端口。命令行启动时没有这个环境变量，保持旧行为。
    const shellPid = Number.parseInt(process.env.PI_WEB_SHELL_PID ?? "", 10);
    if (Number.isInteger(shellPid) && shellPid > 0 && record.ownerPid !== shellPid) {
      return { ...EMPTY_RUNTIME };
    }
    return { lanPort: normalizePort(record.lanPort) };
  } catch {
    return { ...EMPTY_RUNTIME };
  }
}

/** 读取桌面壳回写的公网隧道状态。文件缺失（从没开过）时返回 disabled。 */
export function readDesktopTunnelRuntime(
  databasePath = desktopTunnelRuntimePath(),
): DesktopTunnelRuntime {
  try {
    const parsed = JSON.parse(readFileSync(databasePath, "utf-8")) as unknown;
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return { ...EMPTY_TUNNEL_RUNTIME };
    }
    const record = parsed as Record<string, unknown>;
    const state = typeof record.state === "string" && TUNNEL_STATES.has(record.state)
      ? record.state as DesktopTunnelState
      : "disabled";
    const logTail = Array.isArray(record.logTail)
      ? record.logTail.filter((line): line is string => typeof line === "string").slice(-40)
      : [];
    return {
      state,
      mode: normalizeTunnelMode(record.mode),
      url: typeof record.url === "string" ? record.url : null,
      hostname: typeof record.hostname === "string" ? record.hostname : null,
      pid: normalizePid(record.pid),
      restarts: typeof record.restarts === "number" && Number.isFinite(record.restarts)
        ? Math.max(0, Math.trunc(record.restarts))
        : 0,
      lastError: typeof record.lastError === "string" ? record.lastError : null,
      logTail,
      updatedAtMs: typeof record.updatedAtMs === "number" && Number.isFinite(record.updatedAtMs)
        ? record.updatedAtMs
        : null,
    };
  } catch {
    return { ...EMPTY_TUNNEL_RUNTIME };
  }
}

const TUNNEL_STATES = new Set<string>([
  "disabled",
  "needs-proxy",
  "missing-binary",
  "needs-login",
  "starting",
  "running",
  "error",
]);

export function writeDesktopLanAccess(
  access: DesktopLanAccess,
  databasePath = desktopAccessPath(),
): void {
  mkdirSync(dirname(databasePath), { recursive: true });
  const payload: DesktopLanAccess = {
    enabled: access.enabled && access.password !== null,
    password: access.password,
    public: normalizePublicAccess(access.public),
  };
  writeFileSync(databasePath, JSON.stringify(payload, null, 2), { encoding: "utf-8", mode: 0o600 });
  chmodSync(databasePath, 0o600);
}

export type PasswordValidation =
  | { ok: true; value: string }
  | { ok: false; error: "too-short" };

/**
 * 密码规则：非空且至少 8 位。
 *
 * 这是局域网里唯一的一道闸门 —— 拿到它就能指挥本机的 agent，所以不接受弱口令。
 */
export function validateDesktopAccessPassword(password: unknown): PasswordValidation {
  if (typeof password !== "string") return { ok: false, error: "too-short" };
  const trimmed = password.trim();
  if (trimmed.length < MIN_DESKTOP_ACCESS_PASSWORD_LENGTH) return { ok: false, error: "too-short" };
  return { ok: true, value: trimmed };
}
