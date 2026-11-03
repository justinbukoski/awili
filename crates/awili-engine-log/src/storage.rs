//! Append-only storage. The trait has no mutation surface — tampering is
//! unrepresentable at the interface level (PS-XVIII-04).

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::LogError;

pub trait AppendOnlyLog {
    /// Durably append bytes. Returns only after the bytes are as durable as
    /// this implementation can make them when `sync` is true.
    fn append(&mut self, bytes: &[u8], sync: bool) -> Result<(), LogError>;
    /// The entire current contents (framing handled by the caller).
    fn read_all(&self) -> Result<Vec<u8>, LogError>;
}

/// In-memory log for tests; supports fault injection at append boundaries.
#[derive(Default)]
pub struct MemLog {
    buf: Vec<u8>,
    /// Fail (and drop the bytes) on the Nth upcoming append (0 = next).
    pub fail_on_append: Option<u64>,
    /// Instead of failing cleanly, write only this many bytes then fail —
    /// simulates a torn write / power cut mid-write.
    pub tear_bytes: Option<usize>,
    appends: u64,
}

impl MemLog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AppendOnlyLog for MemLog {
    fn append(&mut self, bytes: &[u8], _sync: bool) -> Result<(), LogError> {
        if let Some(n) = self.fail_on_append {
            if self.appends == n {
                self.appends += 1;
                if let Some(t) = self.tear_bytes {
                    let t = t.min(bytes.len());
                    self.buf.extend_from_slice(&bytes[..t]);
                }
                return Err(LogError::Storage("injected failure".into()));
            }
        }
        self.appends += 1;
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<u8>, LogError> {
        Ok(self.buf.clone())
    }
}

/// File-backed log. Single segment file, append + optional fsync.
/// (Segment rotation is a follow-up; the format is rotation-ready because
/// every file restates the Header record.)
pub struct FileLog {
    file: File,
    path: std::path::PathBuf,
    /// Test seam (RVS5C-FIX1): write only this many bytes of the NEXT
    /// append, then fail — a torn write on a real file. One-shot.
    #[cfg(test)]
    pub(crate) tear_next_append: Option<usize>,
}

impl FileLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .map_err(|e| LogError::Storage(format!("open {path:?}: {e}")))?;
        Ok(Self {
            file,
            path,
            #[cfg(test)]
            tear_next_append: None,
        })
    }

    #[cfg(test)]
    fn injected_tear(&mut self, bytes: &[u8]) -> Option<Result<(), LogError>> {
        let t = self.tear_next_append.take()?.min(bytes.len());
        if let Err(e) = self.file.write_all(&bytes[..t]) {
            return Some(Err(LogError::Storage(format!("write: {e}"))));
        }
        Some(Err(LogError::Storage("injected torn write".into())))
    }

    #[cfg(not(test))]
    fn injected_tear(&mut self, _bytes: &[u8]) -> Option<Result<(), LogError>> {
        None
    }
}

impl AppendOnlyLog for FileLog {
    fn append(&mut self, bytes: &[u8], sync: bool) -> Result<(), LogError> {
        if let Some(injected) = self.injected_tear(bytes) {
            return injected;
        }
        self.file
            .write_all(bytes)
            .map_err(|e| LogError::Storage(format!("write: {e}")))?;
        if sync {
            self.file
                .sync_data()
                .map_err(|e| LogError::Storage(format!("fsync: {e}")))?;
        }
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<u8>, LogError> {
        let mut f = File::open(&self.path)
            .map_err(|e| LogError::Storage(format!("reopen {:?}: {e}", self.path)))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| LogError::Storage(format!("read: {e}")))?;
        Ok(buf)
    }
}
