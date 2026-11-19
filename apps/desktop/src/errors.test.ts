// Refusals and failures in words: the sentence comes from the language file
// by reason, else by code, and the watchdog's or the shell's English
// message is only ever in the details.

import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  ClientError,
  ERROR_REASON,
  errorDetail,
  faultSaid,
  inline,
  noAnswer,
  refusal,
  refusalText,
  snapshotRefusal,
  unexpectedAnswer,
} from "./errors";
import { t } from "./i18n";
import type { AdapterErrorKind, ErrorCode, ErrorReason, ProtocolError, Request } from "./protocol";
import { ERROR_CODE, FAULT } from "./strings";

/// The members of a generated string union, read from its file.
function union(name: string): string[] {
  const source = readFileSync(new URL(`./protocol/generated/${name}.ts`, import.meta.url), "utf8");
  return [...source.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]!);
}

const err = (code: ErrorCode, reason: ErrorReason | null, message = "the original text"): ProtocolError => ({
  code,
  reason,
  message,
});

const MAY_HAVE_RUN = t("errors.timeout_may_have_run");

describe("every reason, code and fault kind", () => {
  it("has a sentence of its own", () => {
    const reasons = union("ErrorReason");
    expect(reasons.length).toBe(32);
    expect(Object.keys(ERROR_REASON).sort()).toEqual([...reasons].sort());
    for (const reason of reasons) expect(ERROR_REASON[reason as ErrorReason], reason).not.toBe("");
    const codes = union("ErrorCode");
    expect(codes.length).toBe(11);
    expect(Object.keys(ERROR_CODE).sort()).toEqual([...codes].sort());
    const kinds = union("AdapterErrorKind");
    expect(Object.keys(FAULT).sort()).toEqual([...kinds].sort());
    for (const kind of kinds) expect(FAULT[kind as AdapterErrorKind], kind).not.toBe("");
  });
});

describe("a refusal", () => {
  const connect: Request = { op: "connect", tunnel_id: "office-1" };

  it("is worded by its reason, the codes and the message only in the details", () => {
    const said = refusal(connect, err("busy", "operation_in_progress", "an operation on this tunnel is under way"));
    expect(said.text).toBe(ERROR_REASON.operation_in_progress);
    expect(said.text).not.toContain("busy");
    expect(said.text).not.toContain("under way");
    expect(said.detail).toBe("busy (operation_in_progress): an operation on this tunnel is under way");
  });

  it("is worded by its code when it has no reason", () => {
    const said = refusal(connect, err("busy", null, "x"));
    expect(said).toEqual({ text: ERROR_CODE.busy, detail: "busy: x" });
    for (const code of union("ErrorCode") as ErrorCode[]) {
      const text = refusalText(connect, err(code, null));
      expect(text, code).not.toContain("the original text");
      expect(text.startsWith(code === "unauthorized" ? ERROR_REASON.caller_not_allowed : ERROR_CODE[code]), code).toBe(true);
    }
  });

  it("says which list an unauthorized answer is about: the watchdog's reason, or the request without one", () => {
    for (const request of [
      connect,
      { op: "register_tunnel", tunnel_id: "office-1" },
      { op: "set_auto_recovery", tunnel_id: "office-1", enabled: true },
    ] as Request[]) {
      expect(refusalText(request, err("unauthorized", "caller_not_allowed"))).toBe(ERROR_REASON.caller_not_allowed);
      expect(refusalText(request, err("unauthorized", "needs_administrator_or_listed_tunnel"))).toContain("需要管理員");
    }
    const widening: Request[] = [
      { op: "register_tunnel", tunnel_id: "office-1" },
      { op: "set_auto_recovery", tunnel_id: "office-1", enabled: true },
      { op: "set_start_when_stopped_externally", tunnel_id: "office-1", enabled: true },
    ];
    for (const request of widening) {
      expect(refusalText(request, err("unauthorized", null)), request.op).toBe(
        ERROR_REASON.needs_administrator_or_listed_tunnel,
      );
    }
    // Turning an automatic action off never needs more than using the watchdog.
    expect(
      refusalText({ op: "set_start_when_stopped_externally", tunnel_id: "office-1", enabled: false }, err("unauthorized", null)),
    ).toBe(ERROR_REASON.caller_not_allowed);
  });

  it("says an operation that got no answer in time may still have been carried out, unless nothing was done", () => {
    for (const request of [
      { op: "disconnect", tunnel_id: "office-1" },
      { op: "reconnect", tunnel_id: "office-1" },
      { op: "send_test_notification" },
    ] as Request[]) {
      expect(refusalText(request, err("timeout", "no_answer_in_time")), request.op).toBe(
        `${ERROR_REASON.no_answer_in_time}${MAY_HAVE_RUN}`,
      );
      expect(refusalText(request, err("timeout", null))).toBe(`${ERROR_CODE.timeout}${MAY_HAVE_RUN}`);
      // Too many waiting: nothing was done, so nothing may have run.
      expect(refusalText(request, err("timeout", "too_many_waiting"))).toBe(ERROR_REASON.too_many_waiting);
    }
    for (const request of [
      { op: "get_snapshot" },
      { op: "discover_tunnels" },
      { op: "read_event_log", filter: { kind: "all" }, before_seq: null },
    ] as Request[]) {
      expect(refusalText(request, err("timeout", "no_answer_in_time")), request.op).not.toContain(MAY_HAVE_RUN);
    }
    // A lost answer says so itself.
    expect(refusalText(connect, err("internal", "answer_lost"))).toBe(ERROR_REASON.answer_lost);
  });

  it("says plainly that a busy tunnel was not removed, and words an unknown time zone", () => {
    const unregister: Request = { op: "unregister_tunnel", tunnel_id: "office-1" };
    for (const error of [err("busy", "operation_in_progress"), err("busy", null)]) {
      const text = refusalText(unregister, error);
      expect(text).toBe(t("errors.busy_unregister"));
      expect(text).toContain("尚未移除");
    }
    const host: Request = { op: "update_host_settings", settings: { workers: null, time_zone: "Mars/Base" } };
    expect(refusalText(host, err("invalid_setting", "unknown_time_zone"))).toBe("背景監控不認得這個時區名稱");
    expect(refusalText(host, err("invalid_setting", null))).toBe(t("errors.invalid_host_setting"));
  });

  it("never quotes the request's values, such as the bot token", () => {
    const token = "123456789:EXAMPLE-not-a-real-token_0000000000";
    const request: Request = { op: "set_bot_token", token };
    for (const code of union("ErrorCode") as ErrorCode[]) {
      for (const reason of [null, ...union("ErrorReason")] as Array<ErrorReason | null>) {
        const said = refusal(request, err(code, reason, "rejected"));
        expect(JSON.stringify(said), `${code} ${reason}`).not.toContain("EXAMPLE");
      }
    }
  });

  it("for a snapshot reads as one", () => {
    expect(snapshotRefusal(err("timeout", "no_answer_in_time")).text).toBe(ERROR_REASON.no_answer_in_time);
    expect(errorDetail(err("internal", null, "m"))).toBe("internal: m");
  });
});

describe("no answer at all", () => {
  it("is the client's own reason in words, its English text in the details", () => {
    const said = noAnswer(new ClientError("client.mismatched", "the bridge's answer names another request"));
    expect(said).toEqual({ text: "回應與請求不符", detail: "the bridge's answer names another request" });
  });

  it("is the general sentence for anything else, what was thrown in the details", () => {
    expect(noAnswer(new Error("pipe broke"))).toEqual({ text: "無法連到背景監控", detail: "pipe broke" });
    expect(noAnswer("ipc rejected")).toEqual({ text: "無法連到背景監控", detail: "ipc rejected" });
    expect(noAnswer(new Error(""))).toEqual({ text: "無法連到背景監控", detail: null });
    expect(unexpectedAnswer()).toEqual({ text: "回應格式不符", detail: null });
  });
});

describe("a tunnel's fault", () => {
  it("is worded by its kind, the watchdog's text only in the details", () => {
    expect(faultSaid({ kind: "timeout", message: "probe of the recovery target timed out" })).toEqual({
      text: FAULT.timeout,
      detail: "timeout: probe of the recovery target timed out",
    });
  });
});

describe("an export's text", () => {
  it("keeps the original beside the sentence, as it has no collapsed part", () => {
    expect(inline({ text: "句子", detail: "code: message" })).toBe("句子（code: message）");
    expect(inline({ text: "句子", detail: null })).toBe("句子");
  });
});
