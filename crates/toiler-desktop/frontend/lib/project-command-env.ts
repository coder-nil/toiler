import {
  createBashToolDefinition,
  createLocalBashOperations,
  createLocalPowerShellOperations,
  createPowerShellToolDefinition,
  getAgentDir,
  type BashOperations,
  type InlineExtension,
  type LoadExtensionsResult,
} from "@earendil-works/pi-coding-agent";
import { execFile } from "node:child_process";
import { userInfo } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { createCredentialBrokerOperations } from "./credential-broker";
import { resolveShellSelection, type DesktopShellSelection } from "./shell-tool";

const execFileAsync = promisify(execFile);
const USER_ENVIRONMENT_MARKER = "__PI_DESKTOP_USER_ENVIRONMENT__";
const USER_ENVIRONMENT_TIMEOUT_MS = 3_000;
const USER_ENVIRONMENT_MAX_BUFFER = 4 * 1024 * 1024;

const HOST_EXTENSION_NAME = "pi-desktop-project-command-environment";
const HOST_EXTENSION_PATH = `<inline:${HOST_EXTENSION_NAME}>`;
const HOST_OVERRIDDEN_TOOL_NAMES = ["bash", "powershell"] as const;
const CREDENTIAL_PROMPT_GUIDELINE = "When Git, SSH, or another command requests a username, password, passphrase, token, or verification code, run the normal command and wait for Pi Desktop's secure credential prompt. Never ask the user to paste secrets into chat or pass secrets through ask_user.";

type ProjectShellSettings = {
  getShellCommandPrefix(): string | undefined;
  getShellPath(): string | undefined;
};

type ProjectCommandBashOperationsOptions = {
  agentBinDir?: string;
  baseEnvironment?: NodeJS.ProcessEnv;
  currentUserEnvironment?: NodeJS.ProcessEnv;
  localOperations?: BashOperations;
  platform?: NodeJS.Platform;
  shellPath?: string;
  requestCredential?: (prompt: string, sensitive: boolean) => Promise<string | undefined>;
};

let currentUserEnvironmentPromise: Promise<NodeJS.ProcessEnv> | undefined;

function parseExportedEnvironment(output: Buffer): NodeJS.ProcessEnv {
  const fields = output.toString("utf8").split("\0");
  const markerIndex = fields.indexOf(USER_ENVIRONMENT_MARKER);
  if (markerIndex < 0) return {} as NodeJS.ProcessEnv;

  const environment = {} as NodeJS.ProcessEnv;
  for (const field of fields.slice(markerIndex + 1)) {
    const separatorIndex = field.indexOf("=");
    if (separatorIndex <= 0) continue;
    const name = field.slice(0, separatorIndex);
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) continue;
    environment[name] = field.slice(separatorIndex + 1);
  }
  return environment;
}

async function loadCurrentUserEnvironment(
  baseEnvironment: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
): Promise<NodeJS.ProcessEnv> {
  if (platform === "win32") return {} as NodeJS.ProcessEnv;

  let shellPath: string | undefined;
  try {
    shellPath = userInfo().shell || baseEnvironment.SHELL;
  } catch {
    shellPath = baseEnvironment.SHELL;
  }
  if (!shellPath) return {} as NodeJS.ProcessEnv;

  try {
    const { stdout } = await execFileAsync(
      shellPath,
      ["-ilc", `printf '\\0${USER_ENVIRONMENT_MARKER}\\0'; /usr/bin/env -0`],
      {
        cwd: baseEnvironment.HOME,
        env: baseEnvironment,
        encoding: "buffer",
        maxBuffer: USER_ENVIRONMENT_MAX_BUFFER,
        timeout: USER_ENVIRONMENT_TIMEOUT_MS,
      },
    );
    return parseExportedEnvironment(Buffer.isBuffer(stdout) ? stdout : Buffer.from(stdout));
  } catch {
    return {} as NodeJS.ProcessEnv;
  }
}

function getCurrentUserEnvironment(
  baseEnvironment: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
): Promise<NodeJS.ProcessEnv> {
  if (!currentUserEnvironmentPromise) {
    currentUserEnvironmentPromise = loadCurrentUserEnvironment(baseEnvironment, platform);
  }
  return currentUserEnvironmentPromise;
}

function pathKey(environment: NodeJS.ProcessEnv, platform: NodeJS.Platform): string {
  return platform === "win32"
    ? Object.keys(environment).find((name) => name.toUpperCase() === "PATH") ?? "PATH"
    : "PATH";
}

function mergeCurrentUserEnvironment(
  baseEnvironment: NodeJS.ProcessEnv,
  currentUserEnvironment: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
): NodeJS.ProcessEnv {
  const environment = { ...currentUserEnvironment, ...baseEnvironment };
  const basePathKey = pathKey(baseEnvironment, platform);
  const userPathKey = pathKey(currentUserEnvironment, platform);
  const mergedPathKey = pathKey(environment, platform);
  const pathDelimiter = platform === "win32" ? ";" : ":";
  const pathEntries = [
    ...(currentUserEnvironment[userPathKey] ?? "").split(pathDelimiter),
    ...(baseEnvironment[basePathKey] ?? "").split(pathDelimiter),
  ].filter(Boolean);
  if (pathEntries.length > 0) {
    environment[mergedPathKey] = [...new Set(pathEntries)].join(pathDelimiter);
  }
  return environment;
}

function isHostRuntimeVariable(name: string, platform: NodeJS.Platform): boolean {
  const comparableName = platform === "win32" ? name.toUpperCase() : name;
  return comparableName === "PORT"
    || comparableName === "NODE_ENV"
    || comparableName.startsWith("NEXT_");
}

export function sanitizeProjectCommandEnvironment(
  baseEnvironment: NodeJS.ProcessEnv,
  platform: NodeJS.Platform = process.platform,
): NodeJS.ProcessEnv {
  const environment = { ...baseEnvironment };
  for (const name of Object.keys(environment)) {
    if (isHostRuntimeVariable(name, platform)) delete environment[name];
  }
  return environment;
}

function withAgentBinDirectory(
  environment: NodeJS.ProcessEnv,
  agentBinDir: string,
  platform: NodeJS.Platform,
): NodeJS.ProcessEnv {
  const resolvedPathKey = pathKey(environment, platform);
  const pathDelimiter = platform === "win32" ? ";" : ":";
  const currentPath = environment[resolvedPathKey] ?? "";
  const pathEntries = currentPath.split(pathDelimiter).filter(Boolean);
  if (!pathEntries.includes(agentBinDir)) {
    environment[resolvedPathKey] = [agentBinDir, currentPath].filter(Boolean).join(pathDelimiter);
  }
  return environment;
}

export function createProjectCommandBashOperations(
  options: ProjectCommandBashOperationsOptions = {},
): BashOperations {
  const {
    agentBinDir = join(getAgentDir(), "bin"),
    baseEnvironment = process.env,
    localOperations = createLocalBashOperations({ shellPath: options.shellPath }),
    platform = process.platform,
    requestCredential,
  } = options;

  const operations = requestCredential
    ? createCredentialBrokerOperations(localOperations, requestCredential, { platform })
    : localOperations;

  return {
    async exec(command, cwd, executionOptions) {
      const baseCommandEnvironment = executionOptions.env ?? baseEnvironment;
      const userEnvironment = options.currentUserEnvironment
        ?? await getCurrentUserEnvironment(baseEnvironment, platform);
      const environment = withAgentBinDirectory(
        sanitizeProjectCommandEnvironment(
          mergeCurrentUserEnvironment(baseCommandEnvironment, userEnvironment, platform),
          platform,
        ),
        agentBinDir,
        platform,
      );
      return operations.exec(command, cwd, {
        ...executionOptions,
        env: environment,
      });
    },
  };
}

export function createProjectCommandBashExtension(options: {
  cwd: string;
  settings: ProjectShellSettings;
  shellSelection?: DesktopShellSelection;
}): InlineExtension {
  return {
    name: HOST_EXTENSION_NAME,
    hidden: true,
    factory: (pi) => {
      const selection = options.shellSelection ?? resolveShellSelection(options.settings);
      const displayDefinition = createBashToolDefinition(options.cwd);
      pi.registerTool({
        ...displayDefinition,
        promptGuidelines: [
          ...(displayDefinition.promptGuidelines ?? []),
          CREDENTIAL_PROMPT_GUIDELINE,
        ],
        execute(toolCallId, params, signal, onUpdate, context) {
          const executionDefinition = createBashToolDefinition(options.cwd, {
            commandPrefix: options.settings.getShellCommandPrefix(),
            operations: createProjectCommandBashOperations({
              shellPath: selection.shellPath,
              requestCredential: credentialPrompter(context),
            }),
          });
          return executionDefinition.execute(toolCallId, params, signal, onUpdate, context);
        },
      });

      if (selection.tool !== "powershell") return;

      // Windows without Git Bash runs commands through PowerShell. Register a
      // host override so the tool keeps the sanitized project environment and
      // the Desktop credential hook instead of dropping to pi's bare builtin.
      const powerShellDefinition = createPowerShellToolDefinition(options.cwd);
      pi.registerTool({
        ...powerShellDefinition,
        promptGuidelines: [
          ...(powerShellDefinition.promptGuidelines ?? []),
          CREDENTIAL_PROMPT_GUIDELINE,
        ],
        execute(toolCallId, params, signal, onUpdate, context) {
          const executionDefinition = createPowerShellToolDefinition(options.cwd, {
            operations: createProjectCommandBashOperations({
              localOperations: createLocalPowerShellOperations(),
              requestCredential: credentialPrompter(context),
            }),
          });
          return executionDefinition.execute(toolCallId, params, signal, onUpdate, context);
        },
      });
    },
  };
}

export function preferUserBashExtension(base: LoadExtensionsResult): LoadExtensionsResult {
  const hostExtensionIndex = base.extensions.findIndex((extension) => extension.path === HOST_EXTENSION_PATH);
  if (hostExtensionIndex < 0) return base;

  const userShellOwner = base.extensions
    .slice(0, hostExtensionIndex)
    .find((extension) => HOST_OVERRIDDEN_TOOL_NAMES.some((name) => extension.tools.has(name)));
  if (!userShellOwner) return base;

  return {
    ...base,
    extensions: base.extensions.filter((_, index) => index !== hostExtensionIndex),
    errors: base.errors.filter((error) => !(
      error.path === HOST_EXTENSION_PATH
      && HOST_OVERRIDDEN_TOOL_NAMES.some((name) => error.error === `Tool "${name}" conflicts with ${userShellOwner.path}`)
    )),
  };
}

function credentialPrompter(context: unknown): (prompt: string, sensitive: boolean) => Promise<string | undefined> {
  return (prompt, sensitive) => {
    const inputUi = (context as {
      ui: {
        input: (title: string, placeholder?: string, options?: { sensitive?: boolean }) => Promise<string | undefined>;
      };
    }).ui;
    return inputUi.input("安全凭据", prompt, { sensitive });
  };
}
