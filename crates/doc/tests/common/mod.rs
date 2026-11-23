//! An in-memory room: one `Authority`, N `Replica`s, and a FIFO queue per
//! link. Loss is modelled only as a disconnect dropping both of a peer's
//! queues, which is what a real socket does.

#![allow(dead_code)]

use std::collections::VecDeque;

use doc::{
    Attr, Authority, BatchNo, Cell, Change, ClientId, Doc, Edit, Extent, Kind, NodeId, Place,
    Position, Reject, Rejected, Replica, Step, To, Update,
};

#[derive(Debug)]
pub struct Peer {
    pub rep: Replica,
    pub online: bool,
    up: VecDeque<Vec<u8>>,
    down: VecDeque<Vec<u8>>,
    pub undo: Vec<(BatchNo, Vec<Change>)>,
    pub rejected: Vec<Rejected>,
}

#[derive(Debug)]
pub struct Net {
    pub auth: Authority,
    pub peers: Vec<Peer>,
    /// Every log record the authority asked the shell to append, in order.
    pub log: Vec<Vec<u8>>,
}

impl Net {
    pub fn new(n: usize) -> Net {
        let mut net = Net {
            auth: Authority::restore(None, []).unwrap(),
            peers: Vec::new(),
            log: Vec::new(),
        };
        for _ in 0..n {
            let (id, step) = net.auth.connect(Replica::hello().as_bytes()).unwrap();
            let welcome = only_to(&step, id);
            net.peers.push(Peer {
                rep: Replica::join(&welcome).unwrap(),
                online: true,
                up: VecDeque::new(),
                down: VecDeque::new(),
                undo: Vec::new(),
                rejected: Vec::new(),
            });
        }
        net
    }

    pub fn doc(&self, i: usize) -> &Doc {
        self.peers[i].rep.doc()
    }

    pub fn id(&mut self, i: usize) -> NodeId {
        self.peers[i].rep.new_id()
    }

    /// Applies locally, records the undo entry, and queues the frame if online.
    pub fn edit(&mut self, i: usize, changes: Vec<Change>) -> Result<Edit, Reject> {
        let p = &mut self.peers[i];
        let edit = p.rep.edit(changes)?;
        p.undo.push((edit.batch, edit.inverse.clone()));
        if p.online {
            p.up.push_back(edit.send.as_bytes().to_vec());
        }
        Ok(edit)
    }

    /// Submits the inverse of peer `i`'s latest undo entry.
    pub fn undo(&mut self, i: usize) -> Option<Result<Edit, Reject>> {
        let (_, inverse) = self.peers[i].undo.pop()?;
        let out = self.edit(i, inverse);
        if out.is_ok() {
            self.peers[i].undo.pop();
        }
        Some(out)
    }

    /// Delivers peer `i`'s oldest outbound frame to the authority.
    pub fn up(&mut self, i: usize) -> bool {
        let Some(frame) = self.peers[i].up.pop_front() else {
            return false;
        };
        self.send_raw(i, &frame);
        true
    }

    /// Hands `frame` to the authority as if peer `i` sent it, and routes the result.
    pub fn send_raw(&mut self, i: usize, frame: &[u8]) -> Step {
        let from = self.peers[i].rep.client();
        let step = self.auth.receive(from, frame).unwrap();
        self.log
            .extend(step.log.iter().map(|r| r.as_bytes().to_vec()));
        self.route(&step);
        step
    }

    /// Delivers the oldest inbound frame to peer `i`.
    pub fn down(&mut self, i: usize) -> Option<Update> {
        let frame = self.peers[i].down.pop_front()?;
        let p = &mut self.peers[i];
        let update = p.rep.receive(&frame).unwrap();
        if let Some(r) = &update.rejected {
            p.undo.retain(|(b, _)| *b != r.batch);
            p.rejected.push(r.clone());
        }
        Some(update)
    }

    /// Drops both of the peer's queues, as a closed socket does.
    pub fn disconnect(&mut self, i: usize) {
        if !self.peers[i].online {
            return;
        }
        let p = &mut self.peers[i];
        p.online = false;
        p.up.clear();
        p.down.clear();
        let step = self.auth.disconnect(p.rep.client());
        self.route(&step);
    }

    pub fn rejoin(&mut self, i: usize) {
        if self.peers[i].online {
            return;
        }
        let hello = self.peers[i].rep.hello_again();
        let (id, step) = self.auth.connect(hello.as_bytes()).unwrap();
        let welcome = only_to(&step, id);
        let p = &mut self.peers[i];
        let resend = p.rep.rejoin(&welcome).unwrap();
        p.online = true;
        p.up.extend(resend.iter().map(|f| f.as_bytes().to_vec()));
    }

    /// Delivers every queued frame, in both directions, until all are empty.
    pub fn settle(&mut self) {
        loop {
            let mut moved = false;
            for i in 0..self.peers.len() {
                while self.up(i) {
                    moved = true;
                }
            }
            for i in 0..self.peers.len() {
                while self.down(i).is_some() {
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }

    fn route(&mut self, step: &Step) {
        for (to, frame) in &step.send {
            for p in self.peers.iter_mut().filter(|p| p.online) {
                let me = p.rep.client();
                let hit = match to {
                    To::One(c) => *c == me,
                    To::AllExcept(c) => *c != me,
                };
                if hit {
                    p.down.push_back(frame.as_bytes().to_vec());
                }
            }
        }
    }

    pub fn queued_up(&self, i: usize) -> usize {
        self.peers[i].up.len()
    }

    /// Every replica's canonical bytes equal the authority's.
    pub fn assert_converged(&self) {
        let want = self.auth.doc().encode();
        for (i, p) in self.peers.iter().enumerate() {
            assert_eq!(p.rep.unsettled(), 0, "peer {i} has unsettled batches");
            assert!(
                p.rep.doc().encode() == want,
                "peer {i} diverged from the authority"
            );
        }
    }
}

fn only_to(step: &Step, id: ClientId) -> Vec<u8> {
    step.send
        .iter()
        .find(|(to, _)| *to == To::One(id))
        .map(|(_, f)| f.as_bytes().to_vec())
        .expect("welcome for the new client")
}

/// Changes creating a node of `kind` at the end of `parent`, with `attrs`.
pub fn create(doc: &Doc, id: NodeId, kind: Kind, parent: NodeId, attrs: Vec<Attr>) -> Vec<Change> {
    let mut out = vec![
        Change::kind(id, kind),
        Change::place(id, doc.slot(parent, Position::Last)),
    ];
    out.extend(attrs.into_iter().map(|a| Change::set(id, a)));
    out
}

pub fn at(x: i32, y: i32, z: i32) -> Vec<Attr> {
    vec![Attr::X(Cell(x)), Attr::Y(Cell(y)), Attr::Z(Cell(z))]
}

pub fn sized(x: i32, y: i32, z: i32, w: u32, h: u32, d: u32) -> Vec<Attr> {
    let mut v = at(x, y, z);
    v.extend([Attr::W(Extent(w)), Attr::H(Extent(h)), Attr::D(Extent(d))]);
    v
}

/// Peer `i` creates a node, sends it, and every peer receives it.
pub fn make(net: &mut Net, i: usize, kind: Kind, parent: NodeId, attrs: Vec<Attr>) -> NodeId {
    let id = net.id(i);
    let changes = create(net.doc(i), id, kind, parent, attrs);
    net.edit(i, changes).unwrap();
    net.settle();
    id
}

/// A room with one page every peer has seen.
pub fn room(n: usize) -> (Net, NodeId) {
    let mut net = Net::new(n);
    let page = make(&mut net, 0, Kind::Page, NodeId::ROOT, vec![]);
    (net, page)
}

pub fn place_of(doc: &Doc, n: NodeId) -> Option<Place> {
    match doc.prop(n, &doc::PropKey::PLACE)? {
        doc::Prop::Intrinsic(doc::Intrinsic::Place(p)) => Some(p.clone()),
        _ => None,
    }
}
