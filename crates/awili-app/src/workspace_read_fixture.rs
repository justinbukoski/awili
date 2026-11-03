//! Only the diagnostic's exclusively created file is eligible for cleanup.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// (device, inode) on Unix. TODO(windows-phase1): the volume serial and file
/// index (`GetFileInformationByHandle`); until then the creation time stands in
/// for the identity so a replaced file is still caught.
#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}
#[cfg(not(unix))]
fn identity(meta: &std::fs::Metadata) -> (u64, u64) {
    let created = meta
        .created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    (0, created)
}

pub(crate) struct Challenge {
    pub id: String,
    pub nonce: String,
    pub path: PathBuf,
    device: u64,
    inode: u64,
}

impl Challenge {
    pub fn create(folder: &Path) -> std::io::Result<Self> {
        let mut random = [0u8; 48];
        #[cfg(unix)]
        {
            use std::io::Read;
            std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        }
        #[cfg(not(unix))]
        for chunk in random.chunks_mut(16) {
            chunk.copy_from_slice(&uuid::Uuid::new_v4().into_bytes()[..chunk.len()]);
        }
        random[6] = (random[6] & 15) | 64;
        random[8] = (random[8] & 63) | 128;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        );
        let nonce = hex[32..].to_string();
        let path = folder.join(format!(".awili-read-challenge-{id}.txt"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        writeln!(file, "{nonce}")?;
        file.sync_all()?;
        let meta = file.metadata()?;
        let (device, inode) = identity(&meta);
        Ok(Self {
            id,
            nonce,
            path,
            device,
            inode,
        })
    }

    pub fn request(&self) -> String {
        format!("Workspace read diagnostic: read the file at {} and return its complete content as a separate exact line. Do not modify files, dispatch reviews, or write to external services.", self.path.display())
    }

    pub fn cleanup(&self, termination_confirmed: bool) -> std::io::Result<bool> {
        if !termination_confirmed {
            return Ok(false);
        }
        let meta = std::fs::symlink_metadata(&self.path)?;
        if !meta.is_file() || identity(&meta) != (self.device, self.inode) {
            return Ok(false);
        }
        if std::fs::read(&self.path)? != format!("{}\n", self.nonce).as_bytes() {
            return Ok(false);
        }
        std::fs::remove_file(&self.path)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_unique_files_and_cleanup_require_termination() {
        let dir = tempfile::tempdir().unwrap();
        let a = Challenge::create(dir.path()).unwrap();
        let b = Challenge::create(dir.path()).unwrap();
        assert!(!a.request().contains(&a.nonce));
        assert!(a.request().contains(a.path.to_str().unwrap()));
        assert_ne!(a.id, b.id);
        assert_ne!(a.nonce, b.nonce);
        assert!(!a.cleanup(false).unwrap());
        assert!(a.path.exists());
        assert!(a.cleanup(true).unwrap());
        assert!(b.path.exists());
    }
    #[test]
    fn modified_or_replaced_fixture_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let a = Challenge::create(dir.path()).unwrap();
        std::fs::write(&a.path, "user content").unwrap();
        assert!(!a.cleanup(true).unwrap());
        std::fs::remove_file(&a.path).unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, "keep").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&other, &a.path).unwrap();
            assert!(!a.cleanup(true).unwrap());
            assert_eq!(std::fs::read_to_string(other).unwrap(), "keep");
        }
    }
}
