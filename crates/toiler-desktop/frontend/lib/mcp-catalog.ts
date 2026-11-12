import catalog from "@/data/mcp-catalog.json";

export type McpServerEntry = {
  url?: string;
  command?: string;
  args?: string[];
  auth?: "oauth";
  protocolVersion?: "auto";
  directTools?: boolean;
};

export type McpCatalogEntry = {
  id: string;
  name: string;
  nameZh: string;
  summary: string;
  summaryZh: string;
  auth: "none" | "oauth";
  entry: McpServerEntry;
  homepage?: string;
};

export const MCP_CATALOG = catalog as McpCatalogEntry[];

export function getMcpCatalogEntry(id: string): McpCatalogEntry | undefined {
  return MCP_CATALOG.find((entry) => entry.id === id);
}
