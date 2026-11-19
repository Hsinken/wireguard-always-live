//! Failure confirmation, recovery, backoff and incidents, driven by a fake
//! clock and a fake platform (plan §6.2, §6.3, §7.2; T02–T08, T22).

#![allow(clippy::unwrap_used)]

mod support;

use support::{Lab, Rig, icmp, id, tcp};
use wgal_core::{Clock, NotificationOutcome, Timing};
use wgal_protocol::{
    AdapterErrorKind, DataHealth, ErrorCode, FirstRecovery, Light, LightReason, LogEvent,
    NoticeKind, NotificationState, Outcome, ProbeResult, ProbeTiming, RecoveryPhase, RecoveryRetry,
    Request, Takeover,
};
use wgal_testkit::Call;

/// Plan §6.2: first failure at t = 0, a round every 15 s. By t = 30 three
/// rounds failed, yet the fault is not confirmed until 60 s have passed too.
#[test]
fn plan_example_three_failures_by_thirty_seconds_do_not_restart() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_until_restart("a");

    // t = 0 of the example is the round at 45 s.
    rig.run_until(75);
    let status = rig.status("a");
    assert_eq!(rig.stops("a"), 0, "three failures in 30 s must not restart");
    assert_eq!(status.light, Light::Yellow);
    assert_eq!(status.light_reason, LightReason::Confirming);
    assert_eq!(status.data_health, DataHealth::Suspect);
    assert_eq!(
        status.deadline_unix_ms,
        Some(support::T0 + 105_000),
        "the confirmation deadline is first failure + 60 s"
    );

    rig.run_until(104);
    assert_eq!(rig.stops("a"), 0, "four failures in 45 s still wait");

    rig.run_until(105);
    assert_eq!(
        rig.stops("a"),
        1,
        "five failures over 60 s confirm the fault"
    );
}

#[test]
fn short_loss_does_not_alarm_or_restart() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(60);
    rig.lab().heal("a");
    rig.run_until(300);

    let status = rig.status("a");
    assert_eq!(rig.stops("a"), 0);
    assert_eq!(status.incident, None);
    assert_eq!(status.light, Light::Green);
}

/// T02 and T05: the manager keeps saying Connected while no traffic flows.
/// Recovery restarts the tunnel and succeeds only once a bound probe through
/// the newly resolved interface gets a reply.
#[test]
fn connected_but_dead_tunnel_is_restarted_and_verified_through_the_new_interface() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    let old = rig.lab().fake.interface(&support::binding("a")).unwrap();
    rig.lab().break_until_restart("a");
    rig.run_until(105);

    let new = rig.lab().fake.interface(&support::binding("a")).unwrap();
    assert_ne!(new, old, "the restart created a new interface");
    let calls = rig.calls("a");
    let stop = calls
        .iter()
        .position(|c| matches!(c, Call::Stop(_)))
        .unwrap();
    let start = calls
        .iter()
        .position(|c| matches!(c, Call::Start(_)))
        .unwrap();
    assert!(stop < start);
    let verified = calls[start..]
        .iter()
        .any(|c| matches!(c, Call::Probe(interface, _) if *interface == new));
    assert!(verified, "verification probed through the new interface");

    let status = rig.status("a");
    assert_eq!(status.light, Light::Green);
    let incident = status.incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::Succeeded);
    assert_eq!(incident.notification, NotificationState::NotEligible);

    // Stable for 120 s closes the incident.
    rig.run_until(240);
    assert_eq!(rig.status("a").incident, None);
}

/// T05: the start command and `Connected` are not success; traffic is.
#[test]
fn restart_without_traffic_is_a_failed_round() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(105);
    assert_eq!(rig.starts("a"), 1);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Yellow);
    assert_eq!(status.light_reason, LightReason::Verifying);
    assert_eq!(status.recovery, RecoveryPhase::Verifying);

    // Verification gives up after 45 s.
    rig.run_until(151);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::RecoveryFailed);
    assert_eq!(status.recovery, RecoveryPhase::Backoff);
    let incident = status.incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::Failed);
}

/// T06 and §7.2: the first failed round makes the incident eligible for one
/// notification; later rounds back off 1, 2, 5, then 15 minutes from the end
/// of the previous round and never re-arm it.
#[test]
fn failed_recovery_backs_off_and_notifies_once() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    // Owner decision of 2026-11-04: the tunnel's own interval and limit
    // replace plan §6.3's 1, 2, 5, 15 minutes.
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(Some(3), 120).unwrap(),
    });
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(150);

    let status = rig.status("a");
    let incident = status.incident.clone().unwrap();
    assert_eq!(incident.notification, NotificationState::Pending);
    assert_eq!(incident.backoff_step, 0);
    assert_eq!(incident.retries_used, 0);
    let round_end = status.next_retry_unix_ms.unwrap() - 120_000;

    let due = rig.wd.notifications_due();
    assert_eq!(due.len(), 1);
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    assert_eq!(
        rig.store.saved().unwrap().tunnels[0]
            .incident
            .as_ref()
            .unwrap()
            .notification,
        NotificationState::Reserved,
        "reserved is durable before anything is sent"
    );
    rig.wd
        .record_notification(&ticket, NotificationOutcome::Sent { message_id: 42 })
        .unwrap();
    assert!(rig.wd.notifications_due().is_empty());

    // Track when each round starts (each stop) over the next half hour.
    let mut stops = Vec::new();
    let mut seen = rig.stops("a");
    for _ in 0..1800 {
        rig.run(1);
        if rig.stops("a") > seen {
            seen = rig.stops("a");
            stops.push(rig.wd.clock().wall_unix_ms());
        }
    }
    // Each failed round takes the stop/start plus 45 s of verification.
    let gaps: Vec<i64> = stops
        .windows(2)
        .map(|w| (w[1] - w[0] - 45_000) / 1000)
        .collect();
    assert_eq!(
        stops[0] - round_end,
        120_000,
        "first retry 2 min after the round ended"
    );
    assert_eq!(gaps, [120, 120], "then every 2 minutes, 3 retries in all");

    let status = rig.status("a");
    let incident = status.incident.unwrap();
    assert_eq!(incident.notification, NotificationState::Sent);
    assert_eq!(incident.backoff_step, 3);
    assert_eq!(incident.retries_used, 3);
    assert!(incident.retries_exhausted);
    assert_eq!(status.next_retry_unix_ms, None);
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::RetriesExhausted);
    // The alert went once; the used-up count is a message of its own (owner
    // decision of 2026-11-04), due once.
    assert_eq!(incident.exhausted_notification, NotificationState::Pending);
    let due = rig.wd.notifications_due();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].kind, NoticeKind::RetriesExhausted);
    assert_eq!(due[0].retries_used, 3);
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::Sent { message_id: 43 })
        .unwrap();
    assert!(rig.wd.notifications_due().is_empty());
    assert_eq!(
        rig.status("a").incident.unwrap().exhausted_notification,
        NotificationState::Sent
    );
}

/// Owner decision of 2026-11-04: an incident automatic recovery worked on
/// sends `recovered` once it has been stable through the closing period,
/// whether or not its alert was sent, and also when the tunnel came back
/// on its own while a retry waited.
#[test]
fn an_automatic_recovery_is_reported_once_the_incident_closes() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(150);
    // The first round failed; the alert is due but never sent here.
    assert_eq!(rig.wd.notifications_due()[0].kind, NoticeKind::Alert);
    // The remote end comes back while the retry waits.
    rig.lab().heal("a");
    let mut closed_at = None;
    for _ in 0..600 {
        rig.run(1);
        if rig.status("a").incident.is_none() {
            closed_at = Some(rig.wd.clock().wall_unix_ms());
            break;
        }
    }
    let closed_at = closed_at.expect("the incident closed");
    let status = rig.status("a");
    let notice = status.recovered_notification.unwrap();
    assert_eq!(notice.notification, NotificationState::Pending);
    let due = rig.wd.notifications_due();
    assert_eq!(due.len(), 1, "{due:?}");
    assert_eq!(due[0].kind, NoticeKind::Recovered);
    assert_eq!(due[0].incident_id, notice.incident_id);
    // Traffic came back the closing period before the incident closed.
    let back = due[0].recovered_at_unix_ms.unwrap();
    assert!(
        (closed_at - back - 120_000).abs() <= 16_000,
        "{back} {closed_at}"
    );
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &due[0].incident_id, due[0].kind)
        .unwrap();
    assert_eq!(
        rig.store.saved().unwrap().tunnels[0].recovered_notices[0]
            .notice
            .state,
        NotificationState::Reserved,
        "reserved is durable before anything is sent"
    );
    rig.wd
        .record_notification(&ticket, NotificationOutcome::Sent { message_id: 7 })
        .unwrap();
    assert!(rig.wd.notifications_due().is_empty());
    rig.run(600);
    assert!(rig.wd.notifications_due().is_empty(), "sent once");
}

/// Breaks the tunnel, lets the first round fail and the tunnel heal, and
/// runs until the incident has closed. Returns the closed incident's ID.
fn close_an_automatic_incident(rig: &mut Rig) -> wgal_protocol::IncidentId {
    rig.lab().break_for_good("a");
    let mut incident = None;
    for _ in 0..600 {
        rig.run(1);
        if let Some(i) = rig.status("a").incident {
            incident = Some(i.incident_id);
            if i.first_recovery == FirstRecovery::Failed {
                break;
            }
        }
    }
    let incident = incident.expect("no incident opened");
    rig.lab().heal("a");
    for _ in 0..600 {
        rig.run(1);
        if rig.status("a").incident.is_none() {
            return incident;
        }
    }
    panic!("the incident never closed");
}

/// Codex P2-N20: a later incident's `recovered` message neither replaces
/// an earlier one still waiting to be sent nor is skipped while an earlier
/// one is in flight. Each is due and sent on its own.
#[test]
fn each_closed_incident_keeps_its_own_recovered_message() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);

    // The first one could not be sent yet.
    let first = close_an_automatic_incident(&mut rig);
    let due = rig.wd.notifications_due();
    assert_eq!(due.len(), 1, "{due:?}");
    let ticket = rig
        .wd
        .reserve_notification(&due[0].tunnel_id, &first, NoticeKind::Recovered)
        .unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::RetryLater)
        .unwrap();

    // The second closes while the first still waits: both are due.
    let second = close_an_automatic_incident(&mut rig);
    assert_ne!(first, second);
    let due: Vec<_> = rig
        .wd
        .notifications_due()
        .into_iter()
        .filter(|d| d.kind == NoticeKind::Recovered)
        .map(|d| d.incident_id)
        .collect();
    assert_eq!(due, [first.clone(), second.clone()]);
    assert_eq!(
        rig.status("a").recovered_notification.unwrap().incident_id,
        second,
        "the snapshot shows the newest"
    );

    // The first goes out; while the second is in flight a third closes.
    let a = id("a");
    let ticket = rig
        .wd
        .reserve_notification(&a, &first, NoticeKind::Recovered)
        .unwrap();
    rig.wd
        .record_notification(&ticket, NotificationOutcome::Sent { message_id: 1 })
        .unwrap();
    let in_flight = rig
        .wd
        .reserve_notification(&a, &second, NoticeKind::Recovered)
        .unwrap();
    let third = close_an_automatic_incident(&mut rig);
    let due: Vec<_> = rig
        .wd
        .notifications_due()
        .into_iter()
        .filter(|d| d.kind == NoticeKind::Recovered)
        .map(|d| d.incident_id)
        .collect();
    assert_eq!(
        due,
        std::slice::from_ref(&third),
        "the third is not skipped"
    );
    rig.wd
        .record_notification(&in_flight, NotificationOutcome::Sent { message_id: 2 })
        .unwrap();
    let stored: Vec<_> = rig.store.saved().unwrap().tunnels[0]
        .recovered_notices
        .iter()
        .map(|n| (n.incident_id.clone(), n.notice.state))
        .collect();
    assert_eq!(
        stored,
        [
            (second, NotificationState::Sent),
            (third, NotificationState::Pending)
        ]
    );
}

/// Codex P2-N27: a retry that comes due while the process stalled, with no
/// wake notice, is not run on the fault confirmed before the stall: a
/// tunnel that came back meanwhile is not restarted.
#[test]
fn a_retry_after_a_stall_waits_for_fresh_evidence() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the first round never failed");
    }
    let stops = rig.stops("a");
    rig.lab().heal("a");
    // Past the retry and the tunnel's freshness at once.
    rig.wd
        .clock()
        .advance(std::time::Duration::from_secs(300))
        .unwrap();
    rig.run(60);
    assert_eq!(
        rig.stops("a"),
        stops,
        "restarted on evidence from before the stall"
    );
    assert!(rig.status("a").incident.is_some(), "the incident goes on");
}

/// Codex P2-N35: the same when the stall comes while a monitoring round is
/// out: its result, applied after the stall, is no evidence for the retry,
/// and a tunnel that came back meanwhile is not restarted.
#[test]
fn a_round_result_across_a_stall_is_no_evidence_for_a_retry() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the first round never failed");
    }
    let stops = rig.stops("a");
    // The next monitoring round runs, and its result is held.
    let mut held = None;
    'plan: for _ in 0..300 {
        rig.wd
            .clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        for item in rig.wd.plan() {
            assert!(!item.is_control(), "retried before the round");
            if rig.wd.still_due(&item) {
                held = Some(wgal_core::execute(rig.wd.adapter(), rig.wd.clock(), &item));
                break 'plan;
            }
        }
    }
    let result = held.expect("no round was planned");
    rig.lab().heal("a");
    // The process stalls past the retry and the tunnel's freshness before
    // the result is applied.
    rig.wd
        .clock()
        .advance(std::time::Duration::from_secs(300))
        .unwrap();
    rig.wd.apply(result);
    rig.run(60);
    assert_eq!(
        rig.stops("a"),
        stops,
        "restarted on a round from before the stall"
    );
}

/// Codex P2-N40: a "probe now" asked for while the round is out does not
/// make its result, applied after the stall, evidence for the retry.
#[test]
fn a_probe_now_during_a_stall_makes_no_old_round_evidence() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 900, "the first round never failed");
    }
    let stops = rig.stops("a");
    let mut held = None;
    'plan: for _ in 0..300 {
        rig.wd
            .clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        for item in rig.wd.plan() {
            assert!(!item.is_control(), "retried before the round");
            if rig.wd.still_due(&item) {
                held = Some(wgal_core::execute(rig.wd.adapter(), rig.wd.clock(), &item));
                break 'plan;
            }
        }
    }
    let result = held.expect("no round was planned");
    rig.lab().heal("a");
    rig.wd
        .clock()
        .advance(std::time::Duration::from_secs(300))
        .unwrap();
    rig.ok(Request::ProbeNow { tunnel_id: id("a") });
    rig.wd.apply(result);
    rig.run(60);
    assert_eq!(
        rig.stops("a"),
        stops,
        "restarted on a round from before the stall"
    );
}

/// Codex P2-N39: a round that takes longer than a result stays fresh, as
/// one with slow platform queries does, still counts its recovery target's
/// failure when that probe's own result is fresh: the fault is confirmed.
#[test]
fn a_long_round_with_a_fresh_failure_still_counts() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let freshness = u64::from(rig.status("a").freshness_secs);
    for _ in 0..400 {
        rig.wd
            .clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        for item in rig.wd.plan() {
            if !rig.wd.still_due(&item) {
                continue;
            }
            let round =
                !item.is_control() && matches!(item.kind, wgal_core::WorkKind::Round { .. });
            if round {
                // Queries before the probe take 20 s, and its result is
                // applied freshness - 5 s after the probe finished: 15 s
                // longer than a result stays fresh since the round left.
                rig.wd
                    .clock()
                    .advance(std::time::Duration::from_secs(20))
                    .unwrap();
            }
            let result = wgal_core::execute(rig.wd.adapter(), rig.wd.clock(), &item);
            if round {
                rig.wd
                    .clock()
                    .advance(std::time::Duration::from_secs(freshness - 5))
                    .unwrap();
            }
            rig.wd.apply(result);
        }
        if rig.stops("a") > 0 {
            return;
        }
    }
    panic!("the fault was never confirmed: long rounds counted no failure");
}

/// Codex P2-N31: when more `recovered` messages wait than a tunnel keeps,
/// the oldest is cancelled, and the log says so; none goes silently.
#[test]
fn a_recovered_message_that_makes_room_is_cancelled_in_the_log() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    // Telegram is out of reach: nothing is reserved or sent.
    let first = close_an_automatic_incident(&mut rig);
    for _ in 1..wgal_core::MAX_RECOVERED_NOTICES {
        close_an_automatic_incident(&mut rig);
    }
    let waiting = |rig: &mut Rig| {
        rig.wd
            .notifications_due()
            .into_iter()
            .filter(|d| d.kind == NoticeKind::Recovered)
            .map(|d| d.incident_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(waiting(&mut rig).len(), wgal_core::MAX_RECOVERED_NOTICES);
    assert_eq!(waiting(&mut rig)[0], first);
    rig.events();
    let newest = close_an_automatic_incident(&mut rig);
    let due = waiting(&mut rig);
    assert_eq!(
        due.len(),
        wgal_core::MAX_RECOVERED_NOTICES,
        "the list never holds more"
    );
    assert!(!due.contains(&first), "the oldest made room");
    assert_eq!(due.last(), Some(&newest));
    let events = rig.events_of("a");
    assert!(
        events.contains(&LogEvent::NotificationCancelled {
            incident_id: first,
            notice: NoticeKind::Recovered,
        }),
        "{events:?}"
    );
}

/// Codex P2-N28: undoing an `all_or_nothing` leaves another tunnel's work in
/// flight as it was: a stop already out is applied when its result comes
/// back, not sent again; and a waiting retry keeps its time.
#[test]
fn an_undone_change_leaves_every_tunnel_as_it_was() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.manage("b");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    let mut held = None;
    'plan: for _ in 0..900 {
        rig.wd
            .clock()
            .advance(std::time::Duration::from_secs(1))
            .unwrap();
        for item in rig.wd.plan() {
            if item.is_control() {
                held = Some(item);
                break 'plan;
            }
            if rig.wd.still_due(&item) {
                let result = wgal_core::execute(rig.wd.adapter(), rig.wd.clock(), &item);
                rig.wd.apply(result);
            }
        }
    }
    let item = held.expect("a's stop was never planned");
    assert!(rig.wd.still_due(&item), "the stop goes out");
    let undone = rig.wd.all_or_nothing(|wd| {
        match wd.handle(&Request::SetRecoveryRetry {
            tunnel_id: id("b"),
            retry: RecoveryRetry::new(Some(3), 600).unwrap(),
        }) {
            Outcome::Ok(_) => {}
            Outcome::Err(e) => return Err(e),
        }
        Err::<(), _>(wgal_protocol::ProtocolError::new(
            ErrorCode::Internal,
            None,
            "undone",
        ))
    });
    assert!(undone.is_err());
    assert_eq!(rig.status("b").retry, RecoveryRetry::DEFAULT, "b as it was");
    let result = wgal_core::execute(rig.wd.adapter(), rig.wd.clock(), &item);
    rig.wd.apply(result);
    rig.run(30);
    assert_eq!(rig.stops("a"), 1, "the stop was sent once");

    // A retry waiting in backoff keeps its time when a change to its
    // interval is undone.
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 1500, "the round never failed");
    }
    let due = rig.status("a").next_retry_unix_ms;
    assert!(due.is_some());
    let undone = rig.wd.all_or_nothing(|wd| {
        match wd.handle(&Request::SetRecoveryRetry {
            tunnel_id: id("a"),
            retry: RecoveryRetry::new(Some(3), 10).unwrap(),
        }) {
            Outcome::Ok(_) => {}
            Outcome::Err(e) => return Err(e),
        }
        Err::<(), _>(wgal_protocol::ProtocolError::new(
            ErrorCode::Internal,
            None,
            "undone",
        ))
    });
    assert!(undone.is_err());
    assert_eq!(
        rig.status("a").next_retry_unix_ms,
        due,
        "the retry keeps its time"
    );
    let stops = rig.stops("a");
    rig.run(20);
    assert_eq!(rig.stops("a"), stops, "not retried early");
}

/// Codex P3-N33: undoing an `all_or_nothing` while the event queue is full
/// leaves the queue exactly as it was, with its count of lost lines.
#[test]
fn an_undone_change_leaves_a_full_event_queue_as_it_was() {
    let toggles = |rig: &mut Rig| {
        for i in 0..(wgal_core::MAX_EVENTS + 20) {
            rig.ok(Request::SetTunnelMonitoring {
                tunnel_id: id("a"),
                enabled: i % 2 == 1,
            });
        }
    };
    let mut undone = Rig::new(&["a"]);
    let mut kept = Rig::new(&["a"]);
    undone.events();
    kept.events();
    toggles(&mut undone);
    toggles(&mut kept);
    let result = undone.wd.all_or_nothing(|wd| {
        match wd.handle(&Request::SetTunnelMonitoring {
            tunnel_id: id("a"),
            enabled: false,
        }) {
            Outcome::Ok(_) => {}
            Outcome::Err(e) => return Err(e),
        }
        Err::<(), _>(wgal_protocol::ProtocolError::new(
            ErrorCode::Internal,
            None,
            "undone",
        ))
    });
    assert!(result.is_err());
    assert_eq!(undone.events(), kept.events());
}

/// A tunnel brought back by the user's own reconnect, or one automatic
/// recovery never worked on, sends no `recovered` message.
#[test]
fn a_manual_or_unattended_recovery_is_not_reported() {
    // Automatic recovery off: the incident has no automatic round.
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(200);
    assert!(rig.status("a").incident.is_some());
    rig.lab().heal("a");
    rig.run(400);
    assert!(rig.status("a").incident.is_none());
    assert_eq!(rig.status("a").recovered_notification, None);

    // A reconnect by the user ends an incident automatic recovery worked on.
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(150);
    rig.lab().heal("a");
    rig.lab().break_until_restart("a");
    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(400);
    let status = rig.status("a");
    assert!(status.incident.is_none());
    assert_eq!(status.recovered_notification, None);
    assert!(rig.wd.notifications_due().is_empty());
}

/// Owner decision of 2026-11-04: a used-up retry limit is said once, the
/// tunnel is not restarted again in the incident, and a manual reconnect
/// starts the count again.
#[test]
fn a_used_up_retry_limit_stops_until_a_manual_reconnect() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(Some(1), 60).unwrap(),
    });
    rig.run_until(44);
    rig.events();
    rig.lab().break_for_good("a");
    rig.run(1200);
    // The first round and one retry.
    assert_eq!(rig.stops("a"), 2);
    let events = rig.events_of("a");
    let exhausted: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, LogEvent::RetriesExhausted { .. }))
        .collect();
    assert_eq!(exhausted, [&LogEvent::RetriesExhausted { retries: 1 }]);
    let incident = rig.status("a").incident.unwrap();
    assert!(incident.retries_exhausted);
    assert!(
        rig.store.saved().unwrap().tunnels[0]
            .incident
            .as_ref()
            .unwrap()
            .retries_exhausted,
        "the used-up count is stored"
    );
    rig.run(1200);
    assert_eq!(rig.stops("a"), 2, "nothing more in this incident");

    rig.ok(Request::Reconnect { tunnel_id: id("a") });
    rig.run(600);
    // The reconnect, then one more automatic retry under the new count.
    assert_eq!(rig.stops("a"), 4);
    assert!(rig.status("a").incident.unwrap().retries_exhausted);
}

/// Codex P2-N17: a retry runs on the tunnel's retry interval, not when the
/// next monitoring round comes back. Probes every 600 s, a retry every 10 s:
/// the retry follows the failed round within its verification and 10 s.
#[test]
fn a_retry_runs_on_its_own_interval_not_the_probe_interval() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", support::timing(600, 2, 0, 0));
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(Some(3), 10).unwrap(),
    });
    rig.lab().break_for_good("a");
    while rig.stops("a") == 0 {
        rig.run(1);
        assert!(rig.now_secs() < 1300, "the fault was never recovered");
    }
    let first = rig.now_secs();
    while rig.stops("a") < 2 {
        rig.run(1);
        assert!(
            rig.now_secs() < first + 120,
            "the retry waited for the next probe round"
        );
    }
    // And again for the next one, with no round in between.
    let second = rig.now_secs();
    while rig.stops("a") < 3 {
        rig.run(1);
        assert!(rig.now_secs() < second + 120, "the second retry waited");
    }
}

/// Codex P2-N19: while the host is found offline, a tunnel left down is
/// neither restarted nor charged a retry. A network change ends the mark
/// and the retry runs at once; a retry whose verification only finds the
/// host offline again is not counted either.
#[test]
fn an_offline_host_neither_runs_nor_counts_a_retry_of_a_tunnel_left_down() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    // A long freshness keeps the offline mark evidence past the retry.
    rig.set_probe("a", support::timing(300, 2, 0, 0));
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(Some(2), 10).unwrap(),
    });
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Verifying {
        rig.run(1);
        assert!(rig.now_secs() < 900, "recovery never reached verification");
    }
    // The verification finds the host offline, then the tunnel goes down.
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(6);
    rig.lab()
        .fake
        .set_os_state(&support::binding("a"), wgal_protocol::OsState::Disconnected);
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 1200, "the round never failed");
    }
    let starts = rig.starts("a");
    rig.run(120);
    assert_eq!(
        rig.starts("a"),
        starts,
        "restarted while the host was offline"
    );
    assert_eq!(rig.status("a").incident.unwrap().retries_used, 0);

    // The network changes: the retry runs, counted.
    rig.network_change();
    rig.run(5);
    assert_eq!(
        rig.starts("a"),
        starts + 1,
        "the retry waited after the change"
    );
    assert_eq!(rig.status("a").incident.unwrap().retries_used, 1);

    // Its verification finds the host offline again: inconclusive, and not
    // counted.
    while rig.status("a").recovery == RecoveryPhase::Verifying
        || rig.status("a").recovery == RecoveryPhase::Starting
    {
        rig.run(1);
        assert!(rig.now_secs() < 3000, "the retry never ended");
    }
    assert_eq!(
        rig.status("a").incident.unwrap().retries_used,
        0,
        "a round that met the host offline is given back"
    );
}

/// Codex P2-N19: with no network change, a tunnel left down is not held
/// forever by an offline mark nothing can clear: the mark stops being
/// evidence after the tunnel's freshness, and the retry runs.
#[test]
fn an_old_offline_mark_does_not_hold_a_tunnel_left_down_forever() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_probe("a", support::timing(300, 2, 0, 0));
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(None, 10).unwrap(),
    });
    rig.lab().break_for_good("a");
    while rig.status("a").recovery != RecoveryPhase::Verifying {
        rig.run(1);
        assert!(rig.now_secs() < 900, "recovery never reached verification");
    }
    rig.lab()
        .fail_probes("a", Some(AdapterErrorKind::LocalNetworkOffline));
    rig.run(6);
    rig.lab()
        .fake
        .set_os_state(&support::binding("a"), wgal_protocol::OsState::Disconnected);
    while rig.status("a").recovery != RecoveryPhase::Backoff {
        rig.run(1);
        assert!(rig.now_secs() < 1200, "the round never failed");
    }
    let starts = rig.starts("a");
    let freshness = u64::from(rig.status("a").freshness_secs);
    rig.run(freshness + 5);
    assert_eq!(rig.starts("a"), starts + 1, "held by an old offline mark");
}

/// A limit raised after it was used up resumes the retries; `null` keeps
/// trying for as long as the fault lasts.
#[test]
fn raising_a_used_up_limit_resumes_the_retries() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(Some(0), 60).unwrap(),
    });
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run(600);
    assert_eq!(rig.stops("a"), 1, "0 retries: only the first round");
    assert!(rig.status("a").incident.unwrap().retries_exhausted);
    rig.ok(Request::SetRecoveryRetry {
        tunnel_id: id("a"),
        retry: RecoveryRetry::new(None, 60).unwrap(),
    });
    assert!(!rig.status("a").incident.unwrap().retries_exhausted);
    rig.run(1200);
    let stops = rig.stops("a");
    assert!(stops >= 8, "{stops} stops: the retries did not resume");
    assert!(!rig.status("a").incident.unwrap().retries_exhausted);
}

/// T07: three tunnels, A breaks. Only A is restarted; B and C keep probing
/// and stay green.
#[test]
fn only_the_failed_tunnel_is_restarted() {
    let mut rig = Rig::new(&["a", "b", "c"]);
    for name in ["a", "b", "c"] {
        rig.manage(name);
    }
    rig.run_until(44);
    rig.lab().break_until_restart("a");
    let probes_before = (rig.probes("b"), rig.probes("c"));
    rig.run_until(200);

    assert_eq!(rig.stops("a"), 1);
    for name in ["b", "c"] {
        assert_eq!(rig.stops(name), 0, "{name} was not touched");
        assert_eq!(rig.starts(name), 0, "{name} was not touched");
        assert_eq!(rig.status(name).light, Light::Green);
    }
    assert!(rig.probes("b") > probes_before.0 + 8);
    assert!(rig.probes("c") > probes_before.1 + 8);
}

/// Plan §5.3: the host queue serializes stop/start phases only. While A
/// spends 45 s verifying, B's restart goes ahead.
#[test]
fn verification_does_not_hold_up_another_tunnels_restart() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.manage("b");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.lab().break_until_restart("b");
    rig.run_until(110);

    let calls = rig.lab().fake.calls();
    let position = |want: &Call| calls.iter().position(|c| c == want).unwrap();
    let (a, b) = (support::binding("a"), support::binding("b"));
    let a_start = position(&Call::Start(a.clone()));
    let b_stop = position(&Call::Stop(b.clone()));
    assert!(position(&Call::Stop(a)) < a_start);
    assert!(a_start < b_stop, "B waited for A's stop and start");
    assert_eq!(rig.status("a").light_reason, LightReason::Verifying);
    assert_eq!(
        rig.status("b").light,
        Light::Green,
        "B recovered while A was verifying"
    );
}

/// T08: the target answers through B, but A's route to it leaves through B.
/// A never turns green on that reply and is not restarted for it.
#[test]
fn reply_through_another_tunnel_never_turns_a_tunnel_green() {
    let mut rig = Rig::new(&["a", "b"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab()
        .fake
        .route_via(support::address(1), Some(&support::binding("b")));
    rig.run_until(400);

    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::RouteConflict);
    assert_eq!(status.recovery, RecoveryPhase::Blocked);
    assert_eq!(status.fault.unwrap().kind, AdapterErrorKind::RouteMismatch);
    assert_eq!(rig.stops("a"), 0);
}

/// T22: one success in the middle of a fault does not open a new incident
/// or re-arm the notification; 120 s of stability closes it, and the next
/// fault is a new incident.
#[test]
fn brief_success_keeps_the_incident_and_stability_closes_it() {
    let lab = Lab::new(&["a"]);
    lab.fake.set_capabilities(support::ALL_SUPPORTED);
    let mut rig = Rig::with_lab(lab, &["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(160);
    let first = rig.status("a").incident.unwrap();
    assert_eq!(first.notification, NotificationState::Pending);

    // One good round, then broken again.
    rig.lab().heal("a");
    rig.run(15);
    let healthy = rig.status("a");
    assert!(
        healthy
            .incident
            .as_ref()
            .unwrap()
            .stable_since_unix_ms
            .is_some()
    );
    assert!(
        rig.wd.notifications_due().is_empty(),
        "a short success defers sending"
    );
    rig.lab().break_for_good("a");
    rig.run(300);
    let again = rig.status("a").incident.unwrap();
    assert_eq!(again.incident_id, first.incident_id);
    assert_eq!(again.notification, NotificationState::Pending);

    rig.lab().heal("a");
    rig.run(400);
    assert_eq!(
        rig.status("a").incident,
        None,
        "closed after 120 s of stability"
    );

    rig.lab().break_for_good("a");
    rig.run(120);
    let next = rig.status("a").incident.unwrap();
    assert_ne!(next.incident_id, first.incident_id);
}

/// T10: a recovery target that never answered cannot drive recovery.
#[test]
fn failed_baseline_blocks_auto_recovery() {
    let lab = Lab::new(&["a"]);
    lab.kill_target(1);
    let mut rig = Rig::with_lab(lab, &["a"]);
    let outcome = rig.request(Request::SetAutoRecovery {
        tunnel_id: id("a"),
        enabled: true,
    });
    let Outcome::Err(error) = outcome else {
        panic!("auto recovery was enabled without a baseline");
    };
    assert_eq!(error.code, ErrorCode::InvalidState);

    rig.run(600);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Red);
    assert_eq!(status.light_reason, LightReason::BaselineFailed);
    assert_eq!(status.takeover, Takeover::ObserveOnly);
    assert_eq!(rig.stops("a"), 0);
}

/// T09: an observe-only target going down only marks that target.
#[test]
fn observe_target_down_never_restarts() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.set_targets("a", Some(icmp(1)), &[icmp(50)]);
    rig.lab().kill_target(50);
    rig.run(600);

    let status = rig.status("a");
    assert_eq!(status.light, Light::Green);
    assert_eq!(rig.stops("a"), 0);
    let observed = &status.observe_targets[0];
    assert_eq!(
        observed.last_probe.as_ref().unwrap().result,
        wgal_protocol::ProbeResult::NoReply
    );
}

/// Without auto recovery a confirmed fault is shown, never acted on, and
/// never reported as a failed recovery (plan §7.2).
#[test]
fn auto_recovery_off_shows_the_fault_only() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run(600);
    let status = rig.status("a");
    assert_eq!(status.light_reason, LightReason::FaultConfirmed);
    assert_eq!(status.data_health, DataHealth::Failed);
    let incident = status.incident.unwrap();
    assert_eq!(incident.first_recovery, FirstRecovery::NotAttempted);
    assert_eq!(incident.notification, NotificationState::NotEligible);
    assert_eq!(rig.stops("a"), 0);
}

#[test]
fn turning_auto_recovery_off_cancels_retries_without_disconnecting() {
    let mut rig = Rig::new(&["a"]);
    rig.manage("a");
    rig.run_until(44);
    rig.lab().break_for_good("a");
    rig.run_until(160);
    assert_eq!(rig.status("a").recovery, RecoveryPhase::Backoff);

    rig.ok(Request::SetAutoRecovery {
        tunnel_id: id("a"),
        enabled: false,
    });
    let stops = rig.stops("a");
    rig.run(1800);
    assert_eq!(rig.stops("a"), stops, "no more automatic rounds");
    let status = rig.status("a");
    assert_eq!(status.takeover, Takeover::ObserveOnly);
    assert_eq!(status.os_state, wgal_protocol::OsState::Connected);
    assert!(rig.probes("a") > 100, "monitoring continues");
}

/// The per-tunnel defaults (`ProbeTiming`, owner decision of 2026-11-03)
/// and the global timers give the plan's §6.2 table.
#[test]
fn default_timing_matches_the_plan() {
    let probe = ProbeTiming::DEFAULT;
    assert_eq!(probe.interval_secs(), 15);
    assert_eq!(probe.timeout_secs(), 2);
    assert_eq!(probe.failures_needed(), 3);
    assert_eq!(probe.min_fault_secs(), 60);
    assert_eq!(probe.freshness_secs(), 35);
    let timing = Timing::default();
    assert_eq!(timing.grace.as_secs(), 30);
    assert_eq!(timing.control_phase_timeout.as_secs(), 30);
    assert_eq!(timing.verify_window.as_secs(), 45);
    assert_eq!(timing.verify_interval.as_secs(), 5);
    assert_eq!(timing.incident_close_after.as_secs(), 120);
    assert_eq!(timing.stall_after.as_secs(), 35);
    assert_eq!(timing.storage_retry.as_secs(), 15);
}

/// Gives tunnel `a` a TCP recovery target that has passed its baseline, with
/// auto recovery on, at t = 80 s.
fn managed_tcp_rig() -> Rig {
    let mut rig = Rig::new(&["a"]);
    rig.set_targets("a", Some(tcp(1, 443)), &[]);
    rig.run(45);
    rig.manage("a");
    rig
}

fn refused() -> ProbeResult {
    ProbeResult::Refused {
        rtt_ms: u32::try_from(support::REFUSED_RTT.as_millis()).unwrap(),
    }
}

/// Plan §4.3: a refused connection came back through the bound interface, so
/// the tunnel works and only the service is down. It keeps the tunnel green,
/// never counts as a failure, never opens an incident, never restarts.
#[test]
fn refusing_recovery_target_keeps_the_tunnel_green_and_never_restarts() {
    let mut rig = managed_tcp_rig();
    rig.lab().refuse_target(1, true);
    rig.run(1800);

    let status = rig.status("a");
    assert_eq!(rig.stops("a"), 0);
    assert_eq!(status.light, Light::Green);
    assert_eq!(status.light_reason, LightReason::Verified);
    assert_eq!(status.data_health, DataHealth::Healthy);
    assert_eq!(status.incident, None);
    let last = status.recovery_target.unwrap().last_probe.unwrap();
    assert_eq!(last.result, refused(), "shown as a refused service");
}

/// A refusal also ends a failure streak: it is evidence the path works.
#[test]
fn refusal_ends_a_failure_streak() {
    let mut rig = managed_tcp_rig();
    rig.lab().break_for_good("a");
    rig.run(40);
    assert_eq!(rig.status("a").data_health, DataHealth::Suspect);
    rig.lab().heal("a");
    rig.lab().refuse_target(1, true);
    rig.run(15);
    let status = rig.status("a");
    assert_eq!(status.data_health, DataHealth::Healthy);
    rig.run(600);
    assert_eq!(rig.stops("a"), 0);
}

/// After a restart, a refused connection through the new interface verifies
/// the path; the round succeeds and nothing restarts again.
#[test]
fn refusal_after_a_restart_verifies_the_path() {
    let mut rig = managed_tcp_rig();
    rig.lab().break_until_restart("a");
    rig.lab().refuse_target(1, false);
    rig.run(70);
    assert_eq!(rig.stops("a"), 1, "the dead path was restarted");
    rig.lab().refuse_target(1, true);
    rig.run(600);
    let status = rig.status("a");
    assert_eq!(rig.stops("a"), 1);
    assert_eq!(status.light, Light::Green);
    assert_eq!(
        status.incident.map(|i| i.first_recovery),
        None,
        "the incident closed after 120 s of answers"
    );
}

/// Only a reply passes the baseline: a target that refuses from the start is
/// probably the wrong service or port, so it cannot drive recovery until it
/// has answered once. The tunnel itself still shows green.
#[test]
fn refusing_target_cannot_pass_the_baseline() {
    let mut rig = Rig::new(&["a"]);
    rig.lab().refuse_target(1, true);
    rig.set_targets("a", Some(tcp(1, 443)), &[]);
    rig.run(60);

    let enable = Request::SetAutoRecovery {
        tunnel_id: id("a"),
        enabled: true,
    };
    let Outcome::Err(error) = rig.request(enable.clone()) else {
        panic!("a refusing target passed the baseline");
    };
    assert_eq!(error.code, ErrorCode::InvalidState);
    let status = rig.status("a");
    assert_eq!(status.light, Light::Green);
    assert_eq!(
        status.recovery_target.unwrap().last_probe.unwrap().result,
        refused()
    );

    rig.lab().refuse_target(1, false);
    rig.run(15);
    assert!(
        matches!(rig.request(enable), Outcome::Ok(_)),
        "a reply passes it"
    );
}

/// Owner decision of 2026-11-04: automatic recovery an import asked for
/// turns on once the recovery target passes its baseline, if it does so in
/// time; the user's own switch replaces it.
#[test]
fn automatic_recovery_from_an_import_waits_for_the_baseline() {
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    // A new target: its baseline has not passed.
    rig.set_targets("a", Some(icmp(9)), &[]);
    let status = rig.status("a");
    assert!(!status.auto_recovery_allowed);
    let wall = rig.wd.clock().wall_unix_ms();
    rig.wd
        .arm_auto_recovery(&id("a"), true, wall + 600_000)
        .unwrap();
    assert!(rig.status("a").auto_recovery_pending);
    assert!(
        rig.store.saved().unwrap().tunnels[0]
            .auto_recovery_after_baseline
            .is_some(),
        "stored at once"
    );
    rig.run(60);
    let status = rig.status("a");
    assert!(status.intent.auto_recovery_enabled);
    assert!(status.intent.start_when_stopped_externally);
    assert!(!status.auto_recovery_pending);

    // Too late: the baseline passes after the deadline, nothing turns on.
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.set_targets("a", Some(icmp(9)), &[]);
    rig.lab().fail_probes("a", Some(AdapterErrorKind::Timeout));
    let wall = rig.wd.clock().wall_unix_ms();
    rig.wd
        .arm_auto_recovery(&id("a"), false, wall + 30_000)
        .unwrap();
    rig.run(60);
    assert!(!rig.status("a").auto_recovery_pending, "expired");
    rig.lab().fail_probes("a", None);
    rig.run(60);
    assert!(!rig.status("a").intent.auto_recovery_enabled);

    // The user's switch replaces it.
    let mut rig = Rig::new(&["a"]);
    rig.run_until(44);
    rig.set_targets("a", Some(icmp(9)), &[]);
    rig.lab().fail_probes("a", Some(AdapterErrorKind::Timeout));
    let wall = rig.wd.clock().wall_unix_ms();
    rig.wd
        .arm_auto_recovery(&id("a"), false, wall + 600_000)
        .unwrap();
    rig.ok(Request::SetAutoRecovery {
        tunnel_id: id("a"),
        enabled: false,
    });
    assert!(!rig.status("a").auto_recovery_pending);
    rig.lab().fail_probes("a", None);
    rig.run(60);
    assert!(!rig.status("a").intent.auto_recovery_enabled);
}
