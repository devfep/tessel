//! The `Watch` replay (R36).
//!
//! A watch reads the stored log from its start a page at a time, sends each page to the socket,
//! then marks the socket a watcher. The gate is held from the first read to the mark, so no other
//! input runs in between: an event is either in the replay or sent live, never both and never
//! neither, and a `Hello` the client sent after the `Watch` is answered only after the last page.
//! The CLI relies on this: it takes the first event after the `Welcome` for the end of the replay.
//!
//! The repo stalls while a replay runs. Replaying exactly without the stall is M4's.

use crate::host::{Gate, Host, Input, Output, SocketId};
use crate::protocol::{Event, ServerMsg};
use crate::shell::{self, ReplayStep, Session};

/// A replay in progress: the socket, where it started, and its session as it was when the watch
/// began, which is marked once the replay ends.
pub struct Replay {
    sock: SocketId,
    from_seq: u64,
    session: Session,
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
    pub(super) fn watch(
        &mut self,
        sock: SocketId,
        session: Session,
        from_seq: u64,
        out: &mut Vec<Output>,
    ) {
        out.push(read_page(sock, from_seq));
        self.gate = Gate::Replaying(Replay {
            sock,
            from_seq,
            session,
        });
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
        if self.abandoned_read == Some(sock) {
            self.abandoned_read = None;
            return;
        }
        let replay = match std::mem::replace(&mut self.gate, Gate::Open) {
            Gate::Replaying(replay) if replay.sock == sock => replay,
            other @ (Gate::Open | Gate::Loading(_) | Gate::Committing(_) | Gate::Replaying(_)) => {
                self.gate = other;
                out.push(self.failure("replay events", "no replay waits for this page"));
                return;
            }
        };
        let last_seq = events.last().map(|event| event.seq);
        let page_len = events.len();
        for event in events {
            out.push(Output::Send {
                sock,
                msg: ServerMsg::Event { event },
            });
        }
        match shell::after_page(page_len, last_seq) {
            ReplayStep::Next { start_seq } if !done => {
                out.push(read_page(sock, start_seq));
                self.gate = Gate::Replaying(replay);
            }
            ReplayStep::Next { .. } | ReplayStep::Done => self.mark_watcher(replay, out),
        }
    }

    /// Whether `input` is the failure of a send to the socket being replayed. It runs at once
    /// instead of queueing, so the replay stops at the first failed send, as `send(..)?` does at
    /// runtime, instead of reading the whole log to a dead socket.
    pub(super) fn fails_replay(&self, input: &Input) -> bool {
        let Gate::Replaying(replay) = &self.gate else {
            return false;
        };
        let Input::SendFailed { sock, .. } = input else {
            return false;
        };
        replay.sock == *sock
    }

    /// End the replay of `sock` without marking it a watcher and reopen the gate. Its next read
    /// is still out; the page that answers it is dropped.
    pub(super) fn abandon_replay(&mut self, sock: SocketId) {
        let Gate::Replaying(replay) = &self.gate else {
            return;
        };
        if replay.sock != sock {
            return;
        }
        self.gate = Gate::Open;
        self.abandoned_read = Some(sock);
    }

    fn mark_watcher(&mut self, replay: Replay, out: &mut Vec<Output>) {
        let Replay {
            sock,
            from_seq,
            mut session,
        } = replay;
        session.watch_from = Some(from_seq);
        out.push(self.sockets.bind(sock, session));
    }
}
