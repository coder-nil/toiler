import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer, type Socket } from "node:net";
import { randomUUID } from "node:crypto";
import type { BashOperations } from "@earendil-works/pi-coding-agent";

type CredentialPrompt = (prompt: string, sensitive: boolean) => Promise<string | undefined>;

type BrokerRequest = { prompt: string };
type BrokerResponse = { value?: string; cancelled?: true };

const SOCKET_PREFIX = "pi-credential-";
const HELPER_NAME = "askpass.js";

function isCredentialPrompt(prompt: string): boolean {
  return /(?:password|passphrase|passcode|token|username|user name|verification code)/i.test(prompt);
}

function helperSource(): string {
  return `#!/usr/bin/env node
const net = require("node:net");
const socketPath = process.env.PI_CREDENTIAL_SOCKET;
const prompt = process.argv.slice(2).join(" ");
if (!socketPath || !prompt) process.exit(1);
const socket = net.createConnection(socketPath);
let response = "";
socket.setEncoding("utf8");
socket.on("data", (chunk) => { response += chunk; });
socket.on("end", () => {
  try {
    const parsed = JSON.parse(response);
    if (typeof parsed.value === "string") process.stdout.write(parsed.value + "\\n");
  } catch {}
  process.exit(0);
});
socket.on("error", () => process.exit(1));
socket.on("connect", () => socket.end(JSON.stringify({ prompt }) + "\\n"));
`;
}

function lineReader(socket: Socket, onLine: (line: string) => void): void {
  let buffer = "";
  socket.setEncoding("utf8");
  socket.on("data", (chunk) => {
    buffer += chunk;
    let newline = buffer.indexOf("\n");
    while (newline >= 0) {
      const line = buffer.slice(0, newline).replace(/\r$/, "");
      buffer = buffer.slice(newline + 1);
      onLine(line);
      newline = buffer.indexOf("\n");
    }
  });
}

/**
 * Wraps shell execution with a process-local Git/SSH AskPass broker.
 * Credentials are exchanged over a private Unix socket and never returned
 * through the agent tool result.
 */
export function createCredentialBrokerOperations(
  base: BashOperations,
  requestCredential: CredentialPrompt,
  options: { platform?: NodeJS.Platform; processPath?: string } = {},
): BashOperations {
  const platform = options.platform ?? process.platform;
  const processPath = options.processPath ?? process.execPath;

  return {
    async exec(command, cwd, executionOptions) {
      if (platform === "win32") return base.exec(command, cwd, executionOptions);

      const directory = mkdtempSync(join(tmpdir(), SOCKET_PREFIX));
      const socketPath = join(directory, "broker.sock");
      const helperPath = join(directory, HELPER_NAME);
      writeFileSync(helperPath, helperSource().replace("#!/usr/bin/env node", `#!${processPath}`), { encoding: "utf8", mode: 0o700 });
      chmodSync(helperPath, 0o700);

      let settled = false;
      const server = createServer((socket) => {
        if (settled) {
          socket.destroy();
          return;
        }
        lineReader(socket, (line) => {
          let request: BrokerRequest;
          try {
            request = JSON.parse(line) as BrokerRequest;
          } catch {
            socket.end(JSON.stringify({ cancelled: true } satisfies BrokerResponse));
            return;
          }
          if (!request.prompt || !isCredentialPrompt(request.prompt)) {
            socket.end(JSON.stringify({ cancelled: true } satisfies BrokerResponse));
            return;
          }
          void requestCredential(
            request.prompt,
            !/\b(?:username|user name)\b/i.test(request.prompt),
          ).then((value) => {
            const response: BrokerResponse = value === undefined
              ? { cancelled: true }
              : { value };
            socket.end(JSON.stringify(response));
          }, () => socket.end(JSON.stringify({ cancelled: true } satisfies BrokerResponse)));
        });
      });

      try {
        await new Promise<void>((resolve, reject) => {
          server.once("error", reject);
          server.listen(socketPath, resolve);
        });
        const env = {
          ...(executionOptions.env ?? process.env),
          GIT_ASKPASS: helperPath,
          SSH_ASKPASS: helperPath,
          SSH_ASKPASS_REQUIRE: "force",
          DISPLAY: executionOptions.env?.DISPLAY ?? process.env.DISPLAY ?? "pi-desktop",
          PI_CREDENTIAL_SOCKET: socketPath,
          PI_CREDENTIAL_OPERATION: randomUUID(),
          GIT_TERMINAL_PROMPT: "0",
        };
        return await base.exec(command, cwd, { ...executionOptions, env });
      } finally {
        settled = true;
        await new Promise<void>((resolve) => server.close(() => resolve()));
        rmSync(directory, { recursive: true, force: true, maxRetries: 3 });
      }
    },
  };
}
