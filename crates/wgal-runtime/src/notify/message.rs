//! Message text (plan §7.4). Plain text, sent without `parse_mode`, so names
//! are shown exactly as they are and cannot inject formatting. A formal
//! message carries only the device name, the tunnel name, a short incident
//! code, when the fault started, the result of the first recovery round and
//! what automatic recovery does next; the messages that a retry count is
//! used up and that traffic came back (owner decision of 2026-11-04) carry
//! the same names and code, the count or when traffic came back: no
//! endpoint, internal address, UUID or diagnostics.
//!
//! The words come from [`NotificationTemplates`] (wave three, stage 8): the
//! GUI hands the watchdog the set from its language file (and `wgal notify
//! language` the set of the language the user names), and until one has,
//! or when the set kept cannot be read, the watchdog writes with its
//! built-in set ([`builtin_templates`]): the texts of the GUI's English
//! file, since English is the default language (owner decision of
//! 2026-11-18; until 3.0.0 they were the Traditional Chinese ones). Only
//! the fixed placeholders are filled in, each value inserted as it is; the
//! contract allows no placeholder for an endpoint, an address, a UUID, a
//! tunnel ID or the token.
//!
//! Times are written in the configured time zone (owner request 9 of
//! 2026-11-03) as local time. The message names the zone and its offset
//! once, on a line of its own (`{time_zone}`, owner request of 2026-11-05),
//! and a later time whose offset differs from the first's carries its own,
//! so a reader elsewhere, or across a daylight saving change, can still tell
//! exactly when it was ([`message_times`]).

use std::sync::LazyLock;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use wgal_core::DueNotification;
use wgal_protocol::{
    DeviceName, FirstRecovery, InvalidTemplate, MessageTemplate, NoticeKind, NotificationTemplates,
    TemplateTexts, TemplatesId, TimeZoneName,
};

/// The Bot API's limit on one message, in characters.
pub const MAX_MESSAGE_CHARS: usize = 4096;

/// The built-in set's ID. Never reported: the snapshot names the built-in
/// set `templates_id: null`. The same as the contract fixture's, so the
/// whole set equals the fixture; and the ID the GUI gives its English set
/// (`<tag>/<FNV-1a 32 of the texts>`, `apps/desktop/src/templates.ts`).
pub const BUILTIN_ID: &str = "en/92c719a8";
/// The built-in formal alert (plan §7.2): device, tunnel, incident, time
/// zone, when the fault started, the first recovery's result and what
/// automatic recovery does next.
pub const BUILTIN_FORMAL: &str = "[WG Always Live] Fault alert\n\
     Device: {device}\n\
     Tunnel: {tunnel}\n\
     Incident: {incident}\n\
     Time zone: {time_zone}\n\
     Fault started: {started_at}\n\
     Recovery result: {result}\n\
     {retry}";
/// The built-in `{result}` when the first recovery round failed.
pub const BUILTIN_RESULT_FAILED: &str =
    "The first automatic recovery failed (still no traffic after restarting the tunnel)";
/// The built-in `{result}` when it could not be carried out.
pub const BUILTIN_RESULT_NOT_EXECUTED: &str =
    "Automatic recovery could not run (the tunnel cannot be started or stopped)";
/// The built-in `{retry}` while a limited count has rounds left.
pub const BUILTIN_RETRY_LIMITED: &str = "WG Always Live will try again every {interval} seconds, up to {limit} times; you will be notified again once traffic is back and stable for 2 minutes.";
/// The built-in `{retry}` while it keeps trying.
pub const BUILTIN_RETRY_FOREVER: &str = "WG Always Live will keep trying every {interval} seconds; you will be notified again once traffic is back and stable for 2 minutes.";
/// The built-in `{retry}` when no round follows.
pub const BUILTIN_RETRY_NONE: &str = "WG Always Live will not retry automatically; please handle it manually. You will be notified again once traffic is back and stable for 2 minutes.";
/// The built-in message that a limited retry count is used up.
pub const BUILTIN_EXHAUSTED: &str = "[WG Always Live] Retries stopped\n\
     Device: {device}\n\
     Tunnel: {tunnel}\n\
     Incident: {incident}\n\
     Time zone: {time_zone}\n\
     Fault started: {started_at}\n\
     Retried automatically {retries} times and reached the limit; automatic recovery has stopped. Please handle it manually; you will be notified again once traffic is back and stable for 2 minutes.";
/// The built-in message that traffic came back.
pub const BUILTIN_RECOVERED: &str = "[WG Always Live] Recovered\n\
     Device: {device}\n\
     Tunnel: {tunnel}\n\
     Incident: {incident}\n\
     Time zone: {time_zone}\n\
     Fault started: {started_at}\n\
     Recovered at: {recovered_at}\n\
     Traffic is back and has been stable for 2 minutes.";
/// The built-in test message (plan §7.4).
pub const BUILTIN_TEST: &str = "[WG Always Live] Test notification\n\
     This is a test message and does not mean any VPN fault.\n\
     Device: {device}\n\
     Time zone: {time_zone}\n\
     Time: {time}";
/// The built-in device name used when none is set.
pub const BUILTIN_UNNAMED_DEVICE: &str = "(unnamed device)";

fn builtin() -> Result<NotificationTemplates, InvalidTemplate> {
    NotificationTemplates::new(
        TemplatesId::new(BUILTIN_ID)?,
        TemplateTexts {
            formal: MessageTemplate::new(BUILTIN_FORMAL)?,
            result_failed: MessageTemplate::new(BUILTIN_RESULT_FAILED)?,
            result_not_executed: MessageTemplate::new(BUILTIN_RESULT_NOT_EXECUTED)?,
            retry_limited: MessageTemplate::new(BUILTIN_RETRY_LIMITED)?,
            retry_forever: MessageTemplate::new(BUILTIN_RETRY_FOREVER)?,
            retry_none: MessageTemplate::new(BUILTIN_RETRY_NONE)?,
            exhausted: MessageTemplate::new(BUILTIN_EXHAUSTED)?,
            recovered: MessageTemplate::new(BUILTIN_RECOVERED)?,
            test: MessageTemplate::new(BUILTIN_TEST)?,
            unnamed_device: MessageTemplate::new(BUILTIN_UNNAMED_DEVICE)?,
        },
    )
}

/// The set messages are written with until a GUI or `wgal` hands over
/// one, and whenever the one kept cannot be read: the texts above, which
/// the tests check against the contract's fixture
/// (`request_set_notification_templates.json`) and the GUI's English
/// language file.
pub fn builtin_templates() -> &'static NotificationTemplates {
    static BUILTIN: LazyLock<NotificationTemplates> = LazyLock::new(|| {
        // Constant texts that form a valid set; the tests below build it.
        builtin().expect("the built-in notification templates are a valid set")
    });
    &BUILTIN
}

/// The device name a message gives: the configured one, or the set's
/// `unnamed_device` when that is blank.
fn device(templates: &NotificationTemplates, name: &DeviceName) -> String {
    if name.as_str().trim().is_empty() {
        templates.unnamed_device().render(&[])
    } else {
        name.as_str().to_owned()
    }
}

/// A Unix time in milliseconds as `YYYY-MM-DD HH:MM:SS` in UTC, for any
/// `i64`.
fn civil_utc(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// A Unix time in milliseconds as `YYYY-MM-DD HH:MM:SS UTC`, for text that
/// is always in UTC (the Windows service's own log file).
pub fn utc(unix_ms: i64) -> String {
    format!("{} UTC", civil_utc(unix_ms))
}

/// A Unix time in milliseconds in `zone`: local time as
/// `YYYY-MM-DD HH:MM:SS` (seconds dropped, not rounded) and the zone's
/// offset from UTC at that instant, in seconds. A time outside the range
/// jiff covers (years -9999 to 9999) is the UTC one, with no offset
/// (`None`).
fn clock(unix_ms: i64, zone: &TimeZone) -> (String, Option<i32>) {
    let Ok(at) = Timestamp::from_millisecond(unix_ms) else {
        return (civil_utc(unix_ms), None);
    };
    let offset = zone.to_offset(at);
    let local = offset.to_datetime(at);
    let when = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute(),
        local.second()
    );
    (when, Some(offset.seconds()))
}

/// What `{time_zone}` says of `zone` at an instant whose offset is `offset`
/// (`None`: a time written in UTC, see [`clock`]): `Asia/Taipei, UTC+08:00`.
/// The offset is the zone's at that instant, so daylight saving time shows
/// as it was. UTC itself is `UTC`; a zone without an IANA name, or with one
/// not in the form [`TimeZoneName`] accepts, shows only its offset,
/// `UTC+08:00`. So it is plain ASCII of at most
/// `NotificationTemplates::MAX_TIME_ZONE_BYTES` (78: a 64-byte name, `, `
/// and `UTC+25:59:59`).
fn zone_text(zone: &TimeZone, offset: Option<i32>) -> String {
    let Some(offset) = offset else {
        return "UTC".to_owned();
    };
    match zone.iana_name() {
        // `TimeZone::UTC` (also what `TimeZone::get("UTC")` and a zero fixed
        // offset give) and the database's own name for it. The offset is
        // checked too, so a zone merely named so is never shown as UTC.
        Some("UTC" | "Etc/UTC") if offset == 0 => "UTC".to_owned(),
        Some(name) if TimeZoneName::new(name).is_ok() => format!("{name}, {}", utc_offset(offset)),
        _ => utc_offset(offset),
    }
}

/// The times of one message in `zone` (owner request of 2026-11-05):
/// `{time_zone}`, the zone at the first of them ([`zone_text`]), and each
/// as local time without the zone, `2026-11-05 00:26:01`. A later time
/// whose offset differs from the first's, a daylight saving change having
/// come between (or one of them written in UTC, see [`clock`]), is followed
/// by its own, `2026-11-01 01:10:00 (UTC-05:00)` or `(UTC)`, so every time
/// stays exact.
pub fn message_times<const N: usize>(unix_ms: [i64; N], zone: &TimeZone) -> (String, [String; N]) {
    const { assert!(N > 0, "a message's times start with one") };
    let clocks = unix_ms.map(|ms| clock(ms, zone));
    let first = clocks[0].1;
    let time_zone = zone_text(zone, first);
    let times = clocks.map(|(when, offset)| match offset {
        _ if offset == first => when,
        Some(offset) => format!("{when} ({})", utc_offset(offset)),
        None => format!("{when} (UTC)"),
    });
    (time_zone, times)
}

/// An offset from UTC as `UTC+08:00` or `UTC-03:30`, with seconds only when
/// it has any (some historical local mean times do).
fn utc_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    let (hours, minutes, secs) = (abs / 3600, abs % 3600 / 60, abs % 60);
    if secs == 0 {
        format!("UTC{sign}{hours:02}:{minutes:02}")
    } else {
        format!("UTC{sign}{hours:02}:{minutes:02}:{secs:02}")
    }
}

/// The alert for an incident whose first automatic recovery failed or could
/// not be carried out (plan §7.2), written with `templates`: `{device}` the
/// device name, or the rendered `unnamed_device` when it is blank,
/// `{tunnel}` the tunnel's display name, `{incident}` the incident ID,
/// `{time_zone}` and `{started_at}` when the fault started, as
/// [`message_times`] in `zone`, `{result}` the rendered
/// `result_not_executed` or `result_failed`, and
/// `{retry}` what automatic recovery does next ([`retry_text`]). "Could not
/// be carried out" is never reported as "failed". The contract bounds every
/// template and value so the result fits in one Telegram message.
pub fn formal_message(
    templates: &NotificationTemplates,
    device_name: &DeviceName,
    due: &DueNotification,
    zone: &TimeZone,
) -> String {
    let result = match due.first_recovery {
        FirstRecovery::NotExecuted => templates.result_not_executed(),
        _ => templates.result_failed(),
    }
    .render(&[]);
    let retry = retry_text(templates, due);
    let device = device(templates, device_name);
    let (time_zone, [started_at]) = message_times([due.opened_at_unix_ms], zone);
    templates.formal().render(&[
        ("device", &device),
        ("tunnel", due.display_name.as_str()),
        ("incident", due.incident_id.as_str()),
        ("time_zone", &time_zone),
        ("started_at", &started_at),
        ("result", &result),
        ("retry", &retry),
    ])
}

/// What automatic recovery does after the alert, rendered: `retry_forever`
/// for a tunnel that keeps trying, `retry_limited` with the rounds still
/// left of its limit, and `retry_none` when none are (a limit of 0, or
/// used up), each with the tunnel's interval in seconds.
pub fn retry_text(templates: &NotificationTemplates, due: &DueNotification) -> String {
    let interval = due.retry.interval_secs().to_string();
    let left = due
        .retry
        .limit()
        .map(|limit| u32::from(limit).saturating_sub(due.retries_used));
    match left {
        _ if due.retries_exhausted => templates.retry_none().render(&[]),
        None => templates.retry_forever().render(&[("interval", &interval)]),
        Some(0) => templates.retry_none().render(&[]),
        Some(left) => templates
            .retry_limited()
            .render(&[("interval", &interval), ("limit", &left.to_string())]),
    }
}

/// The message that a limited retry count is used up (owner decision of
/// 2026-11-04): the alert's names, code, zone and start, and `{retries}`,
/// the rounds that followed the first.
pub fn exhausted_message(
    templates: &NotificationTemplates,
    device_name: &DeviceName,
    due: &DueNotification,
    zone: &TimeZone,
) -> String {
    let device = device(templates, device_name);
    let (time_zone, [started_at]) = message_times([due.opened_at_unix_ms], zone);
    templates.exhausted().render(&[
        ("device", &device),
        ("tunnel", due.display_name.as_str()),
        ("incident", due.incident_id.as_str()),
        ("time_zone", &time_zone),
        ("started_at", &started_at),
        ("retries", &due.retries_used.to_string()),
    ])
}

/// The message that traffic came back and stayed healthy through the
/// closing period (owner decision of 2026-11-04): the alert's names, code,
/// zone and start, and `{recovered_at}`, when traffic came back.
pub fn recovered_message(
    templates: &NotificationTemplates,
    device_name: &DeviceName,
    due: &DueNotification,
    zone: &TimeZone,
) -> String {
    let device = device(templates, device_name);
    let (time_zone, [started_at, recovered_at]) = message_times(
        [
            due.opened_at_unix_ms,
            due.recovered_at_unix_ms.unwrap_or(due.opened_at_unix_ms),
        ],
        zone,
    );
    templates.recovered().render(&[
        ("device", &device),
        ("tunnel", due.display_name.as_str()),
        ("incident", due.incident_id.as_str()),
        ("time_zone", &time_zone),
        ("started_at", &started_at),
        ("recovered_at", &recovered_at),
    ])
}

/// The message `due` is for.
pub fn notice_message(
    templates: &NotificationTemplates,
    device_name: &DeviceName,
    due: &DueNotification,
    zone: &TimeZone,
) -> String {
    match due.kind {
        NoticeKind::Alert => formal_message(templates, device_name, due, zone),
        NoticeKind::RetriesExhausted => exhausted_message(templates, device_name, due, zone),
        NoticeKind::Recovered => recovered_message(templates, device_name, due, zone),
    }
}

/// A test message, which the template must mark as one so it is never
/// taken for a fault (plan §7.4), written with `templates`: `{device}` as in
/// [`formal_message`], and `{time_zone}` and `{time}` now, as
/// [`message_times`] in `zone`.
pub fn test_message(
    templates: &NotificationTemplates,
    device_name: &DeviceName,
    now_unix_ms: i64,
    zone: &TimeZone,
) -> String {
    let device = device(templates, device_name);
    let (time_zone, [time]) = message_times([now_unix_ms], zone);
    templates.test().render(&[
        ("device", &device),
        ("time_zone", &time_zone),
        ("time", &time),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::Offset;
    use wgal_protocol::{DisplayName, IncidentId, TunnelId};

    /// 2026-01-01 00:00:00 UTC.
    const NEW_YEAR: i64 = 1_767_225_600_000;

    fn zone(name: &str) -> TimeZone {
        TimeZone::get(name).unwrap()
    }

    fn fixed(seconds: i32) -> TimeZone {
        TimeZone::fixed(Offset::from_seconds(seconds).unwrap())
    }

    /// A TZif (RFC 8536) version 1 zone with one offset and no transitions,
    /// so a test can give a zone any name.
    fn named(name: &str, offset_secs: i32) -> TimeZone {
        let mut data = b"TZif".to_vec();
        data.push(0);
        data.extend_from_slice(&[0; 15]);
        // isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
        for count in [0_u32, 0, 0, 0, 1, 4] {
            data.extend_from_slice(&count.to_be_bytes());
        }
        data.extend_from_slice(&offset_secs.to_be_bytes());
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(b"ZZZ\0");
        TimeZone::tzif(name, &data).unwrap()
    }

    /// One time as a message writes it, with its `{time_zone}`.
    fn one(unix_ms: i64, zone: &TimeZone) -> (String, String) {
        let (time_zone, [time]) = message_times([unix_ms], zone);
        (time_zone, time)
    }

    fn pair(zone: &str, time: &str) -> (String, String) {
        (zone.to_owned(), time.to_owned())
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_767_225_600_000), "2026-01-01 00:00:00 UTC");
        assert_eq!(utc(1_709_210_096_789), "2024-02-29 12:34:56 UTC");
        assert_eq!(utc(-1), "1969-12-31 23:59:59 UTC");
    }

    #[test]
    fn utc_itself_is_named_utc() {
        for utc in [
            TimeZone::UTC,
            zone("UTC"),
            zone("Etc/UTC"),
            fixed(0),
            named("UTC", 0),
        ] {
            assert_eq!(one(NEW_YEAR, &utc), pair("UTC", "2026-01-01 00:00:00"));
        }
        assert_eq!(
            one(1_709_210_096_789, &TimeZone::UTC),
            pair("UTC", "2024-02-29 12:34:56")
        );
        assert_eq!(one(-1, &TimeZone::UTC), pair("UTC", "1969-12-31 23:59:59"));
        // Only named so: named as any other zone.
        assert_eq!(
            one(NEW_YEAR, &named("UTC", 3600)),
            pair("UTC, UTC+01:00", "2026-01-01 01:00:00")
        );
    }

    #[test]
    fn a_named_zone_is_named_with_its_offset_and_the_time_is_local() {
        // 2026-11-03 09:16:05 UTC.
        assert_eq!(
            one(1_793_697_365_000, &zone("Asia/Taipei")),
            pair("Asia/Taipei, UTC+08:00", "2026-11-03 17:16:05")
        );
        // Milliseconds are dropped, not rounded; the date follows the zone.
        assert_eq!(
            one(NEW_YEAR - 1, &zone("Asia/Taipei")),
            pair("Asia/Taipei, UTC+08:00", "2026-01-01 07:59:59")
        );
        assert_eq!(
            one(NEW_YEAR, &zone("America/New_York")),
            pair("America/New_York, UTC-05:00", "2025-12-31 19:00:00")
        );
        assert_eq!(
            one(NEW_YEAR, &zone("Asia/Kathmandu")),
            pair("Asia/Kathmandu, UTC+05:45", "2026-01-01 05:45:00")
        );
        assert_eq!(
            one(NEW_YEAR, &zone("America/St_Johns")),
            pair("America/St_Johns, UTC-03:30", "2025-12-31 20:30:00")
        );
    }

    #[test]
    fn a_later_time_with_another_offset_keeps_its_own() {
        let new_york = zone("America/New_York");
        // 2026-03-08 07:00:00 UTC: clocks go from 02:00 EST to 03:00 EDT.
        let spring = 1_772_953_200_000;
        assert_eq!(
            one(spring - 1000, &new_york),
            pair("America/New_York, UTC-05:00", "2026-03-08 01:59:59")
        );
        assert_eq!(
            one(spring, &new_york),
            pair("America/New_York, UTC-04:00", "2026-03-08 03:00:00")
        );
        assert_eq!(
            message_times([spring - 1000, spring], &new_york),
            (
                "America/New_York, UTC-05:00".to_owned(),
                [
                    "2026-03-08 01:59:59".to_owned(),
                    "2026-03-08 03:00:00 (UTC-04:00)".to_owned()
                ]
            )
        );
        // 2026-11-01 06:00:00 UTC: clocks go back from 02:00 EDT to 01:00
        // EST, so 01:59:59 happens twice; the offsets tell them apart.
        let autumn = 1_793_512_800_000;
        assert_eq!(
            message_times([autumn - 1000, autumn + 3_599_000], &new_york),
            (
                "America/New_York, UTC-04:00".to_owned(),
                [
                    "2026-11-01 01:59:59".to_owned(),
                    "2026-11-01 01:59:59 (UTC-05:00)".to_owned()
                ]
            )
        );
        // Both after the change: one offset, said once.
        assert_eq!(
            message_times([autumn, autumn + 3_599_000], &new_york),
            (
                "America/New_York, UTC-05:00".to_owned(),
                [
                    "2026-11-01 01:00:00".to_owned(),
                    "2026-11-01 01:59:59".to_owned()
                ]
            )
        );
        // A zone without daylight saving time never adds one.
        let (_, times) = message_times([NEW_YEAR, autumn], &zone("Asia/Taipei"));
        assert!(times.iter().all(|t| !t.contains('(')), "{times:?}");
    }

    #[test]
    fn a_zone_without_a_name_is_its_offset() {
        assert_eq!(
            one(NEW_YEAR, &fixed(8 * 3600)),
            pair("UTC+08:00", "2026-01-01 08:00:00")
        );
        assert_eq!(
            one(NEW_YEAR, &fixed(-(3 * 3600 + 30 * 60))),
            pair("UTC-03:30", "2025-12-31 20:30:00")
        );
        assert_eq!(
            one(NEW_YEAR, &fixed(8 * 3600 + 6 * 60 + 5)),
            pair("UTC+08:06:05", "2026-01-01 08:06:05")
        );
        // A POSIX rule has no IANA name but still follows its own DST rule.
        let posix = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(
            one(NEW_YEAR, &posix),
            pair("UTC-05:00", "2025-12-31 19:00:00")
        );
        assert_eq!(
            one(1_772_953_200_000, &posix),
            pair("UTC-04:00", "2026-03-08 03:00:00")
        );
        // The zone used when the system's could not be found.
        assert_eq!(
            one(NEW_YEAR, &TimeZone::unknown()),
            pair("UTC+00:00", "2026-01-01 00:00:00")
        );
    }

    #[test]
    fn a_name_outside_the_iana_form_is_left_out() {
        let longest = format!("A{}", "b".repeat(TimeZoneName::MAX_BYTES - 1));
        assert_eq!(
            one(NEW_YEAR, &named(&longest, 3600)),
            pair(&format!("{longest}, UTC+01:00"), "2026-01-01 01:00:00")
        );
        for bad in [
            format!("{longest}c"),
            "Asia/Taipei\n裝置：x".to_owned(),
            "../etc/localtime".to_owned(),
            String::new(),
        ] {
            assert_eq!(
                one(NEW_YEAR, &named(&bad, 3600)),
                pair("UTC+01:00", "2026-01-01 01:00:00"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn times_beyond_the_zone_database_fall_back_to_utc() {
        let taipei = zone("Asia/Taipei");
        for extreme in [i64::MAX, i64::MIN] {
            let (time_zone, time) = one(extreme, &taipei);
            assert_eq!(time_zone, "UTC");
            assert!(!time.contains('('), "{time}");
        }
        // After an ordinary first time, one written in UTC says so.
        let (time_zone, [first, last]) = message_times([NEW_YEAR, i64::MAX], &taipei);
        assert_eq!(time_zone, "Asia/Taipei, UTC+08:00");
        assert_eq!(first, "2026-01-01 08:00:00");
        assert!(last.ends_with(" (UTC)"), "{last}");
        // And the other way round.
        let (time_zone, [_, last]) = message_times([i64::MIN, NEW_YEAR], &taipei);
        assert_eq!(time_zone, "UTC");
        assert_eq!(last, "2026-01-01 08:00:00 (UTC+08:00)");
    }

    fn due(first_recovery: FirstRecovery) -> DueNotification {
        DueNotification {
            kind: NoticeKind::Alert,
            tunnel_id: TunnelId::new("office-1").unwrap(),
            incident_id: IncidentId::new("inc-000007").unwrap(),
            display_name: DisplayName::new("*Office* <b>`x`</b> [l](http://e)").unwrap(),
            opened_at_unix_ms: 1_767_225_600_000,
            first_recovery,
            retry: wgal_protocol::RecoveryRetry::DEFAULT,
            retries_used: 0,
            retries_exhausted: false,
            recovered_at_unix_ms: None,
        }
    }

    #[test]
    fn formal_messages_hold_the_fields_and_names_verbatim() {
        let name = DeviceName::new("辦公室筆電").unwrap();
        let text = formal_message(
            zh_tw(),
            &name,
            &due(FirstRecovery::Failed),
            &zone("Asia/Taipei"),
        );
        assert!(text.contains("裝置：辦公室筆電"));
        assert!(text.contains("隧道：*Office* <b>`x`</b> [l](http://e)"));
        assert!(text.contains("事件：inc-000007"));
        assert!(text.contains("\n時區：Asia/Taipei, UTC+08:00\n異常開始：2026-01-01 08:00:00\n"));
        assert!(text.contains("第一輪自動復原失敗"));
        assert!(text.ends_with(
            "\nWG Always Live 會每 60 秒再試一次，最多 100 次；恢復並穩定 2 分鐘後會再通知。"
        ));
        assert!(!text.contains("測試"));
        // The tunnel ID (which can be a UUID or interface name) is not sent.
        assert!(!text.contains("office-1"));
        assert!(text.chars().count() <= MAX_MESSAGE_CHARS);

        let not_executed = formal_message(
            zh_tw(),
            &name,
            &due(FirstRecovery::NotExecuted),
            &TimeZone::UTC,
        );
        assert!(not_executed.contains("未能執行自動復原"));
        assert!(!not_executed.contains("復原失敗"));
        assert!(not_executed.contains("\n時區：UTC\n異常開始：2026-01-01 00:00:00\n"));
    }

    #[test]
    fn the_longest_message_fits() {
        // The longest names: 64 four-byte characters for the device, the
        // most a tunnel's name holds.
        let name = DeviceName::new("\u{10FFFF}".repeat(DeviceName::MAX_CHARS)).unwrap();
        let mut long = due(FirstRecovery::Failed);
        long.display_name =
            DisplayName::new("\u{10FFFF}".repeat(DisplayName::MAX_BYTES / 4)).unwrap();
        long.incident_id = IncidentId::new("i".repeat(wgal_protocol::MAX_ID_LEN)).unwrap();
        // The longest zone text: a name of the most bytes `TimeZoneName`
        // allows and the widest offset, with seconds.
        let longest = format!("A{}", "b".repeat(TimeZoneName::MAX_BYTES - 1));
        let widest = named(&longest, 25 * 3600 + 59 * 60 + 59);
        let (time_zone, time) = one(long.opened_at_unix_ms, &widest);
        assert_eq!(time_zone, format!("{longest}, UTC+25:59:59"));
        assert_eq!(time_zone.len(), 64 + 2 + 12);
        assert!(time_zone.len() <= NotificationTemplates::MAX_TIME_ZONE_BYTES);
        assert_eq!(time, "2026-01-02 01:59:59");
        // The longest canonical name in the time zone database.
        let new_salem = zone("America/North_Dakota/New_Salem");
        assert_eq!(
            one(NEW_YEAR, &new_salem),
            pair(
                "America/North_Dakota/New_Salem, UTC-06:00",
                "2025-12-31 18:00:00"
            )
        );
        for zone in [widest, new_salem] {
            assert!(
                formal_message(builtin_templates(), &name, &long, &zone)
                    .chars()
                    .count()
                    <= MAX_MESSAGE_CHARS
            );
            assert!(
                test_message(builtin_templates(), &name, long.opened_at_unix_ms, &zone)
                    .chars()
                    .count()
                    <= MAX_MESSAGE_CHARS
            );
        }
    }

    #[test]
    fn test_messages_say_so() {
        let text = test_message(
            zh_tw(),
            &DeviceName::new("").unwrap(),
            NEW_YEAR,
            &TimeZone::UTC,
        );
        assert!(text.contains("測試通知"));
        assert!(text.contains("不代表任何 VPN 故障"));
        assert!(text.contains("（未命名裝置）"));
        assert!(text.ends_with("\n時區：UTC\n時間：2026-01-01 00:00:00"));
        let taipei = test_message(
            zh_tw(),
            &DeviceName::new("x").unwrap(),
            NEW_YEAR,
            &zone("Asia/Taipei"),
        );
        assert!(taipei.ends_with("\n時區：Asia/Taipei, UTC+08:00\n時間：2026-01-01 08:00:00"));
    }

    // Templates (wave three, stage 8).

    /// The contract's fixture of the built-in templates.
    fn fixture() -> NotificationTemplates {
        let json =
            include_str!("../../../wgal-protocol/fixtures/request_set_notification_templates.json");
        match wgal_protocol::codec::decode_request(json.as_bytes())
            .unwrap()
            .request
        {
            wgal_protocol::Request::SetNotificationTemplates { templates } => templates,
            other => panic!("not a template request: {other:?}"),
        }
    }

    /// A language file's set, as the GUI hands it over (`templateTexts` in
    /// `apps/desktop/src/templates.ts`).
    fn language_set(tag: &str, file: &str) -> NotificationTemplates {
        let value: serde_json::Value = serde_json::from_str(file).unwrap();
        let texts = &value["notification_templates"];
        let text = |field: &str| template(texts[field].as_str().unwrap());
        NotificationTemplates::new(
            TemplatesId::new(format!("{tag}/test")).unwrap(),
            TemplateTexts {
                formal: text("formal"),
                result_failed: text("result_failed"),
                result_not_executed: text("result_not_executed"),
                retry_limited: text("retry_limited"),
                retry_forever: text("retry_forever"),
                retry_none: text("retry_none"),
                exhausted: text("exhausted"),
                recovered: text("recovered"),
                test: text("test"),
                unnamed_device: text("unnamed_device"),
            },
        )
        .unwrap()
    }

    /// The GUI's Traditional Chinese set, the built-in one until 3.0.0
    /// (owner decision of 2026-11-18).
    fn zh_tw() -> &'static NotificationTemplates {
        static SET: LazyLock<NotificationTemplates> = LazyLock::new(|| {
            language_set(
                "zh-TW",
                include_str!("../../../../apps/desktop/src/lang/zh-TW.json"),
            )
        });
        &SET
    }

    /// English is the default language (owner decision of 2026-11-18): the
    /// built-in set is the GUI's English one, text for text.
    #[test]
    fn the_built_in_set_is_the_guis_english_file() {
        let english = language_set(
            "en",
            include_str!("../../../../apps/desktop/src/lang/en.json"),
        );
        let builtin = builtin_templates();
        assert_eq!(builtin.formal(), english.formal());
        assert_eq!(builtin.result_failed(), english.result_failed());
        assert_eq!(builtin.result_not_executed(), english.result_not_executed());
        assert_eq!(builtin.retry_limited(), english.retry_limited());
        assert_eq!(builtin.retry_forever(), english.retry_forever());
        assert_eq!(builtin.retry_none(), english.retry_none());
        assert_eq!(builtin.exhausted(), english.exhausted());
        assert_eq!(builtin.recovered(), english.recovered());
        assert_eq!(builtin.test(), english.test());
        assert_eq!(builtin.unnamed_device(), english.unnamed_device());
        assert!(builtin.id().as_str().starts_with("en/"));
        // What it writes.
        let name = DeviceName::new("").unwrap();
        let text = formal_message(builtin, &name, &due(FirstRecovery::Failed), &TimeZone::UTC);
        assert_eq!(
            text,
            "[WG Always Live] Fault alert\nDevice: (unnamed device)\n\
             Tunnel: *Office* <b>`x`</b> [l](http://e)\nIncident: inc-000007\n\
             Time zone: UTC\nFault started: 2026-01-01 00:00:00\n\
             Recovery result: The first automatic recovery failed (still no traffic after restarting the tunnel)\n\
             WG Always Live will try again every 60 seconds, up to 100 times; you will be notified again once traffic is back and stable for 2 minutes."
        );
        let test = test_message(builtin, &name, NEW_YEAR, &TimeZone::UTC);
        assert!(
            test.starts_with("[WG Always Live] Test notification\n"),
            "{test}"
        );
        assert!(
            test.ends_with("\nTime zone: UTC\nTime: 2026-01-01 00:00:00"),
            "{test}"
        );
    }

    #[test]
    fn the_built_in_set_is_the_contract_fixture() {
        let builtin = builtin_templates();
        let fixture = fixture();
        assert_eq!(builtin.formal(), fixture.formal());
        assert_eq!(builtin.result_failed(), fixture.result_failed());
        assert_eq!(builtin.result_not_executed(), fixture.result_not_executed());
        assert_eq!(builtin.retry_limited(), fixture.retry_limited());
        assert_eq!(builtin.retry_forever(), fixture.retry_forever());
        assert_eq!(builtin.retry_none(), fixture.retry_none());
        assert_eq!(builtin.exhausted(), fixture.exhausted());
        assert_eq!(builtin.recovered(), fixture.recovered());
        assert_eq!(builtin.test(), fixture.test());
        assert_eq!(builtin.unnamed_device(), fixture.unnamed_device());
        assert_eq!(builtin.id(), fixture.id());
        assert_eq!(builtin, &fixture);
    }

    /// The messages as they were written before templates (the code of
    /// `formal_message` and `test_message` until stage 8), kept here as the
    /// expectation for the Traditional Chinese set, the built-in one until
    /// 3.0.0; the alert's last line is the retry text since the owner's
    /// decision of 2026-11-04, and the zone is a line of its own since the
    /// owner's request of 2026-11-05.
    fn legacy_device(name: &DeviceName) -> &str {
        if name.as_str().trim().is_empty() {
            "（未命名裝置）"
        } else {
            name.as_str()
        }
    }

    fn legacy_formal(device_name: &DeviceName, due: &DueNotification, zone: &TimeZone) -> String {
        let result = match due.first_recovery {
            FirstRecovery::NotExecuted => "未能執行自動復原（無法啟停這條隧道）",
            _ => "第一輪自動復原失敗（重新啟動後仍無法通訊）",
        };
        let (time_zone, started_at) = one(due.opened_at_unix_ms, zone);
        format!(
            "【WG Always Live 異常通知】\n\
             裝置：{}\n\
             隧道：{}\n\
             事件：{}\n\
             時區：{time_zone}\n\
             異常開始：{started_at}\n\
             復原結果：{result}\n\
             WG Always Live 會每 60 秒再試一次，最多 100 次；恢復並穩定 2 分鐘後會再通知。",
            legacy_device(device_name),
            due.display_name.as_str(),
            due.incident_id.as_str(),
        )
    }

    fn legacy_test(device_name: &DeviceName, now_unix_ms: i64, zone: &TimeZone) -> String {
        let (time_zone, time) = one(now_unix_ms, zone);
        format!(
            "【WG Always Live 測試通知】\n\
             這是測試訊息，不代表任何 VPN 故障。\n\
             裝置：{}\n\
             時區：{time_zone}\n\
             時間：{time}",
            legacy_device(device_name),
        )
    }

    #[test]
    fn the_traditional_chinese_set_writes_exactly_the_messages_sent_before() {
        let names = [
            DeviceName::new("辦公室筆電").unwrap(),
            DeviceName::new("").unwrap(),
            DeviceName::new("   ").unwrap(),
            DeviceName::new("{device} {{x}} }").unwrap(),
        ];
        let zones = [
            TimeZone::UTC,
            zone("Asia/Taipei"),
            fixed(-(3 * 3600 + 30 * 60)),
        ];
        for name in &names {
            for zone in &zones {
                for first in [
                    FirstRecovery::Failed,
                    FirstRecovery::NotExecuted,
                    FirstRecovery::InProgress,
                ] {
                    let due = due(first);
                    assert_eq!(
                        formal_message(zh_tw(), name, &due, zone),
                        legacy_formal(name, &due, zone)
                    );
                }
                assert_eq!(
                    test_message(zh_tw(), name, NEW_YEAR, zone),
                    legacy_test(name, NEW_YEAR, zone)
                );
            }
        }
        // One written out in full.
        assert_eq!(
            formal_message(
                zh_tw(),
                &names[0],
                &due(FirstRecovery::Failed),
                &zone("Asia/Taipei")
            ),
            "【WG Always Live 異常通知】\n裝置：辦公室筆電\n\
             隧道：*Office* <b>`x`</b> [l](http://e)\n事件：inc-000007\n\
             時區：Asia/Taipei, UTC+08:00\n\
             異常開始：2026-01-01 08:00:00\n\
             復原結果：第一輪自動復原失敗（重新啟動後仍無法通訊）\n\
             WG Always Live 會每 60 秒再試一次，最多 100 次；恢復並穩定 2 分鐘後會再通知。"
        );
    }

    #[test]
    fn the_retry_line_says_what_automatic_recovery_does_next() {
        let t = zh_tw();
        let mut due = due(FirstRecovery::Failed);
        let retry = |limit, interval| wgal_protocol::RecoveryRetry::new(limit, interval).unwrap();
        due.retry = retry(Some(10), 300);
        due.retries_used = 4;
        assert_eq!(
            retry_text(t, &due),
            "WG Always Live 會每 300 秒再試一次，最多 6 次；恢復並穩定 2 分鐘後會再通知。"
        );
        due.retry = retry(None, 10800);
        assert_eq!(
            retry_text(t, &due),
            "WG Always Live 會每 10800 秒持續重試；恢復並穩定 2 分鐘後會再通知。"
        );
        // A limit of 0, or one used up: the alert says no more come (owner
        // decision of 2026-11-04: one message, not two).
        for (limit, used, exhausted) in
            [(Some(0), 0, false), (Some(4), 4, false), (Some(9), 4, true)]
        {
            due.retry = retry(limit, 60);
            due.retries_used = used;
            due.retries_exhausted = exhausted;
            assert_eq!(
                retry_text(t, &due),
                "WG Always Live 不會再自動重試，請手動處理；恢復並穩定 2 分鐘後會再通知。"
            );
        }
    }

    #[test]
    fn the_used_up_and_recovered_messages_hold_their_fields() {
        let name = DeviceName::new("辦公室筆電").unwrap();
        let taipei = zone("Asia/Taipei");
        let mut exhausted = due(FirstRecovery::Failed);
        exhausted.kind = NoticeKind::RetriesExhausted;
        exhausted.retries_used = 100;
        assert_eq!(
            notice_message(zh_tw(), &name, &exhausted, &taipei),
            "【WG Always Live 停止重試】\n裝置：辦公室筆電\n\
             隧道：*Office* <b>`x`</b> [l](http://e)\n事件：inc-000007\n\
             時區：Asia/Taipei, UTC+08:00\n\
             異常開始：2026-01-01 08:00:00\n\
             已自動重試 100 次，達到上限，停止自動復原。請手動處理；恢復並穩定 2 分鐘後會再通知。"
        );
        let mut recovered = due(FirstRecovery::Failed);
        recovered.kind = NoticeKind::Recovered;
        recovered.recovered_at_unix_ms = Some(1_767_226_500_000);
        // The owner's example of 2026-11-05: the zone once, the times bare.
        assert_eq!(
            notice_message(zh_tw(), &name, &recovered, &taipei),
            "【WG Always Live 已恢復】\n裝置：辦公室筆電\n\
             隧道：*Office* <b>`x`</b> [l](http://e)\n事件：inc-000007\n\
             時區：Asia/Taipei, UTC+08:00\n\
             異常開始：2026-01-01 08:00:00\n\
             恢復時間：2026-01-01 08:15:00\n\
             通訊已恢復，並穩定 2 分鐘。"
        );
        for text in [
            notice_message(zh_tw(), &name, &exhausted, &taipei),
            notice_message(zh_tw(), &name, &recovered, &taipei),
        ] {
            assert!(!text.contains("office-1"));
        }
        // Across a daylight saving change the recovery keeps its offset.
        let new_york = zone("America/New_York");
        let autumn = 1_793_512_800_000;
        recovered.opened_at_unix_ms = autumn - 1000;
        recovered.recovered_at_unix_ms = Some(autumn + 600_000);
        let text = notice_message(zh_tw(), &name, &recovered, &new_york);
        assert!(
            text.contains(
                "\n時區：America/New_York, UTC-04:00\n\
                 異常開始：2026-11-01 01:59:59\n\
                 恢復時間：2026-11-01 01:10:00 (UTC-05:00)\n"
            ),
            "{text}"
        );
        // The alert goes through the same door.
        let alert = due(FirstRecovery::Failed);
        assert_eq!(
            notice_message(zh_tw(), &name, &alert, &taipei),
            formal_message(zh_tw(), &name, &alert, &taipei)
        );
    }

    fn template(text: &str) -> MessageTemplate {
        MessageTemplate::new(text).unwrap()
    }

    /// A set in another language that uses every placeholder, each time
    /// followed by its zone.
    fn english() -> NotificationTemplates {
        NotificationTemplates::new(
            TemplatesId::new("en/0001").unwrap(),
            TemplateTexts {
                formal: template(
                    "ALERT {{WGAL}}\n{result} on {tunnel} ({incident}), {device}, since {started_at} ({time_zone}){retry}",
                ),
                result_failed: template("recovery failed"),
                result_not_executed: template("recovery could not be carried out"),
                retry_limited: template("; again every {interval} s, {limit} more"),
                retry_forever: template("; again every {interval} s"),
                retry_none: template("; no more"),
                exhausted: template(
                    "STOPPED {tunnel} {incident} {device} {started_at} ({time_zone}) after {retries}",
                ),
                recovered: template(
                    "BACK {tunnel} {incident} {device} {started_at} at {recovered_at} ({time_zone})",
                ),
                test: template("TEST from {device} at {time} ({time_zone})"),
                unnamed_device: template("(no name)"),
            },
        )
        .unwrap()
    }

    #[test]
    fn another_set_fills_every_placeholder() {
        let name = DeviceName::new("Laptop").unwrap();
        assert_eq!(
            formal_message(
                &english(),
                &name,
                &due(FirstRecovery::Failed),
                &TimeZone::UTC
            ),
            "ALERT {WGAL}\nrecovery failed on *Office* <b>`x`</b> [l](http://e) \
             (inc-000007), Laptop, since 2026-01-01 00:00:00 (UTC); again every 60 s, 100 more"
        );
        assert_eq!(
            test_message(&english(), &name, NEW_YEAR, &zone("Asia/Taipei")),
            "TEST from Laptop at 2026-01-01 08:00:00 (Asia/Taipei, UTC+08:00)"
        );
        let mut recovered = due(FirstRecovery::Failed);
        recovered.recovered_at_unix_ms = Some(NEW_YEAR + 60_000);
        assert_eq!(
            recovered_message(&english(), &name, &recovered, &TimeZone::UTC),
            "BACK *Office* <b>`x`</b> [l](http://e) inc-000007 Laptop \
             2026-01-01 00:00:00 at 2026-01-01 00:01:00 (UTC)"
        );
    }

    #[test]
    fn could_not_be_carried_out_is_never_called_failed() {
        let name = DeviceName::new("Laptop").unwrap();
        let text = formal_message(
            &english(),
            &name,
            &due(FirstRecovery::NotExecuted),
            &TimeZone::UTC,
        );
        assert!(text.starts_with("ALERT {WGAL}\nrecovery could not be carried out on "));
        assert!(!text.contains("recovery failed"));
    }

    #[test]
    fn a_blank_device_name_is_the_sets_unnamed_device() {
        for blank in ["", " ", "\u{3000}"] {
            let name = DeviceName::new(blank).unwrap();
            let text = test_message(&english(), &name, NEW_YEAR, &TimeZone::UTC);
            assert_eq!(text, "TEST from (no name) at 2026-01-01 00:00:00 (UTC)");
            let alert = formal_message(
                &english(),
                &name,
                &due(FirstRecovery::Failed),
                &TimeZone::UTC,
            );
            assert!(alert.contains(", (no name), since "), "{alert}");
        }
    }

    #[test]
    fn values_are_inserted_as_they_are() {
        // A name that looks like a placeholder is not filled in again.
        let name = DeviceName::new("{tunnel}{{time}}{time_zone}").unwrap();
        let mut due = due(FirstRecovery::Failed);
        due.display_name = DisplayName::new("{device} }{").unwrap();
        let text = formal_message(&english(), &name, &due, &TimeZone::UTC);
        assert!(
            text.contains("on {device} }{ (inc-000007), {tunnel}{{time}}{time_zone}, since"),
            "{text}"
        );
        assert_eq!(
            test_message(&english(), &name, NEW_YEAR, &TimeZone::UTC),
            "TEST from {tunnel}{{time}}{time_zone} at 2026-01-01 00:00:00 (UTC)"
        );
    }

    /// The largest messages any valid set can make, with the longest values
    /// the watchdog fills in, fit in one Telegram message.
    #[test]
    fn the_longest_messages_from_any_set_fit() {
        let fill = |used: &str, limit: usize| {
            let text = format!("{used}{}", "\u{10FFFF}".repeat((limit - used.len()) / 4));
            assert!(text.len() > limit - 4 && text.len() <= limit);
            text
        };
        let max = MessageTemplate::MAX_BYTES;
        let formal = fill(
            "{device}{tunnel}{incident}{time_zone}{started_at}{result}{retry}",
            max,
        );
        let test = fill("{device}{time_zone}{time}", max);
        let result = fill("", max);
        let unnamed = fill("", NotificationTemplates::MAX_UNNAMED_DEVICE_BYTES);
        let largest = NotificationTemplates::new(
            TemplatesId::new("x").unwrap(),
            TemplateTexts {
                formal: template(&formal),
                result_failed: template(&result),
                result_not_executed: template(&result),
                retry_limited: template(&fill("{interval}{limit}", max)),
                retry_forever: template(&fill("{interval}", max)),
                retry_none: template(&result),
                exhausted: template(&fill(
                    "{device}{tunnel}{incident}{time_zone}{started_at}{retries}",
                    max,
                )),
                recovered: template(&fill(
                    "{device}{tunnel}{incident}{time_zone}{started_at}{recovered_at}",
                    max,
                )),
                test: template(&test),
                unnamed_device: template(&unnamed),
            },
        )
        .unwrap();

        // The longest names: 64 four-byte characters for the device, the
        // most a tunnel's name holds.
        let name = DeviceName::new("\u{10FFFF}".repeat(DeviceName::MAX_CHARS)).unwrap();
        let mut long = due(FirstRecovery::Failed);
        long.display_name =
            DisplayName::new("\u{10FFFF}".repeat(DisplayName::MAX_BYTES / 4)).unwrap();
        long.incident_id = IncidentId::new("i".repeat(wgal_protocol::MAX_ID_LEN)).unwrap();
        // The widest zone text (see `the_longest_message_fits`).
        let longest = format!("A{}", "b".repeat(TimeZoneName::MAX_BYTES - 1));
        let widest = named(&longest, 25 * 3600 + 59 * 60 + 59);
        let (time_zone, when) = one(long.opened_at_unix_ms, &widest);
        assert!(
            time_zone.len() <= NotificationTemplates::MAX_TIME_ZONE_BYTES,
            "{time_zone}"
        );
        assert!(
            when.len() <= NotificationTemplates::MAX_TIME_BYTES,
            "{when}"
        );
        // The widest time: one written in UTC after a time in the zone, with
        // its own offset.
        let (_, [_, widest_time]) = message_times([NEW_YEAR, i64::MIN], &widest);
        assert!(widest_time.len() <= NotificationTemplates::MAX_TIME_BYTES);

        let blank = DeviceName::new("").unwrap();
        long.retry = wgal_protocol::RecoveryRetry::new(Some(3600), 10800).unwrap();
        long.retries_used = 0;
        long.recovered_at_unix_ms = Some(i64::MIN);
        for first in [FirstRecovery::Failed, FirstRecovery::NotExecuted] {
            long.first_recovery = first;
            for device in [&name, &blank] {
                let text = formal_message(&largest, device, &long, &widest);
                assert!(text.len() <= NotificationTemplates::MAX_FORMAL_BYTES);
                assert!(text.chars().count() <= MAX_MESSAGE_CHARS);
                assert!(text.encode_utf16().count() <= MAX_MESSAGE_CHARS);
            }
        }
        long.retries_used = u32::MAX;
        for device in [&name, &blank] {
            for (text, bound) in [
                (
                    exhausted_message(&largest, device, &long, &widest),
                    NotificationTemplates::MAX_EXHAUSTED_BYTES,
                ),
                (
                    recovered_message(&largest, device, &long, &widest),
                    NotificationTemplates::MAX_RECOVERED_BYTES,
                ),
            ] {
                assert!(text.len() <= bound);
                assert!(text.encode_utf16().count() <= MAX_MESSAGE_CHARS);
            }
        }
        for device in [&name, &blank] {
            let text = test_message(&largest, device, long.opened_at_unix_ms, &widest);
            assert!(text.len() <= NotificationTemplates::MAX_TEST_BYTES);
            assert!(text.encode_utf16().count() <= MAX_MESSAGE_CHARS);
        }
    }
}
