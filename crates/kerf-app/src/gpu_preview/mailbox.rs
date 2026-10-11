//! A latest-wins mailbox: one request is worked on at a time, and of the requests that arrive
//! meanwhile only the newest is kept.
//!
//! A scrub posts a frame request per playhead move. Waiting for each answer before asking for the
//! next left the renderer idle for the round trip between two frames, and answering every request in
//! order would draw frames nobody is looking at any more. Here a request never waits behind another:
//! it replaces the one waiting (whose sender is told it was [`Delivery::Superseded`], at once), and the
//! worker, when it finishes a frame, takes whatever is waiting then. The last request of a drag is
//! therefore always drawn; the ones in between are drawn when they happen to be the newest.
//!
//! The mailbox starts no thread: [`Mailbox::post`] says whether the caller has to start the worker
//! (it is idle), and the worker loops on [`Mailbox::next`] until it returns `None`, at which point it
//! is idle again. A worker that dies on the way ([`Worker`] dropped by a panic) hands the mailbox back
//! and tells the requests it never answered ([`Delivery::Abandoned`]).

use std::sync::{Mutex, PoisonError};

use tokio::sync::oneshot;

/// What a request got in the end.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery<T> {
    /// It was drawn (or failed to be): the worker's answer.
    Done(T),
    /// A newer request replaced it while it waited; it was never drawn.
    Superseded,
    /// The worker died with it unanswered (a panic): an error, not a skipped frame.
    Abandoned,
}

struct State<R, T> {
    /// A worker is running (or about to be started by the `post` that made this true).
    busy: bool,
    waiting: Option<(R, oneshot::Sender<Delivery<T>>)>,
    posted: u64,
    superseded: u64,
}

/// See the module docs.
pub struct Mailbox<R, T> {
    state: Mutex<State<R, T>>,
}

impl<R, T> Default for Mailbox<R, T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                busy: false,
                waiting: None,
                posted: 0,
                superseded: 0,
            }),
        }
    }
}

/// What the mailbox counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MailboxStats {
    pub posted: u64,
    pub superseded: u64,
}

impl<R, T> Mailbox<R, T> {
    fn state(&self) -> std::sync::MutexGuard<'_, State<R, T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Post `request`. The receiver gets its [`Delivery`]; the flag is whether the caller must start
    /// the worker, which is so exactly when none is running (and then one is, as far as the mailbox
    /// is concerned: `post` has marked it busy).
    pub fn post(&self, request: R) -> (oneshot::Receiver<Delivery<T>>, bool) {
        let (tx, rx) = oneshot::channel();
        let mut st = self.state();
        st.posted += 1;
        if let Some((_, older)) = st.waiting.replace((request, tx)) {
            st.superseded += 1;
            // A caller that gave up has dropped its receiver; nobody to tell.
            let _ = older.send(Delivery::Superseded);
        }
        let start = !st.busy;
        st.busy = true;
        (rx, start)
    }

    /// The worker's next job: the newest waiting request, or `None` (and the mailbox is idle: the
    /// next `post` starts a worker) when nothing waits.
    pub fn next(&self) -> Option<(R, Reply<T>)> {
        let mut st = self.state();
        match st.waiting.take() {
            Some((request, tx)) => Some((request, Reply(Some(tx)))),
            None => {
                st.busy = false;
                None
            }
        }
    }

    /// The worker is gone without having drained the mailbox (a panic, a thread that never
    /// started): whatever waits is told so, and the next `post` starts a new worker.
    pub fn abandon(&self) {
        let mut st = self.state();
        st.busy = false;
        if let Some((_, tx)) = st.waiting.take() {
            let _ = tx.send(Delivery::Abandoned);
        }
    }

    pub fn stats(&self) -> MailboxStats {
        let st = self.state();
        MailboxStats {
            posted: st.posted,
            superseded: st.superseded,
        }
    }
}

/// Where the answer to the request being drawn goes. Dropped unanswered (the worker panicked), the
/// request is told so.
pub struct Reply<T>(Option<oneshot::Sender<Delivery<T>>>);

impl<T> Reply<T> {
    pub fn send(mut self, answer: T) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(Delivery::Done(answer));
        }
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(Delivery::Abandoned);
        }
    }
}

/// The worker's end of the mailbox. Dropped before [`Worker::next`] has returned `None` (a panic
/// unwinding the loop) the mailbox is told the worker is gone, so a request posted after is not
/// left waiting for a worker that no longer exists.
pub struct Worker<'a, R, T> {
    mail: &'a Mailbox<R, T>,
    drained: bool,
}

impl<'a, R, T> Worker<'a, R, T> {
    pub fn new(mail: &'a Mailbox<R, T>) -> Self {
        Self { mail, drained: false }
    }

    /// [`Mailbox::next`].
    pub fn next(&mut self) -> Option<(R, Reply<T>)> {
        let next = self.mail.next();
        self.drained = next.is_none();
        next
    }
}

impl<R, T> Drop for Worker<'_, R, T> {
    fn drop(&mut self) {
        if !self.drained {
            self.mail.abandon();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn drain(mail: &Mailbox<u32, u32>, draw: impl Fn(u32) -> u32) -> Vec<u32> {
        let mut drawn = Vec::new();
        while let Some((request, reply)) = mail.next() {
            drawn.push(request);
            reply.send(draw(request));
        }
        drawn
    }

    #[test]
    fn the_first_post_starts_the_worker_and_one_that_arrives_while_it_runs_does_not() {
        let mail = Mailbox::<u32, u32>::default();
        let (_a, start) = mail.post(1);
        assert!(start);
        let (_b, start) = mail.post(2);
        assert!(!start, "a worker is already on its way");
        assert!(drain(&mail, |r| r).len() == 1);
        let (_c, start) = mail.post(3);
        assert!(start, "idle again once the mailbox ran dry");
    }

    #[test]
    fn of_the_requests_that_wait_only_the_newest_is_drawn_and_the_rest_are_told_so() {
        let mail = Mailbox::<u32, u32>::default();
        let (mut first, start) = mail.post(1);
        assert!(start);
        // The worker takes request 1 and is drawing it while 2, 3 and 4 arrive.
        let (one, reply) = mail.next().expect("a request");
        assert_eq!(one, 1);
        let (mut two, _) = mail.post(2);
        let (mut three, _) = mail.post(3);
        let (mut four, _) = mail.post(4);
        assert_eq!(
            two.try_recv().unwrap(),
            Delivery::Superseded,
            "told at once, not when the worker is done"
        );
        assert_eq!(three.try_recv().unwrap(), Delivery::Superseded);
        assert!(four.try_recv().is_err(), "the newest waits for its turn");
        reply.send(10);
        assert_eq!(first.try_recv().unwrap(), Delivery::Done(10));
        assert_eq!(drain(&mail, |r| r * 10), vec![4]);
        assert_eq!(four.try_recv().unwrap(), Delivery::Done(40));
        assert_eq!(
            mail.stats(),
            MailboxStats {
                posted: 4,
                superseded: 2
            }
        );
    }

    #[test]
    fn a_worker_that_dies_hands_the_mailbox_back_and_tells_what_it_never_answered() {
        let mail = Arc::new(Mailbox::<u32, u32>::default());
        let (mut first, start) = mail.post(1);
        assert!(start);
        let (mut second, _) = mail.post(2);
        // The worker takes request 2... and panics drawing it.
        let panicked = {
            let mail = Arc::clone(&mail);
            std::thread::spawn(move || {
                let mut worker = Worker::new(&mail);
                let (_request, _reply) = worker.next().expect("a request");
                let (_later, _) = mail.post(3);
                panic!("a panic in the drawing");
            })
            .join()
        };
        assert!(panicked.is_err());
        // The request being drawn is told its worker died; the one that was replaced before was only skipped.
        assert_eq!(first.try_recv().unwrap(), Delivery::Superseded);
        assert_eq!(second.try_recv().unwrap(), Delivery::Abandoned);
        // And a request posted now starts a new worker instead of waiting for a dead one.
        let (_next, start) = mail.post(4);
        assert!(start);
    }

    #[test]
    fn a_worker_that_ends_normally_leaves_a_request_posted_just_after_for_the_next_one() {
        let mail = Mailbox::<u32, u32>::default();
        let (_a, start) = mail.post(1);
        assert!(start);
        let mut worker = Worker::new(&mail);
        let (_, reply) = worker.next().expect("a request");
        reply.send(1);
        assert!(worker.next().is_none(), "ran dry");
        // The new request finds an idle mailbox and starts the next worker before this one is gone.
        let (mut b, start) = mail.post(2);
        assert!(start);
        drop(worker);
        assert!(b.try_recv().is_err(), "it was not told anything: it waits for its worker");
        assert_eq!(mail.next().map(|(r, _)| r), Some(2));
    }

    #[test]
    fn a_burst_from_many_threads_draws_the_last_request_of_each_and_never_loses_one() {
        let mail = Arc::new(Mailbox::<u32, u32>::default());
        let drawn = Arc::new(Mutex::new(Vec::<u32>::new()));
        let mut workers = Vec::new();
        let mut receivers = Vec::new();
        for n in 0..200u32 {
            let (rx, start) = mail.post(n);
            receivers.push(rx);
            if start {
                let (mail, drawn) = (Arc::clone(&mail), Arc::clone(&drawn));
                workers.push(std::thread::spawn(move || {
                    let mut worker = Worker::new(&mail);
                    while let Some((request, reply)) = worker.next() {
                        std::thread::sleep(Duration::from_micros(300));
                        drawn.lock().unwrap().push(request);
                        reply.send(request);
                    }
                }));
            }
        }
        for w in workers {
            w.join().unwrap();
        }
        let drawn = drawn.lock().unwrap().clone();
        assert!(drawn.windows(2).all(|w| w[0] < w[1]), "drawn in order: {drawn:?}");
        assert_eq!(drawn.last(), Some(&199), "the newest request of the burst is drawn");
        // Every request got an answer, drawn or superseded; the ones drawn got their own.
        let (mut done, mut skipped) = (0, 0);
        for (n, mut rx) in receivers.into_iter().enumerate() {
            match rx.try_recv().expect("answered") {
                Delivery::Done(v) => {
                    assert_eq!(v as usize, n);
                    done += 1;
                }
                Delivery::Superseded => skipped += 1,
                Delivery::Abandoned => panic!("nobody abandoned anything"),
            }
        }
        assert_eq!(done, drawn.len());
        assert_eq!(done + skipped, 200);
        assert!(skipped > 0, "a burst faster than the drawing must drop frames");
    }
}
