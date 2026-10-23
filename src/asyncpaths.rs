//! Path sets of asynchronous work (`API-pcmEvaluate.md` revision 4, `asyncPaths`): a SimOxide
//! interpreter listener (`simoxide_sim::listener`, SIM-14) that records, per unit of
//! asynchronous work, its trigger, the containers it touched and two contexts. It only observes
//! the run.
//!
//! * A **trigger** is one run of a SEFF (an assembly operation on a process's stack) that starts
//!   asynchronous forked behaviours in its own steps, also in its synchronous forked behaviours
//!   (a fork process without assembly operations of its own belongs to its parent's).
//! * Its **unit** is everything those forks start, transitively. A process inside a unit that
//!   forks adds to that unit. Forks of `asyncIgnoreForks` start nothing that is followed.
//! * A unit **completes** when the trigger's SEFF run has returned and its last process ended;
//!   it counts when that happens in [warmUp, end).
//! * **context**: the containers of the session's synchronous path when the unit started (its
//!   first fork). **localContext**: the containers the trigger's own synchronous execution had
//!   entered by then (the trigger's SEFF and the operations it called synchronously, in its
//!   process and in synchronous forked behaviours that had joined or lead to the fork).
//!
//! Bookkeeping per callback: processes in slots (pid -> slot by an Fx map, slots and their
//! vectors reused), sessions in their user's slot, containers as indices.

use simoxide_model::{ActionId, ContainerId};
use simoxide_sim::CompiledModel;
use simoxide_sim::fxhash::FxMap;
use simoxide_sim::listener::{AssemblyOp, Listener, Pid, ProcessKind};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

const NONE: u32 = u32::MAX;
/// `Proc::unit` of a process of an ignored fork (or one it started).
const IGNORED: u32 = u32::MAX - 1;

struct Frame {
    ac: u32,
    sig: u32,
    container: u32,
    /// The unit its forks started (NONE: none yet).
    unit: u32,
    /// Index into the process's trail where the frame began.
    start: u32,
}

#[derive(Default)]
struct Proc {
    pid: Pid,
    parent: u32,
    parent_pid: Pid,
    sync_child: bool,
    /// Slot of the user whose session the process serves synchronously (NONE: none).
    owner: u32,
    /// Unit the process belongs to (NONE, IGNORED or a unit).
    unit: u32,
    frames: Vec<Frame>,
    /// The container of the SEFF the process was forked in (NONE: none).
    base: u32,
    /// Containers entered, in order (processes outside asynchronous work; for localContext).
    trail: Vec<u32>,
    /// A user's session in progress: scenario and containers.
    in_session: bool,
    scen: u32,
    sset: Vec<u32>,
}

#[derive(Default)]
struct Unit {
    live: u32,
    open: bool,
    scen: u32,
    ac: u32,
    sig: u32,
    context: Vec<u32>,
    local: Vec<u32>,
    set: Vec<u32>,
}

/// (scenario or NONE, trigger assembly, signature, context, localContext, containers).
type Key = (u32, u32, u32, Vec<u32>, Vec<u32>, Vec<u32>);

/// The recorded state (shared with the listener in the run).
#[derive(Default)]
pub struct AsyncState {
    ignore: HashSet<ActionId>,
    w0: f64,
    end: f64,
    slot_of: FxMap<Pid, u32>,
    procs: Vec<Proc>,
    free_procs: Vec<u32>,
    units: Vec<Unit>,
    free_units: Vec<u32>,
    pub done: FxMap<Key, u64>,
}

/// The listener registered with the run.
pub struct AsyncRec(pub Rc<RefCell<AsyncState>>);

impl AsyncRec {
    /// A recorder for the window [w0, end) (model time units), not following the asynchronous
    /// behaviours of the fork actions `ignore`.
    pub fn new(ignore: HashSet<ActionId>, w0: f64, end: f64) -> AsyncRec {
        AsyncRec(Rc::new(RefCell::new(AsyncState { ignore, w0, end, ..AsyncState::default() })))
    }
}

fn cid(c: Option<ContainerId>) -> u32 {
    c.map_or(NONE, |c| c.0)
}

fn add(v: &mut Vec<u32>, c: u32) {
    if c != NONE && !v.contains(&c) {
        v.push(c);
    }
}

impl AsyncState {
    fn slot(&self, p: Pid) -> u32 {
        self.slot_of.get(&p).copied().unwrap_or(NONE)
    }

    fn new_proc(&mut self, p: Pid) -> u32 {
        let s = match self.free_procs.pop() {
            Some(s) => s,
            None => {
                self.procs.push(Proc::default());
                (self.procs.len() - 1) as u32
            }
        };
        let x = &mut self.procs[s as usize];
        x.pid = p;
        x.parent = NONE;
        x.parent_pid = 0;
        x.sync_child = false;
        x.owner = NONE;
        x.unit = NONE;
        x.base = NONE;
        x.in_session = false;
        x.frames.clear();
        x.trail.clear();
        x.sset.clear();
        self.slot_of.insert(p, s);
        s
    }

    fn settle(&mut self, u: u32, t: f64) {
        let x = &self.units[u as usize];
        if x.live > 0 || x.open {
            return;
        }
        let mut x = std::mem::take(&mut self.units[u as usize]);
        self.free_units.push(u);
        if t >= self.w0 && t < self.end {
            x.set.sort_unstable();
            *self.done.entry((x.scen, x.ac, x.sig, x.context, x.local, x.set)).or_default() += 1;
        }
    }

    fn close(&mut self, u: u32, t: f64) {
        self.units[u as usize].open = false;
        self.settle(u, t);
    }

    /// The live sync parent of slot `s` (NONE: none).
    fn sync_parent(&self, s: u32) -> u32 {
        let x = &self.procs[s as usize];
        if !x.sync_child || x.parent == NONE || self.procs[x.parent as usize].pid != x.parent_pid {
            return NONE;
        }
        x.parent
    }

    /// The unit an asynchronous fork of slot `q` starts or joins (NONE: no SEFF run forks).
    fn trigger_unit(&mut self, q: u32) -> u32 {
        // the SEFF run that forks: q's innermost, or (a synchronous fork without one) its parent's
        let mut at = q;
        while self.procs[at as usize].frames.is_empty() {
            at = self.sync_parent(at);
            if at == NONE {
                return NONE;
            }
        }
        let f = self.procs[at as usize].frames.last().expect("frame");
        if f.unit != NONE {
            return f.unit;
        }
        let (ac, sig, start) = (f.ac, f.sig, f.start as usize);
        // localContext: the trigger's trail since it began, and the trails of the synchronous
        // forked behaviours between it and the fork
        let mut local: Vec<u32> = self.procs[at as usize].trail[start..].to_vec();
        let mut s = q;
        while s != at && s != NONE {
            local.extend_from_slice(&self.procs[s as usize].trail);
            s = self.sync_parent(s);
        }
        local.sort_unstable();
        local.dedup();
        let (mut scen, mut context) = (NONE, Vec::new());
        let o = self.procs[q as usize].owner;
        if o != NONE && self.procs[o as usize].in_session {
            scen = self.procs[o as usize].scen;
            context = self.procs[o as usize].sset.clone();
            context.sort_unstable();
        }
        let unit = Unit { live: 0, open: true, scen, ac, sig, context, local, set: Vec::new() };
        let u = match self.free_units.pop() {
            Some(i) => {
                self.units[i as usize] = unit;
                i
            }
            None => {
                self.units.push(unit);
                (self.units.len() - 1) as u32
            }
        };
        self.procs[at as usize].frames.last_mut().expect("frame").unit = u;
        u
    }
}

impl Listener for AsyncRec {
    fn process_created(&mut self, _t: f64, p: Pid, parent: Option<Pid>, kind: ProcessKind) {
        let s = &mut *self.0.borrow_mut();
        let q = parent.map_or(NONE, |q| s.slot(q));
        let me = s.new_proc(p);
        let ProcessKind::Forked { fork, synchronous, .. } = kind else {
            return;
        };
        if q == NONE {
            return;
        }
        let (pu, powner, base, ppid) = {
            let x = &s.procs[q as usize];
            (x.unit, x.owner, x.frames.last().map_or(x.base, |f| f.container), x.pid)
        };
        let unit = if pu == IGNORED || (!synchronous && s.ignore.contains(&fork)) {
            IGNORED
        } else if pu != NONE {
            pu
        } else if synchronous {
            NONE
        } else {
            s.trigger_unit(q)
        };
        if unit != NONE && unit != IGNORED {
            let u = &mut s.units[unit as usize];
            u.live += 1;
            add(&mut u.set, base);
        }
        let x = &mut s.procs[me as usize];
        x.parent = q;
        x.parent_pid = ppid;
        x.sync_child = synchronous;
        x.owner = if synchronous { powner } else { NONE };
        x.unit = unit;
        x.base = base;
    }

    fn process_ended(&mut self, t: f64, p: Pid) {
        let s = &mut *self.0.borrow_mut();
        let Some(me) = s.slot_of.remove(&p) else {
            return;
        };
        s.free_procs.push(me);
        let mut frames = std::mem::take(&mut s.procs[me as usize].frames);
        while let Some(f) = frames.pop() {
            if f.unit != NONE {
                s.close(f.unit, t);
            }
        }
        s.procs[me as usize].frames = frames;
        let x = &s.procs[me as usize];
        let (unit, sync_parent) = (x.unit, s.sync_parent(me));
        // a joined synchronous fork: what it entered belongs to its parent's execution
        if sync_parent != NONE && unit == NONE && !s.procs[me as usize].trail.is_empty() {
            let trail = std::mem::take(&mut s.procs[me as usize].trail);
            if !s.procs[sync_parent as usize].frames.is_empty() {
                s.procs[sync_parent as usize].trail.extend_from_slice(&trail);
            }
            s.procs[me as usize].trail = trail;
        }
        if unit != NONE && unit != IGNORED {
            s.units[unit as usize].live -= 1;
            s.settle(unit, t);
        }
    }

    fn usage_scenario(&mut self, _t: f64, begin: bool, p: Pid, scenario: u32) {
        let s = &mut *self.0.borrow_mut();
        let me = s.slot(p);
        if me == NONE {
            return;
        }
        let x = &mut s.procs[me as usize];
        x.in_session = begin;
        x.scen = scenario;
        x.sset.clear();
        x.owner = if begin { me } else { NONE };
    }

    fn assembly_operation(&mut self, t: f64, begin: bool, p: Pid, op: &AssemblyOp) {
        let s = &mut *self.0.borrow_mut();
        let me = s.slot(p);
        if me == NONE {
            return;
        }
        if !begin {
            let x = &mut s.procs[me as usize];
            let f = x.frames.pop();
            if x.frames.is_empty() && !x.sync_child {
                x.trail.clear();
            }
            if let Some(f) = f
                && f.unit != NONE
            {
                s.close(f.unit, t);
            }
            return;
        }
        let c = cid(op.container);
        let x = &mut s.procs[me as usize];
        let (owner, unit) = (x.owner, x.unit);
        let start = x.trail.len() as u32;
        if unit == NONE && c != NONE && x.trail.last() != Some(&c) {
            x.trail.push(c);
        }
        x.frames.push(Frame { ac: op.assembly.0, sig: op.signature.0, container: c, unit: NONE, start });
        if owner != NONE {
            add(&mut s.procs[owner as usize].sset, c);
        }
        if unit != NONE && unit != IGNORED {
            add(&mut s.units[unit as usize].set, c);
        }
    }
}

/// A completed-units entry with the model's ids (before the caller's names).
#[derive(Clone, Debug)]
pub struct RawUnits {
    pub scenario: Option<String>,
    pub assembly: String,
    pub seff: String,
    pub signature: String,
    pub context: Vec<String>,
    pub local_context: Vec<String>,
    pub containers: Vec<String>,
    pub count: u64,
}

/// The recorded units with ids of `cm`'s model (the run's last); containers mapped by `unit`
/// (pods to their scaled container), assembly contexts that model changes created (a scaled-out
/// pod's copy of an assembly) to the assembly of `base` (the model the run started with) with the
/// same component, if there is exactly one.
pub fn read(st: &AsyncState, cm: &CompiledModel, base: &CompiledModel, unit: impl Fn(ContainerId) -> ContainerId) -> Vec<RawUnits> {
    let m = &cm.model;
    let original = |a: simoxide_model::AssemblyContextId| -> String {
        let id = &m.assembly_contexts[a].id;
        if base.model.assembly_contexts.iter().any(|x| x.id == *id) {
            return id.to_string();
        }
        let comp = m.assembly_contexts[a].component.map(|c| &m.components[c].id);
        let same: Vec<&str> = base
            .model
            .assembly_contexts
            .iter()
            .filter(|x| x.component.map(|c| &base.model.components[c].id) == comp)
            .map(|x| &*x.id)
            .collect();
        match same[..] {
            [one] => one.to_string(),
            _ => id.to_string(),
        }
    };
    let ids = |v: &[u32]| -> Vec<String> { v.iter().map(|&c| m.containers[unit(ContainerId(c))].id.to_string()).collect() };
    let mut out = Vec::new();
    for ((scen, ac, sig, ctx, local, set), n) in &st.done {
        let a = simoxide_model::AssemblyContextId(*ac);
        let g = simoxide_model::SignatureId(*sig);
        let seff = m.assembly_contexts[a]
            .component
            .and_then(|comp| m.components[comp].seffs().iter().copied().find(|&x| m.seffs[x].signature == Some(g)))
            .map(|x| m.seffs[x].id.to_string())
            .unwrap_or_default();
        out.push(RawUnits {
            scenario: (*scen != NONE).then(|| cm.scenario_ids[cm.scenarios[*scen as usize].id.index()].to_string()),
            assembly: original(a),
            seff,
            signature: m.signatures[g].id.to_string(),
            context: ids(ctx),
            local_context: ids(local),
            containers: ids(set),
            count: *n,
        });
    }
    out
}
