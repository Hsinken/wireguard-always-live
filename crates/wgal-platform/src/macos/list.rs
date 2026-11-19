//! `scutil --nc list`: one header line, then one line per VPN service:
//!
//! ```text
//! Available network connection services in the current set (*=enabled):
//! * (Connected)      00000000-0000-4000-8000-0000000000B1 VPN (com.wireguard.macos) "Office"            [VPN:com.wireguard.macos]
//! ```
//!
//! That is the enabled flag, the state in parentheses, the service ID, the
//! interface's display name, the service name in double quotes, and the
//! `[type:subtype]` of the service, separated by padding.
//!
//! macOS 27.2.1 writes the type and subtype with a colon, as above (read on a
//! Mac on 2026-11-20). Until then this parser expected a slash
//! (`[VPN/com.wireguard.macos]`), from documentation rather than a device,
//! and refused the colon: one line it could not read rejected the whole list,
//! so no tunnel was found. Both are accepted: nothing shows that no macOS
//! writes the slash.

use super::status::NcState;
use super::text::{Malformed, lines};
use super::uuid::VpnUuid;

pub const HEADER: &str = "Available network connection services in the current set (*=enabled):";

/// `[type:subtype]` of configurations made by the official WireGuard app:
/// with a colon as macOS 27.2.1 writes it, and with the slash this parser
/// first expected. Only these exact texts are WireGuard.
pub const WIREGUARD_KINDS: [&str; 2] = ["VPN:com.wireguard.macos", "VPN/com.wireguard.macos"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub enabled: bool,
    pub state: NcState,
    pub uuid: VpnUuid,
    pub name: String,
    pub kind: String,
}

impl Service {
    pub fn is_wireguard(&self) -> bool {
        WIREGUARD_KINDS.contains(&self.kind.as_str())
    }
}

/// Parses every line; one line that does not parse rejects the whole list,
/// as does a service ID listed twice.
pub fn parse_list(text: &str) -> Result<Vec<Service>, Malformed> {
    let lines = lines(text, false)?;
    let mut iter = lines.into_iter();
    if iter.next() != Some(HEADER) {
        return Err(Malformed("unknown header"));
    }
    let mut services: Vec<Service> = Vec::new();
    for line in iter {
        let service = parse_line(line)?;
        if services.iter().any(|s| s.uuid == service.uuid) {
            return Err(Malformed("service listed twice"));
        }
        services.push(service);
    }
    Ok(services)
}

fn parse_line(line: &str) -> Result<Service, Malformed> {
    let (enabled, rest) = if let Some(rest) = line.strip_prefix("* ") {
        (true, rest)
    } else if let Some(rest) = line.strip_prefix("  ") {
        (false, rest)
    } else {
        return Err(Malformed(
            "service line does not start with the enabled flag",
        ));
    };

    let rest = rest
        .trim_start_matches(' ')
        .strip_prefix('(')
        .ok_or(Malformed("no state in parentheses"))?;
    let (word, rest) = rest
        .split_once(')')
        .ok_or(Malformed("no state in parentheses"))?;
    let state = NcState::parse(word).ok_or(Malformed("unknown connection status"))?;

    let rest = after_padding(rest)?;
    let uuid_text = rest.get(..36).ok_or(Malformed("no service ID"))?;
    let uuid = VpnUuid::parse(uuid_text).ok_or(Malformed("service ID is not a UUID"))?;
    let rest = after_padding(&rest[36..])?.trim_end_matches(' ');

    let (names, kind) = rest
        .strip_suffix(']')
        .and_then(|r| r.rsplit_once(" ["))
        .ok_or(Malformed("no [type] at the end of the line"))?;
    let valid_kind = !kind.is_empty()
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"./_-:".contains(&b));
    if !valid_kind {
        return Err(Malformed("invalid service type"));
    }

    let open = names
        .find('"')
        .ok_or(Malformed("service name is not quoted"))?;
    let close = names.rfind('"').filter(|close| *close > open);
    let close = close.ok_or(Malformed("service name is not quoted"))?;
    if names[..open].trim().is_empty() {
        return Err(Malformed("no interface display name"));
    }
    if !names[close + 1..].trim().is_empty() {
        return Err(Malformed("text after the service name"));
    }

    Ok(Service {
        enabled,
        state,
        uuid,
        name: names[open + 1..close].to_owned(),
        kind: kind.to_owned(),
    })
}

/// Requires at least one space, then skips the rest of the padding.
fn after_padding(text: &str) -> Result<&str, Malformed> {
    if text.starts_with(' ') {
        Ok(text.trim_start_matches(' '))
    } else {
        Err(Malformed("fields are not separated"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B1: &str = "00000000-0000-4000-8000-0000000000B1";

    fn one(line: &str) -> Result<Vec<Service>, Malformed> {
        parse_list(&format!("{HEADER}\n{line}\n"))
    }

    #[test]
    fn parses_a_wireguard_line() {
        let services = one(&format!(
            "* (Connected)      {B1} VPN (com.wireguard.macos) \"Office \\\"A\\\"\"          [VPN/com.wireguard.macos]"
        ))
        .unwrap();
        assert_eq!(
            services,
            [Service {
                enabled: true,
                state: NcState::Connected,
                uuid: VpnUuid::parse(B1).unwrap(),
                name: "Office \\\"A\\\"".into(),
                kind: "VPN/com.wireguard.macos".into(),
            }]
        );
        assert!(services[0].is_wireguard());
    }

    #[test]
    fn an_empty_list_is_just_the_header() {
        assert_eq!(parse_list(&format!("{HEADER}\n")), Ok(vec![]));
    }

    #[test]
    fn rejects_malformed_lines() {
        let cases = [
            (
                format!("x (Connected) {B1} VPN \"a\" [VPN/x]"),
                "service line does not start with the enabled flag",
            ),
            (
                format!("* Connected {B1} VPN \"a\" [VPN/x]"),
                "no state in parentheses",
            ),
            (
                format!("* (Online) {B1} VPN \"a\" [VPN/x]"),
                "unknown connection status",
            ),
            (
                format!("* (Connected){B1} VPN \"a\" [VPN/x]"),
                "fields are not separated",
            ),
            (
                "* (Connected) 00000000-0000-4000-8000-0000000000ZZ VPN \"a\" [VPN/x]".into(),
                "service ID is not a UUID",
            ),
            (
                format!("* (Connected) {B1} VPN \"a\""),
                "no [type] at the end of the line",
            ),
            (
                format!("* (Connected) {B1} VPN \"a\" [VPN x]"),
                "invalid service type",
            ),
            (
                format!("* (Connected) {B1} VPN a [VPN/x]"),
                "service name is not quoted",
            ),
            (
                format!("* (Connected) {B1} \"a\" [VPN/x]"),
                "no interface display name",
            ),
            (
                format!("* (Connected) {B1} VPN \"a\" b [VPN/x]"),
                "text after the service name",
            ),
        ];
        for (line, reason) in cases {
            assert_eq!(one(&line), Err(Malformed(reason)), "{line}");
        }
        assert_eq!(
            parse_list(&format!(
                "Services:\n* (Connected) {B1} VPN \"a\" [VPN/x]\n"
            )),
            Err(Malformed("unknown header"))
        );
    }

    /// The line as macOS 27.2.1 writes it (the ID and name are invented).
    #[test]
    fn parses_the_colon_macos_27_writes() {
        let services = one(&format!(
            "* (Connected)      {B1} VPN (com.wireguard.macos) \"Home\"             [VPN:com.wireguard.macos]"
        ))
        .unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].kind, "VPN:com.wireguard.macos");
        assert_eq!(services[0].name, "Home");
        assert_eq!(services[0].state, NcState::Connected);
        assert!(services[0].is_wireguard());
    }

    /// Another type with a colon: not seen on a device, written here only to
    /// show that it parses and is not taken for WireGuard.
    #[test]
    fn another_type_with_a_colon_parses_but_is_not_wireguard() {
        let services = one(&format!(
            "* (Disconnected)   {B1} PPP --> L2TP       \"Corporate L2TP\"     [PPP:L2TP]"
        ))
        .unwrap();
        assert_eq!(services[0].kind, "PPP:L2TP");
        assert!(!services[0].is_wireguard());
    }

    /// Only the two exact texts are WireGuard: no prefix, case or extra text.
    #[test]
    fn wireguard_is_only_the_exact_types() {
        for kind in [
            "VPN:com.wireguard.macos.extra",
            "VPN:com.wireguard",
            "vpn:com.wireguard.macos",
            "VPN::com.wireguard.macos",
            "VPN-com.wireguard.macos",
        ] {
            let services = one(&format!(
                "* (Connected)      {B1} VPN (com.wireguard.macos) \"Home\"   [{kind}]"
            ))
            .unwrap();
            assert!(!services[0].is_wireguard(), "{kind}");
        }
    }

    /// Characters outside letters, digits and `./_-:` still reject the type,
    /// and with it the whole list.
    #[test]
    fn other_characters_in_the_type_are_still_refused() {
        for kind in [
            "VPN;com.wireguard.macos",
            "VPN com.wireguard.macos",
            "VPN:com.wireguard.macos\u{e9}",
            "",
        ] {
            assert_eq!(
                one(&format!(
                    "* (Connected)      {B1} VPN (com.wireguard.macos) \"Home\"   [{kind}]"
                )),
                Err(Malformed("invalid service type")),
                "{kind:?}"
            );
        }
    }
}
