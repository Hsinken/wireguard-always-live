// How the GUI reaches the watchdog: every operation is one `Request` from the
// closed contract, sent through the Tauri bridge (`watchdog_request`), which
// validates it and forwards it over the watchdog's local IPC. The GUI has no
// other way to affect a tunnel or send a notification (AGENTS.md rule 9).

import { invoke, isTauri } from "@tauri-apps/api/core";

import { ClientError } from "./errors";

import {
  PROTOCOL_VERSION,
  type Outcome,
  type Request,
  type RequestEnvelope,
  type ResponseEnvelope,
} from "./protocol";

export interface WatchdogClient {
  request(requestId: string, request: Request): Promise<Outcome>;
}

/// The real client. Any failure to get an answer is thrown, and the caller
/// shows the watchdog as lost; it is never read as a state.
export class TauriClient implements WatchdogClient {
  async request(requestId: string, request: Request): Promise<Outcome> {
    if (!isTauri()) {
      throw new ClientError("client.not_in_shell", "not running inside the WG Always Live desktop shell");
    }
    const envelope: RequestEnvelope = {
      protocol_version: PROTOCOL_VERSION,
      request_id: requestId,
      request,
    };
    const response = await invoke<ResponseEnvelope>("watchdog_request", { envelope });
    if (response.protocol_version !== PROTOCOL_VERSION) {
      throw new ClientError("client.wrong_version", "the bridge answered in another protocol version");
    }
    // The bridge answers every request with its own ID, its refusals
    // included, and drops a watchdog answer that names another request or
    // none. Anything else here is not an answer to this request.
    if (response.request_id !== requestId) {
      throw new ClientError("client.mismatched", "the bridge's answer names another request");
    }
    return response.outcome;
  }
}
