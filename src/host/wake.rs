//! The wake (R20 to R22, R24, R25, R27, R41, and the trunk poke, R21).
//!
//! A wake runs expiry, merge recovery, verification recovery, one merge, one verification and the
//! head read, then sets the next wake, each step its own batch. The Durable Object awaits the
//! steward inside the alarm, so the steps after a call run only once it is answered. The host
//! keeps that: the wake issues its call and suspends, and resumes on `Replied` or `CallLost`.
//! While it is suspended another `Wake` expires the lapsed leases, if any, and sets the wake
//! again, which is the watchdog once nothing else is due. Setting it without the expiry would leave an overdue
//! lease due at once, and the wake would fire again and again until the call is answered.

use crate::host::calls;
use crate::host::core_slot::{ApplyError, Prepared, Waits};
use crate::host::{wake_at_ms, Host, Job, Output};
use crate::shell::{self, PokeReply, Work};

/// How long after a failed wake the next one is set: the first retry delay of a Durable Object
/// alarm that returned an error (R41, C9).
pub const RETRY_MS: u64 = 2_000;

/// A step of the wake, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeStep {
    Expiry,
    RecoverMerge,
    RecoverVerification,
    Merge,
    Verification,
    HeadRead,
    /// Set the next wake and end the wake.
    Finish,
    /// What a wake does while another is suspended at its call: expire the lapsed leases, then
    /// set the wake. The suspended wake resumes when its call is answered.
    Lapse,
}

impl Host {
    /// The wake is due. A wake suspended at its call is not run again.
    pub(super) fn woken(&mut self, out: &mut Vec<Output>) {
        self.waking = true;
        let step = if self.calls.is_empty() {
            WakeStep::Expiry
        } else {
            WakeStep::Lapse
        };
        self.run(Job::Wake(step), out);
    }

    pub(super) fn run_wake(&mut self, step: WakeStep, out: &mut Vec<Output>) {
        match step {
            WakeStep::Expiry => self.expire_step(WakeStep::RecoverMerge, out),
            WakeStep::Lapse => self.lapse_step(out),
            WakeStep::RecoverMerge => self.recover_merge_step(out),
            WakeStep::RecoverVerification => self.recover_verification_step(out),
            WakeStep::Merge => self.merge_step(out),
            WakeStep::Verification => self.verification_step(out),
            WakeStep::HeadRead => self.head_read_step(out),
            WakeStep::Finish => {
                self.waking = false;
                self.set_wake(out);
            }
        }
    }

    fn expire_step(&mut self, next: WakeStep, out: &mut Vec<Output>) {
        let (waits, now_ms) = (self.waits(), self.now_ms);
        let prepared = self
            .slot
            .apply(Work::Plain, waits, now_ms, |core| core.expire(now_ms));
        let then = Some(Job::Wake(next));
        self.commit_unsent("expire leases", prepared, Vec::new(), then, out);
    }

    /// Nothing is written when no lease is due (R28).
    fn lapse_step(&mut self, out: &mut Vec<Output>) {
        if self.slot.has_due_expiry(self.now_ms) {
            self.expire_step(WakeStep::Finish, out);
        } else {
            self.run(Job::Wake(WakeStep::Finish), out);
        }
    }

    /// A merge in flight that no call here waits for was cut off by a restart (R24).
    fn recover_merge_step(&mut self, out: &mut Vec<Output>) {
        let in_flight = self.slot.has_merge_in_flight();
        if !shell::merge_cut_off(true, in_flight) {
            self.run(Job::Wake(WakeStep::RecoverVerification), out);
            return;
        }
        let (waits, now_ms) = (self.waits(), self.now_ms);
        let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
            core.recover_merge(now_ms)
        });
        let then = Some(Job::Wake(WakeStep::RecoverVerification));
        self.commit_unsent("recover cut-off merge", prepared, Vec::new(), then, out);
    }

    fn recover_verification_step(&mut self, out: &mut Vec<Output>) {
        let in_flight = self.slot.has_verification_in_flight();
        if !shell::merge_cut_off(true, in_flight) {
            self.run(Job::Wake(WakeStep::Merge), out);
            return;
        }
        let (waits, now_ms) = (self.waits(), self.now_ms);
        let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
            core.recover_verification(now_ms)
        });
        let then = Some(Job::Wake(WakeStep::Merge));
        self.commit_unsent(
            "recover cut-off verification",
            prepared,
            Vec::new(),
            then,
            out,
        );
    }

    /// Dispatch the next merge the core offers, if one is due. The marker is stored before the
    /// call goes out, and the wake of that batch counts the call (R27).
    fn merge_step(&mut self, out: &mut Vec<Output>) {
        let now_ms = self.now_ms;
        let waits = Waits {
            ..self.waits()
        };
        let mut dispatch = None;
        let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
            dispatch = core.begin_merge(now_ms);
            Vec::new()
        });
        let Some(dispatch) = dispatch else {
            self.skip(prepared, WakeStep::Verification, out);
            return;
        };
        let call = calls::merge_call(self.new_call_id(), &dispatch, now_ms);
        self.commit_unsent("start merge", prepared, vec![call], None, out);
    }

    /// Dispatch the next verification the core offers, if one is due (R25).
    fn verification_step(&mut self, out: &mut Vec<Output>) {
        let now_ms = self.now_ms;
        let waits = Waits {
            verifying: true,
            ..self.waits()
        };
        let mut dispatch = None;
        let prepared = self.slot.apply(Work::Plain, waits, now_ms, |core| {
            dispatch = core.begin_verification(now_ms);
            Vec::new()
        });
        let Some(dispatch) = dispatch else {
            self.skip(prepared, WakeStep::HeadRead, out);
            return;
        };
        let call = calls::trial_call(self.new_call_id(), &dispatch, now_ms);
        self.commit_unsent("start verification", prepared, vec![call], None, out);
    }

    /// A step that found nothing to dispatch stores nothing, and the wake goes on at `next`. An
    /// error in it has already dropped the core, so `next` reloads it.
    fn skip(
        &mut self,
        prepared: Result<Prepared, ApplyError>,
        next: WakeStep,
        out: &mut Vec<Output>,
    ) {
        if let Err(e) = prepared {
            out.push(self.failure(e.operation, &e.cause));
        }
        self.run(Job::Wake(next), out);
    }

    /// Read the trunk head if the core offers a read. Nothing is stored first: a cut-off read
    /// leaves no marker.
    fn head_read_step(&mut self, out: &mut Vec<Output>) {
        let Some(read) = self.slot.begin_head_read() else {
            self.run(Job::Wake(WakeStep::Finish), out);
            return;
        };
        let call = calls::head_call(self.new_call_id(), self.now_ms);
        self.reading = Some(read);
        self.calls.insert(call.id, call.clone());
        self.waking = false;
        out.push(Output::Call(call));
    }

    /// The wake ends by setting the next one from the core as it is now, with this instance's
    /// waits (R16, R41).
    fn set_wake(&mut self, out: &mut Vec<Output>) {
        let waits = self.waits();
        let next_ms = self.slot.next_wake_ms(waits, self.now_ms);
        out.push(Output::SetWake {
            at_ms: wake_at_ms(next_ms, self.now_ms),
        });
    }

    /// A step of the wake failed and the core has been dropped: the wake ends, and the next one
    /// is set after a delay, as the runtime retries an alarm that returned an error (R41).
    pub(super) fn wake_failed(&mut self, out: &mut Vec<Output>) {
        self.waking = false;
        let _ = out;
    }

    /// A trunk poke counts for a repo with stored state, and stores nothing for any other (R21).
    pub(super) fn run_poke(&mut self, out: &mut Vec<Output>) {
        let (waits, now_ms) = (self.waits(), self.now_ms);
        let prepared = self
            .slot
            .apply(Work::Plain, waits, now_ms, |core| core.trunk_moved(now_ms));
        self.commit_unsent("record trunk move", prepared, Vec::new(), None, out);
    }

    /// Whether a poke is for a repo with no stored state, which it must not create.
    pub(super) fn is_unknown_repo(stored: Option<&str>) -> bool {
        shell::poke_reply(stored) == PokeReply::UnknownRepo
    }
}
