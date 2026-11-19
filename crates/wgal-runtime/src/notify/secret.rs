//! Where the bot token is kept (plan §7.1). Never in the settings file, a
//! log, diagnostics or the GUI; only the watchdog reads it, to send.
//!
//! - Linux: [`FileSecretStore`], a file of its own with mode 0600 in a
//!   directory with mode 0700, both owned by the watchdog's account, apart
//!   from the settings and excluded from any export. This is protection by
//!   permissions, not encryption, and is reported as such.
//! - macOS: [`KeychainSecretStore`], the login user's Keychain. A Keychain
//!   that cannot be read now (locked, access denied) is reported unavailable;
//!   the watchdog never prompts in the background.
//! - Windows: [`DpapiSecretStore`], a file holding the token protected with
//!   DPAPI for the watchdog's account (LocalSystem for the service), so only
//!   that account on this machine can read it. Restricting the directory's
//!   ACL to SYSTEM and Administrators is the installer's job.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use wgal_protocol::BotToken;

/// The secret store cannot be used now: locked, wrong owner or permissions,
/// unreadable, or holding something that is not a token. Never more detail
/// than this, so nothing about the secret can leak through an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecretError;

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the bot token store is unavailable")
    }
}

impl std::error::Error for SecretError {}

pub trait SecretStore: Send + fmt::Debug {
    /// The stored token; `Ok(None)` if none was set.
    fn load(&self) -> Result<Option<BotToken>, SecretError>;
    fn save(&mut self, token: &BotToken) -> Result<(), SecretError>;
    fn clear(&mut self) -> Result<(), SecretError>;
}

#[derive(Debug, Default)]
struct Memory {
    token: Option<BotToken>,
    failing: bool,
}

/// In-memory store for tests, shareable to inspect what was stored.
#[derive(Clone, Debug, Default)]
pub struct MemorySecretStore {
    inner: Arc<Mutex<Memory>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every call fails while set, as a locked Keychain would.
    pub fn set_failing(&self, failing: bool) {
        self.lock().failing = failing;
    }

    pub fn stored(&self) -> Option<BotToken> {
        self.lock().token.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for MemorySecretStore {
    fn load(&self) -> Result<Option<BotToken>, SecretError> {
        let inner = self.lock();
        if inner.failing {
            return Err(SecretError);
        }
        Ok(inner.token.clone())
    }

    fn save(&mut self, token: &BotToken) -> Result<(), SecretError> {
        let mut inner = self.lock();
        if inner.failing {
            return Err(SecretError);
        }
        inner.token = Some(token.clone());
        Ok(())
    }

    fn clear(&mut self) -> Result<(), SecretError> {
        let mut inner = self.lock();
        if inner.failing {
            return Err(SecretError);
        }
        inner.token = None;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub use linux::FileSecretStore;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{self, DirBuilder};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use wgal_protocol::BotToken;

    use super::{SecretError, SecretStore};
    use crate::notify::files;

    const TOKEN_FILE: &str = "telegram-bot-token";

    /// The token in `<dir>/telegram-bot-token`. `dir` must be a real
    /// directory (not a symlink) owned by this process's user with no group
    /// or other permissions; it is created with mode 0700 if missing.
    #[derive(Debug)]
    pub struct FileSecretStore {
        dir: PathBuf,
    }

    impl FileSecretStore {
        pub fn new(dir: &Path) -> Self {
            Self {
                dir: dir.to_path_buf(),
            }
        }

        fn checked_dir(&self, create: bool) -> Result<bool, SecretError> {
            let meta = match fs::symlink_metadata(&self.dir) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                    DirBuilder::new()
                        .mode(0o700)
                        .create(&self.dir)
                        .map_err(|_| SecretError)?;
                    fs::symlink_metadata(&self.dir).map_err(|_| SecretError)?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(SecretError),
            };
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(SecretError);
            }
            owned_privately(&meta)?;
            Ok(true)
        }
    }

    /// Owned by this process's effective user, no access for anyone else.
    fn owned_privately(meta: &fs::Metadata) -> Result<(), SecretError> {
        // /proc/self belongs to the process's effective user.
        let me = fs::metadata("/proc/self").map_err(|_| SecretError)?.uid();
        if meta.uid() != me || meta.permissions().mode() & 0o077 != 0 {
            return Err(SecretError);
        }
        Ok(())
    }

    impl SecretStore for FileSecretStore {
        fn load(&self) -> Result<Option<BotToken>, SecretError> {
            if !self.checked_dir(false)? {
                return Ok(None);
            }
            let path = self.dir.join(TOKEN_FILE);
            if let Ok(meta) = fs::symlink_metadata(&path) {
                if !meta.is_file() {
                    return Err(SecretError);
                }
                owned_privately(&meta)?;
            }
            let Some(bytes) =
                files::read_capped(&path, BotToken::MAX_BYTES).map_err(|_| SecretError)?
            else {
                return Ok(None);
            };
            let text = String::from_utf8(bytes).map_err(|_| SecretError)?;
            BotToken::new(text).map(Some).map_err(|_| SecretError)
        }

        fn save(&mut self, token: &BotToken) -> Result<(), SecretError> {
            self.checked_dir(true)?;
            files::replace(&self.dir.join(TOKEN_FILE), token.expose_secret().as_bytes())
                .map_err(|_| SecretError)
        }

        fn clear(&mut self) -> Result<(), SecretError> {
            if !self.checked_dir(false)? {
                return Ok(());
            }
            files::remove(&self.dir.join(TOKEN_FILE)).map_err(|_| SecretError)
        }
    }
}

#[cfg(windows)]
pub use windows::DpapiSecretStore;

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::path::{Path, PathBuf};

    use wgal_platform::windows::{protect_secret, unprotect_secret};
    use wgal_protocol::BotToken;

    use super::{SecretError, SecretStore};
    use crate::notify::files;

    const TOKEN_FILE: &str = "telegram-bot-token.dpapi";
    const MAX_SEALED_BYTES: usize = 16 * 1024;

    /// The token, DPAPI-protected, in `<dir>/telegram-bot-token.dpapi`. `dir`
    /// must be a real directory, not a link; it is created if missing.
    #[derive(Debug)]
    pub struct DpapiSecretStore {
        dir: PathBuf,
    }

    impl DpapiSecretStore {
        pub fn new(dir: &Path) -> Self {
            Self {
                dir: dir.to_path_buf(),
            }
        }

        fn checked_dir(&self, create: bool) -> Result<bool, SecretError> {
            match fs::symlink_metadata(&self.dir) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(true),
                Ok(_) => Err(SecretError),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                    fs::create_dir(&self.dir).map_err(|_| SecretError)?;
                    Ok(true)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(_) => Err(SecretError),
            }
        }
    }

    impl SecretStore for DpapiSecretStore {
        fn load(&self) -> Result<Option<BotToken>, SecretError> {
            if !self.checked_dir(false)? {
                return Ok(None);
            }
            let Some(sealed) = files::read_capped(&self.dir.join(TOKEN_FILE), MAX_SEALED_BYTES)
                .map_err(|_| SecretError)?
            else {
                return Ok(None);
            };
            let plain = unprotect_secret(&sealed).map_err(|_| SecretError)?;
            let text = String::from_utf8(plain).map_err(|_| SecretError)?;
            BotToken::new(text).map(Some).map_err(|_| SecretError)
        }

        fn save(&mut self, token: &BotToken) -> Result<(), SecretError> {
            self.checked_dir(true)?;
            let sealed =
                protect_secret(token.expose_secret().as_bytes()).map_err(|_| SecretError)?;
            files::replace(&self.dir.join(TOKEN_FILE), &sealed).map_err(|_| SecretError)
        }

        fn clear(&mut self) -> Result<(), SecretError> {
            if !self.checked_dir(false)? {
                return Ok(());
            }
            files::remove(&self.dir.join(TOKEN_FILE)).map_err(|_| SecretError)
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::KeychainSecretStore;

#[cfg(target_os = "macos")]
mod macos {
    use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};
    use security_framework::os::macos::keychain::SecKeychain;
    use security_framework::passwords::{delete_generic_password, set_generic_password};
    use wgal_protocol::BotToken;

    use super::{SecretError, SecretStore};

    const SERVICE: &str = "io.github.hsinken.wg-always-live";
    const ACCOUNT: &str = "telegram-bot-token";
    /// errSecItemNotFound.
    const ITEM_NOT_FOUND: i32 = -25300;

    /// The token as a generic password in the login user's Keychain. Reads
    /// turn the Keychain's user interaction off for their duration
    /// (`SecKeychainSetUserInteractionAllowed`, which covers a locked login
    /// keychain and access-list prompts) and skip items that need user
    /// authentication (`kSecUseAuthenticationUISkip`), so the background
    /// watchdog never brings up a dialog (plan §7.1). Such an item reads as
    /// absent or fails; the notification service then reports the token as
    /// unavailable because it knows one was stored. Not run on a Mac yet
    /// (capability check M9).
    #[derive(Debug, Default)]
    pub struct KeychainSecretStore;

    impl KeychainSecretStore {
        pub fn new() -> Self {
            Self
        }
    }

    impl SecretStore for KeychainSecretStore {
        fn load(&self) -> Result<Option<BotToken>, SecretError> {
            // Restored when the guard drops, right after the search.
            let _no_ui = SecKeychain::disable_user_interaction().map_err(|_| SecretError)?;
            let found = ItemSearchOptions::new()
                .class(ItemClass::generic_password())
                .service(SERVICE)
                .account(ACCOUNT)
                .load_data(true)
                .skip_authenticated_items(true)
                .search();
            match found {
                Ok(items) => match items.into_iter().next() {
                    Some(SearchResult::Data(bytes)) => {
                        let text = String::from_utf8(bytes).map_err(|_| SecretError)?;
                        BotToken::new(text).map(Some).map_err(|_| SecretError)
                    }
                    Some(_) => Err(SecretError),
                    None => Ok(None),
                },
                Err(e) if e.code() == ITEM_NOT_FOUND => Ok(None),
                Err(_) => Err(SecretError),
            }
        }

        fn save(&mut self, token: &BotToken) -> Result<(), SecretError> {
            set_generic_password(SERVICE, ACCOUNT, token.expose_secret().as_bytes())
                .map_err(|_| SecretError)
        }

        fn clear(&mut self) -> Result<(), SecretError> {
            match delete_generic_password(SERVICE, ACCOUNT) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == ITEM_NOT_FOUND => Ok(()),
                Err(_) => Err(SecretError),
            }
        }
    }
}
