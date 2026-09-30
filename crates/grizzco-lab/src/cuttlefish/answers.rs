//! Answers of the model that take longer than a request may: a chat
//! message (a minute or more while the model looks things up), a
//! translation, an eval question asked again.
//!
//! The `POST` starts the work on a thread of its own and answers at once
//! with the job, `{"job": "<id>"}`; the page then asks `GET
//! /api/cuttlefish/answer?job=<id>`, which is `202` while the work runs
//! (the page's request queue asks again twice a second, each a moment's
//! work) and then the answer, or its error with the status it failed with.
//! So a long call holds none of the page's few connections to the lab. An
//! answer is kept for [`KEPT`] after it is made; ids carry the lab's start
//! time, so an id of an earlier run is never another answer. The thread is
//! named for the exit while it works (`crate::exit::Busy`).

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use serde_json::{Value, json};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;
use warp::http::StatusCode;

use crate::cuttlefish::knowledge::Status;

/// How long an answer is kept once made
pub const KEPT: Duration = Duration::from_secs(600);

/// One piece of work: running, or its result and when it was made
struct Job {
    result: Option<Result<Value, (StatusCode, String)>>,
    made: Option<Instant>,
}

/// The answers being made and made lately
pub struct Answers {
    /// The lab's start, in the ids
    run: String,
    next: AtomicU64,
    jobs: Mutex<BTreeMap<String, Job>>,
}

impl Default for Answers {
    fn default() -> Self {
        Answers {
            run: alloc::format!("{:x}", crate::cuttlefish::knowledge::now_ms()),
            next: AtomicU64::new(0),
            jobs: Mutex::default(),
        }
    }
}

impl Answers {
    /// Starts `work` on a thread of its own, named `what` while it runs;
    /// answers with `{"job": "<id>"}`
    pub fn start(
        self: &Arc<Self>,
        what: String,
        work: impl FnOnce() -> Result<Value, Status> + Send + 'static,
    ) -> Value {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let id = alloc::format!("{}-{n}", self.run);
        self.lock().insert(
            id.clone(),
            Job {
                result: None,
                made: None,
            },
        );
        let answers = Arc::clone(self);
        let job = id.clone();
        let busy = crate::exit::Busy::new(what);
        std::thread::spawn(move || {
            let result = work().map_err(|Status(status, e)| (status, alloc::format!("{e:#}")));
            drop(busy);
            if let Some(j) = answers.lock().get_mut(&job) {
                j.result = Some(result);
                j.made = Some(Instant::now());
            }
        });
        json!({ "job": id })
    }

    /// The answer of job `id`: `202` while it is made, the answer, or its
    /// error; `404` for a job this run never had or no longer keeps
    pub fn get(&self, id: &str) -> Result<Value, Status> {
        let mut jobs = self.lock();
        jobs.retain(|_, j| j.made.is_none_or(|at| at.elapsed() < KEPT));
        match jobs.get(id).map(|j| &j.result) {
            None => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no answer {id}: the lab started again, or it was made long ago"),
            )),
            Some(None) => Err(Status(
                StatusCode::ACCEPTED,
                anyhow::anyhow!("still answering; ask again"),
            )),
            Some(Some(Ok(answer))) => Ok(answer.clone()),
            Some(Some(Err((status, e)))) => Err(Status(*status, anyhow::anyhow!("{e}"))),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks for an answer until it is made
    fn wait(answers: &Answers, id: &str) -> Result<Value, Status> {
        for _ in 0..200 {
            match answers.get(id) {
                Err(Status(StatusCode::ACCEPTED, _)) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                done => return done,
            }
        }
        panic!("no answer");
    }

    #[test]
    fn answers_come_later_and_stay() {
        let answers = Arc::new(Answers::default());
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let started = answers.start(String::from("a test"), move || {
            rx.recv().unwrap();
            Ok(json!({"text": "done"}))
        });
        let id = String::from(started["job"].as_str().unwrap());
        assert!(id.starts_with(&answers.run));
        // Being made: 202, the work named for the exit
        let Err(Status(status, _)) = answers.get(&id) else {
            panic!("answered too soon")
        };
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(crate::exit::busy().iter().any(|b| b == "a test"));
        tx.send(()).unwrap();
        assert_eq!(wait(&answers, &id).unwrap()["text"], "done");
        // Kept: asked again, the same answer
        assert_eq!(answers.get(&id).unwrap()["text"], "done");
        // A failure keeps its status
        let failed = answers.start(String::from("a failing test"), || {
            Err(Status(
                StatusCode::BAD_GATEWAY,
                anyhow::anyhow!("the model failed"),
            ))
        });
        let Err(Status(status, e)) = wait(&answers, failed["job"].as_str().unwrap()) else {
            panic!("no failure")
        };
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(e.to_string(), "the model failed");
        // Unknown ids, and an earlier run's
        let Err(Status(status, _)) = answers.get("0-0") else {
            panic!("an unknown answer")
        };
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
