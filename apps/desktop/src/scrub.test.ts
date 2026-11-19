// The export's scrubber: every kind of secret-looking value is masked, and
// the GUI's own words, times, codes and durations pass unchanged.

import { describe, expect, it } from "vitest";

import { eventText } from "./log";
import type { LogEvent } from "./protocol";
import { MASK, scrub } from "./scrub";
import { ADAPTER_ERROR, ERROR_CODE, LIGHT_REASON, NOTIFICATION_FAILURE } from "./strings";
import { judgmentText } from "./timing";

describe("masks what must not leave the machine", () => {
  it("masks IPv4 addresses and endpoints", () => {
    expect(scrub("probe of 192.0.2.1 timed out")).toBe(`probe of ${MASK.ipv4} timed out`);
    expect(scrub("route via 10.0.0.254, gateway 198.51.100.7")).toBe(`route via ${MASK.ipv4}, gateway ${MASK.ipv4}`);
    expect(scrub("endpoint 203.0.113.9:51820 unreachable")).toBe(`endpoint ${MASK.endpoint} unreachable`);
    expect(scrub("peer vpn.example.com:51820 refused")).toBe(`peer ${MASK.endpoint} refused`);
    expect(scrub("[2001:db8::1]:51820")).toBe(MASK.endpoint);
  });

  it("masks IPv6 addresses, with zones and mapped IPv4", () => {
    expect(scrub("addr 2001:db8:0:0:1:0:0:1 down")).toBe(`addr ${MASK.ipv6} down`);
    expect(scrub("fe80::1%eth0 and ::1")).toBe(`${MASK.ipv6} and ${MASK.ipv6}`);
    expect(scrub("::ffff:192.0.2.1")).toBe(MASK.ipv6);
  });

  it("masks UUIDs, with or without braces", () => {
    expect(scrub("adapter {6F9A2C1E-1B2C-4D5E-8F90-0123456789AB} gone")).toBe(`adapter ${MASK.uuid} gone`);
    expect(scrub("id 123e4567-e89b-12d3-a456-426614174000")).toBe(`id ${MASK.uuid}`);
  });

  it("masks account paths on Windows, Linux and macOS", () => {
    expect(scrub("C:\\Users\\alice\\AppData\\x.conf")).toBe(`C:\\Users\\${MASK.account}\\AppData\\x.conf`);
    expect(scrub("c:/users/Bob Smith/x")).toBe(`c:/users/${MASK.account}/x`);
    expect(scrub("open /home/carol/.config failed")).toBe(`open /home/${MASK.account}/.config failed`);
    expect(scrub("/Users/dave/Library")).toBe(`/Users/${MASK.account}/Library`);
  });

  it("masks a whole account name with spaces, up to the next separator or quote (Codex P2-7)", () => {
    const A = MASK.account;
    // Windows, either slash, any case.
    expect(scrub("C:\\Users\\Bob Smith\\AppData\\x.conf")).toBe(`C:\\Users\\${A}\\AppData\\x.conf`);
    expect(scrub("D:/USERS/Mary Ann  Lee/Downloads")).toBe(`D:/USERS/${A}/Downloads`);
    // macOS and Linux.
    expect(scrub("/Users/Jane Doe/Library/Logs")).toBe(`/Users/${A}/Library/Logs`);
    expect(scrub("open /home/Jane Doe/.config failed")).toBe(`open /home/${A}/.config failed`);
    // A quote ends it, and so does the end of the text or of the line.
    expect(scrub('open "C:\\Users\\Bob Smith" failed')).toBe(`open "C:\\Users\\${A}" failed`);
    expect(scrub("cd '/Users/Jane Doe'")).toBe(`cd '/Users/${A}'`);
    expect(scrub("profile C:\\Users\\Bob Smith")).toBe(`profile C:\\Users\\${A}`);
    expect(scrub("home /home/Jane Doe\nnext line")).toBe(`home /home/${A}\nnext line`);
    expect(scrub("cannot read /home/bob: permission denied")).toBe(`cannot read /home/${A}: permission denied`);
    // Words after a name with no separator behind them are masked with it,
    // but never the next path, which is masked on its own.
    expect(scrub("C:\\Users\\Bob Smith\\a, C:\\Users\\Ann Lee\\b")).toBe(`C:\\Users\\${A}\\a, C:\\Users\\${A}\\b`);
    expect(scrub("copy C:\\Users\\Bob Smith to C:\\Users\\Ann Lee")).toBe(`copy C:\\Users\\${A} C:\\Users\\${A}`);
    expect(scrub("/home/Jane Doe and /home/bob")).toBe(`/home/${A} /home/${A}`);
    expect(scrub("/Users/Jane Doe c:/users/Bob Smith")).toBe(`/Users/${A} c:/users/${A}`);
    // Nothing of the name is left anywhere.
    for (const text of ["C:\\Users\\Bob Smith\\x", "/Users/Bob Smith/x", "/home/Bob Smith/x", "c:/users/Bob Smith"]) {
      expect(scrub(text), text).not.toMatch(/Bob|Smith/);
    }
  });

  it("masks anything shaped like a WireGuard key", () => {
    const key = "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    expect(key).toHaveLength(44);
    expect(scrub(`peer ${key} has no handshake`)).toBe(`peer ${MASK.key} has no handshake`);
    expect(scrub(`(${key})`)).toBe(`(${MASK.key})`);
  });

  it("masks Telegram chat IDs and usernames", () => {
    expect(scrub("chat -1001234567890 rejected")).toBe(`chat ${MASK.chat} rejected`);
    expect(scrub("chat_id=987654321 not found")).toBe(`chat_id=${MASK.chat} not found`);
    expect(scrub("Chat ID: -4567890")).toBe(`Chat ID: ${MASK.chat}`);
    expect(scrub("sent to @my_channel")).toBe(`sent to ${MASK.chat}`);
    // An e-mail is not a username.
    expect(scrub("user@example.org")).toBe("user@example.org");
  });
});

describe("leaves ordinary text alone", () => {
  const unchanged = (text: string) => expect(scrub(text), text).toBe(text);

  it("passes times, dates, offsets, durations and codes", () => {
    for (const text of [
      "2026-11-03 21:30:05 +08:00",
      "11-03 21:30:05",
      "下次重試：剩 1 分 05 秒",
      "kind=probe_failed streak=2 needed=3",
      "seq=1041 kind=recovery_failed next_retry_unix_ms=1767225660000",
      "inc-000042",
      "GUI 版本：0.1.0",
      "interval=15s timeout=2s retries=2 min_fault=60s",
      "UTC+08:00 Asia/Taipei America/Argentina/Buenos_Aires",
      "port=51820 rtt=12ms",
      "tcp_connect port=443",
      "隧道 1 | 異常 | 探測失敗（連續 2 / 3）",
    ]) {
      unchanged(text);
    }
  });

  it("passes the GUI's own words", () => {
    for (const table of [LIGHT_REASON, ADAPTER_ERROR, ERROR_CODE, NOTIFICATION_FAILURE]) {
      for (const text of Object.values(table)) unchanged(text);
    }
    unchanged(judgmentText({ interval_secs: 15, timeout_secs: 2, retries: 2, min_fault_secs: 60 }));
    const events: LogEvent[] = [
      { kind: "probe_failed", streak: 2, needed: 3 },
      { kind: "recovery_started", attempt: 1 },
      { kind: "local_network_offline" },
      { kind: "light_changed", light: "red", reason: "fault_confirmed" },
    ];
    for (const event of events) unchanged(eventText(event, false, () => "2026-11-03 21:30:05 +08:00"));
  });
});
