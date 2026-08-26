import type { LucideIcon } from "lucide-react";
import type { ChatInputReference } from "../../types";

export type CliSlashCommandPanel =
  | { type: "review" }
  | { type: "fork" }
  | { type: "rollback" }
  | { type: "compact" }
  | { type: "personality" }
  | { type: "skills" }
  | { type: "plugins" }
  | { type: "agents" }
  | { type: "commands" }
  | { type: "sessions" }
  | { type: "mcp" }
  | { type: "experimental" };

export type CliSlashCommandAction =
  | { type: "reference"; reference: ChatInputReference }
  | { type: "insert"; text: string }
  | { type: "panel"; panel: CliSlashCommandPanel }
  | { type: "fast" };

export interface CliSlashCommand {
  id: string;
  name: string;
  description: string;
  icon: LucideIcon;
  group?: string;
  searchTerms?: string[];
  disabled?: boolean;
  action?: CliSlashCommandAction;
}
