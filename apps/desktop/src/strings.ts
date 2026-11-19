// The words for every value the watchdog reports (plan §1.1), from the
// language file (`i18n/`). Each table is checked with `satisfies` against
// the generated union, so `tsc` fails if the contract gains a value without
// a text in the reference file, and reads the current language every time
// a property is read. System error codes and names stay visible next to
// the text in the details.

import { REFERENCE as R, table } from "./i18n";
import type {
  AdapterErrorKind,
  Capability,
  CredentialState,
  DataHealth,
  ErrorCode,
  FirstRecovery,
  Light,
  LightReason,
  NoticeKind,
  NotificationFailure,
  NotificationState,
  OsState,
  RecoveryPhase,
  Takeover,
  TestNotificationState,
} from "./protocol";

const ICON: Record<Light, string> = { green: "●", yellow: "▲", red: "■", gray: "○" };
const LIGHT_LABEL = table("light", R.light satisfies Record<Light, string>);

/// Color is never the only cue: every light has a shape and a word.
export const LIGHT: Readonly<Record<Light, { readonly icon: string; readonly label: string }>> = Object.freeze(
  Object.fromEntries(
    (Object.keys(ICON) as Light[]).map((light) => [
      light,
      Object.freeze({
        icon: ICON[light],
        get label() {
          return LIGHT_LABEL[light];
        },
      }),
    ]),
  ) as Record<Light, { icon: string; label: string }>,
);

export const LIGHT_REASON = table("light_reason", R.light_reason satisfies Record<LightReason, string>);
export const OS_STATE = table("os_state", R.os_state satisfies Record<OsState, string>);
export const DATA_HEALTH = table("data_health", R.data_health satisfies Record<DataHealth, string>);
export const RECOVERY_PHASE = table("recovery_phase", R.recovery_phase satisfies Record<RecoveryPhase, string>);
export const TAKEOVER = table("takeover", R.takeover satisfies Record<Takeover, string>);
export const FIRST_RECOVERY = table("first_recovery", R.first_recovery satisfies Record<FirstRecovery, string>);
export const NOTIFICATION_STATE = table(
  "notification_state",
  R.notification_state satisfies Record<NotificationState, string>,
);
export const ERROR_CODE = table("error_code", R.error_code satisfies Record<ErrorCode, string>);
export const ADAPTER_ERROR = table("adapter_error", R.adapter_error satisfies Record<AdapterErrorKind, string>);
/// A tunnel's fault as a sentence, by its kind; the watchdog's own text is
/// shown only among the details.
export const FAULT = table("fault", R.fault satisfies Record<AdapterErrorKind, string>);
export const CAPABILITY = table("capability", R.capability satisfies Record<Capability, string>);
export const CREDENTIAL = table("credential", R.credential satisfies Record<CredentialState, string>);
/// Which of an incident's messages a notification line is about.
export const NOTICE_KIND = table("notice_kind", R.notice_kind satisfies Record<NoticeKind, string>);
export const TEST_STATE = table("test_state", R.test_state satisfies Record<TestNotificationState, string>);
export const NOTIFICATION_FAILURE = table(
  "notification_failure",
  R.notification_failure satisfies Record<NotificationFailure, string>,
);
