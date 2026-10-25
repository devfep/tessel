//! Client traffic: a seeded script of agents that connect, claim, amend, release, heartbeat,
//! probe retired fences, submit work (covered and not), join races and watch the log, and a
//! reviewer that opens races and decides held submissions, while the clock moves and faults
//! strike. A seed's script depends on the order and weights of
//! `act`'s actions, so changing them changes what every seed does: seeds kept as regression cases
//! must be re-found after such a change.

use crate::host::SocketId;
use crate::protocol::{
    ClaimId, ClientMsg, CommitId, Criterion, DecisionRecord, Fence, Intent, Mode, OnConflict,
    RaceId, RequestId, Scope, ScopeClaim,
};
use crate::shell::LEASE_MS;
use crate::sim::driver::{RequestKind, Sim};
use crate::sim::rng::Rng;

const AGENTS: [&str; 3] = ["agent-a", "agent-b", "agent-c"];
/// The configured reviewer: decides held submissions and opens races.
pub const REVIEWER: &str = "reviewer";
const WATCHERS: [&str; 2] = ["watcher-a", "watcher-b"];
const PATHS: [&str; 4] = ["src/a.rs", "src/b.rs", "src/c.rs", "lib/d.rs"];
const DIRS: [&str; 2] = ["src", "lib"];
const MODES: [Mode; 4] = [
    Mode::Depend,
    Mode::EditBody,
    Mode::EditSignature,
    Mode::Create,
];

pub struct Traffic {
    rng: Rng,
    next_req: u64,
}

impl Traffic {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed),
            next_req: 1,
        }
    }

    /// One step: maybe arm a fault, do one thing, and let the host finish. False when the host
    /// never finished.
    pub fn step(&mut self, sim: &mut Sim) -> bool {
        self.arm_fault(sim);
        let agent = AGENTS[self.rng.below(AGENTS.len())];
        let mut finished = true;
        if sim.view(agent).and_then(|view| view.sock).is_none() {
            sim.connect(agent);
            finished = sim.quiet();
        }
        self.act(sim, agent);
        finished &= sim.quiet();
        sim.disarm();
        finished
    }

    fn arm_fault(&mut self, sim: &mut Sim) {
        match self.rng.below(24) {
            0 | 1 => sim.crash_after_outputs(1 + self.rng.below(8) as u64),
            2 => sim.crash_after_apply(),
            3 | 4 => sim.fail_next_commit(),
            _ => {}
        }
    }

    fn req(&mut self) -> RequestId {
        let req = RequestId(self.next_req);
        self.next_req += 1;
        req
    }

    fn act(&mut self, sim: &mut Sim, agent: &str) {
        let roll = self.rng.below(100);
        match roll {
            0..=19 => self.claim(sim, agent),
            20..=27 => self.amend(sim, agent),
            28..=35 => self.release(sim, agent),
            36..=43 => self.heartbeat(sim, agent),
            44..=51 => self.stale_release(sim, agent),
            52..=65 => self.submit(sim, agent),
            66..=72 => self.review(sim),
            73..=81 => self.race(sim, agent),
            82..=86 => self.watch(sim),
            87..=91 => sim.advance(self.rng.below(5_000) as u64 + 1),
            92..=96 => self.jump(sim),
            _ => self.disconnect(sim, agent),
        }
    }

    fn scope(&mut self) -> ScopeClaim {
        let scope = if self.rng.chance(0.2) {
            Scope::Dir {
                path: DIRS[self.rng.below(DIRS.len())].to_string(),
            }
        } else {
            Scope::File {
                path: PATHS[self.rng.below(PATHS.len())].to_string(),
            }
        };
        ScopeClaim {
            scope,
            mode: MODES[self.rng.below(MODES.len())],
        }
    }

    fn claim(&mut self, sim: &mut Sim, agent: &str) {
        let Some(view) = sim.view(agent) else { return };
        let Some(sock) = view.sock else { return };
        let on_conflict = if view.held.is_empty() && self.rng.chance(0.3) {
            OnConflict::Wait
        } else {
            OnConflict::Fail
        };
        let scopes = (0..=self.rng.below(2)).map(|_| self.scope()).collect();
        let msg = ClientMsg::Claim {
            req: self.req(),
            intent: Intent {
                summary: format!("work {}", self.next_req),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes,
            on_conflict,
        };
        sim.send(sock, &msg, RequestKind::Claim, None);
    }

    /// A claim the agent holds, with its fence.
    fn held(&mut self, sim: &Sim, agent: &str) -> Option<(ClaimId, Fence)> {
        let view = sim.view(agent)?;
        let open: Vec<_> = view
            .held
            .iter()
            .filter(|(claim, _)| !view.submitted.contains(claim))
            .collect();
        let (claim, fence) = open.get(self.rng.below(open.len()))?;
        Some((ClaimId(**claim), **fence))
    }

    fn amend(&mut self, sim: &mut Sim, agent: &str) {
        let Some(sock) = sim.view(agent).and_then(|view| view.sock) else {
            return;
        };
        let Some((claim, fence)) = self.held(sim, agent) else {
            return;
        };
        let msg = ClientMsg::Amend {
            req: self.req(),
            claim,
            fence,
            add: vec![self.scope()],
        };
        sim.send(sock, &msg, RequestKind::Amend, Some(claim));
    }

    fn release(&mut self, sim: &mut Sim, agent: &str) {
        let Some(sock) = sim.view(agent).and_then(|view| view.sock) else {
            return;
        };
        let Some((claim, fence)) = self.held(sim, agent) else {
            return;
        };
        let msg = ClientMsg::Release {
            claim,
            fence,
            req: Some(self.req()),
        };
        sim.send(sock, &msg, RequestKind::Release, Some(claim));
    }

    fn heartbeat(&mut self, sim: &mut Sim, agent: &str) {
        if let Some(sock) = sim.view(agent).and_then(|view| view.sock) {
            sim.send(sock, &ClientMsg::Heartbeat, RequestKind::Other, None);
        }
    }

    /// Present a fence the agent knows is retired: the host must refuse it.
    fn stale_release(&mut self, sim: &mut Sim, agent: &str) {
        let Some(view) = sim.view(agent) else { return };
        let Some(sock) = view.sock else { return };
        if view.retired.is_empty() {
            return;
        }
        // Most probes use a fence an amend retired on a claim still held: a host that ignores
        // the fence would release it.
        let live: Vec<(ClaimId, Fence)> = view
            .retired
            .iter()
            .filter(|(claim, _)| view.held.contains_key(&claim.0))
            .copied()
            .collect();
        let pool = if live.is_empty() {
            &view.retired
        } else {
            &live
        };
        let (claim, fence) = pool[self.rng.below(pool.len())];
        let msg = ClientMsg::Release {
            claim,
            fence,
            req: Some(self.req()),
        };
        sim.send(sock, &msg, RequestKind::StaleRelease, Some(claim));
    }

    /// Jump the clock: to the held wake, or past a whole lease.
    fn jump(&mut self, sim: &mut Sim) {
        if sim.wake().is_some() && self.rng.chance(0.6) {
            sim.jump_to_wake();
        } else {
            sim.advance(LEASE_MS + self.rng.below(60_000) as u64);
        }
    }

    fn disconnect(&mut self, sim: &mut Sim, agent: &str) {
        if let Some(sock) = sim.view(agent).and_then(|view| view.sock) {
            sim.disconnect(sock);
        }
    }

    /// `name`'s socket, connecting first if it has none.
    fn socket(&mut self, sim: &mut Sim, name: &str) -> Option<SocketId> {
        if sim.view(name).and_then(|view| view.sock).is_none() {
            sim.connect(name);
            sim.quiet();
        }
        sim.view(name).and_then(|view| view.sock)
    }

    /// Submit a claim's work: usually what it claimed, sometimes something else.
    fn submit(&mut self, sim: &mut Sim, agent: &str) {
        let Some(sock) = sim.view(agent).and_then(|view| view.sock) else {
            return;
        };
        let Some((claim, fence)) = self.held(sim, agent) else {
            return;
        };
        let claimed = sim
            .view(agent)
            .and_then(|view| view.scopes.get(&claim.0))
            .cloned()
            .unwrap_or_default();
        let touched = if claimed.is_empty() || self.rng.chance(0.2) {
            vec![self.scope()]
        } else {
            vec![claimed[self.rng.below(claimed.len())].clone()]
        };
        let req = self.req();
        let evidence = if self.rng.chance(0.75) {
            vec!["tests passed".to_string()]
        } else {
            Vec::new()
        };
        let msg = ClientMsg::Submit {
            req,
            claim,
            fence,
            fork_commit: CommitId(format!("{:040x}", req.0)),
            touched,
            decisions: DecisionRecord {
                rejected: Vec::new(),
                evidence,
                transcript: None,
            },
        };
        sim.send(sock, &msg, RequestKind::Submit, Some(claim));
    }

    /// The reviewer decides a submission held for review.
    fn review(&mut self, sim: &mut Sim) {
        let waiting: Vec<u64> = sim.awaiting_review().iter().copied().collect();
        if waiting.is_empty() {
            return;
        }
        let claim = ClaimId(waiting[self.rng.below(waiting.len())]);
        let Some(sock) = self.socket(sim, REVIEWER) else {
            return;
        };
        let msg = ClientMsg::Review {
            req: self.req(),
            claim,
            approve: self.rng.chance(0.6),
            note: self.rng.chance(0.5).then(|| "checked".to_string()),
        };
        sim.send(sock, &msg, RequestKind::Other, None);
    }

    /// Open a race, join one, or pick a winner.
    fn race(&mut self, sim: &mut Sim, agent: &str) {
        let open: Vec<u64> = sim.open_races().iter().copied().collect();
        if open.is_empty() {
            self.open_race(sim);
            return;
        }
        let race = RaceId(open[self.rng.below(open.len())]);
        if self.rng.chance(0.65) {
            let Some(sock) = sim.view(agent).and_then(|view| view.sock) else {
                return;
            };
            let msg = ClientMsg::JoinRace {
                req: self.req(),
                race,
            };
            sim.send(sock, &msg, RequestKind::Other, None);
            return;
        }
        let entrants = sim.entrants(race.0).to_vec();
        if entrants.is_empty() {
            return;
        }
        let claim = ClaimId(entrants[self.rng.below(entrants.len())]);
        let Some(sock) = self.socket(sim, REVIEWER) else {
            return;
        };
        let msg = ClientMsg::PickWinner {
            req: self.req(),
            race,
            claim,
        };
        sim.send(sock, &msg, RequestKind::Other, None);
    }

    fn open_race(&mut self, sim: &mut Sim) {
        let Some(sock) = self.socket(sim, REVIEWER) else {
            return;
        };
        let criteria = if self.rng.chance(0.5) {
            vec![Criterion::FirstSubmitted]
        } else {
            vec![Criterion::HumanPick]
        };
        let msg = ClientMsg::OpenRace {
            req: self.req(),
            intent: Intent {
                summary: "race".to_string(),
                task_ref: None,
                assumptions: Vec::new(),
            },
            scopes: vec![self.scope()],
            max_entrants: 2,
            deadline_ms: sim.now_ms() + 20_000 + self.rng.below(40_000) as u64,
            criteria,
        };
        sim.send(sock, &msg, RequestKind::Other, None);
    }

    /// Start a watcher: from the start of the log or from somewhere in it, sometimes with a hello
    /// right behind the watch, sometimes on a socket whose sends fail.
    fn watch(&mut self, sim: &mut Sim) {
        let name = WATCHERS[self.rng.below(WATCHERS.len())];
        let from = if self.rng.chance(0.5) {
            0
        } else {
            self.rng.below(sim.stored_events() + 3) as u64
        };
        let hello_mid_replay = self.rng.chance(0.5);
        let sock = sim.watch(name, from, hello_mid_replay);
        if self.rng.chance(0.3) {
            sim.drop_sends(sock);
        }
    }
}
