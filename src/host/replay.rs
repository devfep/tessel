//! The `Watch` replay (R36).
//!
//! A watch reads the stored log from its start a page at a time, sends each page to the socket,
//! then marks the socket a watcher. The gate stays open: other inputs run between the pages (C3).
//!
//! Exact means no event twice and none missed. A live event is sent to a socket only once it is
//! a watcher, so the events committed while the replay runs wait in the replay's buffer, in
//! commit order, and are sent right after the last page, then the mark, in one step. A page may
//! hold an event that is also in the buffer, because the read ran after its commit; the buffer
//! drops every event at or below the last `seq` the pages sent. Every event is therefore in a
//! page (it was stored before the last read) or in the buffer (it was released after the watch
//! began). The buffer is bounded: one event over `REPLAY_BUFFER_EVENTS` closes the socket.
//!
//! The socket's own frames wait for the replay, so a `Hello` the client sent after the `Watch` is
//! answered after the last page and the CLI can take the first event after the `Welcome` for the
//! end of the replay. A frame waits whether it reached the host before the `Watch` ran (it was
//! queued behind a storage operation) or after; it is parked when the host would apply it, so
//! the frames keep their order. Only that socket waits; the repo does not.

use crate::host::sockets::CLOSE_TRY_AGAIN_LATER;
use crate::host::{Host, Input, Output, SocketId};
use crate::protocol::{Event, ServerMsg};
use crate::shell::{self, ReplayStep};

/// The most live events a replaying socket may have waiting; one more closes it.
pub(super) const REPLAY_BUFFER_EVENTS: usize = 1_000;

const BUFFER_FULL_REASON: &str = "replay buffer full";

/// A replay in progress: where it started, the last `seq` its pages sent, the live events
/// committed since, and the frames its socket sent after the `Watch`.
pub struct Replay {
    from_seq: u64,
    last_sent: Option<u64>,
    buffer: Vec<Event>,
    parked: Vec<Input>,
}

fn read_page(sock: SocketId, from_seq: u64) -> Output {
    Output::ReadEvents {
        sock,
        from_seq,
        limit: shell::REPLAY_PAGE,
    }
}

impl Host {
    /// Start replaying the log to `sock` from `from_seq` (already clamped by `shell::decide`).
    pub(super) fn watch(&mut self, sock: SocketId, from_seq: u64, out: &mut Vec<Output>) {
        out.push(read_page(sock, from_seq));
        self.replays.insert(
            sock,
            Replay {
                from_seq,
                last_sent: None,
                buffer: Vec::new(),
                parked: Vec::new(),
            },
        );
    }

    /// A frame of a replaying socket waits for its replay: `None` when it was kept, else `input`
    /// back.
    pub(super) fn park_during_replay(&mut self, input: Input) -> Option<Input> {
        let Input::Frame { sock, .. } = &input else {
            return Some(input);
        };
        let Some(replay) = self.replays.get_mut(sock) else {
            return Some(input);
        };
        replay.parked.push(input);
        None
    }

    /// A page of the log arrived: send it, then read the next page, or end the replay on a short
    /// or empty page or when the adapter says `done`.
    pub(super) fn replayed(
        &mut self,
        sock: SocketId,
        events: Vec<Event>,
        done: bool,
        out: &mut Vec<Output>,
    ) {
        if self.drop_abandoned_read(sock) {
            return;
        }
        let Some(replay) = self.replays.get_mut(&sock) else {
            out.push(self.failure("replay events", "no replay waits for this page"));
            return;
        };
        let last_seq = events.last().map(|event| event.seq);
        let page_len = events.len();
        for event in events {
            replay.last_sent = Some(event.seq);
            out.push(Output::Send {
                sock,
                msg: ServerMsg::Event { event },
            });
        }
        match shell::after_page(page_len, last_seq) {
            ReplayStep::Next { start_seq } if !done => out.push(read_page(sock, start_seq)),
            ReplayStep::Next { .. } | ReplayStep::Done => self.end_replay(sock, out),
        }
    }

    /// Whether the page is the answer to a read of a replay that already ended; it is dropped.
    fn drop_abandoned_read(&mut self, sock: SocketId) -> bool {
        let Some(reads) = self.abandoned_reads.get_mut(&sock) else {
            return false;
        };
        *reads -= 1;
        if *reads == 0 {
            self.abandoned_reads.remove(&sock);
        }
        true
    }

    /// Whether `input` is the failure of a send to a socket being replayed. It runs at once
    /// instead of queueing, so the replay stops at the first failed send, as `send(..)?` does at
    /// runtime, instead of reading the whole log to a dead socket.
    pub(super) fn fails_replay(&self, input: &Input) -> bool {
        let Input::SendFailed { sock, .. } = input else {
            return false;
        };
        self.replays.contains_key(sock)
    }

    /// End the replay of `sock`, if there is one, without marking it a watcher. Its next read is
    /// still out; the page that answers it is dropped, and so are the frames it parked: the
    /// socket is gone.
    ///
    /// Unlike `send(..)?` at runtime, which stops at the first failed send and logs once, the
    /// sends of the page already handed to the adapter may each fail, and each failure is
    /// logged like any other failed send (R31): up to `REPLAY_PAGE` lines. When the last, short
    /// page fails, the watcher is already marked when its failures arrive: the socket is then
    /// closed and withdrawn, but stays in the table, and so in a live event's plan, until the
    /// adapter reports it closed.
    pub(super) fn abandon_replay(&mut self, sock: SocketId) {
        if self.replays.remove(&sock).is_none() {
            return;
        }
        *self.abandoned_reads.entry(sock).or_insert(0) += 1;
    }

    /// The live events a commit stored wait in the buffer of every replay that began before it.
    /// A buffer over its bound closes its socket.
    pub(super) fn buffer_for_replays(
        &mut self,
        now_ms: u64,
        events: &[Event],
        out: &mut Vec<Output>,
    ) {
        let mut overflowed = Vec::new();
        for (sock, replay) in &mut self.replays {
            for event in events {
                if event.seq >= replay.from_seq {
                    replay.buffer.push(event.clone());
                }
            }
            if replay.buffer.len() > REPLAY_BUFFER_EVENTS {
                overflowed.push(*sock);
            }
        }
        for sock in overflowed {
            let session = self.sockets.session(sock);
            let agent = shell::log_agent(session.and_then(shell::session_agent));
            let cause =
                format!("agent={agent} has over {REPLAY_BUFFER_EVENTS} live events waiting");
            out.push(self.failure("replay buffer", &cause));
            let code = CLOSE_TRY_AGAIN_LATER;
            self.close_and_withdraw(now_ms, sock, code, BUFFER_FULL_REASON, out);
        }
    }

    /// The last page is out: send the events committed since that no page held, mark the socket a
    /// watcher, and let the frames it parked run. A socket the adapter reported closed meanwhile
    /// has no replay left to end.
    fn end_replay(&mut self, sock: SocketId, out: &mut Vec<Output>) {
        let Some(replay) = self.replays.remove(&sock) else {
            return;
        };
        let Some(mut session) = self.sockets.session(sock).cloned() else {
            return;
        };
        for event in replay.buffer {
            if replay.last_sent.is_some_and(|sent| event.seq <= sent) {
                continue;
            }
            out.push(Output::Send {
                sock,
                msg: ServerMsg::Event { event },
            });
        }
        session.watch_from = Some(replay.from_seq);
        out.push(self.sockets.bind(sock, session));
        for input in replay.parked.into_iter().rev() {
            self.queue.push_front(input);
        }
    }
}
