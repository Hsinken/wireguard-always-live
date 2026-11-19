//! The notification service (plan §7): answers the notification requests,
//! decides what to send next, and records what came of it.
//!
//! Sending is split so the watchdog is never locked during HTTP:
//! [`Notifier::next_job`] picks one message and, for an alert, reserves it
//! through the core (persisted before anything is sent); [`Job::run`] sends
//! it with no lock held; [`Notifier::finish`] records the result. One message
//! is in flight at a time.
//!
//! Formal alerts come only from `Watchdog::notifications_due` (plan §7.2): the
//! core decides eligibility and keeps the per-incident state. This service
//! adds the delivery rules of plan §7.3: nothing is reserved without
//! settings, a readable token and working storage; a message that provably
//! never left (`NotSent`) or that Telegram asked to retry later (429) goes
//! back to pending with a wait; a refusal or an uncertain result is final.
//! Changing the token or chat never makes a settled alert eligible again.
//!
//! A tunnel's own notification switch (owner request 4 of 2026-11-03) is
//! the core's too: with it off the tunnel's incidents are never due, so they
//! are never reserved here, and a wait for a retry kept here is forgotten on
//! the next pass. Turning it off cancels a pending alert for good and treats
//! one in flight as turning notifications off does: its result is recorded,
//! and "try later" becomes `cancelled`.
//!
//! Test messages (plan §7.4) use the same transport and secret store, are
//! identified by the request ID (a repeated ID sends nothing), never touch an
//! incident, and are reported through `NotificationStatus::last_test`.
//!
//! Times in messages are written in the zone given to
//! [`Notifier::set_time_zone`], UTC until then.
//!
//! Messages are written with the templates a GUI handed over
//! (`Request::SetNotificationTemplates`, wave three, stage 8), kept with the
//! settings, or with the built-in set until then and whenever the set kept
//! cannot be read; `NotificationStatus::templates_id` says which. A new set
//! applies from the next message written: one already handed out keeps its
//! text. Setting templates sends nothing, resends nothing and leaves every
//! incident's notification state as it is.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::Duration;

use jiff::tz::TimeZone;
use wgal_core::{
    Clock, DueNotification, MonoInstant, NotificationError, NotificationOutcome,
    NotificationTicket, PlatformAdapter, Watchdog,
};
use wgal_protocol::{
    BotToken, ChatId, CredentialState, DeviceName, ErrorCode, ErrorReason, IncidentId, LanguageTag,
    NoticeKind, NotificationFailure, NotificationStatus, NotificationTemplates, Outcome,
    ProtocolError, Request, RequestId, Response, StorageHealth, TestNotificationState,
    TestNotificationStatus,
};

use super::message::{builtin_templates, notice_message, test_message};
use super::secret::SecretStore;
use super::settings::{NotificationSettings, SettingsStore};
use super::transport::{Delivery, Rejection, Transport};

/// How many recent test request IDs are remembered to drop repeats.
const RECENT_TESTS: usize = 32;
/// Waits before retrying an alert that provably never left: 30 s doubling
/// to at most 15 minutes. In memory only; a pending alert is provably
/// undelivered, so retrying sooner after a restart cannot duplicate it.
const FIRST_RETRY: Duration = Duration::from_secs(30);
const LONGEST_RETRY: Duration = Duration::from_secs(15 * 60);
/// After the token store could not be read, or held no token, how long
/// alerts wait before it is read again, so a store is not asked over and
/// over (plan §7.1; AR-12 for no token). A test notification, a new or
/// cleared token and new settings read it at once. (A reservation that
/// cannot be stored needs no wait here: the core reports nothing due until
/// its storage works again.)
const CREDENTIAL_RETRY: Duration = Duration::from_secs(5 * 60);

/// One message to send.
pub struct Job {
    kind: JobKind,
    token: BotToken,
    chat: ChatId,
    text: String,
}

#[derive(Clone, Debug)]
enum JobKind {
    Alert(NotificationTicket),
    Test(RequestId),
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The token prints as BotToken(***); the text is not secret but long.
        f.debug_struct("Job")
            .field("kind", &self.kind)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

/// A message [`Notifier::next_message`] chose, reserved and counted as in
/// flight, whose text is not written yet: [`Notifier::write`] writes it,
/// with the time zone and host name the notifier has then. The runtime
/// reads both again in between (owner decision of 2026-11-19), so only
/// when a message is certainly written. Always written: the notifier waits
/// for its job until [`Notifier::finish`].
pub struct Pending {
    kind: JobKind,
    token: BotToken,
    chat: ChatId,
    what: Writing,
}

/// What a [`Pending`] message says.
enum Writing {
    /// The test message, at the moment it was chosen.
    Test {
        now_unix_ms: i64,
    },
    Notice(DueNotification),
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The token prints as BotToken(***).
        f.debug_struct("Pending")
            .field("kind", &self.kind)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// Sends the message. Takes no lock; may block for as long as the
    /// transport's timeouts allow.
    pub fn run(&self, transport: &dyn Transport) -> Delivery {
        transport.send_message(&self.token, &self.chat, &self.text)
    }

    pub fn is_test(&self) -> bool {
        matches!(self.kind, JobKind::Test(_))
    }
}

#[derive(Clone, Copy, Debug)]
struct Retry {
    at: MonoInstant,
    attempts: u32,
}

/// See the module documentation.
pub struct Notifier<S, T> {
    settings: NotificationSettings,
    settings_store: T,
    settings_ok: bool,
    secrets: S,
    credential: CredentialState,
    last_test: Option<TestNotificationStatus>,
    test_waiting: Option<RequestId>,
    recent_tests: VecDeque<RequestId>,
    last_failure: Option<NotificationFailure>,
    /// When each message that could not be sent may be tried again.
    retries: BTreeMap<(IncidentId, NoticeKind), Retry>,
    credential_retry_at: Option<MonoInstant>,
    /// Pending alerts were cancelled in memory while notifications were off,
    /// but the core could not store that yet (FIX-02).
    cancel_owed: bool,
    in_flight: bool,
    time_zone: TimeZone,
    /// The computer's host name, for messages when no device name is set.
    host_name: Option<DeviceName>,
}

impl<S, T> fmt::Debug for Notifier<S, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notifier")
            .field("enabled", &self.settings.enabled)
            .field("credential", &self.credential)
            .field("in_flight", &self.in_flight)
            .finish_non_exhaustive()
    }
}

/// A refused notification request: its code, the reason the GUI shows a
/// sentence for, and English text for the details.
type Rejected = (ErrorCode, ErrorReason, &'static str);

fn reject(code: ErrorCode, reason: ErrorReason, message: &'static str) -> Outcome {
    Outcome::Err(ProtocolError::new(code, Some(reason), message))
}

impl<S: SecretStore, T: SettingsStore> Notifier<S, T> {
    /// Loads the settings and checks whether a token is stored. Settings that
    /// cannot be read leave notifications off until they are saved again.
    pub fn new(settings_store: T, secrets: S) -> Self {
        let (settings, settings_ok) = match settings_store.load() {
            Ok(settings) => (settings, true),
            Err(_) => (NotificationSettings::default(), false),
        };
        let credential = match secrets.load() {
            Ok(Some(_)) => CredentialState::Stored,
            Ok(None) if settings.token_set => CredentialState::Unavailable,
            Ok(None) => CredentialState::NotSet,
            Err(_) => CredentialState::Unavailable,
        };
        Self {
            settings,
            settings_store,
            settings_ok,
            secrets,
            credential,
            last_test: None,
            test_waiting: None,
            recent_tests: VecDeque::new(),
            last_failure: None,
            retries: BTreeMap::new(),
            credential_retry_at: None,
            cancel_owed: false,
            in_flight: false,
            time_zone: TimeZone::UTC,
            host_name: None,
        }
    }

    /// The zone message times are written in (owner request 9), for the
    /// runtime to call when the host settings are loaded or change. UTC
    /// until it is called. It applies from the next message formatted; one
    /// already handed out keeps the time it was written with.
    pub fn set_time_zone(&mut self, zone: TimeZone) {
        self.time_zone = zone;
    }

    /// The computer's host name (owner decision of 2026-11-19), for the
    /// runtime to call whenever it reads it: messages give it when no
    /// device name is set. `None` until it is called, or when it cannot be
    /// read. It applies from the next message written.
    pub fn set_host_name(&mut self, host_name: Option<DeviceName>) {
        self.host_name = host_name;
    }

    /// The name messages give this computer: the device name, or the host
    /// name when that is blank; still blank when there is neither, and the
    /// set's `unnamed_device` is written then.
    fn device_for_messages(&self) -> &DeviceName {
        match &self.host_name {
            Some(host) if self.settings.device_name.as_str().trim().is_empty() => host,
            _ => &self.settings.device_name,
        }
    }

    /// What the GUI shows (never the token).
    pub fn status(&self) -> NotificationStatus {
        NotificationStatus {
            enabled: self.settings.enabled,
            device_name: self.settings.device_name.clone(),
            chat_id: self.settings.chat_id.clone(),
            token: self.credential,
            last_test: self.last_test.clone(),
            last_failure: self.last_failure,
            templates_id: self.settings.templates.as_ref().map(|t| t.id().clone()),
            cli_language: self.settings.cli_language.clone(),
            host_name: self.host_name.clone(),
        }
    }

    /// The templates messages are written with now.
    fn templates(&self) -> &NotificationTemplates {
        self.settings
            .templates
            .as_ref()
            .unwrap_or_else(|| builtin_templates())
    }

    /// Fills the notification part of a snapshot the core produced.
    pub fn decorate(&self, outcome: &mut Outcome) {
        if let Outcome::Ok(Response::Snapshot { snapshot }) = outcome {
            snapshot.notification = self.status();
        }
    }

    /// Answers a notification request; `None` for any other request, which
    /// goes to the core. The caller has decoded it with the codec and checked
    /// the peer's identity.
    pub fn handle<A: PlatformAdapter, C: Clock>(
        &mut self,
        watchdog: &mut Watchdog<A, C>,
        request_id: &RequestId,
        request: &Request,
    ) -> Option<Outcome> {
        let result = match request {
            Request::UpdateNotificationSettings {
                enabled,
                device_name,
                chat_id,
            } => self.update_settings(watchdog, *enabled, device_name, chat_id.as_ref()),
            Request::SetBotToken { token } => self.set_token(Some(token)),
            Request::ClearBotToken => self.set_token(None),
            Request::SetNotificationTemplates { templates } => self.set_templates(templates),
            Request::SetCliLanguage { language } => self.set_cli_language(language.as_ref()),
            Request::SendTestNotification => {
                self.request_test(request_id, watchdog.clock().wall_unix_ms())
            }
            _ => return None,
        };
        Some(match result {
            Ok(()) => Outcome::Ok(Response::Accepted),
            Err((code, reason, message)) => reject(code, reason, message),
        })
    }

    fn update_settings<A: PlatformAdapter, C: Clock>(
        &mut self,
        watchdog: &mut Watchdog<A, C>,
        enabled: bool,
        device_name: &DeviceName,
        chat_id: Option<&ChatId>,
    ) -> Result<(), Rejected> {
        let next = NotificationSettings {
            enabled,
            device_name: device_name.clone(),
            chat_id: chat_id.cloned(),
            token_set: self.settings.token_set,
            templates: self.settings.templates.clone(),
            cli_language: self.settings.cli_language.clone(),
            ..NotificationSettings::default()
        };
        if enabled && !self.settings.enabled {
            // Turning notifications on from off: every alert still pending
            // was pending while they were off and is never sent (plan
            // §7.3). `next_job` cancels those that come due while off on
            // every pass; this covers the rest, and a restart before an
            // earlier cancellation was stored (Codex review of FIX-02,
            // P2-01): they are cancelled and stored first. When that cannot
            // be stored, or an earlier cancellation is still not stored
            // (`cancel_owed`), on is refused and the settings stay off.
            let cancelled = watchdog.cancel_pending_notifications();
            if cancelled.is_err()
                || (self.cancel_owed && watchdog.storage_health() != StorageHealth::Ok)
            {
                self.cancel_owed = true;
                return Err((
                    ErrorCode::StorageUnavailable,
                    watchdog.storage_reason(),
                    "the alerts cancelled while notifications were off are not stored yet",
                ));
            }
            self.cancel_owed = false;
        }
        // The settings first (FIX-02, N-02): when they cannot be stored,
        // nothing has changed, pending alerts included.
        self.settings_store.save(&next).map_err(|_| {
            (
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "notification settings could not be stored",
            )
        })?;
        self.settings = next;
        self.settings_ok = true;
        self.credential_retry_at = None;
        if !enabled {
            // Turning notifications off cancels every pending alert for good
            // (plan §7.3); nothing settled is touched. The core cancels them
            // in memory even when that cannot be stored, and stores it once
            // its storage works again; until then notifications are off, so
            // nothing is sent, and a restart finds them off and cancels them
            // on its first pass. The answer says the watchdog's state was
            // not stored, and turning them on waits for it (`cancel_owed`).
            if watchdog.cancel_pending_notifications().is_err() {
                self.cancel_owed = true;
                return Err((
                    ErrorCode::StorageUnavailable,
                    watchdog.storage_reason(),
                    "notifications are off; cancelling the pending ones could not be stored yet",
                ));
            }
        }
        Ok(())
    }

    fn set_token(&mut self, token: Option<&BotToken>) -> Result<(), Rejected> {
        let stored = match token {
            Some(token) => self.secrets.save(token),
            None => self.secrets.clear(),
        };
        if stored.is_err() {
            self.credential = CredentialState::Unavailable;
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::SecretStoreUnavailable,
                "the bot token store is unavailable",
            ));
        }
        self.credential = if token.is_some() {
            CredentialState::Stored
        } else {
            CredentialState::NotSet
        };
        self.credential_retry_at = None;
        // Only a hint for reporting; the token is stored either way. Settings
        // that could not be read are never overwritten with defaults.
        if self.settings_ok {
            let mut settings = self.settings.clone();
            settings.token_set = token.is_some();
            if self.settings_store.save(&settings).is_ok() {
                self.settings = settings;
            }
        }
        Ok(())
    }

    /// Keeps `templates` with the settings for every message written from
    /// now on. The same set again is accepted without a save. Settings that
    /// could not be read are never overwritten with defaults, so then the
    /// set is refused as `storage_load_failed`, and one that cannot be stored
    /// as `storage_save_failed`; either way the set in use stays.
    fn set_templates(&mut self, templates: &NotificationTemplates) -> Result<(), Rejected> {
        if self.settings.templates.as_ref() == Some(templates) {
            return Ok(());
        }
        if !self.settings_ok {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "notification settings could not be loaded; they are not overwritten",
            ));
        }
        let mut next = self.settings.clone();
        next.templates = Some(templates.clone());
        self.settings_store.save(&next).map_err(|_| {
            (
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the message templates could not be stored",
            )
        })?;
        self.settings = next;
        Ok(())
    }

    /// Keeps `wgal`'s shared language with the settings (owner decision of
    /// 2026-11-19), `None` to clear it; it changes nothing a message says.
    /// The same language again, as given, is accepted without a save.
    /// Clearing always writes the file again, without the key, even when
    /// none is in use: the file may still hold an explicit `null` or a value
    /// that did not read as a tag, and a 3.x watchdog reads it only without
    /// the key (`wgal language --clear` before a rollback). As with the
    /// templates, settings that could not be read are never overwritten
    /// (`storage_load_failed`), clearing included, and a change that cannot
    /// be stored is refused (`storage_save_failed`); either way the language
    /// in use stays.
    fn set_cli_language(&mut self, language: Option<&LanguageTag>) -> Result<(), Rejected> {
        if language.is_some() && self.settings.cli_language.as_ref() == language {
            return Ok(());
        }
        if !self.settings_ok {
            return Err((
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageLoadFailed,
                "notification settings could not be loaded; they are not overwritten",
            ));
        }
        let mut next = self.settings.clone();
        next.cli_language = language.cloned();
        self.settings_store.save(&next).map_err(|_| {
            (
                ErrorCode::StorageUnavailable,
                ErrorReason::StorageSaveFailed,
                "the command line's language could not be stored",
            )
        })?;
        self.settings = next;
        Ok(())
    }

    fn request_test(&mut self, request_id: &RequestId, wall: i64) -> Result<(), Rejected> {
        if self.recent_tests.contains(request_id) {
            // The same request again (an IPC retry): never sent twice.
            return Ok(());
        }
        if self.test_waiting.is_some()
            || self
                .last_test
                .as_ref()
                .is_some_and(|t| t.state == TestNotificationState::Sending)
        {
            return Err((
                ErrorCode::Busy,
                ErrorReason::TestNotificationInProgress,
                "a test notification is already being sent",
            ));
        }
        if self.recent_tests.len() == RECENT_TESTS {
            self.recent_tests.pop_front();
        }
        self.recent_tests.push_back(request_id.clone());
        self.last_test = Some(TestNotificationStatus {
            request_id: request_id.clone(),
            state: TestNotificationState::Sending,
            requested_at_unix_ms: wall,
            finished_at_unix_ms: None,
            failure: None,
        });
        self.test_waiting = Some(request_id.clone());
        Ok(())
    }

    fn finish_test(
        &mut self,
        request_id: &RequestId,
        delivery: Result<Delivery, NotificationFailure>,
        wall: i64,
    ) {
        let (state, failure) = match delivery {
            Ok(Delivery::Accepted { .. }) => (TestNotificationState::Accepted, None),
            Ok(Delivery::Unknown) => (
                TestNotificationState::Unknown,
                Some(NotificationFailure::OutcomeUnknown),
            ),
            Ok(other) => (TestNotificationState::Failed, Some(failure_of(other))),
            Err(failure) => (TestNotificationState::Failed, Some(failure)),
        };
        if let Some(test) = &mut self.last_test
            && &test.request_id == request_id
        {
            test.state = state;
            test.finished_at_unix_ms = Some(wall);
            test.failure = failure;
        }
    }

    /// The chat and token to send with, or why there are none.
    fn destination(&mut self) -> Result<(ChatId, BotToken), NotificationFailure> {
        let chat = self
            .settings
            .chat_id
            .clone()
            .ok_or(NotificationFailure::NotConfigured)?;
        match self.secrets.load() {
            Ok(Some(token)) => {
                self.credential = CredentialState::Stored;
                Ok((chat, token))
            }
            // Set, yet not found: unreadable now (a Keychain item it may not
            // read without asking). Not after the user cleared it in this
            // process, even if recording that in the settings failed.
            Ok(None) if self.settings.token_set && self.credential != CredentialState::NotSet => {
                self.credential = CredentialState::Unavailable;
                Err(NotificationFailure::CredentialUnavailable)
            }
            Ok(None) => {
                self.credential = CredentialState::NotSet;
                Err(NotificationFailure::NotConfigured)
            }
            Err(_) => {
                self.credential = CredentialState::Unavailable;
                Err(NotificationFailure::CredentialUnavailable)
            }
        }
    }

    /// The next message to send, written: [`Self::next_message`], then
    /// [`Self::write`].
    pub fn next_job<A: PlatformAdapter, C: Clock>(
        &mut self,
        watchdog: &mut Watchdog<A, C>,
    ) -> Option<Job> {
        let pending = self.next_message(watchdog)?;
        Some(self.write(pending))
    }

    /// The next message to send, if any, chosen but not yet written: a
    /// requested test first, then a due alert. An alert is reserved (and
    /// that persisted) before it is returned; if that fails, nothing is
    /// returned for it. `None` while a message is in flight. What it
    /// returns is in flight from now on and must be written.
    pub fn next_message<A: PlatformAdapter, C: Clock>(
        &mut self,
        watchdog: &mut Watchdog<A, C>,
    ) -> Option<Pending> {
        if self.in_flight {
            return None;
        }
        let now = watchdog.clock().now();
        let wall = watchdog.clock().wall_unix_ms();
        if let Some(request_id) = self.test_waiting.take() {
            match self.destination() {
                Ok((chat, token)) => {
                    self.in_flight = true;
                    return Some(Pending {
                        kind: JobKind::Test(request_id),
                        token,
                        chat,
                        what: Writing::Test { now_unix_ms: wall },
                    });
                }
                Err(failure) => self.finish_test(&request_id, Err(failure), wall),
            }
        }
        if !self.settings_ok {
            return None;
        }
        if !self.settings.enabled {
            // An alert that became due while notifications are off is never
            // sent later (plan §7.3: cancelled when notifications are off).
            if !watchdog.notifications_due().is_empty()
                && watchdog.cancel_pending_notifications().is_err()
            {
                self.cancel_owed = true;
            }
            return None;
        }
        if self.credential_retry_at.is_some_and(|at| now < at) {
            return None;
        }
        let due = watchdog.notifications_due();
        // Forget waits for messages that are no longer due: settled,
        // cancelled, or their tunnel's notifications turned off.
        self.retries
            .retain(|(id, kind), _| due.iter().any(|d| &d.incident_id == id && d.kind == *kind));
        let next = due.into_iter().find(|d| {
            self.retries
                .get(&(d.incident_id.clone(), d.kind))
                .is_none_or(|retry| now >= retry.at)
        })?;
        let (chat, token) = match self.destination() {
            Ok(found) => found,
            Err(failure) => {
                // Stays pending; nothing is reserved without a way to send.
                // The store is read again after a wait, not on every pass,
                // whether it could not be read or held no token (AR-12).
                if matches!(
                    failure,
                    NotificationFailure::CredentialUnavailable | NotificationFailure::NotConfigured
                ) {
                    self.credential_retry_at = now.checked_add(CREDENTIAL_RETRY);
                }
                self.last_failure = Some(failure);
                return None;
            }
        };
        let ticket =
            match watchdog.reserve_notification(&next.tunnel_id, &next.incident_id, next.kind) {
                Ok(ticket) => ticket,
                Err(NotificationError::Storage(_)) => {
                    self.last_failure = Some(NotificationFailure::StorageUnavailable);
                    return None;
                }
                Err(_) => return None,
            };
        self.in_flight = true;
        Some(Pending {
            kind: JobKind::Alert(ticket),
            token,
            chat,
            what: Writing::Notice(next),
        })
    }

    /// Writes a message [`Self::next_message`] chose, with the time zone
    /// and host name as they are now.
    pub fn write(&self, pending: Pending) -> Job {
        let device = self.device_for_messages();
        let text = match &pending.what {
            Writing::Test { now_unix_ms } => {
                test_message(self.templates(), device, *now_unix_ms, &self.time_zone)
            }
            Writing::Notice(due) => notice_message(self.templates(), device, due, &self.time_zone),
        };
        Job {
            kind: pending.kind,
            token: pending.token,
            chat: pending.chat,
            text,
        }
    }

    /// Records what came of a job from [`Self::next_job`].
    pub fn finish<A: PlatformAdapter, C: Clock>(
        &mut self,
        watchdog: &mut Watchdog<A, C>,
        job: Job,
        delivery: Delivery,
    ) {
        self.in_flight = false;
        let now = watchdog.clock().now();
        let wall = watchdog.clock().wall_unix_ms();
        let ticket = match job.kind {
            JobKind::Test(request_id) => {
                self.finish_test(&request_id, Ok(delivery), wall);
                return;
            }
            JobKind::Alert(ticket) => ticket,
        };
        let outcome = match delivery {
            Delivery::Accepted { message_id } => NotificationOutcome::Sent { message_id },
            Delivery::NotSent | Delivery::RetryAfter { .. } => NotificationOutcome::RetryLater,
            Delivery::Rejected(_) => NotificationOutcome::Rejected,
            Delivery::Unknown => NotificationOutcome::Unknown,
        };
        self.last_failure = match delivery {
            Delivery::Accepted { .. } => None,
            other => Some(failure_of(other)),
        };
        match delivery {
            Delivery::NotSent => {
                let attempts = self
                    .retries
                    .get(&(ticket.incident_id.clone(), ticket.kind))
                    .map_or(0, |r| r.attempts)
                    .saturating_add(1);
                let wait = FIRST_RETRY
                    .saturating_mul(1_u32 << attempts.saturating_sub(1).min(5))
                    .min(LONGEST_RETRY);
                self.retries.insert(
                    (ticket.incident_id.clone(), ticket.kind),
                    Retry {
                        at: now.checked_add(wait).unwrap_or(now),
                        attempts,
                    },
                );
            }
            Delivery::RetryAfter { seconds } => {
                let wait = Duration::from_secs(u64::from(seconds));
                let attempts = self
                    .retries
                    .get(&(ticket.incident_id.clone(), ticket.kind))
                    .map_or(0, |r| r.attempts);
                self.retries.insert(
                    (ticket.incident_id.clone(), ticket.kind),
                    Retry {
                        at: now.checked_add(wait).unwrap_or(now),
                        attempts,
                    },
                );
            }
            _ => {
                self.retries
                    .remove(&(ticket.incident_id.clone(), ticket.kind));
            }
        }
        if let Err(NotificationError::Storage(_)) = watchdog.record_notification(&ticket, outcome) {
            // Still `reserved` in memory, so it is not sent again; after a
            // restart it becomes `unknown` (plan §7.3). Other errors (the
            // incident closed meanwhile) change nothing worth reporting.
            self.last_failure = Some(NotificationFailure::StorageUnavailable);
        }
    }
}

fn failure_of(delivery: Delivery) -> NotificationFailure {
    match delivery {
        Delivery::NotSent => NotificationFailure::NetworkUnreachable,
        Delivery::RetryAfter { .. } => NotificationFailure::RateLimited,
        Delivery::Rejected(Rejection::Credentials) => NotificationFailure::CredentialsRejected,
        Delivery::Rejected(Rejection::Request) => NotificationFailure::ChatRejected,
        Delivery::Unknown | Delivery::Accepted { .. } => NotificationFailure::OutcomeUnknown,
    }
}
