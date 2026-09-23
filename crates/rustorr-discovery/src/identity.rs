//! The names and identifiers MatriX.145 announces: the Bonjour instance and
//! host names (`server/bonjour`) and the DLNA friendly name and device UUID
//! (`server/dlna`, `anacrolix/dms`).

use md5::{Digest, Md5};

use crate::interfaces::Interface;

const DEFAULT_NAME: &str = "TorrServer";

/// `bonjour.instanceName`: the configured friendly name, cleaned up for a
/// DNS-SD instance label.
pub fn bonjour_instance(friendly_name: &str) -> Vec<u8> {
    if friendly_name.is_empty() {
        return DEFAULT_NAME.into();
    }
    let name = strip_local_suffix(friendly_name.trim()).replace('.', "-");
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return DEFAULT_NAME.into();
    }
    let bytes = name.as_bytes();
    if bytes.len() > 63 {
        // Go slices the string by bytes, even inside a character.
        return trim_ascii_space(&bytes[..63]).to_vec();
    }
    bytes.to_vec()
}

fn trim_ascii_space(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |end| end + 1);
    &bytes[start..end]
}

/// `bonjour.mdnsHostname` for `hostname`: letters, digits and hyphens only.
pub fn bonjour_host(hostname: Option<&str>) -> String {
    let Some(hostname) = hostname.filter(|name| !name.is_empty()) else {
        return "torrserver".into();
    };
    let mapped: String = strip_local_suffix(hostname)
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    if trimmed.is_empty() {
        "torrserver".into()
    } else {
        trimmed.into()
    }
}

/// `stripLocalSuffix`: a trailing `.local` (any case) and dots removed.
fn strip_local_suffix(name: &str) -> &str {
    let name = name.strip_suffix('.').unwrap_or(name);
    let name = if name.len() >= 6
        && name.is_char_boundary(name.len() - 6)
        && name[name.len() - 6..].eq_ignore_ascii_case(".local")
    {
        &name[..name.len() - 6]
    } else {
        name
    };
    name.strip_suffix('.').unwrap_or(name)
}

/// The process's host name, as `os.Hostname` reports it.
pub fn hostname() -> Option<String> {
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
}

/// The full name (GECOS) of the process's user, as Go's `user.Current().Name`.
pub fn user_full_name() -> Option<String> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok()??;
    let gecos = user.gecos.into_string().ok()?;
    // Go keeps the field up to the first comma.
    Some(gecos.split(',').next().unwrap_or_default().to_owned())
}

/// `dlna.getDefaultFriendlyName`: the configured name, or one made of the
/// user and host names.
pub fn dlna_friendly_name(
    configured: &str,
    user: Option<&str>,
    host: Option<&str>,
    interfaces: &[Interface],
) -> String {
    if !configured.is_empty() {
        return configured.into();
    }
    let user = user.unwrap_or_default();
    let host = host.unwrap_or_default();
    if user.is_empty() && host.is_empty() {
        return DEFAULT_NAME.into();
    }
    if !user.is_empty() && !host.is_empty() {
        if user == host {
            return format!("{DEFAULT_NAME}: {user}");
        }
        return format!("{DEFAULT_NAME}: {user} on {host}");
    }
    if host == "localhost" {
        let mut addresses: Vec<String> = interfaces
            .iter()
            .filter(|interface| !interface.loopback && interface.up && interface.multicast)
            .flat_map(|interface| &interface.addresses)
            .filter(|address| address.ip.is_ipv4() && !address.ip.is_loopback())
            .map(|address| address.ip.to_string())
            .collect();
        addresses.sort();
        if let Some(first) = addresses.first() {
            return format!("{DEFAULT_NAME} {first}");
        }
    }
    format!("{DEFAULT_NAME}: {user}@{host}")
}

/// `makeDeviceUuid`: `uuid:` and the MD5 of the friendly name in UUID groups.
pub fn device_uuid(friendly_name: &str) -> String {
    let digest = Md5::digest(friendly_name.as_bytes());
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    format!(
        "uuid:{}-{}-{}-{}-{}",
        hex(&digest[..4]),
        hex(&digest[4..6]),
        hex(&digest[6..8]),
        hex(&digest[8..10]),
        hex(&digest[10..16])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_follow_the_reference_cleanup() {
        assert_eq!(bonjour_instance(""), b"TorrServer");
        assert_eq!(bonjour_instance("   "), b"TorrServer");
        // Observed from MatriX.145: "  My.Media   Server.local.  ".
        assert_eq!(
            bonjour_instance("  My.Media   Server.local.  "),
            b"My-Media Server"
        );
        assert_eq!(bonjour_instance(&"x".repeat(70)), "x".repeat(63).as_bytes());
    }

    #[test]
    fn host_names_keep_letters_digits_and_hyphens() {
        assert_eq!(bonjour_host(Some("discovery-target")), "discovery-target");
        assert_eq!(bonjour_host(Some("My_Box.local")), "My-Box");
        assert_eq!(bonjour_host(Some("__")), "torrserver");
        assert_eq!(bonjour_host(None), "torrserver");
    }

    #[test]
    fn default_dlna_names_combine_user_and_host() {
        assert_eq!(
            dlna_friendly_name("Mine", Some("root"), Some("h"), &[]),
            "Mine"
        );
        assert_eq!(
            dlna_friendly_name("", Some("root"), Some("h"), &[]),
            "TorrServer: root on h"
        );
        assert_eq!(
            dlna_friendly_name("", Some("h"), Some("h"), &[]),
            "TorrServer: h"
        );
        assert_eq!(
            dlna_friendly_name("", Some(""), Some("h"), &[]),
            "TorrServer: @h"
        );
        assert_eq!(dlna_friendly_name("", None, None, &[]), "TorrServer");
    }

    #[test]
    fn device_uuids_are_the_md5_of_the_friendly_name() {
        // Observed from MatriX.145 with FriendlyName "Contract DLNA".
        assert_eq!(
            device_uuid("Contract DLNA"),
            "uuid:9c75442a-03bc-b28c-d59b-05f614916334"
        );
    }
}
