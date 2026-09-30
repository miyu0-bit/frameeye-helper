//! The frameeyeosc release that ships inside this program, so installing needs no download.

use sha2::{Digest, Sha256};

/// The bundled release (github.com/sasaken1102r/frameeyeosc, tag v0.5.3).
pub const VERSION: &str = "0.5.3";
pub const TARBALL: &[u8] = include_bytes!("../assets/frameeyeosc-0.5.3-steamframe-aarch64.tar.gz");
/// From the release's own SHA256SUMS file.
pub const TARBALL_SHA256: &str = "553abc2b8f2842f2a4bb5ed9e3452e5bdbe1a511dba47439f058a93d2368cf1c";
/// The `frameeyeosc` binary inside it, to recognise this version when the panel isn't installed.
pub const DAEMON_SHA256: &str = "a021fda0fb83cf4760201ded03f4158c644f95e1438cc1de1e48e93e452cdfd5";

pub const PROJECT_URL: &str = "https://github.com/miyu0-bit/frameeye-helper";
pub const UPSTREAM_URL: &str = "https://github.com/sasaken1102r/frameeyeosc";
pub const ORIGINAL_URL: &str = "https://github.com/konsti219/frameeyeosc";
pub const LICENSE: &str = include_str!("../assets/FRAMEEYEOSC_LICENSE.txt");

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// True if the embedded tarball is intact.
pub fn verify() -> bool {
    sha256_hex(TARBALL) == TARBALL_SHA256
}

/// Compare dotted versions ("0.5.3" vs "0.10.0"); non-numeric parts count as 0.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .take(3)
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let (mut a, mut b) = (parse(a), parse(b));
    a.resize(3, 0);
    b.resize(3, 0);
    a.cmp(&b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn bundled_tarball_matches_release_checksum() {
        assert!(verify());
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(compare_versions("0.5.3", "0.5.3"), Ordering::Equal);
        assert_eq!(compare_versions("0.4.0", "0.5.3"), Ordering::Less);
        assert_eq!(compare_versions("0.10.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("v0.6", "0.5.3"), Ordering::Greater);
    }
}
