use sha2::{Digest, Sha256};

pub fn calculate_bytes_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_hash_is_stable() {
        assert_eq!(
            calculate_bytes_hash(b"hello"),
            calculate_bytes_hash(b"hello")
        );
        assert_ne!(
            calculate_bytes_hash(b"hello"),
            calculate_bytes_hash(b"world")
        );
    }
}
