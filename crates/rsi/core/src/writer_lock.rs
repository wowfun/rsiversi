use std::fs::{File, TryLockError};

// Owns the transaction's lock, not merely a descriptor lifetime. A concurrent
// fork can retain the same open file description until its close-on-exec runs.
#[derive(Debug)]
pub(crate) struct WriterLock(File);
impl WriterLock {
    pub(crate) fn acquire(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }
}
impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_writer_does_not_leave_a_lock_on_an_inherited_file_description() {
        let temporary = tempfile::tempdir().unwrap();
        let open = || File::open(temporary.path()).unwrap();
        let acquire = || WriterLock::acquire(open());
        let lock = acquire().unwrap();
        // dup and fork both retain the same open file description. Keep it live
        // deterministically without depending on a child reaching exec slowly.
        let inherited = lock.0.try_clone().unwrap();
        assert!(matches!(acquire(), Err(TryLockError::WouldBlock)));
        drop(lock);
        let next = acquire().expect("the finished writer explicitly unlocks");
        drop(inherited);
        assert!(matches!(acquire(), Err(TryLockError::WouldBlock)));
        drop(next);
        assert!(acquire().is_ok());
    }
}
