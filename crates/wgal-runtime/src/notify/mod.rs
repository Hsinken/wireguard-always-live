//! Telegram notifications (plan §7, work package WGW-001-G): settings, the
//! bot token store, message text and its templates, the Bot API transport
//! and the service that ties them to the core's incident state.
//!
//! Nothing here decides whether an incident deserves an alert; the core does
//! (`Watchdog::notifications_due`). Nothing here talks to the GUI except
//! through the contract's requests and `NotificationStatus`, which never
//! carries the token.

mod files;
mod message;
mod secret;
mod service;
mod settings;
#[cfg(feature = "telegram")]
mod telegram;
mod transport;

pub use message::{
    BUILTIN_FORMAL, BUILTIN_ID, BUILTIN_RESULT_FAILED, BUILTIN_RESULT_NOT_EXECUTED, BUILTIN_TEST,
    BUILTIN_UNNAMED_DEVICE, MAX_MESSAGE_CHARS, builtin_templates, formal_message, message_times,
    test_message, utc,
};
#[cfg(windows)]
pub use secret::DpapiSecretStore;
#[cfg(target_os = "linux")]
pub use secret::FileSecretStore;
#[cfg(target_os = "macos")]
pub use secret::KeychainSecretStore;
pub use secret::{MemorySecretStore, SecretError, SecretStore};
pub use service::{Job, Notifier, Pending};
pub use settings::{
    FileSettingsStore, MemorySettingsStore, NotificationSettings, SETTINGS_FILE, SETTINGS_VERSION,
    SettingsError, SettingsStore,
};
#[cfg(feature = "telegram")]
pub use telegram::HttpTransport;
pub use transport::{
    API_HOST, DEFAULT_RETRY_AFTER_SECS, Delivery, MAX_RESPONSE_BYTES, MAX_RETRY_AFTER_SECS,
    Rejection, Transport, classify_response,
};
