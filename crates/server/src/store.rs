//! One room's files under `data/<room>/`.
//!
//! - `log` is append-only. Each record is a little-endian `u32` length
//!   followed by one `LogRecord`'s bytes.
//! - `snapshot` is the latest `Authority::snapshot`, replaced atomically.
//!
//! Restart is snapshot plus log replay. `Authority::restore` skips log records
//! the snapshot covers, so a crash between replacing the snapshot and
//! truncating the log restores the same room.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const LOG: &str = "log";
const SNAPSHOT: &str = "snapshot";
const SNAPSHOT_TMP: &str = "snapshot.tmp";

#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    log: File,
}

/// What a room was persisted as, ready for `Authority::restore`.
#[derive(Debug, Default)]
pub struct Saved {
    pub snapshot: Option<Vec<u8>>,
    pub log: Vec<Vec<u8>>,
}

impl Store {
    /// Opens or creates the room directory and reads what it holds.
    ///
    /// A torn record at the end of the log is a write that crashed before its
    /// flush, so nothing was sent for it. It is cut off here, which keeps later
    /// appends readable.
    pub fn open(dir: &Path) -> io::Result<(Store, Saved)> {
        fs::create_dir_all(dir)?;
        let snapshot = match fs::read(dir.join(SNAPSHOT)) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let mut log = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(dir.join(LOG))?;
        let mut bytes = Vec::new();
        log.read_to_end(&mut bytes)?;
        let (records, whole) = split_records(&bytes);
        if whole < bytes.len() {
            log.set_len(whole as u64)?;
            log.sync_data()?;
        }
        let saved = Saved {
            snapshot,
            log: records,
        };
        Ok((
            Store {
                dir: dir.to_path_buf(),
                log,
            },
            saved,
        ))
    }

    /// Appends one `LogRecord`'s bytes and waits until they are on disk.
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "log record over 4 GiB"))?;
        let mut buf = Vec::with_capacity(4 + bytes.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(bytes);
        self.log.write_all(&buf)?;
        self.log.sync_data()
    }

    /// Replaces the snapshot, then empties the log it covers.
    pub fn checkpoint(&mut self, snapshot: &[u8]) -> io::Result<()> {
        self.write_snapshot(snapshot)?;
        self.log.set_len(0)?;
        self.log.sync_data()
    }

    /// Temp file, fsync, rename, fsync the directory. A crash at any point
    /// leaves either the old snapshot or the new one.
    fn write_snapshot(&self, snapshot: &[u8]) -> io::Result<()> {
        let tmp = self.dir.join(SNAPSHOT_TMP);
        let mut file = File::create(&tmp)?;
        file.write_all(snapshot)?;
        file.sync_all()?;
        fs::rename(&tmp, self.dir.join(SNAPSHOT))?;
        File::open(&self.dir)?.sync_all()
    }
}

/// The complete records in `bytes`, and how many bytes they span.
fn split_records(bytes: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut records = Vec::new();
    let mut at = 0;
    while let Some(head) = bytes.get(at..at + 4) {
        let len = u32::from_le_bytes(head.try_into().expect("four bytes")) as usize;
        let Some(body) = bytes.get(at + 4..at + 4 + len) else {
            break;
        };
        records.push(body.to_vec());
        at += 4 + len;
    }
    (records, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_survive_reopen_and_a_torn_tail_is_cut() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, saved) = Store::open(dir.path()).unwrap();
        assert!(saved.snapshot.is_none() && saved.log.is_empty());
        store.append(b"one").unwrap();
        store.append(b"").unwrap();
        store.append(b"three").unwrap();
        drop(store);

        let mut log = OpenOptions::new()
            .append(true)
            .open(dir.path().join(LOG))
            .unwrap();
        log.write_all(&[9, 0, 0, 0, b'x']).unwrap();
        drop(log);

        let (mut store, saved) = Store::open(dir.path()).unwrap();
        assert_eq!(saved.log, vec![b"one".to_vec(), vec![], b"three".to_vec()]);
        store.append(b"four").unwrap();
        drop(store);
        let (_, saved) = Store::open(dir.path()).unwrap();
        assert_eq!(
            saved.log.len(),
            4,
            "an append after a torn tail is readable"
        );
        assert_eq!(saved.log[3], b"four");
    }

    #[test]
    fn checkpoint_replaces_the_snapshot_and_empties_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, _) = Store::open(dir.path()).unwrap();
        store.append(b"a").unwrap();
        store.checkpoint(b"first").unwrap();
        store.append(b"b").unwrap();
        store.checkpoint(b"second").unwrap();
        store.append(b"c").unwrap();
        drop(store);

        let (_, saved) = Store::open(dir.path()).unwrap();
        assert_eq!(saved.snapshot.as_deref(), Some(&b"second"[..]));
        assert_eq!(saved.log, vec![b"c".to_vec()]);
        assert!(!dir.path().join(SNAPSHOT_TMP).exists());
    }
}
