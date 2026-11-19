// The settings file (owner decision of 2026-11-04): exporting WGAL's
// settings to a file in Downloads, named after the device, and importing
// one back. The watchdog builds and applies the file (`ExportSettings`,
// `ImportSettings`); this window only reads a file the user picked, shows
// what an import would do (a dry run) with the two choices the owner asked
// for, and applies it on the user's word. The file never holds the device
// name, the Bot Token or the Chat ID; the device name is only in the file's
// name, chosen by the shell.

import { ERROR_REASON } from "./errors";
import { t } from "./i18n";
import {
  type AutoRecoveryImport,
  type ImportOutcome,
  type ImportResult,
  SETTINGS_DOCUMENT_VERSION,
  type SettingsDocument,
} from "./protocol";
import { ERROR_CODE } from "./strings";

/// The largest settings file read: far above any the contract allows.
export const MAX_SETTINGS_FILE_BYTES = 256 * 1024;

/// A file's text as a settings document, or why it is not one. Only the
/// outer shape is judged here; the shell and the watchdog check every field
/// when it is imported.
export function readSettingsFile(text: string): { ok: true; document: SettingsDocument } | { ok: false; error: string } {
  if (new TextEncoder().encode(text).length > MAX_SETTINGS_FILE_BYTES) {
    return { ok: false, error: t("settings_file.too_large") };
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return { ok: false, error: t("settings_file.not_json") };
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { ok: false, error: t("settings_file.not_settings") };
  }
  const v = value as Record<string, unknown>;
  if (v.format !== "wgal-settings") return { ok: false, error: t("settings_file.not_settings") };
  if (v.version !== SETTINGS_DOCUMENT_VERSION) {
    return { ok: false, error: t("settings_file.other_version", { version: String(v.version) }) };
  }
  if (typeof v.host !== "object" || v.host === null || !Array.isArray(v.tunnels)) {
    return { ok: false, error: t("settings_file.not_settings") };
  }
  return { ok: true, document: value as SettingsDocument };
}

/// Why the watchdog refused a part, in words.
function refusedText(code: string, reason: string | null): string {
  const words = reason !== null && reason in ERROR_REASON ? ERROR_REASON[reason as keyof typeof ERROR_REASON] : null;
  return words ?? ERROR_CODE[code as keyof typeof ERROR_CODE] ?? code;
}

/// What became of one entry, or would in a dry run.
export function importOutcomeText(outcome: ImportOutcome, dryRun: boolean): string {
  switch (outcome.kind) {
    case "applied":
      return t(dryRun ? "settings_file.will_apply" : "settings_file.applied");
    case "registered":
      return t(dryRun ? "settings_file.will_register" : "settings_file.registered");
    case "refused":
      return t("settings_file.refused", { why: refusedText(outcome.code, outcome.reason) });
    default:
      return t(`settings_file.${outcome.kind}`);
  }
}

/// What became of an entry's automatic recovery.
export function autoRecoveryText(auto: AutoRecoveryImport): string {
  if (auto.kind === "refused") {
    return t("settings_file.auto_refused", { why: refusedText(auto.code, auto.reason) });
  }
  return t(`settings_file.auto_${auto.kind}`);
}

/// One line per entry, after the host settings and the notification
/// switch, which are stored apart and so have outcomes of their own.
export function importLines(result: ImportResult): string[] {
  return [
    t("settings_file.host_line", { outcome: importOutcomeText(result.host, result.dry_run) }),
    ...(result.notifications === null
      ? []
      : [t("settings_file.notifications_line", { outcome: importOutcomeText(result.notifications, result.dry_run) })]),
    ...result.tunnels.map((item) =>
      t("settings_file.tunnel_line", {
        name: item.name,
        outcome: importOutcomeText(item.outcome, result.dry_run),
        auto: autoRecoveryText(item.auto_recovery),
      }),
    ),
  ];
}
