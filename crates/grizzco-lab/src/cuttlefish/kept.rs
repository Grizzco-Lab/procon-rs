//! Values made from files of the knowledge folder, kept ([`Kept`]): the
//! glossary every view reads and the Studio's weapons and specials
//!
//! Making one reads many files, on a network mount maybe (rclone on
//! Dropbox, where every folder not listed lately is a round trip), and
//! takes seconds; so a value is kept in memory and in the local cache
//! (`cuttlefish::store::cache_dir()`), and a request answers at once with
//! the value kept while a thread looks at the files it was made from
//! (their sizes and times as their folders list them; none is read) and
//! makes it again only when those changed. While none was ever made on
//! this machine, a request answers `202` at once ([`Kept::get`]) and the
//! page asks again. The lab's own changes to those files make it again at
//! once ([`Kept::remake`]), and whatever must see the files as they are now
//! looks at them first ([`Kept::current`]).

use crate::inspect::objects::write_atomic;
use alloc::sync::Arc;
use anyhow::{Result, anyhow};
use core::time::Duration;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Instant, SystemTime};

/// How long a value is answered as it is before its files are looked at
/// again, on the next request
const FRESH: Duration = Duration::from_secs(10);

/// A file by its path in the knowledge folder, with its size and time
pub type FileStamp = (String, u64, Option<SystemTime>);

/// Makes a value from the knowledge folder
pub type Make<T> = Box<dyn Fn(&Path) -> Result<T> + Send + Sync>;

/// A value made from files of the knowledge folder, kept in memory and in
/// the local cache (see the module docs)
pub struct Kept<T> {
    /// What it is, for the log
    what: &'static str,
    /// The knowledge folder
    root: PathBuf,
    /// The copy in the local cache
    file: PathBuf,
    /// The files it is made from
    sources: fn(&Path) -> Vec<PathBuf>,
    make: Make<T>,
    state: Mutex<State<T>>,
    /// Signalled when a look at the files ends
    looked: Condvar,
}

struct State<T> {
    /// The value as last made, with the files it was made from: at first
    /// the local copy of an earlier run
    kept: Option<(Vec<FileStamp>, Arc<T>)>,
    /// The local copy was read (on the first use)
    opened: bool,
    /// A look at the files, or a making, is under way
    looking: bool,
    /// When the files were last looked at
    looked_at: Option<Instant>,
    /// Why the value could not be made the last time
    error: Option<String>,
}

/// The local copy as written
#[derive(Serialize)]
struct CopyOut<'a, T> {
    root: &'a Path,
    files: &'a [FileStamp],
    value: &'a T,
}

/// The local copy as read
#[derive(Deserialize)]
struct CopyIn<T> {
    root: PathBuf,
    files: Vec<FileStamp>,
    value: T,
}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> Kept<T> {
    /// A value of the knowledge folder `root`, made by `make` from the
    /// files `sources` names, kept in `file` too
    pub fn new(
        what: &'static str,
        root: PathBuf,
        file: PathBuf,
        sources: fn(&Path) -> Vec<PathBuf>,
        make: Make<T>,
    ) -> Self {
        Self {
            what,
            root,
            file,
            sources,
            make,
            state: Mutex::new(State {
                kept: None,
                opened: false,
                looking: false,
                looked_at: None,
                error: None,
            }),
            looked: Condvar::new(),
        }
    }

    /// The value kept and whether its files are being looked at again (it
    /// may change then), at once; none while it was never made on this
    /// machine, which a thread makes now (a request answers `202`: ask
    /// again)
    pub fn get(self: &Arc<Self>) -> Result<Option<(Arc<T>, bool)>> {
        self.open();
        self.refresh();
        let state = self.state.lock().unwrap();
        match &state.kept {
            Some((_, value)) => Ok(Some((Arc::clone(value), state.looking))),
            None if state.looking => Ok(None),
            None => Err(anyhow!(
                "{}",
                state.error.as_deref().unwrap_or("not made yet")
            )),
        }
    }

    /// The value kept, waiting only while none was ever made (for what may
    /// wait, such as a change)
    pub fn wait(self: &Arc<Self>) -> Result<Arc<T>> {
        self.open();
        self.refresh();
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some((_, value)) = &state.kept {
                return Ok(Arc::clone(value));
            }
            if !state.looking {
                return Err(anyhow!(
                    "{}",
                    state.error.as_deref().unwrap_or("not made yet")
                ));
            }
            state = self.looked.wait(state).unwrap();
        }
    }

    /// The value as the files are now: they are looked at at once (after a
    /// look under way) and the value made again when they changed
    pub fn current(&self) -> Result<Arc<T>> {
        self.open();
        self.look_now(false)
    }

    /// Makes the value again now, whatever the files' sizes and times say:
    /// after the lab changed one of them
    pub fn remake(&self) -> Result<Arc<T>> {
        self.open();
        self.look_now(true)
    }

    /// Looks at the files on a thread, unless one does or they were
    /// looked at lately
    pub fn refresh(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if state.looking || state.looked_at.is_some_and(|t| t.elapsed() < FRESH) {
            return;
        }
        state.looking = true;
        let kept = Arc::clone(self);
        std::thread::spawn(move || {
            let _ = kept.look(false);
        });
    }

    /// Reads the local copy of an earlier run, once, when it is of this
    /// folder
    fn open(&self) {
        let mut state = self.state.lock().unwrap();
        if state.opened {
            return;
        }
        state.opened = true;
        state.kept = std::fs::read(&self.file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CopyIn<T>>(&bytes).ok())
            .filter(|copy| copy.root == self.root)
            .map(|copy| (copy.files, Arc::new(copy.value)));
    }

    /// Looks at the files now, after a look under way; made again when
    /// they changed, or with `force`
    fn look_now(&self, force: bool) -> Result<Arc<T>> {
        {
            let mut state = self.state.lock().unwrap();
            while state.looking {
                state = self.looked.wait(state).unwrap();
            }
            state.looking = true;
        }
        self.look(force)?;
        let state = self.state.lock().unwrap();
        match &state.kept {
            Some((_, value)) => Ok(Arc::clone(value)),
            None => Err(anyhow!("{} not made", self.what)),
        }
    }

    /// Looks at the files the value is made from and makes it again when
    /// they changed (or with `force`, or none is kept), into the local
    /// cache too; the caller set `looking`
    fn look(&self, force: bool) -> Result<()> {
        // Taken before the value is made: a file changed meanwhile is
        // seen at the next look
        let files = stamps(&self.root, (self.sources)(&self.root));
        let same = !force
            && self
                .state
                .lock()
                .unwrap()
                .kept
                .as_ref()
                .is_some_and(|(kept, _)| *kept == files);
        let made = (!same).then(|| {
            let started = Instant::now();
            let value = (self.make)(&self.root)?;
            log::debug!(
                "The {} made in {} ms",
                self.what,
                started.elapsed().as_millis()
            );
            let copy = CopyOut {
                root: &self.root,
                files: &files,
                value: &value,
            };
            let written = serde_json::to_vec(&copy)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| write_atomic(&self.file, &bytes));
            if let Err(e) = written {
                log::warn!("Could not keep the {}: {:#}", self.what, e);
            }
            Ok::<_, anyhow::Error>(value)
        });
        let mut state = self.state.lock().unwrap();
        state.looking = false;
        state.looked_at = Some(Instant::now());
        self.looked.notify_all();
        match made {
            Some(Ok(value)) => {
                state.kept = Some((files, Arc::new(value)));
                state.error = None;
                Ok(())
            }
            Some(Err(e)) => {
                log::warn!("Could not make the {}: {:#}", self.what, e);
                state.error = Some(format!("{e:#}"));
                Err(e)
            }
            None => Ok(()),
        }
    }
}

/// The sizes and times of `paths`, by their paths in the knowledge folder
/// `root`
fn stamps(root: &Path, paths: Vec<PathBuf>) -> Vec<FileStamp> {
    paths
        .into_iter()
        .map(|path| {
            let meta = std::fs::metadata(&path).ok();
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            let size = meta.as_ref().map_or(0, |m| m.len());
            (name, size, meta.and_then(|m| m.modified().ok()))
        })
        .collect()
}

/// The files of a folder, in name order (none when it cannot be listed)
pub fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// The text of `made.txt` in the folder, counting the makings
    fn kept(root: &Path, file: &Path, makings: &Arc<AtomicUsize>) -> Arc<Kept<String>> {
        let makings = Arc::clone(makings);
        Arc::new(Kept::new(
            "test value",
            root.to_path_buf(),
            file.to_path_buf(),
            |root| vec![root.join("made.txt")],
            Box::new(move |root| {
                makings.fetch_add(1, Ordering::Relaxed);
                Ok(std::fs::read_to_string(root.join("made.txt"))?)
            }),
        ))
    }

    #[test]
    fn values_are_kept_until_their_files_change() {
        let dir = std::env::temp_dir().join(format!("procon-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("knowledge");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("made.txt"), "one").unwrap();
        let file = dir.join("value.json");
        let makings = Arc::new(AtomicUsize::new(0));

        // Nothing kept: a request is answered none (202) while a thread
        // makes it, and it is kept in the local cache
        let first = kept(&root, &file, &makings);
        assert!(first.get().unwrap().is_none());
        assert_eq!(*first.wait().unwrap(), "one");
        assert!(file.is_file());
        // Its files the same: looked at, not made again
        assert_eq!(*first.current().unwrap(), "one");
        assert_eq!(makings.load(Ordering::Relaxed), 1);

        // A later run answers with the local copy at once, and keeps it
        // while the files are the same
        let second = kept(&root, &file, &makings);
        assert_eq!(*second.get().unwrap().unwrap().0, "one");
        assert_eq!(*second.current().unwrap(), "one");
        assert_eq!(makings.load(Ordering::Relaxed), 1);
        // A file changed: made again
        std::fs::write(root.join("made.txt"), "two!").unwrap();
        assert_eq!(*second.current().unwrap(), "two!");
        assert_eq!(makings.load(Ordering::Relaxed), 2);
        // Made again whatever the files say
        assert_eq!(*second.remake().unwrap(), "two!");
        assert_eq!(makings.load(Ordering::Relaxed), 3);

        // The copy of another knowledge folder is not taken; a value that
        // cannot be made is an error
        let other = kept(&dir.join("other"), &file, &makings);
        assert!(other.wait().is_err());
        assert!(other.get().is_err());
        assert!(other.current().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
