// The Telegram message templates the window hands the watchdog (owner's
// requests of 2026-11-03 and 2026-11-04): the ten texts of a language file's
// `notification_templates`, checked here by the contract's own rules
// (`crates/wgal-protocol/src/template.rs`) before they are sent, so a
// broken language file is caught in this window and its tests rather than
// by a refusal. The watchdog fills the placeholders in when it sends, so
// alerts still go out while no window runs. No DOM here.

import { LANGUAGES, type Language, raw, t } from "./i18n";
import {
  DEFAULT_RECOVERY_RETRY,
  type NotificationTemplates,
  type RecoveryRetry,
  TEMPLATE_PLACEHOLDERS,
  type TimeZoneStatus,
} from "./protocol";
import { formatDateTime, offsetSecsAt, offsetText, zoneOf } from "./time";

/// The texts of a set, by their contract field names.
export type TemplateField = keyof typeof TEMPLATE_PLACEHOLDERS;
export const TEMPLATE_FIELDS = Object.keys(TEMPLATE_PLACEHOLDERS) as TemplateField[];

/// `MessageTemplate::MAX_BYTES`, `NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES`
/// and `TemplatesId::MAX_BYTES` in the contract (a test reads them there).
export const MAX_TEMPLATE_BYTES = 1024;
export const MAX_UNNAMED_DEVICE_BYTES = 96;
export const MAX_TEMPLATES_ID_BYTES = 48;

const utf8 = new TextEncoder();
const bytes = (text: string) => utf8.encode(text).length;

/// One piece of a template: literal text (a `{{` or `}}` already one brace)
/// or a placeholder's name.
type Piece = { text: string } | { name: string };

/// Splits a template as the contract does, or says why it is not one: a
/// `{` opens a name that runs to the next `}` and must be 1 to 24 of `a`–`z`
/// and `_`; any other brace must be doubled.
function parse(template: string): Piece[] | string {
  const pieces: Piece[] = [];
  let rest = template;
  for (let at = rest.search(/[{}]/); at !== -1; at = rest.search(/[{}]/)) {
    if (at > 0) pieces.push({ text: rest.slice(0, at) });
    const tail = rest.slice(at);
    if (tail.startsWith("{{") || tail.startsWith("}}")) {
      pieces.push({ text: tail[0]! });
      rest = tail.slice(2);
    } else if (tail.startsWith("{")) {
      const end = tail.indexOf("}");
      if (end === -1) return "a '{' is not closed; write '{{' for a literal brace";
      const name = tail.slice(1, end);
      if (!/^[a-z_]{1,24}$/.test(name)) {
        return "a placeholder is '{' and 1 to 24 of 'a' to 'z' and '_', then '}'";
      }
      pieces.push({ name });
      rest = tail.slice(end + 1);
    } else {
      return "a lone '}'; write '}}' for a literal brace";
    }
  }
  if (rest !== "") pieces.push({ text: rest });
  return pieces;
}

/// Why `text` is not a valid template for `field`, or null when it is: the
/// contract's `MessageTemplate` rules, then the placeholders its field must
/// use, each exactly once and no other, and for `unnamed_device` a single
/// line of at most a device name's length.
export function templateProblem(field: TemplateField, text: string): string | null {
  if (text === "" || bytes(text) > MAX_TEMPLATE_BYTES) return "must be 1 to 1024 bytes";
  if (/[^\P{Cc}\n]/u.test(text)) return "control characters other than a line break are not allowed";
  if (/^\p{White_Space}|\p{White_Space}$/u.test(text)) return "must not start or end with whitespace";
  const pieces = parse(text);
  if (typeof pieces === "string") return pieces;
  const names = pieces.flatMap((p) => ("name" in p ? [p.name] : []));
  const required: readonly string[] = TEMPLATE_PLACEHOLDERS[field];
  for (const [i, name] of names.entries()) {
    if (!required.includes(name)) return `uses {${name}}, which is not one of its own`;
    if (names.indexOf(name) !== i) return `uses {${name}} more than once`;
  }
  if (names.length !== required.length) return "leaves out a placeholder it must use";
  if (field === "unnamed_device" && (bytes(text) > MAX_UNNAMED_DEVICE_BYTES || text.includes("\n"))) {
    return "must be one line of at most 96 bytes, like a device name";
  }
  return null;
}

/// Why `id` is not a valid `TemplatesId`, or null when it is.
export function templatesIdProblem(id: string): string | null {
  if (id.length < 1 || id.length > MAX_TEMPLATES_ID_BYTES) return "must be 1 to 48 bytes";
  if (!/^[A-Za-z0-9._@/-]+$/.test(id)) return "only ASCII letters, digits, '.', '_', '@', '/' and '-' are allowed";
  if (!/^[A-Za-z0-9]/.test(id)) return "must start with a letter or digit";
  return null;
}

/// Every problem of a set, as `field: problem`; empty when the watchdog
/// takes it.
export function templatesProblems(set: NotificationTemplates): string[] {
  const problems: string[] = [];
  const id = templatesIdProblem(set.id);
  if (id !== null) problems.push(`id: ${id}`);
  for (const field of TEMPLATE_FIELDS) {
    const problem = templateProblem(field, set[field]);
    if (problem !== null) problems.push(`${field}: ${problem}`);
  }
  return problems;
}

/// The texts of `language`'s `notification_templates`, by field.
export function templateTexts(language: Language): Record<TemplateField, string> {
  return Object.fromEntries(
    TEMPLATE_FIELDS.map((field) => [field, raw(`notification_templates.${field}`, language)]),
  ) as Record<TemplateField, string>;
}

/// The template with each placeholder replaced by its value as it is and
/// `{{`, `}}` written as one brace, as the watchdog renders it; a name with
/// no value is left empty. A template that does not parse is returned as
/// it is.
export function renderTemplate(template: string, values: Readonly<Record<string, string>>): string {
  const pieces = parse(template);
  if (typeof pieces === "string") return template;
  return pieces.map((p) => ("name" in p ? (Object.hasOwn(values, p.name) ? values[p.name] : "") : p.text)).join("");
}

/// FNV-1a, 32 bits (offset basis 0x811c9dc5, prime 0x01000193), over the
/// UTF-8 bytes of `text`.
export function fnv1a32(text: string): number {
  let hash = 0x811c9dc5;
  for (const byte of utf8.encode(text)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

/// What the texts are joined with for the hash: a line break and a
/// NUL, which no template may contain (only `\n` is allowed in one), so
/// no two different sets join to the same input.
export const HASH_SEPARATOR = "\n\u0000";

/// A set's name: `<tag>/<hash>`, the hash being the 8 lowercase hex digits
/// of FNV-1a 32 over the UTF-8 of every text, in the contract's order
/// (`TEMPLATE_FIELDS`), joined with `HASH_SEPARATOR`. The same texts give the same
/// name in any GUI, so a snapshot tells whether the watchdog has them.
export function templatesId(tag: string, texts: Readonly<Record<TemplateField, string>>): string {
  const input = TEMPLATE_FIELDS.map((field) => texts[field]).join(HASH_SEPARATOR);
  return `${tag}/${fnv1a32(input).toString(16).padStart(8, "0")}`;
}

/// The set `language` hands the watchdog, or the problems that keep it from
/// being sent (a broken language file; the tests keep every file valid).
export function templatesFor(language: Language): { set: NotificationTemplates; problems: string[] } {
  const texts = templateTexts(language);
  const set: NotificationTemplates = { id: templatesId(language.tag, texts), ...texts };
  return { set, problems: templatesProblems(set) };
}

/// The language tag a set's name starts with: what comes before its first
/// `/`, or the whole name when it has none.
export function tagOf(id: string): string {
  const slash = id.indexOf("/");
  return slash === -1 ? id : id.slice(0, slash);
}

/// Which set the watchdog uses, as against this window's `ours`: not known
/// (no snapshot), its built-in one, ours, ours in another version (our tag,
/// other texts: this GUI's texts changed since it was sent), or one another
/// GUI on this machine chose in another language.
export type TemplateUse =
  | { kind: "unknown" }
  | { kind: "builtin" }
  | { kind: "ours" }
  | { kind: "ours_changed" }
  | { kind: "foreign"; tag: string };

export function templateUse(inUse: string | null | undefined, ours: string): TemplateUse {
  if (inUse === undefined) return { kind: "unknown" };
  if (inUse === null) return { kind: "builtin" };
  if (inUse === ours) return { kind: "ours" };
  const tag = tagOf(inUse);
  return tag === tagOf(ours) ? { kind: "ours_changed" } : { kind: "foreign", tag };
}

/// When the window hands the watchdog its set (owner's decision of
/// 2026-11-03): by itself when the watchdog uses its built-in set or an
/// older version of ours, and when the user asked (a language change, or
/// "use this window's language" over another GUI's choice); never by itself
/// over a set another GUI chose in another language. An automatic send
/// happens at most once per (set in use, our set) pair in a session, so a
/// refusal is never sent again in a loop; an explicit request counts for
/// its pair too.
export class TemplateSync {
  private readonly tried = new Set<string>();
  private requested = false;
  /// A send is under way; nothing else is sent until it is answered.
  sending = false;
  /// The window's language is final. While it follows the system and the
  /// shell has not yet said the system's languages, the language shown is
  /// the webview's guess, and its set is never sent: once sent, a set in
  /// another language than the one the window then settles on would never
  /// be replaced by itself (Codex P2, 2026-11-05). A request made meanwhile
  /// waits for it.
  languageSettled = true;

  /// The user asked for this window's set: it is sent with the next
  /// current snapshot that does not already name it.
  request(): void {
    this.requested = true;
  }

  /// What to send now, given the set the latest current snapshot names
  /// (`inUse`) and ours, and whether the user asked for it; null for
  /// nothing. Not called while the watchdog is lost.
  next(inUse: string | null, ours: NotificationTemplates): { set: NotificationTemplates; asked: boolean } | null {
    if (this.sending || !this.languageSettled) return null;
    const use = templateUse(inUse, ours.id);
    const pair = JSON.stringify([inUse, ours.id]);
    if (this.requested) {
      this.requested = false;
      if (use.kind === "ours") return null;
      this.tried.add(pair);
      return { set: ours, asked: true };
    }
    if (use.kind !== "builtin" && use.kind !== "ours_changed") return null;
    if (this.tried.has(pair)) return null;
    this.tried.add(pair);
    return { set: ours, asked: false };
  }
}

/// Which set is in use, in words (`notify.templates_*`): a language by the
/// name its file gives, or by its tag when this window has no file for it.
export function templateUseText(use: TemplateUse, ours: Language): string {
  switch (use.kind) {
    case "unknown":
      return t("notify.templates_unknown");
    case "builtin":
      return t("notify.templates_builtin");
    case "ours":
      return t("notify.templates_ours", { name: ours.name });
    case "ours_changed":
      return t("notify.templates_ours_changed", { name: ours.name });
    case "foreign": {
      const other = LANGUAGES.find((l) => l.tag === use.tag);
      return t("notify.templates_foreign", { name: other?.name ?? use.tag });
    }
  }
}

/// What a message's `{time_zone}` says of the zone at `unixMs`, as the
/// watchdog writes it (`zone_text` in `crates/wgal-runtime/src/notify/message.rs`):
/// `Asia/Taipei, UTC+08:00`, `UTC` for UTC itself, the offset alone for a
/// zone without a name.
export function messageZone(unixMs: number, zone: TimeZoneStatus | null): string {
  const offset = offsetSecsAt(unixMs, zoneOf(zone));
  const name = zone?.effective ?? null;
  if ((name === "UTC" || name === "Etc/UTC") && offset === 0) return "UTC";
  return name === null ? offsetText(offset) : `${name}, ${offsetText(offset)}`;
}

/// The times of one message as the watchdog writes them (owner request of
/// 2026-11-05, `message_times`): `{time_zone}` at the first, and each time
/// as local time without the zone (`2026-11-05 00:26:01`); a later time
/// whose offset differs from the first's, a daylight saving change having
/// come between, keeps its own (`2026-11-01 01:10:00 (UTC-05:00)`).
export function messageTimes<T extends readonly [number, ...number[]]>(
  unixMs: T,
  zone: TimeZoneStatus | null,
): { timeZone: string; times: { [K in keyof T]: string } } {
  const z = zoneOf(zone);
  const first = offsetSecsAt(unixMs[0], z);
  const times = unixMs.map((ms) => {
    const when = formatDateTime(ms, z);
    const offset = offsetSecsAt(ms, z);
    return offset === first ? when : `${when} (${offsetText(offset)})`;
  });
  return { timeZone: messageZone(unixMs[0], zone), times: times as { [K in keyof T]: string } };
}

/// Sample values for the preview, the same placeholders the watchdog fills.
export interface PreviewValues {
  /// The configured device name; blank gives the set's `unnamed_device`.
  deviceName: string;
  tunnel: string;
  incident: string;
  startedAtMs: number;
  nowMs: number;
  zone: TimeZoneStatus | null;
  /// The tunnel's recovery retry, for the alert's last line and the count
  /// in the used-up message.
  retry?: RecoveryRetry;
}

/// What automatic recovery does after the alert, as the watchdog writes it
/// (`retry_text`): keep trying, the rounds still left, or none.
export function retryText(texts: Readonly<Record<TemplateField, string>>, retry: RecoveryRetry, used = 0): string {
  const interval = String(retry.interval_secs);
  if (retry.limit === null) return renderTemplate(texts.retry_forever, { interval });
  const left = Math.max(retry.limit - used, 0);
  return left === 0
    ? renderTemplate(texts.retry_none, {})
    : renderTemplate(texts.retry_limited, { interval, limit: String(left) });
}

/// Unicode's White_Space characters, which Rust's `trim` removes. JavaScript's
/// own `trim` also removes U+FEFF, which is not one, so a name of it alone
/// would look unset here while the watchdog uses it.
const RUST_WHITE_SPACE = /^[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]*$/;

/// Whether the watchdog takes this device name as not set: empty or nothing
/// but white space, as Rust's `trim` judges it.
export function blankName(name: string): boolean {
  return RUST_WHITE_SPACE.test(name);
}

/// Every message `texts` give with `values`, as the watchdog would write
/// them (`crates/wgal-runtime/src/notify/message.rs`): the alert, the
/// message that a limited count is used up, the one that traffic came back
/// (two minutes after it did, as the watchdog waits) and the test.
export function previewMessages(
  texts: Readonly<Record<TemplateField, string>>,
  values: PreviewValues,
): { formal: string; exhausted: string; recovered: string; test: string } {
  const device = blankName(values.deviceName) ? renderTemplate(texts.unnamed_device, {}) : values.deviceName;
  const retry = values.retry ?? DEFAULT_RECOVERY_RETRY;
  const {
    timeZone,
    times: [startedAt, recoveredAt],
  } = messageTimes([values.startedAtMs, values.nowMs - 2 * 60_000] as const, values.zone);
  const common = {
    device,
    tunnel: values.tunnel,
    incident: values.incident,
    time_zone: timeZone,
    started_at: startedAt,
  };
  return {
    formal: renderTemplate(texts.formal, {
      ...common,
      result: renderTemplate(texts.result_failed, {}),
      retry: retryText(texts, retry),
    }),
    exhausted: renderTemplate(texts.exhausted, { ...common, retries: String(retry.limit ?? 0) }),
    recovered: renderTemplate(texts.recovered, { ...common, recovered_at: recoveredAt }),
    test: (() => {
      const now = messageTimes([values.nowMs] as const, values.zone);
      return renderTemplate(texts.test, { device, time_zone: now.timeZone, time: now.times[0] });
    })(),
  };
}
