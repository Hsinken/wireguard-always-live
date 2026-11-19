//! Mapping between the watchdog's [`TunnelId`]s and the manager's
//! [`TunnelBinding`]s.

use wgal_protocol::{MAX_ID_LEN, TunnelId};

use crate::platform::TunnelBinding;

/// Room left for the hash suffix (`-` and 8 hex digits).
const PREFIX_LEN: usize = MAX_ID_LEN - 9;

/// The ID the watchdog offers for a discovered binding. Deterministic, so the
/// ID a discovery listed is the one a later registration names, and stable
/// across restarts. Bindings that are already valid IDs are used as they are;
/// any other binding is sanitized and given a hash suffix, so two bindings
/// never sanitize to the same ID.
pub fn candidate_id(binding: &TunnelBinding) -> Option<TunnelId> {
    let raw = binding.as_str();
    if let Ok(id) = TunnelId::new(raw) {
        return Some(id);
    }
    let mut sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    sanitized.truncate(PREFIX_LEN);
    TunnelId::new(format!("{sanitized}-{:08x}", fnv1a(raw.as_bytes()))).ok()
}

fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5_u32, |hash, &b| {
        (hash ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(binding: &str) -> String {
        let binding = TunnelBinding::new(binding).expect("valid binding");
        candidate_id(&binding).expect("an ID").as_str().to_owned()
    }

    #[test]
    fn valid_bindings_are_used_as_they_are() {
        assert_eq!(id("wg0"), "wg0");
        assert_eq!(
            id("00000000-0000-4000-8000-000000000001"),
            "00000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn other_bindings_get_distinct_sanitized_ids() {
        let dotted = id("office.vpn");
        let plus = id("office+vpn");
        assert!(dotted.starts_with("office_vpn-"), "{dotted}");
        assert_ne!(dotted, plus);
        assert_ne!(dotted, id("office_vpn"));
        assert_eq!(dotted, id("office.vpn"), "deterministic");
        let long = id(&format!("{}.", "x".repeat(63)));
        assert!(long.len() <= MAX_ID_LEN);
    }
}
