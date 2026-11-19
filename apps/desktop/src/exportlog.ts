// The log tab's export button: reads the whole event log, builds the export
// text (`report.ts`) and asks the shell to save it. The shell alone picks
// the folder and the file name; this side passes only the text and the
// configured zone's offset for the name's local time, and words the
// shell's answer, a folder and a name or a reason, from the language file.

import { byId, setAttr, setControl, setText, speak, unavailable } from "./dom";
import { inline } from "./errors";
import { t } from "./i18n";
import type { EventEntry, Outcome, Request, Snapshot } from "./protocol";
import { EXPORT_CAP, buildReport, collectLog } from "./report";
import { type Said, type Words, plain } from "./said";
import { offsetSecsAt, zoneOf } from "./time";
import type { LinkRecord } from "./watchlog";

/// Where the shell saved an export (`src-tauri/src/export.rs` `Saved`).
export interface SavedExport {
  folder: "downloads" | "documents";
  name: string;
}

/// Why the shell did not save it (`export.rs` `NotSaved`): the reason, and
/// the OS's error kind when one says more.
export type ExportReason =
  | "empty"
  | "too_large"
  | "bad_offset"
  | "no_folder"
  | "interrupted"
  | "cannot_create"
  | "cannot_write"
  | "too_many"
  | "linked_folder"
  | "redirected";

export interface NotSaved {
  reason: ExportReason;
  detail: string | null;
}

const REASONS: ReadonlySet<string> = new Set<ExportReason>([
  "empty",
  "too_large",
  "bad_offset",
  "no_folder",
  "interrupted",
  "cannot_create",
  "cannot_write",
  "too_many",
  "linked_folder",
  "redirected",
]);

/// The shell's refusal, when `value` is one.
export function notSaved(value: unknown): NotSaved | null {
  if (typeof value !== "object" || value === null) return null;
  const { reason, detail } = value as Record<string, unknown>;
  if (typeof reason !== "string" || !REASONS.has(reason)) return null;
  return { reason: reason as ExportReason, detail: typeof detail === "string" ? detail : null };
}

/// Why an export failed, in words: the shell's reason, or the thrown
/// error's text (`detail`, which is only for the details).
export function exportFailure(error: unknown): Said {
  const refused = notSaved(error);
  if (refused !== null) return { text: t(`export.refused_${refused.reason}`), detail: refused.detail };
  return { text: t("export.refused_other"), detail: error instanceof Error ? error.message : String(error) };
}

/// Saves the text; resolves to where it went, or rejects with why not.
export type SaveExport = (text: string, utcOffsetMinutes: number) => Promise<SavedExport>;

export interface ExportContext {
  /// Sends a request; throws when the watchdog does not answer.
  ask(request: Request): Promise<Outcome>;
  refusal(request: Request, outcome: Outcome): Said;
  snapshot(): Snapshot | null;
  lost(): boolean;
  lastError(): string | null;
  /// The desktop binary's version, null until the shell has answered.
  guiVersion(): string | null;
  /// The window's own records of its link to the watchdog, newest first,
  /// and whether they are kept beyond this window.
  linkRecords(): readonly LinkRecord[];
  linkStored(): boolean;
  save: SaveExport;
  /// The window's message line.
  say(words: Words): void;
}

export class LogExport {
  private busy = false;
  private readonly button = byId("log-export");
  private readonly names = byId("log-export-names") as HTMLInputElement;
  private readonly state = byId("log-export-state");

  constructor(private readonly ctx: ExportContext) {
    this.button.addEventListener("click", () => {
      if (!unavailable(this.button)) void this.run();
    });
  }

  private show(words: Words): void {
    speak(this.state, words);
  }

  private async readPage(beforeSeq: number | null): Promise<{ entries: EventEntry[]; more: boolean }> {
    const request: Request = { op: "read_event_log", filter: { kind: "all" }, before_seq: beforeSeq };
    const outcome = await this.ctx.ask(request);
    if ("ok" in outcome && outcome.ok.kind === "event_log") {
      return { entries: outcome.ok.entries, more: outcome.ok.more };
    }
    // The export's text names the reason and keeps the original beside it.
    throw new Error(inline(this.ctx.refusal(request, outcome)));
  }

  async run(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    // Marked, not disabled: focus stays on the button while it works.
    setControl(this.button, { kind: "disabled", why: t("export.busy_why") });
    setAttr(this.button, "aria-busy", "true");
    setText(this.button, t("export.busy"));
    let message: Words;
    try {
      this.show(() => plain(t("export.reading")));
      const log = await collectLog(
        (before) => this.readPage(before),
        EXPORT_CAP,
        (n) => this.show(() => plain(t("export.reading_count", { count: n }))),
      );
      const now = Date.now();
      const snapshot = this.ctx.snapshot();
      const text = buildReport({
        snapshot,
        lost: this.ctx.lost(),
        lastError: this.ctx.lastError(),
        entries: log.entries,
        truncated: log.truncated,
        logError: log.error,
        includeNames: this.names.checked,
        nowMs: now,
        guiVersion: this.ctx.guiVersion(),
        userAgent: navigator.userAgent,
        linkRecords: this.ctx.linkRecords(),
        linkStored: this.ctx.linkStored(),
      });
      this.show(() => plain(t("export.writing")));
      const offset = Math.round(offsetSecsAt(now, zoneOf(snapshot?.host.time_zone)) / 60);
      const saved = await this.ctx.save(text, offset);
      const partial = log.error !== null;
      message = () =>
        plain(
          t("export.saved", {
            folder: t(saved.folder === "downloads" ? "export.folder_downloads" : "export.folder_documents"),
            name: saved.name,
            partial: partial ? t("export.partial") : "",
          }),
        );
    } catch (e) {
      message = () => {
        const failure = exportFailure(e);
        return { text: t("export.failed", { error: failure.text }), detail: failure.detail };
      };
    }
    this.show(message);
    this.ctx.say(message);
    setControl(this.button, { kind: "enabled" });
    setAttr(this.button, "aria-busy", null);
    setText(this.button, t("export.button"));
    this.busy = false;
  }
}
