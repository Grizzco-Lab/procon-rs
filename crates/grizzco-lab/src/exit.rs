//! The lab's exit: the step under way and the blocking work still in
//! flight, so a slow exit names what it waits for
//!
//! The web server runs its file, ffmpeg and model work on tokio's blocking
//! threads, and a tokio runtime lets the process go only once every one of
//! them has returned (dropping it waits without a limit): a read on the
//! Dropbox mount with a cold cache, ffmpeg over a whole video there, or an
//! answer from the model can take minutes. That work goes through
//! [`blocking`], which names it while it runs ([`Busy`]); `main` gives it
//! a moment ([`busy`] empty), then shuts the runtime down without waiting
//! and names what it leaves unfinished. A second Ctrl-C exits at once,
//! naming the [`step`] it cut short and the work in flight ([`doing`]).

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// Blocking work in flight by a number of its own, oldest first
static BUSY: Mutex<BTreeMap<u64, String>> = Mutex::new(BTreeMap::new());

/// The number the next [`Busy`] gets
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The step of the exit under way; empty until the exit starts
static STEP: Mutex<String> = Mutex::new(String::new());

/// Longest query value [`request`] names in full
const VALUE_CHARS: usize = 120;

/// Lock a mutex even if a holder panicked; the names stay usable
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Blocking work in flight, named until it is dropped
pub struct Busy(u64);

impl Busy {
    pub fn new(what: impl Into<String>) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        lock(&BUSY).insert(id, what.into());
        Self(id)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        lock(&BUSY).remove(&self.0);
    }
}

/// Run `work` on tokio's blocking threads, named `what` while it runs,
/// awaited or not
pub async fn blocking<T: Send + 'static>(
    what: impl Into<String>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, tokio::task::JoinError> {
    let busy = Busy::new(what);
    tokio::task::spawn_blocking(move || {
        let _busy = busy;
        work()
    })
    .await
}

/// A request as [`Busy`] names it: `GET /api/…?key=value…`, the query in
/// key order, long values cut short
pub fn request(method: &str, path: &str, query: &HashMap<String, String>) -> String {
    let mut pairs: Vec<(&String, &String)> = query.iter().collect();
    pairs.sort();
    let query: Vec<String> = pairs
        .into_iter()
        .map(|(key, value)| match value.char_indices().nth(VALUE_CHARS) {
            Some((end, _)) => format!("{key}={}…", &value[..end]),
            None => format!("{key}={value}"),
        })
        .collect();
    match query.is_empty() {
        true => format!("{method} {path}"),
        false => format!("{method} {path}?{}", query.join("&")),
    }
}

/// The blocking work in flight, oldest first
pub fn busy() -> Vec<String> {
    lock(&BUSY).values().cloned().collect()
}

/// Start a step of the exit: logged, and named by [`doing`]
pub fn step(what: &str) {
    log::info!("Exiting: {what}");
    *lock(&STEP) = what.to_string();
}

/// What the exit is doing: its step, and the blocking work in flight the
/// step does not name
pub fn doing() -> String {
    let step = lock(&STEP).clone();
    let mut busy = busy();
    busy.retain(|what| !step.contains(what.as_str()));
    match (step.is_empty(), busy.is_empty()) {
        (_, true) => step,
        (true, false) => format!("still running: {}", busy.join(", ")),
        (false, false) => format!("{step}; still running: {}", busy.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_is_named_while_it_runs() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let answer = rt.block_on(blocking("GET /api/test/answer", || {
            assert!(busy().iter().any(|what| what == "GET /api/test/answer"));
            42
        }));
        assert_eq!(answer.unwrap(), 42);
        assert!(!busy().iter().any(|what| what == "GET /api/test/answer"));
        let held = Busy::new("POST /api/test/held");
        assert!(doing().contains("POST /api/test/held"));
        drop(held);
        assert!(!doing().contains("POST /api/test/held"));
    }

    #[test]
    fn requests_are_named_with_their_query() {
        let query = HashMap::from([
            (String::from("t_ms"), String::from("1500")),
            (String::from("kind"), String::from("file")),
            (String::from("ref"), "x".repeat(200)),
        ]);
        let name = request("GET", "/api/cuttlefish/thumb", &query);
        let long = format!("ref={}…", "x".repeat(VALUE_CHARS));
        assert_eq!(
            name,
            format!("GET /api/cuttlefish/thumb?kind=file&{long}&t_ms=1500")
        );
        assert_eq!(
            request("GET", "/api/pipeline/state", &HashMap::new()),
            "GET /api/pipeline/state"
        );
    }
}
