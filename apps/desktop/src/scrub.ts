// Masks what must not leave this machine in a log export (plan §9.3,
// AGENTS.md rule 10): IP addresses, endpoints, UUIDs, account paths,
// anything shaped like a WireGuard key, and Telegram chat IDs. It is the
// last pass over every string from the watchdog that reaches an export, so
// it errs on the side of masking; the GUI's own words, times, durations and
// codes pass unchanged. Secrets the GUI never has (the bot token, configs,
// `wg` output) are not its job: they are never put into an export at all.

import { REFERENCE, table } from "./i18n";

/// What each kind of masked value becomes, from the language file.
export const MASK = table("mask", REFERENCE.mask);

/// An account name in a path: words without a slash, a quote or a character
/// Windows forbids in a name, joined by spaces or tabs (never a line break),
/// so `Bob Smith` is one name. It runs to the next separator or quote, or
/// the end of the line, so words after a name with nothing behind it are
/// masked with it; it never runs into the next path (`C:\…`, `/…`).
const ACCOUNT_NAME = String.raw`[^\\/\s"'<>|:*?]+(?:[^\S\r\n]+(?![A-Za-z]:[\\/])[^\\/\s"'<>|:*?]+)*`;
/// A Windows profile path, with either slash and any case: `C:\Users\<name>`.
const WINDOWS_ACCOUNT = new RegExp(String.raw`\b([A-Za-z]:[\\/]+Users[\\/]+)${ACCOUNT_NAME}`, "gi");
/// A Unix home: `/home/<name>`, `/Users/<name>` (macOS), `/root` stays.
const UNIX_ACCOUNT = new RegExp(String.raw`(^|[^\w.-])(\/(?:home|Users)\/)${ACCOUNT_NAME}`, "g");
const UUID = /\{?\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b\}?/g;
/// 44 characters of base64 ending in `=`, the form of a WireGuard key.
const WG_KEY = /(?<![A-Za-z0-9+/])[A-Za-z0-9+/]{43}=(?![A-Za-z0-9+/=])/g;
/// `[address]:port`, `a.b.c.d:port` and `host.name:port`.
const BRACKET_ENDPOINT = /\[[0-9A-Fa-f:.%\w]*:[0-9A-Fa-f:.%\w]*\]:\d{1,5}\b/g;
const IPV4_ENDPOINT = /(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}:\d{1,5}\b/g;
const HOST_ENDPOINT = /(?<![\w.-])[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z][A-Za-z0-9-]*:\d{1,5}\b/g;
/// Candidate IPv6 text: hex groups and colons, possibly ending in a dotted
/// quad or a zone. Only a candidate with `::` or at least three colons is
/// masked, so clock times (`21:30:05`) and `+08:00` are left alone.
const IPV6_CANDIDATE = /(?<![\w:.])(?:[0-9A-Fa-f]{0,4}:){2,7}(?:\d{1,3}(?:\.\d{1,3}){3}|[0-9A-Fa-f]{1,4})?(?:%[\w.-]+)?(?![\w:])/g;
const IPV4 = /(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.]*\d)/g;
/// Supergroup and channel IDs (`-100…`), any number after "chat id", and
/// public `@usernames`.
const CHAT_SUPERGROUP = /(?<![\w-])-100\d{7,}\b/g;
const CHAT_LABELLED = /\b(chat[\s_-]?id\W{0,3}?)-?\d{3,}/gi;
const CHAT_USERNAME = /(?<![\w@.])@[A-Za-z][A-Za-z0-9_]{4,31}\b/g;

function maskIpv6(candidate: string): string {
  const colons = candidate.split(":").length - 1;
  return candidate.includes("::") || colons >= 3 ? MASK.ipv6 : candidate;
}

/// `text` with every secret-looking value replaced by a marker.
export function scrub(text: string): string {
  // The markers are inserted by functions, so a `$` in a language file's
  // marker is never read as a replacement pattern.
  const mask = (marker: string) => () => marker;
  return text
    .replace(WINDOWS_ACCOUNT, (_, path: string) => `${path}${MASK.account}`)
    .replace(UNIX_ACCOUNT, (_, before: string, home: string) => `${before}${home}${MASK.account}`)
    .replace(UUID, mask(MASK.uuid))
    .replace(WG_KEY, mask(MASK.key))
    .replace(BRACKET_ENDPOINT, mask(MASK.endpoint))
    .replace(IPV4_ENDPOINT, mask(MASK.endpoint))
    .replace(HOST_ENDPOINT, mask(MASK.endpoint))
    .replace(IPV6_CANDIDATE, maskIpv6)
    .replace(IPV4, mask(MASK.ipv4))
    .replace(CHAT_SUPERGROUP, mask(MASK.chat))
    .replace(CHAT_LABELLED, (_, label: string) => `${label}${MASK.chat}`)
    .replace(CHAT_USERNAME, mask(MASK.chat));
}
