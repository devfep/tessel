//! Client traffic: a seeded script of agents that connect, claim, amend, release, heartbeat and
//! probe retired fences while the clock moves and faults strike. The races, reviews and watchers
//! of the second half join `Traffic::act`. A seed's script depends on the order and weights of
//! `act`'s actions, so changing them changes what every seed does: seeds kept as regression cases
//! must be re-found after such a change.

use crate::protocol::{
    ClaimId, ClientMsg, Fence, Intent, Mode, OnConflict, RequestId, Scope, ScopeClaim,
};
use crate::shell::LEASE_MS;
use crate::sim::driver::{RequestKind, Sim};
use crate::sim::rng::Rng;

const AGENTS: [&str; 3] = ["agent-a", "agent-b", "agent-c"];
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
            0..=29 => self.claim(sim, agent),
            30..=44 => self.amend(sim, agent),
            45..=59 => self.release(sim, agent),
            60..=71 => self.heartbeat(sim, agent),
            72..=83 => self.stale_release(sim, agent),
            84..=89 => sim.advance(self.rng.below(5_000) as u64 + 1),
            90..=96 => self.jump(sim),
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
        let held = &sim.view(agent)?.held;
        let index = self.rng.below(held.len());
        held.iter()
            .nth(index)
            .map(|(claim, fence)| (ClaimId(*claim), *fence))
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
}
