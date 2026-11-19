//! The notification service against the real core, a fake platform, a fake
//! clock and a scripted transport (plan §7, T18–T22). No network, no device.

#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::{self, TimeZone};
use wgal_core::{
    AdapterError, Clock, MemoryStore, ProbeOutcome, Startup, Timing, TunnelBinding, Watchdog,
};
use wgal_protocol::{
    AdapterCapabilities, AdapterErrorKind, BotToken, BoundedVec, Capability, ChatId,
    CredentialState, DeviceName, ErrorCode, ErrorReason, FirstRecovery, IncidentStatus,
    LanguageTag, MessageTemplate, MonitorSettings, NotificationFailure, NotificationState,
    NotificationTemplates, OsState, Outcome, ProbeKind, ProbeTarget, ProbeTiming, RecoveryRetry,
    Request, RequestId, Response, TargetAddress, TemplateTexts, TemplatesId, TestNotificationState,
    TunnelId,
};
use wgal_runtime::notify::{
    Delivery, MemorySecretStore, MemorySettingsStore, Notifier, Rejection, SecretStore,
    SettingsStore, Transport,
};
use wgal_testkit::{FakeClock, FakePlatform};

const T0: i64 = 1_767_225_600_000;
const TOKEN: &str = "123456789:EXAMPLE-not-a-real-token_0000000000";

type Wd = Watchdog<FakePlatform, FakeClock>;
type Service = Notifier<MemorySecretStore, MemorySettingsStore>;

/// Answers with scripted deliveries (then `Accepted`) and records every send.
#[derive(Clone, Debug, Default)]
struct Script {
    answers: Arc<Mutex<VecDeque<Delivery>>>,
    sent: Arc<Mutex<Vec<(String, String)>>>,
    next_id: Arc<Mutex<i64>>,
}

impl Script {
    fn answer(&self, delivery: Delivery) {
        lock(&self.answers).push_back(delivery);
    }

    fn sent(&self) -> Vec<(String, String)> {
        lock(&self.sent).clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Transport for Script {
    fn send_message(&self, token: &BotToken, chat: &ChatId, text: &str) -> Delivery {
        assert_eq!(token.expose_secret(), TOKEN);
        lock(&self.sent).push((chat.as_str().to_owned(), text.to_owned()));
        lock(&self.answers).pop_front().unwrap_or_else(|| {
            let mut id = lock(&self.next_id);
            *id += 1;
            Delivery::Accepted { message_id: *id }
        })
    }
}

fn binding() -> TunnelBinding {
    binding_of("office")
}

fn tunnel() -> TunnelId {
    tunnel_of("office")
}

fn binding_of(name: &str) -> TunnelBinding {
    TunnelBinding::new(name).unwrap()
}

fn tunnel_of(name: &str) -> TunnelId {
    TunnelId::new(name).unwrap()
}

fn rid(id: &str) -> RequestId {
    RequestId::new(id).unwrap()
}

fn run(wd: &mut Wd, secs: u64) {
    for _ in 0..secs {
        wd.clock().advance(Duration::from_secs(1)).unwrap();
        wd.tick();
    }
}

fn ok(wd: &mut Wd, request: Request) {
    match wd.handle(&request) {
        Outcome::Ok(_) => {}
        Outcome::Err(e) => panic!("{request:?}: {e:?}"),
    }
}

/// Managed tunnels, given as (binding, display name), with automatic
/// recovery on, after 35 healthy seconds. Each has its own recovery target:
/// 192.0.2.1, .2, ... in order.
fn managed(store: &MemoryStore, tunnels: &[(&str, &str)]) -> Wd {
    let fake = FakePlatform::new();
    fake.set_capabilities(AdapterCapabilities {
        query_state: Capability::Supported,
        start_stop: Capability::Supported,
        bound_icmp_probe: Capability::Supported,
        bound_tcp_probe: Capability::Supported,
        read_metrics: Capability::Unsupported,
        verified_concurrent_tunnels: 1,
        background_operation: Capability::Unknown,
    });
    for (name, display) in tunnels {
        fake.add_tunnel(binding_of(name), display, OsState::Connected);
    }
    let mut wd = Watchdog::new(
        fake,
        FakeClock::new(T0),
        Timing::default(),
        Box::new(store.clone()),
        Startup::Empty,
    );
    ok(&mut wd, Request::DiscoverTunnels);
    for (target, (name, _)) in (1..).zip(tunnels) {
        ok(
            &mut wd,
            Request::RegisterTunnel {
                tunnel_id: tunnel_of(name),
            },
        );
        ok(
            &mut wd,
            Request::UpdateMonitorSettings {
                tunnel_id: tunnel_of(name),
                settings: MonitorSettings {
                    recovery_target: Some(ProbeTarget {
                        address: TargetAddress::new(Ipv4Addr::new(192, 0, 2, target)).unwrap(),
                        probe: ProbeKind::Icmp,
                    }),
                    observe_targets: BoundedVec::new(),
                    probe: ProbeTiming::DEFAULT,
                },
            },
        );
    }
    run(&mut wd, 35);
    for (name, _) in tunnels {
        ok(
            &mut wd,
            Request::SetAutoRecovery {
                tunnel_id: tunnel_of(name),
                enabled: true,
            },
        );
    }
    wd
}

/// The tunnel's data path dies for good.
fn break_tunnel(wd: &Wd, name: &str) {
    wd.adapter().script_probes(
        &binding_of(name),
        (0..2000).map(|_| Ok(ProbeOutcome::NoReply)),
    );
}

/// Runs a second at a time until `done`, for at most `limit` seconds.
fn run_until(wd: &mut Wd, limit: u64, done: impl Fn(&mut Wd) -> bool) {
    for _ in 0..limit {
        run(wd, 1);
        if done(wd) {
            return;
        }
    }
    panic!("not reached within {limit} s");
}

/// A managed tunnel whose data path then dies for good: by the time this
/// returns, its first recovery round has failed and the alert is due.
fn faulted(store: &MemoryStore) -> Wd {
    let mut wd = managed(store, &[("office", "Office")]);
    break_tunnel(&wd, "office");
    run_until(&mut wd, 600, |wd| !wd.notifications_due().is_empty());
    wd
}

fn service(chat: bool, token: bool) -> (Service, MemorySecretStore, MemorySettingsStore) {
    let settings = MemorySettingsStore::new();
    let mut secrets = MemorySecretStore::new();
    if token {
        secrets.save(&BotToken::new(TOKEN).unwrap()).unwrap();
    }
    let mut notifier = Notifier::new(settings.clone(), secrets.clone());
    {
        // Settings are applied through the request, as the GUI would:
        // notifications on, with or without a chat.
        let store = MemoryStore::new();
        let fake = FakePlatform::new();
        let mut wd = Watchdog::new(
            fake,
            FakeClock::new(T0),
            Timing::default(),
            Box::new(store),
            Startup::Empty,
        );
        let outcome = notifier
            .handle(
                &mut wd,
                &rid("settings-1"),
                &Request::UpdateNotificationSettings {
                    enabled: true,
                    device_name: DeviceName::new("辦公室筆電").unwrap(),
                    chat_id: chat.then(|| ChatId::new("-1001234567890").unwrap()),
                },
            )
            .unwrap();
        assert!(matches!(outcome, Outcome::Ok(Response::Accepted)));
    }
    (notifier, secrets, settings)
}

/// A watchdog with nothing in it, to turn notifications on before a fault,
/// as a running service would have: an alert that is pending when they are
/// turned on from off was pending while they were off and is cancelled.
fn quiet_watchdog() -> Wd {
    Watchdog::new(
        FakePlatform::new(),
        FakeClock::new(T0),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Empty,
    )
}

fn incident(wd: &mut Wd) -> IncidentStatus {
    wd.snapshot().tunnels[0].incident.clone().unwrap()
}

fn incident_of(wd: &mut Wd, name: &str) -> Option<IncidentStatus> {
    wd.snapshot()
        .tunnels
        .iter()
        .find(|t| t.tunnel_id == tunnel_of(name))
        .and_then(|t| t.incident.clone())
}

/// Runs one job if there is one; returns whether one ran.
fn deliver<S: SecretStore, T: SettingsStore>(
    notifier: &mut Notifier<S, T>,
    wd: &mut Wd,
    transport: &Script,
) -> bool {
    let Some(job) = notifier.next_job(wd) else {
        return false;
    };
    let delivery = job.run(transport);
    notifier.finish(wd, job, delivery);
    true
}

#[test]
fn an_alert_is_sent_once_with_the_required_fields() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();

    assert!(deliver(&mut notifier, &mut wd, &transport));
    let sent = transport.sent();
    assert_eq!(sent.len(), 1);
    let (chat, text) = &sent[0];
    assert_eq!(chat, "-1001234567890");
    assert!(text.contains("辦公室筆電"));
    assert!(text.contains("Office"));
    assert!(text.contains(incident(&mut wd).incident_id.as_str()));
    assert!(text.contains("The first automatic recovery failed"));
    assert!(
        !text.contains("192.0.2.1"),
        "no internal address in the alert"
    );
    assert_eq!(incident(&mut wd).notification, NotificationState::Sent);

    // Recovery keeps retrying and failing for an hour: no second alert.
    run(&mut wd, 3600);
    while deliver(&mut notifier, &mut wd, &transport) {}
    assert_eq!(transport.sent().len(), 1);
    assert_eq!(notifier.status().last_failure, None);
}

/// Owner decision of 2026-11-04: with a limited retry count, the alert says
/// how many rounds are left, a message says when they are used up, and once
/// traffic is back and stable a `recovered` message follows; each is sent
/// once, in that order.
#[test]
fn a_used_up_count_and_a_recovery_are_each_sent_once() {
    let store = MemoryStore::new();
    let mut wd = managed(&store, &[("office", "Office")]);
    ok(
        &mut wd,
        Request::SetRecoveryRetry {
            tunnel_id: tunnel_of("office"),
            retry: RecoveryRetry::new(Some(1), 60).unwrap(),
        },
    );
    break_tunnel(&wd, "office");
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    run_until(&mut wd, 600, |wd| !wd.notifications_due().is_empty());
    assert!(deliver(&mut notifier, &mut wd, &transport));
    let alert = transport.sent()[0].1.clone();
    assert!(alert.contains("Fault alert"), "{alert}");
    assert!(
        alert.ends_with("will try again every 60 seconds, up to 1 times; you will be notified again once traffic is back and stable for 2 minutes."),
        "{alert}"
    );

    run_until(&mut wd, 1200, |wd| {
        incident(wd).exhausted_notification == NotificationState::Pending
    });
    while deliver(&mut notifier, &mut wd, &transport) {}
    let sent = transport.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent[1]
            .1
            .starts_with("[WG Always Live] Retries stopped\nDevice: 辦公室筆電\nTunnel: Office\n")
    );
    assert!(
        sent[1]
            .1
            .contains("Retried automatically 1 times and reached the limit")
    );
    assert_eq!(
        incident(&mut wd).exhausted_notification,
        NotificationState::Sent
    );

    // The remote end comes back by itself.
    wd.adapter().clear_probe_script(&binding_of("office"));
    run_until(&mut wd, 900, |wd| {
        wd.snapshot().tunnels[0].incident.is_none()
    });
    while deliver(&mut notifier, &mut wd, &transport) {}
    let sent = transport.sent();
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert!(
        sent[2]
            .1
            .starts_with("[WG Always Live] Recovered\nDevice: 辦公室筆電\nTunnel: Office\n")
    );
    assert!(sent[2].1.contains("\nRecovered at: "));
    assert!(!sent[2].1.contains("192.0.2.1"));

    run(&mut wd, 900);
    while deliver(&mut notifier, &mut wd, &transport) {}
    assert_eq!(transport.sent().len(), 3, "each once");
}

#[test]
fn reserved_is_stored_before_sending_and_a_crash_never_resends() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let job = notifier.next_job(&mut wd).unwrap();
    // The process dies here, before or after the HTTP request went out.
    let saved = store.saved().unwrap();
    assert_eq!(
        saved.tunnels[0].incident.as_ref().unwrap().notification,
        NotificationState::Reserved
    );
    drop(job);

    let fake = FakePlatform::new();
    fake.add_tunnel(binding(), "Office", OsState::Connected);
    let mut restarted = Watchdog::new(
        fake,
        FakeClock::new(T0 + 900_000),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(saved),
    );
    restarted
        .adapter()
        .script_probes(&binding(), (0..2000).map(|_| Ok(ProbeOutcome::NoReply)));
    run(&mut restarted, 600);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    while deliver(&mut notifier, &mut restarted, &transport) {}
    assert!(transport.sent().is_empty(), "resent after a crash");
    assert_eq!(
        restarted.snapshot().tunnels[0]
            .incident
            .as_ref()
            .unwrap()
            .notification,
        NotificationState::Unknown
    );
}

/// A message is chosen (and reserved) first and written after: the text
/// takes the time zone the notifier has when it is written, not the one it
/// had when the message was chosen. Nothing more is chosen while it is in
/// flight; while an alert waits to be sent again, nothing is chosen at all.
#[test]
fn a_chosen_message_is_written_with_the_zone_of_the_moment() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    transport.answer(Delivery::NotSent);

    let pending = notifier.next_message(&mut wd).unwrap();
    assert_eq!(incident(&mut wd).notification, NotificationState::Reserved);
    assert!(notifier.next_message(&mut wd).is_none(), "one in flight");
    notifier.set_time_zone(jiff::tz::db().get("America/New_York").unwrap());
    let job = notifier.write(pending);
    let delivery = job.run(&transport);
    notifier.finish(&mut wd, job, delivery);
    assert!(
        transport.sent()[0].1.contains("America/New_York"),
        "{:?}",
        transport.sent()
    );

    // Waiting 30 s to send it again: nothing is chosen until then.
    run(&mut wd, 29);
    assert!(notifier.next_message(&mut wd).is_none());
    run(&mut wd, 1);
    notifier.set_time_zone(jiff::tz::db().get("Europe/Berlin").unwrap());
    let pending = notifier.next_message(&mut wd).unwrap();
    notifier.set_time_zone(jiff::tz::db().get("Asia/Tokyo").unwrap());
    let job = notifier.write(pending);
    let delivery = job.run(&transport);
    notifier.finish(&mut wd, job, delivery);
    let text = &transport.sent()[1].1;
    assert!(
        text.contains("Asia/Tokyo") && !text.contains("Europe/Berlin"),
        "{text}"
    );
}

#[test]
fn not_sent_and_rate_limited_alerts_wait_and_go_out_once() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    transport.answer(Delivery::NotSent);
    transport.answer(Delivery::RetryAfter { seconds: 7 });

    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(incident(&mut wd).notification, NotificationState::Pending);
    assert_eq!(
        notifier.status().last_failure,
        Some(NotificationFailure::NetworkUnreachable)
    );
    // Waits 30 s before trying again.
    run(&mut wd, 29);
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    run(&mut wd, 1);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(
        notifier.status().last_failure,
        Some(NotificationFailure::RateLimited)
    );
    // Honours retry_after.
    run(&mut wd, 6);
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    run(&mut wd, 1);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(incident(&mut wd).notification, NotificationState::Sent);
    assert_eq!(transport.sent().len(), 3);
    while deliver(&mut notifier, &mut wd, &transport) {}
    assert_eq!(transport.sent().len(), 3);
}

#[test]
fn refused_and_uncertain_alerts_are_final_whatever_changes_later() {
    for (answer, state, failure) in [
        (
            Delivery::Rejected(Rejection::Credentials),
            NotificationState::Rejected,
            NotificationFailure::CredentialsRejected,
        ),
        (
            Delivery::Rejected(Rejection::Request),
            NotificationState::Rejected,
            NotificationFailure::ChatRejected,
        ),
        (
            Delivery::Unknown,
            NotificationState::Unknown,
            NotificationFailure::OutcomeUnknown,
        ),
    ] {
        let store = MemoryStore::new();
        let mut wd = faulted(&store);
        let (mut notifier, _, _) = service(true, true);
        let transport = Script::default();
        transport.answer(answer);
        assert!(deliver(&mut notifier, &mut wd, &transport));
        assert_eq!(incident(&mut wd).notification, state);
        assert_eq!(notifier.status().last_failure, Some(failure));

        // A new token, a new chat, a manual reconnect: still not sent again
        // (plan §7.2).
        notifier.handle(
            &mut wd,
            &rid("token-2"),
            &Request::SetBotToken {
                token: BotToken::new(TOKEN).unwrap(),
            },
        );
        notifier.handle(
            &mut wd,
            &rid("settings-2"),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("x").unwrap(),
                chat_id: Some(ChatId::new("@another_chat").unwrap()),
            },
        );
        let _ = wd.handle(&Request::Reconnect {
            tunnel_id: tunnel(),
        });
        run(&mut wd, 1800);
        while deliver(&mut notifier, &mut wd, &transport) {}
        assert_eq!(transport.sent().len(), 1, "{answer:?} was sent again");
    }
}

#[test]
fn turning_notifications_off_cancels_the_pending_alert_for_good() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let outcome = notifier
        .handle(
            &mut wd,
            &rid("off"),
            &Request::UpdateNotificationSettings {
                enabled: false,
                device_name: DeviceName::new("x").unwrap(),
                chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            },
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(_)));
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
    notifier.handle(
        &mut wd,
        &rid("on"),
        &Request::UpdateNotificationSettings {
            enabled: true,
            device_name: DeviceName::new("x").unwrap(),
            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
        },
    );
    let transport = Script::default();
    run(&mut wd, 600);
    while deliver(&mut notifier, &mut wd, &transport) {}
    assert!(transport.sent().is_empty());
}

#[test]
fn nothing_is_reserved_without_a_readable_token() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets, _) = service(true, true);
    secrets.set_failing(true);
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(incident(&mut wd).notification, NotificationState::Pending);
    assert_eq!(notifier.status().token, CredentialState::Unavailable);
    assert_eq!(
        notifier.status().last_failure,
        Some(NotificationFailure::CredentialUnavailable)
    );
    // Not asked again right away (plan §7.1: no repeated prompts) ...
    secrets.set_failing(false);
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    // ... but after the wait, and then the alert goes out, once.
    run(&mut wd, 301);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(transport.sent().len(), 1);
    assert_eq!(notifier.status().token, CredentialState::Stored);
}

#[test]
fn nothing_is_sent_when_the_reservation_cannot_be_stored() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    store.set_failing(true);
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert!(transport.sent().is_empty());
}

#[test]
fn nothing_is_sent_without_settings() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let transport = Script::default();
    for (chat, token) in [(false, true), (true, false)] {
        let (mut notifier, _, _) = service(chat, token);
        assert!(!deliver(&mut notifier, &mut wd, &transport));
    }
    assert!(transport.sent().is_empty());
    assert_eq!(incident(&mut wd).notification, NotificationState::Pending);
}

#[test]
fn test_notifications_are_separate_and_deduplicated() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    // Notifications switched off: tests still work, alerts do not.
    notifier.handle(
        &mut wd,
        &rid("off"),
        &Request::UpdateNotificationSettings {
            enabled: false,
            device_name: DeviceName::new("辦公室筆電").unwrap(),
            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
        },
    );
    let before = incident(&mut wd).notification;
    let transport = Script::default();

    let first = notifier
        .handle(&mut wd, &rid("test-1"), &Request::SendTestNotification)
        .unwrap();
    assert!(matches!(first, Outcome::Ok(_)));
    assert_eq!(
        notifier.status().last_test.unwrap().state,
        TestNotificationState::Sending
    );
    // A second click while the first is out is refused.
    let busy = notifier
        .handle(&mut wd, &rid("test-2"), &Request::SendTestNotification)
        .unwrap();
    assert!(matches!(
        busy,
        Outcome::Err(e) if e.code == ErrorCode::Busy
            && e.reason == Some(ErrorReason::TestNotificationInProgress)
    ));
    assert!(deliver(&mut notifier, &mut wd, &transport));
    let test = notifier.status().last_test.unwrap();
    assert_eq!(test.request_id, rid("test-1"));
    assert_eq!(test.state, TestNotificationState::Accepted);
    assert!(test.finished_at_unix_ms.is_some());
    assert!(transport.sent()[0].1.contains("Test notification"));

    // The same request again (an IPC retry) sends nothing.
    notifier.handle(&mut wd, &rid("test-1"), &Request::SendTestNotification);
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    // A new click sends a new test.
    notifier.handle(&mut wd, &rid("test-3"), &Request::SendTestNotification);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(transport.sent().len(), 2);
    // No incident was touched.
    assert_eq!(incident(&mut wd).notification, before);
}

#[test]
fn a_test_without_settings_fails_visibly() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(false, true);
    notifier.handle(&mut wd, &rid("test-1"), &Request::SendTestNotification);
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    let test = notifier.status().last_test.unwrap();
    assert_eq!(test.state, TestNotificationState::Failed);
    assert_eq!(test.failure, Some(NotificationFailure::NotConfigured));
    assert!(transport.sent().is_empty());
}

#[test]
fn the_token_never_shows() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    let job = notifier.next_job(&mut wd).unwrap();
    let secret = TOKEN.split_once(':').unwrap().1;
    assert!(!format!("{job:?}").contains(secret));
    assert!(!format!("{notifier:?}").contains(secret));
    assert!(
        !serde_json::to_string(&notifier.status())
            .unwrap()
            .contains(secret)
    );
    assert!(!format!("{:?}", settings.saved()).contains(secret));
    let mut snapshot = wd.handle(&Request::GetSnapshot);
    notifier.decorate(&mut snapshot);
    assert!(!format!("{snapshot:?}").contains(secret));
    assert!(matches!(
        &snapshot,
        Outcome::Ok(Response::Snapshot { snapshot }) if snapshot.notification.token == CredentialState::Stored
    ));
}

#[test]
fn clearing_the_token_is_reported() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets, _) = service(true, true);
    notifier.handle(&mut wd, &rid("clear"), &Request::ClearBotToken);
    assert_eq!(notifier.status().token, CredentialState::NotSet);
    assert!(secrets.stored().is_none());
    secrets.set_failing(true);
    let outcome = notifier
        .handle(
            &mut wd,
            &rid("set"),
            &Request::SetBotToken {
                token: BotToken::new(TOKEN).unwrap(),
            },
        )
        .unwrap();
    assert!(matches!(
        outcome,
        Outcome::Err(e) if e.code == ErrorCode::StorageUnavailable
            && e.reason == Some(ErrorReason::SecretStoreUnavailable)
    ));
    assert_eq!(notifier.status().token, CredentialState::Unavailable);
}

#[test]
fn other_requests_go_to_the_core() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    assert!(
        notifier
            .handle(&mut wd, &rid("snap"), &Request::GetSnapshot)
            .is_none()
    );
}

/// A fresh directory under the system temp directory, removed when dropped.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("wgal-notify-{}-{name}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn settings_survive_a_restart_and_never_hold_the_token() {
    use wgal_runtime::notify::{FileSettingsStore, SETTINGS_FILE, SettingsStore};
    let dir = TempDir::new("settings");
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let mut notifier = Notifier::new(FileSettingsStore::new(&dir.0), MemorySecretStore::new());
    for request in [
        Request::UpdateNotificationSettings {
            enabled: true,
            device_name: DeviceName::new("辦公室筆電").unwrap(),
            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
        },
        Request::SetBotToken {
            token: BotToken::new(TOKEN).unwrap(),
        },
    ] {
        assert!(matches!(
            notifier.handle(&mut wd, &rid("s"), &request),
            Some(Outcome::Ok(_))
        ));
    }
    let text = std::fs::read_to_string(dir.0.join(SETTINGS_FILE)).unwrap();
    assert!(text.contains("-1001234567890"), "chat id kept as a string");
    assert!(!text.contains(TOKEN.split_once(':').unwrap().1));
    let loaded = FileSettingsStore::new(&dir.0).load().unwrap();
    assert!(loaded.enabled);
    assert_eq!(loaded.chat_id, Some(ChatId::new("-1001234567890").unwrap()));

    // A damaged file leaves notifications off rather than guessing.
    std::fs::write(dir.0.join(SETTINGS_FILE), b"{ not json").unwrap();
    let restarted = Notifier::new(FileSettingsStore::new(&dir.0), MemorySecretStore::new());
    assert!(!restarted.status().enabled);
}

#[cfg(target_os = "linux")]
mod linux_secret_file {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use wgal_runtime::notify::{FileSecretStore, SecretStore};

    #[test]
    fn the_token_file_is_private_and_round_trips() {
        let dir = TempDir::new("secret");
        let secrets = dir.0.join("secrets");
        let mut store = FileSecretStore::new(&secrets);
        assert_eq!(store.load().unwrap(), None);
        store.save(&BotToken::new(TOKEN).unwrap()).unwrap();
        let dir_mode = std::fs::metadata(&secrets).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        let file = secrets.join("telegram-bot-token");
        let file_mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(store.load().unwrap().unwrap().expose_secret(), TOKEN);
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn loose_permissions_and_symlinks_are_refused() {
        let dir = TempDir::new("secret-loose");
        let secrets = dir.0.join("secrets");
        let mut store = FileSecretStore::new(&secrets);
        store.save(&BotToken::new(TOKEN).unwrap()).unwrap();
        let file = secrets.join("telegram-bot-token");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            store.load().is_err(),
            "a group/other-readable token file was used"
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            store.load().is_err(),
            "a group/other-readable directory was used"
        );
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700)).unwrap();

        let other = dir.0.join("elsewhere");
        std::fs::write(&other, TOKEN).unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&other, &file).unwrap();
        assert!(store.load().is_err(), "a symlinked token file was followed");
    }
}

#[cfg(windows)]
mod windows_secret_file {
    use super::*;
    use wgal_runtime::notify::{DpapiSecretStore, SecretStore};

    #[test]
    fn the_token_is_stored_protected_and_round_trips() {
        let dir = TempDir::new("dpapi");
        let secrets = dir.0.join("secrets");
        let mut store = DpapiSecretStore::new(&secrets);
        assert_eq!(store.load().unwrap(), None);
        store.save(&BotToken::new(TOKEN).unwrap()).unwrap();
        let bytes = std::fs::read(secrets.join("telegram-bot-token.dpapi")).unwrap();
        let secret = TOKEN.split_once(':').unwrap().1.as_bytes();
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret),
            "the token is stored in plain text"
        );
        assert_eq!(store.load().unwrap().unwrap().expose_secret(), TOKEN);
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn a_tampered_or_foreign_file_is_unavailable_not_a_token() {
        let dir = TempDir::new("dpapi-bad");
        let secrets = dir.0.join("secrets");
        std::fs::create_dir(&secrets).unwrap();
        std::fs::write(secrets.join("telegram-bot-token.dpapi"), TOKEN).unwrap();
        assert!(DpapiSecretStore::new(&secrets).load().is_err());
    }
}

/// F/G review P3-3: an alert that becomes due while notifications are off is
/// cancelled, never sent when they are turned on later.
#[test]
fn an_alert_due_while_notifications_are_off_is_cancelled() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    // Notifications were never turned on: the alert became due while off.
    let secrets = MemorySecretStore::new();
    let mut notifier = Notifier::new(MemorySettingsStore::new(), secrets);
    assert!(!notifier.status().enabled);
    assert!(notifier.next_job(&mut wd).is_none());
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
}

/// Counts reads of a token store that cannot be read.
#[derive(Debug, Default)]
struct LockedStore {
    reads: Arc<Mutex<u32>>,
}

impl SecretStore for LockedStore {
    fn load(&self) -> Result<Option<BotToken>, wgal_runtime::notify::SecretError> {
        *lock(&self.reads) += 1;
        Err(wgal_runtime::notify::SecretError)
    }
    fn save(&mut self, _: &BotToken) -> Result<(), wgal_runtime::notify::SecretError> {
        Err(wgal_runtime::notify::SecretError)
    }
    fn clear(&mut self) -> Result<(), wgal_runtime::notify::SecretError> {
        Err(wgal_runtime::notify::SecretError)
    }
}

/// F/G review P2-2: a token store that cannot be read (a locked Keychain) is
/// not asked again on every pass, only after a wait.
#[test]
fn a_locked_token_store_is_not_asked_over_and_over() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let settings = MemorySettingsStore::new();
    let locked = LockedStore::default();
    let reads = Arc::clone(&locked.reads);
    let mut notifier = Notifier::new(settings, locked);
    notifier.handle(
        &mut quiet_watchdog(),
        &rid("settings"),
        &Request::UpdateNotificationSettings {
            enabled: true,
            device_name: DeviceName::new("x").unwrap(),
            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
        },
    );
    let before = *lock(&reads);
    for _ in 0..100 {
        assert!(notifier.next_job(&mut wd).is_none());
    }
    assert!(
        *lock(&reads) - before <= 1,
        "read {} times",
        *lock(&reads) - before
    );
    assert_eq!(notifier.status().token, CredentialState::Unavailable);
    // After the wait it is tried again.
    run(&mut wd, 301);
    let _ = notifier.next_job(&mut wd);
    assert_eq!(*lock(&reads) - before, 2);
}

/// F/G review P2-2: a store that finds nothing although a token was set
/// (a Keychain item it may not read without asking) is unavailable, not
/// "not set".
#[test]
fn a_token_that_was_set_but_cannot_be_found_is_unavailable() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets, settings) = service(true, true);
    notifier.handle(
        &mut wd,
        &rid("token"),
        &Request::SetBotToken {
            token: BotToken::new(TOKEN).unwrap(),
        },
    );
    assert!(settings.saved().unwrap().token_set);
    secrets.clone().clear().unwrap();
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(notifier.status().token, CredentialState::Unavailable);
    assert_eq!(
        notifier.status().last_failure,
        Some(NotificationFailure::CredentialUnavailable)
    );
    // A restarted service knows it too.
    let restarted = Notifier::new(settings.clone(), secrets.clone());
    assert_eq!(restarted.status().token, CredentialState::Unavailable);
}

// Message times in the configured zone (owner request 9 of 2026-11-03).

/// `unix_ms` as `YYYY-MM-DD HH:MM:SS` local time in `zone`, worked out with
/// jiff directly rather than by the code under test.
fn wall_in(unix_ms: i64, zone: &TimeZone) -> String {
    Timestamp::from_millisecond(unix_ms)
        .unwrap()
        .to_zoned(zone.clone())
        .strftime("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Asks for a test message and sends it; returns its text and the time it
/// was asked for.
fn test_text(notifier: &mut Service, wd: &mut Wd, transport: &Script, id: &str) -> (String, i64) {
    let requested = wd.clock().wall_unix_ms();
    let outcome = notifier
        .handle(wd, &rid(id), &Request::SendTestNotification)
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(_)));
    assert!(deliver(notifier, wd, transport));
    (transport.sent().last().unwrap().1.clone(), requested)
}

#[test]
fn message_times_are_utc_until_a_zone_is_set() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    let opened = incident(&mut wd).opened_at_unix_ms;
    assert!(deliver(&mut notifier, &mut wd, &transport));
    let alert = &transport.sent()[0].1;
    let expected = format!(
        "\nTime zone: UTC\nFault started: {}\n",
        wall_in(opened, &TimeZone::UTC)
    );
    assert!(alert.contains(&expected), "{alert}");

    run(&mut wd, 5);
    let (text, at) = test_text(&mut notifier, &mut wd, &transport, "test-1");
    assert!(text.ends_with(&format!(
        "\nTime zone: UTC\nTime: {}",
        wall_in(at, &TimeZone::UTC)
    )));
}

#[test]
fn message_times_are_written_in_the_configured_zone() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    let taipei = TimeZone::get("Asia/Taipei").unwrap();
    notifier.set_time_zone(taipei.clone());
    let opened = incident(&mut wd).opened_at_unix_ms;
    assert!(deliver(&mut notifier, &mut wd, &transport));
    let alert = &transport.sent()[0].1;
    // The zone once, on its own line (owner request of 2026-11-05), and
    // the time without it.
    let expected = format!(
        "\nTime zone: Asia/Taipei, UTC+08:00\nFault started: {}\n",
        wall_in(opened, &taipei)
    );
    assert!(alert.contains(&expected), "{alert}");
    // No time carries its own offset (the English texts have other
    // parentheses).
    assert!(!alert.contains("(UTC"), "{alert}");

    // The zone can change at any time; the next message uses the new one,
    // with the offset in force then (2026-01-01 in New York: standard time).
    let new_york = TimeZone::get("America/New_York").unwrap();
    notifier.set_time_zone(new_york.clone());
    run(&mut wd, 5);
    let (text, at) = test_text(&mut notifier, &mut wd, &transport, "test-1");
    assert!(text.ends_with(&format!(
        "\nTime zone: America/New_York, UTC-05:00\nTime: {}",
        wall_in(at, &new_york)
    )));

    // A zone with no name shows only its offset.
    let fixed = TimeZone::fixed(tz::offset(9));
    notifier.set_time_zone(fixed.clone());
    run(&mut wd, 5);
    let (text, at) = test_text(&mut notifier, &mut wd, &transport, "test-2");
    assert!(text.ends_with(&format!(
        "\nTime zone: UTC+09:00\nTime: {}",
        wall_in(at, &fixed)
    )));

    // Back to UTC.
    notifier.set_time_zone(TimeZone::UTC);
    run(&mut wd, 5);
    let (text, at) = test_text(&mut notifier, &mut wd, &transport, "test-3");
    assert!(text.ends_with(&format!(
        "\nTime zone: UTC\nTime: {}",
        wall_in(at, &TimeZone::UTC)
    )));
}

// The per-tunnel notification switch (owner request 4 of 2026-11-03). The
// core decides eligibility; these check that no path through the service
// still sends for a tunnel whose switch is off.

fn set_tunnel_notifications(wd: &mut Wd, name: &str, enabled: bool) {
    ok(
        wd,
        Request::SetTunnelNotifications {
            tunnel_id: tunnel_of(name),
            enabled,
        },
    );
}

/// Runs for `secs`, sending whatever the service hands out each second, as
/// the loop would.
fn run_delivering(wd: &mut Wd, notifier: &mut Service, transport: &Script, secs: u64) {
    for _ in 0..secs {
        run(wd, 1);
        while deliver(notifier, wd, transport) {}
    }
}

#[test]
fn a_tunnel_switched_off_never_sends_its_pending_alert() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    set_tunnel_notifications(&mut wd, "office", false);
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
    assert!(!wd.snapshot().tunnels[0].notifications_enabled);
    run_delivering(&mut wd, &mut notifier, &transport, 1800);

    // On again: a cancelled alert is never re-armed (plan §7.3), and the
    // fault going on is the same incident.
    set_tunnel_notifications(&mut wd, "office", true);
    run_delivering(&mut wd, &mut notifier, &transport, 1800);
    assert!(transport.sent().is_empty());
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
    assert_eq!(notifier.status().last_failure, None);
}

#[test]
fn a_retry_waiting_when_a_tunnel_is_switched_off_is_dropped() {
    for answer in [Delivery::NotSent, Delivery::RetryAfter { seconds: 7 }] {
        let store = MemoryStore::new();
        let mut wd = faulted(&store);
        let (mut notifier, _, _) = service(true, true);
        let transport = Script::default();
        transport.answer(answer);
        assert!(deliver(&mut notifier, &mut wd, &transport));
        assert_eq!(incident(&mut wd).notification, NotificationState::Pending);

        // The service now waits to try again; the switch goes off meanwhile.
        set_tunnel_notifications(&mut wd, "office", false);
        assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
        run_delivering(&mut wd, &mut notifier, &transport, 60);
        set_tunnel_notifications(&mut wd, "office", true);
        run_delivering(&mut wd, &mut notifier, &transport, 1800);
        assert_eq!(transport.sent().len(), 1, "{answer:?} was tried again");
        assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
    }
}

/// How notifications are turned off and on again in a test.
#[derive(Clone, Copy, Debug)]
enum Switch {
    /// The global switch (`UpdateNotificationSettings`).
    Global,
    /// The tunnel's own (`SetTunnelNotifications`).
    Tunnel,
}

impl Switch {
    fn set(self, notifier: &mut Service, wd: &mut Wd, enabled: bool) {
        match self {
            Switch::Global => {
                let outcome = notifier
                    .handle(
                        wd,
                        &rid(if enabled { "global-on" } else { "global-off" }),
                        &Request::UpdateNotificationSettings {
                            enabled,
                            device_name: DeviceName::new("辦公室筆電").unwrap(),
                            chat_id: Some(ChatId::new("-1001234567890").unwrap()),
                        },
                    )
                    .unwrap();
                assert!(matches!(outcome, Outcome::Ok(_)));
            }
            Switch::Tunnel => set_tunnel_notifications(wd, "office", enabled),
        }
    }
}

/// A send already reserved and formatted when the tunnel's switch goes off
/// ends exactly as when the global switch goes off: the request may already
/// be out, so its result is recorded as it came; "try later" becomes
/// cancelled; nothing is sent again, even with the switch back on.
#[test]
fn a_send_in_flight_when_a_tunnel_is_switched_off_ends_as_with_the_global_switch() {
    for (answer, state) in [
        (
            Delivery::Accepted { message_id: 41 },
            NotificationState::Sent,
        ),
        (Delivery::NotSent, NotificationState::Cancelled),
        (
            Delivery::RetryAfter { seconds: 7 },
            NotificationState::Cancelled,
        ),
        (
            Delivery::Rejected(Rejection::Request),
            NotificationState::Rejected,
        ),
        (Delivery::Unknown, NotificationState::Unknown),
    ] {
        for switch in [Switch::Global, Switch::Tunnel] {
            let store = MemoryStore::new();
            let mut wd = faulted(&store);
            let (mut notifier, _, _) = service(true, true);
            let transport = Script::default();
            transport.answer(answer);
            let job = notifier.next_job(&mut wd).unwrap();
            assert_eq!(incident(&mut wd).notification, NotificationState::Reserved);

            switch.set(&mut notifier, &mut wd, false);
            assert_eq!(incident(&mut wd).notification, NotificationState::Reserved);
            let delivery = job.run(&transport);
            notifier.finish(&mut wd, job, delivery);
            assert_eq!(
                incident(&mut wd).notification,
                state,
                "{switch:?}, {answer:?}"
            );

            switch.set(&mut notifier, &mut wd, true);
            run_delivering(&mut wd, &mut notifier, &transport, 1800);
            assert_eq!(transport.sent().len(), 1, "{switch:?}, {answer:?}");
            assert_eq!(incident(&mut wd).notification, state);
            let saved = store.saved().unwrap();
            assert_eq!(
                saved.tunnels[0].incident.as_ref().unwrap().notification,
                state,
                "{switch:?}, {answer:?}: not what a restart would see"
            );
        }
    }
}

#[test]
fn one_tunnel_switched_off_leaves_the_others_alerts_alone() {
    let store = MemoryStore::new();
    // A third, healthy tunnel, so the two faults are never all there is.
    let mut wd = managed(
        &store,
        &[("office", "Office"), ("home", "Home"), ("lab", "Lab")],
    );
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    set_tunnel_notifications(&mut wd, "office", false);
    break_tunnel(&wd, "office");
    break_tunnel(&wd, "home");
    run_delivering(&mut wd, &mut notifier, &transport, 1800);

    let sent = transport.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1.contains("Tunnel: Home\n"));
    let home = incident_of(&mut wd, "home").unwrap();
    assert_eq!(home.notification, NotificationState::Sent);
    // The office tunnel's first recovery failed too: only its switch kept
    // the alert from going out.
    let office = incident_of(&mut wd, "office").unwrap();
    assert_eq!(office.first_recovery, FirstRecovery::Failed);
    assert!(
        matches!(
            office.notification,
            NotificationState::Pending | NotificationState::Cancelled
        ),
        "{:?}",
        office.notification
    );
    assert!(incident_of(&mut wd, "lab").is_none());
}

/// An incident that becomes eligible while its tunnel's switch is already
/// off is cancelled then, like one that is pending when the switch goes off,
/// so turning the switch on later never sends a stale alert. The core's
/// step 1 leaves it `pending` (not due while off), and it is sent once the
/// switch is on again; the core's stage 2 closes that.
#[test]
fn an_alert_due_while_its_tunnel_is_switched_off_is_never_sent_later() {
    let store = MemoryStore::new();
    let mut wd = managed(&store, &[("office", "Office")]);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    set_tunnel_notifications(&mut wd, "office", false);
    break_tunnel(&wd, "office");
    run_until(&mut wd, 600, |wd| {
        incident_of(wd, "office").is_some_and(|i| i.first_recovery == FirstRecovery::Failed)
    });
    while deliver(&mut notifier, &mut wd, &transport) {}
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);

    set_tunnel_notifications(&mut wd, "office", true);
    run_delivering(&mut wd, &mut notifier, &transport, 1800);
    assert!(transport.sent().is_empty());
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
}

// Message templates handed over by the GUI (wave three, stage 8).

fn template(text: &str) -> MessageTemplate {
    MessageTemplate::new(text).unwrap()
}

/// A set in another language that uses every placeholder, each time
/// followed by its zone.
fn english(id: &str) -> NotificationTemplates {
    NotificationTemplates::new(
        TemplatesId::new(id).unwrap(),
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

fn set_templates<S: SecretStore, T: SettingsStore>(
    notifier: &mut Notifier<S, T>,
    wd: &mut Wd,
    templates: NotificationTemplates,
) -> Outcome {
    notifier
        .handle(
            wd,
            &rid("templates"),
            &Request::SetNotificationTemplates { templates },
        )
        .unwrap()
}

fn refusal(outcome: &Outcome) -> Option<(ErrorCode, Option<ErrorReason>)> {
    match outcome {
        Outcome::Err(e) => Some((e.code, e.reason)),
        Outcome::Ok(_) => None,
    }
}

fn templates_id<S: SecretStore, T: SettingsStore>(notifier: &Notifier<S, T>) -> Option<String> {
    notifier
        .status()
        .templates_id
        .map(|id| id.as_str().to_owned())
}

/// `unix_ms` as the English set writes it in UTC: the time, then the zone.
fn utc_time(unix_ms: i64) -> String {
    format!("{} (UTC)", wall_in(unix_ms, &TimeZone::UTC))
}

#[test]
fn the_built_in_set_is_used_until_a_gui_sends_one() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    assert_eq!(templates_id(&notifier), None);
    assert_eq!(settings.saved().unwrap().templates, None);
    let transport = Script::default();
    assert!(deliver(&mut notifier, &mut wd, &transport));
    let alert = &transport.sent()[0].1;
    assert!(alert.starts_with("[WG Always Live] Fault alert\nDevice: 辦公室筆電\n"));
    let mut snapshot = wd.handle(&Request::GetSnapshot);
    notifier.decorate(&mut snapshot);
    let Outcome::Ok(Response::Snapshot { snapshot }) = snapshot else {
        panic!("no snapshot");
    };
    assert_eq!(snapshot.notification.templates_id, None);
}

#[test]
fn a_set_fills_every_placeholder_in_alerts_and_tests() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    assert!(matches!(
        set_templates(&mut notifier, &mut wd, english("en/0001")),
        Outcome::Ok(Response::Accepted)
    ));
    assert_eq!(templates_id(&notifier).as_deref(), Some("en/0001"));
    assert_eq!(
        settings.saved().unwrap().templates,
        Some(english("en/0001"))
    );
    let mut snapshot = wd.handle(&Request::GetSnapshot);
    notifier.decorate(&mut snapshot);
    let Outcome::Ok(Response::Snapshot { snapshot }) = snapshot else {
        panic!("no snapshot");
    };
    assert_eq!(
        snapshot.notification.templates_id.unwrap().as_str(),
        "en/0001"
    );

    let transport = Script::default();
    let incident = incident(&mut wd);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(
        transport.sent()[0].1,
        format!(
            "ALERT {{WGAL}}\nrecovery failed on Office ({}), 辦公室筆電, since {}; again every 60 s, 100 more",
            incident.incident_id.as_str(),
            utc_time(incident.opened_at_unix_ms)
        )
    );
    run(&mut wd, 5);
    let (text, at) = test_text(&mut notifier, &mut wd, &transport, "test-1");
    assert_eq!(text, format!("TEST from 辦公室筆電 at {}", utc_time(at)));
}

#[test]
fn a_recovery_that_could_not_be_carried_out_and_a_blank_name_use_their_texts() {
    let store = MemoryStore::new();
    let mut wd = managed(&store, &[("office", "Office")]);
    // The configuration became unsafe: the recovery round is refused before
    // any stop or start, so it is not carried out.
    wd.adapter().set_registration(
        &binding(),
        Err(AdapterError::new(
            AdapterErrorKind::PermissionDenied,
            "writable by others",
        )),
    );
    break_tunnel(&wd, "office");
    run_until(&mut wd, 600, |wd| !wd.notifications_due().is_empty());
    let incident = incident(&mut wd);
    assert_eq!(incident.first_recovery, FirstRecovery::NotExecuted);

    let (mut notifier, _, _) = service(true, true);
    for (id, name) in [("blank", ""), ("spaces", "   ")] {
        let outcome = notifier
            .handle(
                &mut wd,
                &rid(id),
                &Request::UpdateNotificationSettings {
                    enabled: true,
                    device_name: DeviceName::new(name).unwrap(),
                    chat_id: Some(ChatId::new("-1001234567890").unwrap()),
                },
            )
            .unwrap();
        assert!(matches!(outcome, Outcome::Ok(_)));
    }
    // The settings kept the set: it survives a change of the device name.
    set_templates(&mut notifier, &mut wd, english("en/0001"));
    let outcome = notifier
        .handle(
            &mut wd,
            &rid("rename"),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("").unwrap(),
                chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            },
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(_)));
    assert_eq!(templates_id(&notifier).as_deref(), Some("en/0001"));

    let transport = Script::default();
    assert!(deliver(&mut notifier, &mut wd, &transport));
    let alert = &transport.sent()[0].1;
    assert_eq!(
        alert,
        &format!(
            "ALERT {{WGAL}}\nrecovery could not be carried out on Office ({}), (no name), since {}; again every 60 s, 100 more",
            incident.incident_id.as_str(),
            utc_time(incident.opened_at_unix_ms)
        )
    );
    assert!(!alert.contains("recovery failed"));
    let (text, at) = test_text(&mut notifier, &mut wd, &transport, "test-1");
    assert_eq!(text, format!("TEST from (no name) at {}", utc_time(at)));
}

#[test]
fn a_new_set_applies_from_the_next_message_and_never_resends() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();

    // An alert already handed out keeps the text it was written with.
    let job = notifier.next_job(&mut wd).unwrap();
    assert_eq!(incident(&mut wd).notification, NotificationState::Reserved);
    set_templates(&mut notifier, &mut wd, english("en/0001"));
    assert_eq!(incident(&mut wd).notification, NotificationState::Reserved);
    let delivery = job.run(&transport);
    notifier.finish(&mut wd, job, delivery);
    assert!(
        transport.sent()[0]
            .1
            .starts_with("[WG Always Live] Fault alert")
    );
    assert_eq!(incident(&mut wd).notification, NotificationState::Sent);

    // Another set later: nothing is sent again, the incident stays as it is.
    set_templates(&mut notifier, &mut wd, english("en/0002"));
    run(&mut wd, 3600);
    while deliver(&mut notifier, &mut wd, &transport) {}
    assert_eq!(transport.sent().len(), 1);
    assert_eq!(incident(&mut wd).notification, NotificationState::Sent);
    // The next message is written with the set in use now.
    let (text, _) = test_text(&mut notifier, &mut wd, &transport, "test-1");
    assert!(text.starts_with("TEST from 辦公室筆電 at "), "{text}");
}

#[test]
fn a_pending_alert_is_written_with_the_set_in_use_when_it_goes_out() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, _) = service(true, true);
    let transport = Script::default();
    transport.answer(Delivery::NotSent);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(incident(&mut wd).notification, NotificationState::Pending);
    assert!(
        transport.sent()[0]
            .1
            .starts_with("[WG Always Live] Fault alert")
    );

    set_templates(&mut notifier, &mut wd, english("en/0001"));
    assert_eq!(incident(&mut wd).notification, NotificationState::Pending);
    // Setting templates sends nothing by itself.
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(transport.sent().len(), 1);
    run(&mut wd, 30);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert!(
        transport.sent()[1]
            .1
            .starts_with("ALERT {WGAL}\nrecovery failed on Office")
    );
    assert_eq!(incident(&mut wd).notification, NotificationState::Sent);
}

#[test]
fn a_set_that_cannot_be_stored_is_refused_and_the_old_one_stays() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    set_templates(&mut notifier, &mut wd, english("en/0001"));
    settings.set_failing(true);
    let refused = set_templates(&mut notifier, &mut wd, english("en/0002"));
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    // The same set as in use needs no save.
    assert!(matches!(
        set_templates(&mut notifier, &mut wd, english("en/0001")),
        Outcome::Ok(_)
    ));
    settings.set_failing(false);
    assert_eq!(templates_id(&notifier).as_deref(), Some("en/0001"));
    assert_eq!(
        settings.saved().unwrap().templates,
        Some(english("en/0001"))
    );
    let transport = Script::default();
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert!(transport.sent()[0].1.starts_with("ALERT {WGAL}\n"));
}

#[test]
fn a_set_is_not_written_over_settings_that_could_not_be_read() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let settings = MemorySettingsStore::new();
    settings.set_failing(true);
    let mut notifier = Notifier::new(settings.clone(), MemorySecretStore::new());
    settings.set_failing(false);
    let refused = set_templates(&mut notifier, &mut wd, english("en/0001"));
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageLoadFailed)
        ))
    );
    assert_eq!(settings.saved(), None, "nothing was written");
    assert_eq!(templates_id(&notifier), None);
}

#[test]
fn a_set_is_kept_across_a_restart() {
    use wgal_runtime::notify::{FileSettingsStore, SETTINGS_FILE};
    let dir = TempDir::new("templates");
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let mut secrets = MemorySecretStore::new();
    secrets.save(&BotToken::new(TOKEN).unwrap()).unwrap();
    let mut notifier = Notifier::new(FileSettingsStore::new(&dir.0), secrets.clone());
    let outcome = notifier
        .handle(
            &mut quiet_watchdog(),
            &rid("settings"),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("辦公室筆電").unwrap(),
                chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            },
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(_)));
    set_templates(&mut notifier, &mut wd, english("en/0001"));
    let text = std::fs::read_to_string(dir.0.join(SETTINGS_FILE)).unwrap();
    assert!(text.contains("en/0001"), "{text}");
    drop(notifier);

    let mut restarted = Notifier::new(FileSettingsStore::new(&dir.0), secrets);
    assert_eq!(templates_id(&restarted).as_deref(), Some("en/0001"));
    assert!(restarted.status().enabled);
    let transport = Script::default();
    assert!(deliver(&mut restarted, &mut wd, &transport));
    assert!(
        transport.sent()[0]
            .1
            .starts_with("ALERT {WGAL}\nrecovery failed on Office")
    );
}

#[test]
fn a_kept_set_that_cannot_be_read_falls_back_to_the_built_in_one() {
    use wgal_runtime::notify::{FileSettingsStore, SETTINGS_FILE};
    let settings_with = |templates: Option<serde_json::Value>| {
        let mut json = serde_json::json!({
            "schema_version": 1,
            "enabled": true,
            "device_name": "辦公室筆電",
            "chat_id": "-1001234567890",
            "token_set": true,
        });
        if let Some(templates) = templates {
            json["templates"] = templates;
        }
        serde_json::to_vec_pretty(&json).unwrap()
    };
    let mut unreadable = serde_json::to_value(english("en/0001")).unwrap();
    // A formal template without {result} is not a valid set.
    unreadable["formal"] =
        "ALERT on {tunnel} ({incident}), {device}, since {started_at} ({time_zone})".into();
    // A set an older watchdog kept, from before {time_zone} (1.2.x): each
    // time was followed by its zone, so the templates have no {time_zone}.
    let mut before_zone = serde_json::to_value(english("en/0001")).unwrap();
    for field in ["formal", "exhausted", "recovered", "test"] {
        let text = before_zone[field]
            .as_str()
            .unwrap()
            .replace(" ({time_zone})", "");
        assert!(!text.contains("{time_zone}"), "{field}");
        before_zone[field] = text.into();
    }
    for (name, file) in [
        ("unreadable", settings_with(Some(unreadable))),
        ("before-time-zone", settings_with(Some(before_zone))),
        ("older", settings_with(None)),
    ] {
        let dir = TempDir::new(name);
        std::fs::write(dir.0.join(SETTINGS_FILE), file).unwrap();
        let store = MemoryStore::new();
        let mut wd = faulted(&store);
        let mut secrets = MemorySecretStore::new();
        secrets.save(&BotToken::new(TOKEN).unwrap()).unwrap();
        let mut notifier = Notifier::new(FileSettingsStore::new(&dir.0), secrets);
        // Notifications go on, with the built-in set, to the same chat.
        assert!(notifier.status().enabled, "{name}");
        assert_eq!(
            notifier
                .status()
                .chat_id
                .map(|c| c.as_str().to_owned())
                .as_deref(),
            Some("-1001234567890"),
            "{name}"
        );
        assert_eq!(templates_id(&notifier), None, "{name}");
        let transport = Script::default();
        assert!(deliver(&mut notifier, &mut wd, &transport), "{name}");
        assert!(
            transport.sent()[0]
                .1
                .starts_with("[WG Always Live] Fault alert\nDevice: 辦公室筆電\n"),
            "{name}"
        );
        assert!(
            transport.sent()[0].1.contains("\nTime zone: UTC\n"),
            "{name}"
        );
        // A GUI then sends its set again, which replaces what was there.
        assert!(matches!(
            set_templates(&mut notifier, &mut wd, english("en/0002")),
            Outcome::Ok(_)
        ));
        let restarted = Notifier::new(FileSettingsStore::new(&dir.0), MemorySecretStore::new());
        assert_eq!(templates_id(&restarted).as_deref(), Some("en/0002"));
    }
}

// The notification service's refusals say why (stage 8).

#[test]
fn settings_that_cannot_be_stored_say_save_failed() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    settings.set_failing(true);
    let refused = notifier
        .handle(
            &mut wd,
            &rid("settings"),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("x").unwrap(),
                chat_id: None,
            },
        )
        .unwrap();
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
}

fn notifications_off() -> Request {
    Request::UpdateNotificationSettings {
        enabled: false,
        device_name: DeviceName::new("x").unwrap(),
        chat_id: None,
    }
}

#[test]
fn pending_alerts_that_cannot_be_cancelled_say_why() {
    // The watchdog's state cannot be saved.
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    store.set_failing(true);
    let refused = notifier
        .handle(&mut wd, &rid("off"), &notifications_off())
        .unwrap();
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    // FIX-02 (N-02): the settings are stored before anything is cancelled,
    // so the switch is off; the core keeps the cancellation and stores it
    // once its storage works (n02_off_whose_cancellation_cannot_be_stored_*).
    assert!(!settings.saved().unwrap().enabled);

    // The watchdog's state was not loaded: restored next to a record that
    // does not validate, so nothing is written over it.
    store.set_failing(false);
    let mut state = store.saved().unwrap();
    let mut damaged = state.tunnels[0].clone();
    damaged.tunnel_id = tunnel_of("damaged");
    damaged.binding = "not a binding".to_owned();
    state.tunnels.push(damaged);
    let fake = FakePlatform::new();
    fake.add_tunnel(binding(), "Office", OsState::Connected);
    let mut restored = Watchdog::new(
        fake,
        FakeClock::new(T0),
        Timing::default(),
        Box::new(MemoryStore::new()),
        Startup::Restored(state),
    );
    assert_eq!(
        incident(&mut restored).notification,
        NotificationState::Pending
    );
    let refused = notifier
        .handle(&mut restored, &rid("off-2"), &notifications_off())
        .unwrap();
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageLoadFailed)
        ))
    );
}

// FIX-02, N-02: turning notifications off when one of the two stores fails.
// The answer, the switch and the pending alerts agree: the settings are
// stored first, and nothing is cancelled unless they were.

#[test]
fn n02_off_whose_settings_cannot_be_stored_changes_nothing() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    settings.set_failing(true);
    let refused = notifier
        .handle(&mut wd, &rid("off"), &notifications_off())
        .unwrap();
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    assert!(notifier.status().enabled);
    assert!(settings.saved().unwrap().enabled);
    // Refused, so the alert was not cancelled either: it still goes out.
    assert_eq!(incident(&mut wd).notification, NotificationState::Pending);
    settings.set_failing(false);
    let transport = Script::default();
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(transport.sent().len(), 1);
}

/// One pass a second, as the driver's turns would.
fn run_delivering_with<S: SecretStore, T: SettingsStore>(
    wd: &mut Wd,
    notifier: &mut Notifier<S, T>,
    transport: &Script,
    secs: u64,
) {
    for _ in 0..secs {
        run(wd, 1);
        while deliver(notifier, wd, transport) {}
    }
}

fn stored_notification(store: &MemoryStore) -> NotificationState {
    store.saved().unwrap().tunnels[0]
        .incident
        .as_ref()
        .unwrap()
        .notification
}

fn notifications_on() -> Request {
    Request::UpdateNotificationSettings {
        enabled: true,
        device_name: DeviceName::new("x").unwrap(),
        chat_id: Some(ChatId::new("-1001234567890").unwrap()),
    }
}

#[test]
fn n02_off_whose_cancellation_cannot_be_stored_is_off_and_never_sends() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets, settings) = service(true, true);
    store.set_failing(true);
    let refused = notifier
        .handle(&mut wd, &rid("off"), &notifications_off())
        .unwrap();
    // The settings were stored: the switch is off, and the answer says the
    // watchdog's own state could not be.
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    assert!(!notifier.status().enabled);
    assert!(!settings.saved().unwrap().enabled);
    assert_eq!(incident(&mut wd).notification, NotificationState::Cancelled);
    assert_eq!(
        stored_notification(&store),
        NotificationState::Pending,
        "not stored yet"
    );

    // Turning notifications on again is refused while the cancellation is
    // not stored: a restart would find the alert pending, and on.
    let refused = notifier
        .handle(&mut wd, &rid("on"), &notifications_on())
        .unwrap();
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    assert!(!notifier.status().enabled);
    assert!(!settings.saved().unwrap().enabled);

    // Codex review of FIX-02, P2-01: a restart before the cancellation was
    // stored finds the alert pending and notifications off, and forgets
    // that it was owed. Turning them on before any pass ran cancels and
    // stores what is pending first, so it is never sent; when that cannot
    // be stored, on is refused.
    let restart = |state| {
        let fake = FakePlatform::new();
        fake.add_tunnel(binding(), "Office", OsState::Connected);
        let restarted_store = MemoryStore::new();
        let wd = Watchdog::new(
            fake,
            FakeClock::new(T0),
            Timing::default(),
            Box::new(restarted_store.clone()),
            Startup::Restored(state),
        );
        (wd, restarted_store)
    };
    let (mut restarted, restarted_store) = restart(store.saved().unwrap());
    let mut again = Notifier::new(settings.clone(), secrets.clone());
    assert!(!again.status().enabled);
    assert_eq!(
        incident(&mut restarted).notification,
        NotificationState::Pending
    );
    restarted_store.set_failing(true);
    let refused = again
        .handle(&mut restarted, &rid("on-r1"), &notifications_on())
        .unwrap();
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    assert!(!again.status().enabled);
    assert!(!settings.saved().unwrap().enabled);
    restarted_store.set_failing(false);
    run(&mut restarted, 120);
    assert!(matches!(
        again
            .handle(&mut restarted, &rid("on-r2"), &notifications_on())
            .unwrap(),
        Outcome::Ok(_)
    ));
    assert_eq!(
        stored_notification(&restarted_store),
        NotificationState::Cancelled
    );
    break_tunnel(&restarted, "office");
    let transport = Script::default();
    run_delivering_with(&mut restarted, &mut again, &transport, 600);
    assert!(transport.sent().is_empty());

    // In the first process the core stores the cancellation once its
    // storage works again; then notifications may be turned on, and the
    // cancelled alert is never sent.
    store.set_failing(false);
    run(&mut wd, 120);
    assert_eq!(stored_notification(&store), NotificationState::Cancelled);
    assert!(matches!(
        notifier
            .handle(&mut wd, &rid("on-2"), &notifications_on())
            .unwrap(),
        Outcome::Ok(_)
    ));
    let transport = Script::default();
    run_delivering_with(&mut wd, &mut notifier, &transport, 600);
    assert!(transport.sent().is_empty());
}

// FIX-02, AR-12: a pending alert that cannot be sent for want of a token
// waits instead of reading the token store on every pass.

/// A token store that counts its reads.
#[derive(Clone, Debug, Default)]
struct CountingStore {
    inner: MemorySecretStore,
    reads: Arc<Mutex<u32>>,
}

impl CountingStore {
    fn reads(&self) -> u32 {
        *lock(&self.reads)
    }
}

impl SecretStore for CountingStore {
    fn load(&self) -> Result<Option<BotToken>, wgal_runtime::notify::SecretError> {
        *lock(&self.reads) += 1;
        self.inner.load()
    }
    fn save(&mut self, token: &BotToken) -> Result<(), wgal_runtime::notify::SecretError> {
        self.inner.save(token)
    }
    fn clear(&mut self) -> Result<(), wgal_runtime::notify::SecretError> {
        self.inner.clear()
    }
}

type Counting = Notifier<CountingStore, MemorySettingsStore>;

fn counting_service(_: &mut Wd, token: bool) -> (Counting, CountingStore) {
    let mut secrets = CountingStore::default();
    if token {
        secrets.save(&BotToken::new(TOKEN).unwrap()).unwrap();
    }
    let mut notifier = Notifier::new(MemorySettingsStore::new(), secrets.clone());
    assert!(matches!(
        notifier
            .handle(&mut quiet_watchdog(), &rid("on"), &notifications_on())
            .unwrap(),
        Outcome::Ok(_)
    ));
    (notifier, secrets)
}

#[test]
fn ar12_no_token_is_not_read_over_and_over_and_a_new_token_ends_the_wait() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets) = counting_service(&mut wd, false);
    let before = secrets.reads();
    let transport = Script::default();
    for _ in 0..100 {
        assert!(!deliver(&mut notifier, &mut wd, &transport));
        wd.clock().advance(Duration::from_secs(1)).unwrap();
    }
    assert_eq!(notifier.status().token, CredentialState::NotSet);
    assert_eq!(
        notifier.status().last_failure,
        Some(NotificationFailure::NotConfigured)
    );
    assert!(
        secrets.reads() - before <= 1,
        "read {} times",
        secrets.reads() - before
    );
    // Setting the token ends the wait: the alert goes out on the next pass.
    assert!(matches!(
        notifier
            .handle(
                &mut wd,
                &rid("token"),
                &Request::SetBotToken {
                    token: BotToken::new(TOKEN).unwrap()
                }
            )
            .unwrap(),
        Outcome::Ok(_)
    ));
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(transport.sent().len(), 1);
}

#[test]
fn ar12_a_test_notification_still_reads_the_store_at_once() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets) = counting_service(&mut wd, false);
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    let before = secrets.reads();
    assert!(matches!(
        notifier
            .handle(&mut wd, &rid("test"), &Request::SendTestNotification)
            .unwrap(),
        Outcome::Ok(_)
    ));
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(secrets.reads() - before, 1);
    assert_eq!(
        notifier.status().last_test.unwrap().state,
        TestNotificationState::Failed
    );
}

#[test]
fn ar12_new_settings_end_the_wait() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets) = counting_service(&mut wd, false);
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    let before = secrets.reads();
    assert!(matches!(
        notifier
            .handle(&mut wd, &rid("on-2"), &notifications_on())
            .unwrap(),
        Outcome::Ok(_)
    ));
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(secrets.reads() - before, 1);
}

/// Already so before FIX-02, kept as a guard: once a reservation cannot be
/// stored the core reports nothing due until its storage works again, so
/// the token store is not read meanwhile and no wait of its own is needed.
#[test]
fn ar12_a_reservation_that_cannot_be_stored_is_held_by_the_core() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, secrets) = counting_service(&mut wd, true);
    store.set_failing(true);
    let transport = Script::default();
    assert!(!deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(
        notifier.status().last_failure,
        Some(NotificationFailure::StorageUnavailable)
    );
    let before = secrets.reads();
    store.set_failing(false);
    for _ in 0..10 {
        assert!(!deliver(&mut notifier, &mut wd, &transport));
    }
    assert_eq!(secrets.reads(), before, "held");
    // Once the core has stored its state again, it goes out once.
    run(&mut wd, 31);
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert_eq!(transport.sent().len(), 1);
}

// --- wgal's shared language (owner decision of 2026-11-19) ------------------

fn set_language<S: SecretStore, T: SettingsStore>(
    notifier: &mut Notifier<S, T>,
    wd: &mut Wd,
    language: Option<&str>,
) -> Outcome {
    notifier
        .handle(
            wd,
            &rid("language"),
            &Request::SetCliLanguage {
                language: language.map(|tag| LanguageTag::new(tag).unwrap()),
            },
        )
        .unwrap()
}

fn cli_language<S: SecretStore, T: SettingsStore>(notifier: &Notifier<S, T>) -> Option<String> {
    notifier
        .status()
        .cli_language
        .map(|tag| tag.as_str().to_owned())
}

#[test]
fn the_shared_language_is_kept_with_the_settings_and_shown() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    assert_eq!(cli_language(&notifier), None);
    assert!(matches!(
        set_language(&mut notifier, &mut wd, Some("ja")),
        Outcome::Ok(Response::Accepted)
    ));
    assert_eq!(cli_language(&notifier).as_deref(), Some("ja"));
    let saved = settings.saved().unwrap();
    assert_eq!(saved.cli_language, Some(LanguageTag::new("ja").unwrap()));
    // The rest of the settings stay as they were.
    assert!(saved.enabled);
    assert_eq!(saved.chat_id, Some(ChatId::new("-1001234567890").unwrap()));
    let mut snapshot = wd.handle(&Request::GetSnapshot);
    notifier.decorate(&mut snapshot);
    let Outcome::Ok(Response::Snapshot { snapshot }) = snapshot else {
        panic!("no snapshot");
    };
    assert_eq!(snapshot.notification.cli_language.unwrap().as_str(), "ja");

    // It changes nothing a message says: the built-in set still writes.
    let transport = Script::default();
    assert!(deliver(&mut notifier, &mut wd, &transport));
    assert!(transport.sent()[0].1.starts_with(
        "[WG Always Live] Fault alert
"
    ));

    // A change of the other settings keeps it.
    let outcome = notifier
        .handle(
            &mut wd,
            &rid("rename"),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("Lab").unwrap(),
                chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            },
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(_)));
    assert_eq!(cli_language(&notifier).as_deref(), Some("ja"));
    assert_eq!(
        settings.saved().unwrap().cli_language,
        Some(LanguageTag::new("ja").unwrap())
    );
    // And so does a set of templates.
    set_templates(&mut notifier, &mut wd, english("en/0001"));
    assert_eq!(cli_language(&notifier).as_deref(), Some("ja"));

    // Cleared: never set again.
    assert!(matches!(
        set_language(&mut notifier, &mut wd, None),
        Outcome::Ok(Response::Accepted)
    ));
    assert_eq!(cli_language(&notifier), None);
    assert_eq!(settings.saved().unwrap().cli_language, None);
    assert_eq!(templates_id(&notifier).as_deref(), Some("en/0001"));
}

#[test]
fn the_same_language_again_needs_no_save_and_one_that_cannot_be_stored_changes_nothing() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let (mut notifier, _, settings) = service(true, true);
    set_language(&mut notifier, &mut wd, Some("de"));
    settings.set_failing(true);
    // The same language, or none again after none, is accepted unsaved.
    assert!(matches!(
        set_language(&mut notifier, &mut wd, Some("de")),
        Outcome::Ok(Response::Accepted)
    ));
    let refused = set_language(&mut notifier, &mut wd, Some("fr"));
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    let refused = set_language(&mut notifier, &mut wd, None);
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageSaveFailed)
        ))
    );
    settings.set_failing(false);
    assert_eq!(cli_language(&notifier).as_deref(), Some("de"));
    assert_eq!(
        settings.saved().unwrap().cli_language,
        Some(LanguageTag::new("de").unwrap())
    );

    // The same language in other letters is another value: it is written,
    // as given.
    assert!(matches!(
        set_language(&mut notifier, &mut wd, Some("DE")),
        Outcome::Ok(Response::Accepted)
    ));
    assert_eq!(
        settings.saved().unwrap().cli_language,
        Some(LanguageTag::new("DE").unwrap())
    );
}

#[test]
fn a_language_is_not_written_over_settings_that_could_not_be_read() {
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let settings = MemorySettingsStore::new();
    settings.set_failing(true);
    let mut notifier = Notifier::new(settings.clone(), MemorySecretStore::new());
    settings.set_failing(false);
    let refused = set_language(&mut notifier, &mut wd, Some("ko"));
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageLoadFailed)
        ))
    );
    assert_eq!(settings.saved(), None, "nothing was written");
    assert_eq!(cli_language(&notifier), None);
    // Clearing too: the none in use is only the default here.
    let refused = set_language(&mut notifier, &mut wd, None);
    assert_eq!(
        refusal(&refused),
        Some((
            ErrorCode::StorageUnavailable,
            Some(ErrorReason::StorageLoadFailed)
        ))
    );
    assert_eq!(settings.saved(), None, "nothing was written");
}

/// `notify.json` as watchdog 3.0.0 reads it: refusing fields it does not
/// have (the plan's §5).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ReadBy300 {
    schema_version: u32,
    enabled: bool,
    device_name: String,
    chat_id: Option<String>,
    #[serde(default)]
    token_set: bool,
    #[serde(default)]
    templates: Option<serde_json::Value>,
}

/// Clearing removes the key from the file whatever it held, an explicit
/// `null` or a value that is not a tag included, so that 3.0.0 reads the
/// file again (`wgal language --clear` before a rollback).
#[test]
fn clearing_removes_the_key_whatever_the_file_held() {
    use wgal_runtime::notify::{FileSettingsStore, SETTINGS_FILE};
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    for held in [
        Some(serde_json::Value::Null),
        Some(serde_json::json!("../ja")),
        Some(serde_json::json!(7)),
        Some(serde_json::json!("ja")),
        None,
    ] {
        let dir = TempDir::new("clear");
        let mut json = serde_json::json!({
            "schema_version": 1,
            "enabled": true,
            "device_name": "辦公室筆電",
            "chat_id": "-1001234567890",
            "token_set": false,
        });
        if let Some(held) = &held {
            json["cli_language"] = held.clone();
        }
        let path = dir.0.join(SETTINGS_FILE);
        std::fs::write(&path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        let mut notifier = Notifier::new(FileSettingsStore::new(&dir.0), MemorySecretStore::new());
        assert!(notifier.status().enabled, "{held:?}");
        assert!(matches!(
            set_language(&mut notifier, &mut wd, None),
            Outcome::Ok(Response::Accepted)
        ));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("cli_language"), "{held:?}: {text}");
        assert!(
            serde_json::from_str::<ReadBy300>(&text).is_ok(),
            "{held:?}: {text}"
        );
        assert_eq!(cli_language(&notifier), None);
        assert!(notifier.status().enabled);
    }
}

#[test]
fn the_shared_language_is_kept_across_a_restart_and_cleared_from_the_file() {
    use wgal_runtime::notify::{FileSettingsStore, SETTINGS_FILE};
    let dir = TempDir::new("language");
    let store = MemoryStore::new();
    let mut wd = faulted(&store);
    let secrets = MemorySecretStore::new();
    let mut notifier = Notifier::new(FileSettingsStore::new(&dir.0), secrets.clone());
    let outcome = notifier
        .handle(
            &mut quiet_watchdog(),
            &rid("settings"),
            &Request::UpdateNotificationSettings {
                enabled: true,
                device_name: DeviceName::new("辦公室筆電").unwrap(),
                chat_id: Some(ChatId::new("-1001234567890").unwrap()),
            },
        )
        .unwrap();
    assert!(matches!(outcome, Outcome::Ok(_)));
    let text = std::fs::read_to_string(dir.0.join(SETTINGS_FILE)).unwrap();
    assert!(!text.contains("cli_language"), "{text}");
    set_language(&mut notifier, &mut wd, Some("zh-TW"));
    let text = std::fs::read_to_string(dir.0.join(SETTINGS_FILE)).unwrap();
    assert!(text.contains("\"cli_language\": \"zh-TW\""), "{text}");
    drop(notifier);

    let mut restarted = Notifier::new(FileSettingsStore::new(&dir.0), secrets.clone());
    assert_eq!(cli_language(&restarted).as_deref(), Some("zh-TW"));
    assert!(restarted.status().enabled);
    set_language(&mut restarted, &mut wd, None);
    let text = std::fs::read_to_string(dir.0.join(SETTINGS_FILE)).unwrap();
    assert!(!text.contains("cli_language"), "{text}");
    drop(restarted);
    let again = Notifier::new(FileSettingsStore::new(&dir.0), secrets);
    assert_eq!(cli_language(&again), None);
    assert!(again.status().enabled);
}
