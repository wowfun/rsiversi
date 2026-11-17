//! Agent-owned JSON staging; callers choose their business byte boundary.
use std::io::Write;

pub(crate) struct BoundedBytes {
    pub(crate) bytes: Vec<u8>,
    pub(crate) maximum: usize,
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.maximum {
            return Err(std::io::Error::other("extension JSON byte capacity"));
        }
        let needed = self.bytes.len() + bytes.len();
        if needed > self.bytes.capacity() {
            let capacity = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.maximum);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_growth_stays_within_the_supplied_budget() {
        let mut output = BoundedBytes {
            bytes: vec![b'['],
            maximum: 101,
        };
        for _ in 0..10 {
            output.write_all(b"0123456789").unwrap();
            assert!(output.bytes.capacity() <= output.maximum);
        }
        assert!(output.write_all(b"x").is_err());
        assert_eq!(output.bytes.len(), 101);
    }
}
