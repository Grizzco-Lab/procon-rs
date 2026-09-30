//! The knowledge folder's write lock, `<knowledge>/.lock`.
//!
//! Every writer (the lab's import jobs, the CLI's `ingest`, `delete` and
//! `reindex`) holds it while writing ([`acquire`]); reading needs no lock.
//! The lock is an exclusive `flock` on the file, which the kernel releases
//! when the process ends, however it ends. The holder also writes who it is
//! into the file (program, pid, host, start time) and clears it when done,
//! for two reasons: a writer that finds the lock held can say who holds it
//! ([`Busy`]); and `flock` does not reach across machines, so on a synced
//! folder (Dropbox) a record naming another host tells a second machine
//! that an instance there may be writing. A record left by a process of
//! this host that is gone is stale and taken over; one of another host
//! stays until that host's writer clears it, or the user deletes the file.

use alloc::string::String;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use core::fmt;
use serde::{Deserialize, Serialize};
use std::fs::{File, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// The lock file in the knowledge folder
pub const FILE: &str = ".lock";

/// Who holds the lock, as written into the file
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    /// What writes (`cuttlefish ingest`, `grizzco-lab import`)
    pub program: String,
    pub host: String,
    pub since: DateTime<Utc>,
}

impl fmt::Display for Holder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} pid {} on {} since {}",
            self.program,
            self.pid,
            self.host,
            self.since.format("%Y-%m-%d %H:%M:%S UTC")
        )
    }
}

/// The lock is held by someone else
#[derive(Clone, Debug, PartialEq)]
pub struct Busy {
    /// The record in the file, when it has one
    pub holder: Option<Holder>,
    /// The lock file
    pub path: PathBuf,
}

impl fmt::Display for Busy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.holder {
            Some(h) => write!(f, "the knowledge store is being written by {h}")?,
            None => write!(f, "the knowledge store is being written by another process")?,
        }
        write!(
            f,
            "; wait for it, or if that process is gone, delete {}",
            self.path.display()
        )
    }
}

impl std::error::Error for Busy {}

/// This machine's name
pub fn host_name() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| String::from(h.trim()))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| String::from("unknown host"))
}

/// Whether a process of this host with this pid is running
fn running(pid: u32) -> bool {
    Path::new(&alloc::format!("/proc/{pid}")).exists()
}

/// The write lock, held until dropped; dropping clears the record
#[derive(Debug)]
pub struct WriteLock {
    file: File,
}

/// The record in an open lock file, if it holds one
fn read_holder(file: &mut File) -> Option<Holder> {
    let mut text = String::new();
    file.rewind().ok()?;
    file.read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

/// Takes the write lock of the knowledge folder `root` for `program`, or
/// fails with [`Busy`] (as the error's root cause) when another writer has
/// it: a process of this machine, or a record of another machine (see the
/// module's notes). Creates the folder when its parent exists.
pub fn acquire(root: &Path, program: &str) -> Result<WriteLock> {
    let path = root.join(FILE);
    if !root.is_dir() {
        let parent = root.parent().filter(|p| !p.as_os_str().is_empty());
        ensure!(
            parent.is_none_or(Path::is_dir),
            "{} does not exist (is the synced folder there?)",
            parent.unwrap_or(root).display()
        );
        std::fs::create_dir_all(root)
            .with_context(|| alloc::format!("creating {}", root.display()))?;
    }
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| alloc::format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let holder = read_holder(&mut file);
            return Err(Busy { holder, path }.into());
        }
        Err(TryLockError::Error(e)) => {
            return Err(e).with_context(|| alloc::format!("locking {}", path.display()));
        }
    }
    let host = host_name();
    let pid = std::process::id();
    if let Some(h) = read_holder(&mut file) {
        // Another machine's writer, which flock does not see; or one of
        // this machine on a file that was replaced (a sync) since it locked
        let elsewhere = h.host != host;
        let here = h.host == host && h.pid != pid && running(h.pid);
        if elsewhere || here {
            return Err(Busy {
                holder: Some(h),
                path,
            }
            .into());
        }
    }
    let holder = Holder {
        pid,
        program: String::from(program),
        host,
        since: Utc::now(),
    };
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(&serde_json::to_vec(&holder)?)?;
    file.sync_all()?;
    Ok(WriteLock { file })
}

impl Drop for WriteLock {
    /// Clears the record; closing the file releases the flock
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-lock-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn busy(e: anyhow::Error) -> Busy {
        e.downcast::<Busy>().expect("a Busy error")
    }

    #[test]
    fn one_writer_at_a_time_and_who_it_is() {
        let root = temp("one");
        let lock = acquire(&root, "cuttlefish ingest").unwrap();
        let record: Holder =
            serde_json::from_slice(&std::fs::read(root.join(FILE)).unwrap()).unwrap();
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.program, "cuttlefish ingest");
        assert_eq!(record.host, host_name());

        // A second writer (flock is per open file, so this process too)
        let err = busy(acquire(&root, "grizzco-lab import").unwrap_err());
        assert_eq!(err.holder.as_ref(), Some(&record));
        let text = err.to_string();
        assert!(
            text.starts_with("the knowledge store is being written by cuttlefish ingest pid "),
            "{text}"
        );
        assert!(text.contains(&alloc::format!(" on {} since ", record.host)));
        assert!(text.ends_with(&alloc::format!(
            "; wait for it, or if that process is gone, delete {}",
            root.join(FILE).display()
        )));

        // Released and cleared when dropped; reading never needed it
        drop(lock);
        assert!(std::fs::read(root.join(FILE)).unwrap().is_empty());
        let again = acquire(&root, "grizzco-lab import").unwrap();
        drop(again);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn records_left_behind() {
        let root = temp("left");
        std::fs::create_dir_all(&root).unwrap();
        let write = |h: &Holder| std::fs::write(root.join(FILE), serde_json::to_vec(h).unwrap());
        let mut left = Holder {
            pid: u32::MAX - 1,
            program: String::from("cuttlefish ingest"),
            host: host_name(),
            since: Utc::now(),
        };
        // This host, the process gone (flock free): stale, taken over
        write(&left).unwrap();
        drop(acquire(&root, "cuttlefish reindex").unwrap());
        // Another host (a synced folder): refused, named
        left.host = String::from("laptop-elsewhere");
        write(&left).unwrap();
        let err = busy(acquire(&root, "cuttlefish ingest").unwrap_err());
        assert!(
            err.to_string()
                .contains("pid 4294967294 on laptop-elsewhere")
        );
        // Until the file is deleted, as the message says
        std::fs::remove_file(root.join(FILE)).unwrap();
        drop(acquire(&root, "cuttlefish ingest").unwrap());
        // Garbage in the file is no record
        std::fs::write(root.join(FILE), b"{half").unwrap();
        drop(acquire(&root, "cuttlefish ingest").unwrap());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_synced_folder_is_not_made() {
        let root = temp("missing").join("Dropbox").join("Knowledge");
        let err = acquire(&root, "cuttlefish ingest").unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }
}
