//! Recognize line-ending variants of an embedded compatibility profile.
//! Inputs contain reviewed profile text and a stored SHA-256 receipt.
use sha2::{Digest, Sha256};

pub(crate) fn hashes(raw: &str) -> Vec<String> {
    let lf = raw.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    let mut hashes = Vec::with_capacity(3);
    for bytes in [raw.as_bytes(), lf.as_bytes(), crlf.as_bytes()] {
        let hash = format!("{:x}", Sha256::digest(bytes));
        if !hashes.contains(&hash) {
            hashes.push(hash);
        }
    }
    hashes
}

pub(crate) fn matches(raw: &str, expected: &str) -> bool {
    hashes(raw).iter().any(|hash| hash == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_variants_keep_existing_receipts_and_reject_changed_content() {
        let lf = "{\n  \"version\": 1\n}\n";
        let crlf = lf.replace('\n', "\r\n");
        let original = format!("{:x}", Sha256::digest(crlf.as_bytes()));
        assert_eq!(hashes(&crlf)[0], original);
        assert!(matches(lf, &original));
        assert!(matches(&crlf, &hashes(lf)[0]));
        assert!(!matches(&lf.replace("1", "2"), &original));
        assert!(!matches(&lf.replace("  ", " "), &original));
        assert!(!matches(lf, &"f".repeat(64)));
        assert_eq!(hashes("{}").len(), 1);
    }
}
