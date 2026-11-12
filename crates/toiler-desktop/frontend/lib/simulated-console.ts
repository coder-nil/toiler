export type SimulatedConsoleOutputKind = "output" | "error";

export interface SimulatedConsoleContext {
  cwd: string | null;
  sessionId: string | null;
  sessionName?: string | null;
  history?: string[];
  now?: Date;
}

export interface SimulatedConsoleOutputLine {
  kind: SimulatedConsoleOutputKind;
  text: string;
}

export interface SimulatedConsoleCommandResult {
  clear?: boolean;
  lines: SimulatedConsoleOutputLine[];
}

const HELP_LINES = [
  "Available commands:",
  "  help          Show commands",
  "  clear, cls    Clear the screen",
  "  pwd           Show the active project directory",
  "  session       Show the active Pi session",
  "  echo <text>   Print text",
  "  date          Show the current time",
  "  whoami        Show the simulated user",
  "  history       Show commands from this console",
  "  pi            Show console mode",
];

function output(text: string): SimulatedConsoleOutputLine {
  return { kind: "output", text };
}

function error(text: string): SimulatedConsoleOutputLine {
  return { kind: "error", text };
}

export function executeSimulatedConsoleCommand(
  input: string,
  context: SimulatedConsoleContext,
): SimulatedConsoleCommandResult {
  const command = input.trim();
  if (!command) return { lines: [] };

  const [name = "", ...rest] = command.split(/\s+/);
  const commandName = name.toLowerCase();
  const argText = command.slice(name.length).trimStart();

  switch (commandName) {
    case "help":
      return { lines: HELP_LINES.map(output) };
    case "clear":
    case "cls":
      return { clear: true, lines: [] };
    case "pwd":
      return { lines: [output(context.cwd ?? "No project selected")] };
    case "session":
      if (!context.sessionId) return { lines: [output("No active saved session")] };
      return {
        lines: [
          output(context.sessionName ? `Name: ${context.sessionName}` : "Name: Untitled session"),
          output(`ID: ${context.sessionId}`),
        ],
      };
    case "echo":
      return { lines: [output(argText)] };
    case "date":
      return { lines: [output((context.now ?? new Date()).toISOString())] };
    case "whoami":
      return { lines: [output("pi-desktop")] };
    case "history": {
      const history = context.history ?? [];
      if (history.length === 0) return { lines: [output("No commands yet")] };
      return { lines: history.map((item, index) => output(`${index + 1}  ${item}`)) };
    }
    case "pi":
      return {
        lines: [
          output("Pi Desktop simulated console"),
          output("Commands run only inside this panel; no shell process is started."),
        ],
      };
    default:
      return {
        lines: [
          error(`${rest.length ? command : name}: command not found`),
          output("Type help to see available commands."),
        ],
      };
  }
}
