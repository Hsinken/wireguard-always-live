// One-click MCP client setup without the DOM (owner request of
// 2026-11-12): the AI tools the shell can add the `wgal` server to
// (`mcp_clients`), what each one's settings say now, and the words for
// what happened when the user confirmed (`set_up_mcp_client`). The shell
// picks every file and the command; the webview only names a tool from the
// closed list below and shows what the shell answered.

import { type Key, t } from "./i18n";

export const CLIENT_IDS = [
  "claude_code",
  "claude_desktop",
  "cursor",
  "vs_code",
  "trae",
  "kiro",
  "codex",
  "antigravity",
  "gemini_cli",
] as const;
export type ClientId = (typeof CLIENT_IDS)[number];

/// The tools' own names, which are not translated.
export const CLIENT_NAMES: Readonly<Record<ClientId, string>> = {
  claude_code: "Claude Code",
  claude_desktop: "Claude Desktop",
  cursor: "Cursor",
  vs_code: "VS Code",
  trae: "Trae",
  kiro: "Kiro",
  codex: "Codex",
  antigravity: "Antigravity",
  gemini_cli: "Gemini CLI",
};

export const CLIENT_STATES = ["configured", "other", "not_configured", "no_folder", "unreadable"] as const;
export type ClientState = (typeof CLIENT_STATES)[number];

/// One tool as the shell describes it: its settings file, whether the file
/// has the server, and the entry that is (or would be) written.
export interface ClientView {
  client: ClientId;
  path: string;
  state: ClientState;
  preview: string;
}

export const NOT_SET_UP = [
  "no_server",
  "no_folder",
  "link",
  "too_large",
  "not_parsable",
  "changed",
  "io",
  "damaged",
  "not_there",
] as const;
export type NotSetUp = (typeof NOT_SET_UP)[number];

function isOneOf<T extends string>(list: readonly T[], value: unknown): value is T {
  return typeof value === "string" && (list as readonly string[]).includes(value);
}

/// Most characters of a path or preview shown: far above any real one.
const MAX_TEXT = 4096;

function isText(value: unknown): value is string {
  return typeof value === "string" && value.length <= MAX_TEXT;
}

/// One view from the shell, checked: anything else is `null`.
export function asClientView(value: unknown): ClientView | null {
  if (typeof value !== "object" || value === null) return null;
  const v = value as Record<string, unknown>;
  if (!isOneOf(CLIENT_IDS, v.client) || !isOneOf(CLIENT_STATES, v.state)) return null;
  if (!isText(v.path) || !isText(v.preview)) return null;
  return { client: v.client, path: v.path, state: v.state, preview: v.preview };
}

/// The shell's list, checked: `null` unless every entry is a view and no
/// tool is listed twice.
export function asClientViews(value: unknown): ClientView[] | null {
  if (!Array.isArray(value) || value.length > CLIENT_IDS.length) return null;
  const views: ClientView[] = [];
  for (const item of value) {
    const view = asClientView(item);
    if (view === null || views.some((v) => v.client === view.client)) return null;
    views.push(view);
  }
  return views;
}

/// Why the shell did not set a tool up, from its error; anything else is `io`.
export function asNotSetUp(value: unknown): NotSetUp {
  if (typeof value === "object" && value !== null) {
    const reason = (value as Record<string, unknown>).reason;
    if (isOneOf(NOT_SET_UP, reason)) return reason;
  }
  return "io";
}

const STATE_KEYS: Readonly<Record<ClientState, Key>> = {
  configured: "mcp.clients_state_configured",
  other: "mcp.clients_state_other",
  not_configured: "mcp.clients_state_not_configured",
  no_folder: "mcp.clients_state_no_folder",
  unreadable: "mcp.clients_state_unreadable",
};

/// What a tool's settings say now, with the file.
export function stateText(view: ClientView): string {
  return t(STATE_KEYS[view.state], { path: view.path });
}

const REASON_KEYS: Readonly<Record<NotSetUp, Key>> = {
  no_server: "mcp.clients_no_server",
  no_folder: "mcp.clients_refused_no_folder",
  link: "mcp.clients_refused_link",
  too_large: "mcp.clients_refused_too_large",
  not_parsable: "mcp.clients_refused_not_parsable",
  changed: "mcp.clients_refused_changed",
  io: "mcp.clients_refused_io",
  damaged: "mcp.clients_refused_damaged",
  not_there: "mcp.clients_refused_not_there",
};

/// Why the tool was not set up, for `client`, whose settings file is at
/// `path` (which a damaged file is to be checked in).
export function reasonText(reason: NotSetUp, client: ClientId, path: string): string {
  return t(REASON_KEYS[reason], { client: CLIENT_NAMES[client], path });
}

/// The question before removing the entry: the file, and that the tool is
/// to be quit first.
export function removeQuestionText(view: ClientView): string {
  const question = t("mcp.clients_remove_question", { path: view.path, client: CLIENT_NAMES[view.client] });
  return view.client === "claude_code" ? `${question} ${t("mcp.clients_claude_code_note")}` : question;
}

/// What "check" found for `view`'s tool.
export function checkedText(view: ClientView): string {
  return t("mcp.clients_checked", { client: CLIENT_NAMES[view.client], state: stateText(view) });
}

/// The question before writing: the file, and that the tool is to be quit
/// first (the shell keeps a tool from writing meanwhile only on Windows).
/// Claude Code rewrites its file all the time, so it says so.
export function questionText(view: ClientView): string {
  const question = t("mcp.clients_question", { path: view.path, client: CLIENT_NAMES[view.client] });
  return view.client === "claude_code" ? `${question} ${t("mcp.clients_claude_code_note")}` : question;
}
