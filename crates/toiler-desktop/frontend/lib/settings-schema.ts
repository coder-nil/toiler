import { existsSync, readFileSync } from "node:fs";
import { writePrivateFileAtomicSync } from "./atomic-file";

export const SETTINGS_SCHEMA_URL = "https://pi.dev/schemas/settings.schema.json";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Add Pi's settings schema without changing an explicitly selected schema. */
export function withSettingsSchema(settings: Record<string, unknown>): Record<string, unknown> {
  return Object.hasOwn(settings, "$schema")
    ? settings
    : { $schema: SETTINGS_SCHEMA_URL, ...settings };
}

/** Add the schema after an SDK SettingsManager has created or updated a file. */
export function ensureSettingsSchemaFile(settingsPath: string): void {
  if (!existsSync(settingsPath)) return;
  const parsed: unknown = JSON.parse(readFileSync(settingsPath, "utf8"));
  if (!isRecord(parsed) || Object.hasOwn(parsed, "$schema")) return;
  writePrivateFileAtomicSync(settingsPath, JSON.stringify(withSettingsSchema(parsed), null, 2));
}
