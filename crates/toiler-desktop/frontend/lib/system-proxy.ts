import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const SYSTEM_PROXY_TIMEOUT_MS = 2_000;

type SystemProxyValues = Record<string, string>;

function hasProxyEnvironment(env: Readonly<NodeJS.ProcessEnv>): boolean {
  return Boolean(
    env.HTTP_PROXY || env.HTTPS_PROXY || env.ALL_PROXY
    || env.http_proxy || env.https_proxy || env.all_proxy,
  );
}

export function parseMacOsSystemProxy(output: string): SystemProxyValues {
  const values: SystemProxyValues = {};
  for (const match of output.matchAll(/^\s*([^:\s]+)\s*:\s*(.*?)\s*$/gm)) {
    values[match[1]] = match[2];
  }
  return values;
}

function proxyUrl(values: SystemProxyValues, prefix: "HTTP" | "HTTPS" | "SOCKS"): string | undefined {
  if (values[`${prefix}Enable`] !== "1") return undefined;
  const host = values[`${prefix}Proxy`]?.trim();
  const port = Number(values[`${prefix}Port`]);
  if (!host || !Number.isInteger(port) || port < 1 || port > 65_535) return undefined;

  const normalizedHost = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  return `${prefix === "SOCKS" ? "socks5h" : "http"}://${normalizedHost}:${port}`;
}

export function systemProxyEnvironmentFromValues(values: SystemProxyValues): Partial<NodeJS.ProcessEnv> {
  const httpProxy = proxyUrl(values, "HTTP");
  const httpsProxy = proxyUrl(values, "HTTPS") ?? httpProxy;
  const socksProxy = proxyUrl(values, "SOCKS");
  const proxyEnv: Partial<NodeJS.ProcessEnv> = {};

  if (httpProxy) {
    proxyEnv.HTTP_PROXY = httpProxy;
    proxyEnv.http_proxy = httpProxy;
  }
  if (httpsProxy) {
    proxyEnv.HTTPS_PROXY = httpsProxy;
    proxyEnv.https_proxy = httpsProxy;
  }
  if (socksProxy) {
    proxyEnv.ALL_PROXY = socksProxy;
    proxyEnv.all_proxy = socksProxy;
  }
  return proxyEnv;
}

/**
 * Translate macOS's manual proxy settings into environment variables used by
 * Git, npm, and the skills CLI. Explicit environment settings always win.
 */
export async function getSystemProxyEnvironment(
  env: Readonly<NodeJS.ProcessEnv> = process.env,
): Promise<Partial<NodeJS.ProcessEnv>> {
  if (process.platform !== "darwin" || hasProxyEnvironment(env)) return {};

  try {
    const { stdout } = await execFileAsync("scutil", ["--proxy"], {
      timeout: SYSTEM_PROXY_TIMEOUT_MS,
      windowsHide: true,
    });
    const values = parseMacOsSystemProxy(stdout);
    return systemProxyEnvironmentFromValues(values);
  } catch {
    return {};
  }
}
