// What a refusal or a failure to get an answer says (owner's request of
// 2026-11-03): the sentence comes from the language file, by the refusal's
// `reason` when it has one and by its `code` otherwise; the codes and the
// watchdog's or the shell's English `message` are the details, shown only
// collapsed (plan §1.1), never as the sentence. A tunnel's fault is worded
// the same way, by its kind. No DOM here.

import { type Key, REFERENCE as R, t, table } from "./i18n";
import type { ErrorReason, Fault, ProtocolError, Request } from "./protocol";
import type { Said } from "./said";
import { ERROR_CODE, FAULT } from "./strings";

/// The sentence for every reason; `tsc` fails when the contract gains one
/// the reference file has no text for.
export const ERROR_REASON = table("error_reason", R.error_reason satisfies Record<ErrorReason, string>);

/// Requests that widen what the watchdog does on its own: registering a
/// tunnel, or turning an automatic action on. The watchdog allows them only
/// within the installation's access list (plan §9.2).
function widensScope(request: Request): boolean {
  switch (request.op) {
    case "register_tunnel":
      return true;
    case "set_auto_recovery":
    case "set_start_when_stopped_externally":
      return request.enabled;
    default:
      return false;
  }
}

/// Requests that only read. A request that got no answer in time may still
/// be carried out unless it only reads.
function readsOnly(request: Request): boolean {
  return request.op === "get_snapshot" || request.op === "discover_tunnels" || request.op === "read_event_log";
}

/// Wording for a refusal that means something particular for one
/// operation, or null.
function specific(request: Request, error: ProtocolError): Key | null {
  const busy = error.reason === "operation_in_progress" || (error.reason === null && error.code === "busy");
  if (busy && request.op === "unregister_tunnel") return "errors.busy_unregister";
  if (error.reason === null && error.code === "invalid_setting" && request.op === "update_host_settings") {
    return "errors.invalid_host_setting";
  }
  return null;
}

/// The codes and the original message, for the details:
/// `unauthorized (caller_not_allowed): …`.
export function errorDetail(error: ProtocolError): string {
  return `${error.code}${error.reason === null ? "" : ` (${error.reason})`}: ${error.message}`;
}

/// The sentence for a refusal. Only the operation and its on/off choice are
/// read from `request`, never its values: it can carry the bot token.
export function refusalText(request: Request, error: ProtocolError): string {
  const own = specific(request, error);
  let text: string;
  if (own !== null) {
    text = t(own);
  } else if (error.reason !== null) {
    text = ERROR_REASON[error.reason];
  } else if (error.code === "unauthorized") {
    // An older answer without a reason: which list it is about follows
    // from the request.
    text = widensScope(request) ? ERROR_REASON.needs_administrator_or_listed_tunnel : ERROR_REASON.caller_not_allowed;
  } else {
    text = ERROR_CODE[error.code];
  }
  // No answer in time: the watchdog may have recorded the operation anyway,
  // so the next snapshot, not this answer, tells what happened.
  const late = error.reason === "no_answer_in_time" || (error.reason === null && error.code === "timeout");
  return late && !readsOnly(request) ? `${text}${t("errors.timeout_may_have_run")}` : text;
}

/// A refusal in words: the sentence, and the codes with the original
/// message in the details.
export function refusal(request: Request, error: ProtocolError): Said {
  return { text: refusalText(request, error), detail: errorDetail(error) };
}

/// Why no snapshot could be read, for the lost-link overlay.
export function snapshotRefusal(error: ProtocolError): Said {
  return refusal({ op: "get_snapshot" }, error);
}

/// A failure of the window's own side to get an answer, with the key of
/// its sentence; `message` is English, for the details.
export class ClientError extends Error {
  constructor(
    readonly key: Key,
    message: string,
  ) {
    super(message);
    this.name = "ClientError";
  }
}

/// What is said when the watchdog could not be asked at all: the client's
/// own reason, or the general sentence with whatever the failure said in the
/// details.
export function noAnswer(error: unknown): Said {
  if (error instanceof ClientError) return { text: t(error.key), detail: error.message };
  const detail = error instanceof Error ? error.message : typeof error === "string" ? error : JSON.stringify(error);
  return { text: t("errors.no_answer"), detail: detail === undefined || detail === "" ? null : detail };
}

/// An answer that is not one of the contract's for this request.
export function unexpectedAnswer(): Said {
  return { text: t("errors.unexpected_answer"), detail: null };
}

/// A tunnel's fault: the sentence by its kind, the kind and the watchdog's
/// own text in the details.
export function faultSaid(fault: Fault): Said {
  return { text: FAULT[fault.kind], detail: `${fault.kind}: ${fault.message}` };
}

/// The sentence with its details in parentheses, for a text that has no
/// collapsed part (an export).
export function inline(said: Said): string {
  return said.detail === null ? said.text : t("common.with_code", { text: said.text, code: said.detail });
}
