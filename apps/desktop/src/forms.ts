// Input checks the GUI does before sending, mirroring the contract's own
// validation (the watchdog validates again; these only give a useful message
// early). Version 1 probes IPv4 literals only, never names (plan §1.2).

import {
  BOT_ID_DIGITS,
  BOT_SECRET_CHARS,
  CHAT_ID_DIGITS,
  CHAT_USERNAME_CHARS,
  DEVICE_NAME_MAX_CHARS,
  MAX_OBSERVE_TARGETS,
  MAX_WORKERS,
  MIN_WORKERS,
  PROBE_INTERVAL_SECS,
  PROBE_MIN_FAULT_SECS,
  PROBE_RETRIES,
  PROBE_TIMEOUT_SECS,
  RECOVERY_RETRY_INTERVAL_SECS,
  RECOVERY_RETRY_LIMIT,
  type ChatId,
  type ProbeTarget,
  type ProbeTiming,
  type RecoveryRetry,
} from "./protocol";
import { type Key, t } from "./i18n";

/// A value as typed, or why it is not one. `error` is worded each time it
/// is read, in the language of the moment, so a message already shown can
/// be said again after a language change.
export type Parsed<T> = { ok: true; value: T } | { readonly ok: false; readonly error: string };

const fail = <T>(words: () => string): Parsed<T> => ({
  ok: false,
  get error() {
    return words();
  },
});

/// A dotted-quad IPv4 literal as Rust's `Ipv4Addr` reads it (no leading
/// zeros, no names), and one `TargetAddress` accepts.
export function parseAddress(text: string): Parsed<string> {
  const value = text.trim();
  if (value.includes(":")) {
    return fail(() => t("forms.ipv6"));
  }
  const parts = value.split(".");
  if (parts.length !== 4 || !parts.every((p) => /^(0|[1-9][0-9]{0,2})$/.test(p))) {
    return fail(() => t("forms.not_ipv4"));
  }
  const octets = parts.map(Number);
  if (octets.some((o) => o > 255)) {
    return fail(() => t("forms.octet_range"));
  }
  const [a = 0, b = 0] = octets;
  if (octets.every((o) => o === 0)) {
    return fail(() => t("forms.unspecified"));
  }
  if (a === 127) {
    return fail(() => t("forms.loopback"));
  }
  if (a >= 224 && a <= 239) {
    return fail(() => t("forms.multicast"));
  }
  if (octets.every((o) => o === 255)) {
    return fail(() => t("forms.broadcast"));
  }
  if (a === 169 && b === 254) {
    return fail(() => t("forms.link_local"));
  }
  return { ok: true, value: octets.join(".") };
}

/// An ICMP target, or a TCP target when a port is given.
export function parseTarget(address: string, port: string): Parsed<ProbeTarget> {
  const parsed = parseAddress(address);
  if (!parsed.ok) {
    return parsed;
  }
  const p = port.trim();
  if (p === "") {
    return { ok: true, value: { address: parsed.value, probe: { kind: "icmp" } } };
  }
  if (!/^[1-9][0-9]{0,4}$/.test(p) || Number(p) > 65535) {
    return fail(() => t("forms.port_range"));
  }
  return {
    ok: true,
    value: { address: parsed.value, probe: { kind: "tcp_connect", port: Number(p) } },
  };
}

/// The observe targets typed one per line, "address" or "address:port".
/// Blank lines are skipped; an error names the line it is about.
export function parseObserveTargets(text: string): Parsed<ProbeTarget[]> {
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l !== "");
  if (lines.length > MAX_OBSERVE_TARGETS) {
    return fail(() => t("forms.observe_too_many", { max: MAX_OBSERVE_TARGETS }));
  }
  const targets: ProbeTarget[] = [];
  for (const line of lines) {
    const [address = "", port = "", ...rest] = line.split(":");
    if (rest.length > 0) return fail(() => t("forms.line_error", { line, error: t("forms.ipv6") }));
    const parsed = parseTarget(address, port);
    if (!parsed.ok) return fail(() => t("forms.line_error", { line, error: parsed.error }));
    targets.push(parsed.value);
  }
  return { ok: true, value: targets };
}

/// A whole number in `range`, typed in the field the key `name` names.
export function parseWhole(
  text: string,
  range: { readonly min: number; readonly max: number },
  name: Key,
): Parsed<number> {
  const value = text.trim();
  if (!/^[0-9]{1,5}$/.test(value)) {
    return fail(() => t("forms.whole_invalid", { name: t(name), min: range.min, max: range.max }));
  }
  const n = Number(value);
  return n >= range.min && n <= range.max
    ? { ok: true, value: n }
    : fail(() => t("forms.whole_range", { name: t(name), min: range.min, max: range.max }));
}

/// The four timing fields as typed, checked against the contract's ranges.
export interface TimingInput {
  interval: string;
  timeout: string;
  retries: string;
  minFault: string;
}

export function parseTiming(input: TimingInput): Parsed<ProbeTiming> {
  const interval = parseWhole(input.interval, PROBE_INTERVAL_SECS, "forms.name_interval");
  if (!interval.ok) return interval;
  const timeout = parseWhole(input.timeout, PROBE_TIMEOUT_SECS, "forms.name_timeout");
  if (!timeout.ok) return timeout;
  const retries = parseWhole(input.retries, PROBE_RETRIES, "forms.name_retries");
  if (!retries.ok) return retries;
  const minFault = parseWhole(input.minFault, PROBE_MIN_FAULT_SECS, "forms.name_min_fault");
  if (!minFault.ok) return minFault;
  return {
    ok: true,
    value: {
      interval_secs: interval.value,
      timeout_secs: timeout.value,
      retries: retries.value,
      min_fault_secs: minFault.value,
    },
  };
}

/// The recovery retry as typed (owner decision of 2026-11-04): `forever`
/// keeps trying, otherwise `limit` rounds at most after the first, one every
/// `interval` seconds, each checked against the contract's range.
export interface RetryInput {
  forever: boolean;
  limit: string;
  interval: string;
}

export function parseRetry(input: RetryInput): Parsed<RecoveryRetry> {
  const interval = parseWhole(input.interval, RECOVERY_RETRY_INTERVAL_SECS, "forms.name_retry_interval");
  if (!interval.ok) return interval;
  if (input.forever) return { ok: true, value: { limit: null, interval_secs: interval.value } };
  const limit = parseWhole(input.limit, RECOVERY_RETRY_LIMIT, "forms.name_retry_limit");
  if (!limit.ok) return limit;
  return { ok: true, value: { limit: limit.value, interval_secs: interval.value } };
}

/// A worker count from `MIN_WORKERS` up to `limit` (monitored tunnels + 1),
/// or the count already configured, which may be above a limit that fell
/// since: saving other settings must not force it down.
export function parseWorkers(text: string, limit: number, configured: number | null): Parsed<number> {
  const max = Math.min(Math.max(limit, MIN_WORKERS), MAX_WORKERS);
  const parsed = parseWhole(text, { min: MIN_WORKERS, max: MAX_WORKERS }, "forms.name_workers");
  if (!parsed.ok) return fail(() => t("forms.workers_invalid", { min: MIN_WORKERS, max }));
  return parsed.value <= max || parsed.value === configured
    ? parsed
    : fail(() => t("forms.workers_too_many", { max }));
}

/// Mirrors `TimeZoneName`'s form: whether the zone exists is the watchdog's
/// check, answered with `invalid_setting`.
export function parseTimeZoneName(text: string): Parsed<string> {
  const value = text.trim();
  const ok =
    value.length >= 1 &&
    value.length <= 64 &&
    /^[A-Za-z][A-Za-z0-9/_+-]*$/.test(value) &&
    !value.endsWith("/") &&
    !value.includes("//");
  return ok ? { ok: true, value } : fail(() => t("forms.zone_invalid"));
}

const CHAT_NUMERIC = new RegExp(`^-?[1-9][0-9]{${CHAT_ID_DIGITS.min - 1},${CHAT_ID_DIGITS.max - 1}}$`);
const CHAT_USERNAME = new RegExp(
  `^@[A-Za-z][A-Za-z0-9_]{${CHAT_USERNAME_CHARS.min - 1},${CHAT_USERNAME_CHARS.max - 1}}$`,
);
const BOT_TOKEN = new RegExp(
  `^[0-9]{${BOT_ID_DIGITS.min},${BOT_ID_DIGITS.max}}:[A-Za-z0-9_-]{${BOT_SECRET_CHARS.min},${BOT_SECRET_CHARS.max}}$`,
);

/// Mirrors `ChatId`: a numeric ID not starting with 0 (negative for groups)
/// or an @username, of the lengths `limits.json` gives. Kept as a string, never
/// converted to a number (plan §7.1).
export function parseChatId(text: string): Parsed<ChatId> {
  const value = text.trim();
  const numeric = CHAT_NUMERIC.test(value);
  const user = CHAT_USERNAME.test(value);
  return numeric || user
    ? { ok: true, value }
    : fail(() => t("forms.chat_invalid"));
}

/// Mirrors `DeviceName::fitted` (owner decision of 2026-11-06): any text the
/// user likes, control characters made spaces, and a name of more than
/// `DEVICE_NAME_MAX_CHARS` characters cut to three fewer and "...". It
/// counts characters as Rust does (code points, not UTF-16 units).
export function fitDeviceName(text: string): string {
  // eslint-disable-next-line no-control-regex
  const chars = Array.from(text.replace(/[\u0000-\u001f\u007f-\u009f]/g, " "));
  return chars.length <= DEVICE_NAME_MAX_CHARS
    ? chars.join("")
    : `${chars.slice(0, DEVICE_NAME_MAX_CHARS - 3).join("")}...`;
}

/// Mirrors `BotToken`'s format check only, with the lengths `limits.json`
/// gives; the value is passed on unchanged and never stored by the GUI.
export function looksLikeBotToken(text: string): boolean {
  return BOT_TOKEN.test(text);
}

let counter = 0;

/// A new request ID for every user action (plan §3.2). A repeated click is a
/// new request; an IPC retry of the same request reuses its ID.
export function newRequestId(now: number = Date.now()): string {
  counter = (counter + 1) % 1_000_000;
  return `gui-${now.toString(36)}-${counter.toString(36)}`;
}
