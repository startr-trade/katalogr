//! Git blob ids.

use sha1::{Digest, Sha1};

/// Same value as `git hash-object`: SHA-1 of `"blob <len>\0" + content`, lower-case hex.
pub fn git_blob_id(content: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", content.len()).as_bytes());
    h.update(content);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_git_hash_object() {
        assert_eq!(
            git_blob_id(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
}
