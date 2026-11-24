//! Undo and redo from the inverses `Replica::edit` returns.
//!
//! One entry per gesture: every edit between `begin` and `end` folds into one
//! entry, newest inverse first, so undo of a create still clears `Kind` last.
//! An entry remembers its batches, so a server rejection drops it.

use std::collections::HashSet;

use doc::{BatchNo, Change, NodeId, PropKey};

#[derive(Clone, Debug, Default, PartialEq)]
struct Entry {
    inverse: Vec<Change>,
    batches: Vec<BatchNo>,
}

impl Entry {
    fn of(batch: BatchNo, inverse: Vec<Change>) -> Entry {
        Entry {
            inverse,
            batches: vec![batch],
        }
    }

    /// Folds a newer edit in front. Applying the result restores every
    /// register to its value before the oldest edit, so for a register
    /// restored twice only the later (oldest edit's) restore is kept, at its
    /// own position.
    fn fold(&mut self, batch: BatchNo, inverse: Vec<Change>) {
        let mut all = inverse;
        all.append(&mut self.inverse);
        let mut seen: HashSet<(NodeId, PropKey)> = HashSet::new();
        let mut kept: Vec<Change> = all
            .into_iter()
            .rev()
            .filter(|c| seen.insert((c.node, c.key())))
            .collect();
        kept.reverse();
        self.inverse = kept;
        self.batches.push(batch);
    }
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    open: Option<Entry>,
}

impl History {
    /// Starts folding edits into one entry. Idempotent while open.
    pub fn begin(&mut self) {
        self.open.get_or_insert_with(Entry::default);
    }

    /// Closes the open entry. An entry with no edits is dropped.
    pub fn end(&mut self) {
        if let Some(e) = self.open.take()
            && !e.batches.is_empty()
        {
            self.undo.push(e);
        }
    }

    /// Records a local edit. Outside a gesture it is its own entry.
    pub fn record(&mut self, batch: BatchNo, inverse: Vec<Change>) {
        self.redo.clear();
        match &mut self.open {
            Some(e) => e.fold(batch, inverse),
            None => self.undo.push(Entry::of(batch, inverse)),
        }
    }

    /// Drops the open gesture and returns the changes that revert it.
    pub fn abort(&mut self) -> Option<Vec<Change>> {
        self.open
            .take()
            .filter(|e| !e.batches.is_empty())
            .map(|e| e.inverse)
    }

    pub fn undo(&mut self) -> Option<Vec<Change>> {
        self.end();
        self.undo.pop().map(|e| e.inverse)
    }

    pub fn redo(&mut self) -> Option<Vec<Change>> {
        self.end();
        self.redo.pop().map(|e| e.inverse)
    }

    /// The edit that ran an undo. Its inverse is the redo.
    pub fn undone(&mut self, batch: BatchNo, inverse: Vec<Change>) {
        self.redo.push(Entry::of(batch, inverse));
    }

    /// The edit that ran a redo. Its inverse goes back on the undo stack
    /// without clearing the redo stack.
    pub fn redone(&mut self, batch: BatchNo, inverse: Vec<Change>) {
        self.undo.push(Entry::of(batch, inverse));
    }

    /// The server refused `batch` and the replica rolled it back.
    pub fn rejected(&mut self, batch: BatchNo) {
        let keep = |e: &Entry| !e.batches.contains(&batch);
        self.undo.retain(keep);
        self.redo.retain(keep);
        if self.open.as_ref().is_some_and(|e| !keep(e)) {
            self.open = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc::{Attr, Authority, Cell, Kind, Position, Replica};

    fn replica() -> Replica {
        let mut room = Authority::restore(None, []).expect("empty room");
        let (_, step) = room.connect(Replica::hello().as_bytes()).expect("hello");
        Replica::join(step.send[0].1.as_bytes()).expect("welcome")
    }

    fn edit(rep: &mut Replica, h: &mut History, changes: Vec<Change>) {
        let e = rep.edit(changes).expect("valid edit");
        h.record(e.batch, e.inverse);
    }

    #[test]
    fn a_gesture_undoes_in_one_step_and_redoes_in_one_step() {
        let mut rep = replica();
        let mut h = History::default();
        let page = rep.new_id();
        let place = rep.doc().slot(NodeId::ROOT, Position::Last);
        edit(
            &mut rep,
            &mut h,
            vec![Change::kind(page, Kind::Page), Change::place(page, place)],
        );
        let before = rep.doc().encode();

        h.begin();
        let shape = rep.new_id();
        let place = rep.doc().slot(page, Position::Last);
        edit(
            &mut rep,
            &mut h,
            vec![
                Change::kind(shape, Kind::Shape),
                Change::place(shape, place),
                Change::set(shape, Attr::X(Cell(1))),
            ],
        );
        for x in [2, 3] {
            edit(&mut rep, &mut h, vec![Change::set(shape, Attr::X(Cell(x)))]);
        }
        h.end();
        let after = rep.doc().encode();

        let e = rep.edit(h.undo().expect("entry")).expect("undo is valid");
        assert_eq!(
            rep.doc().encode(),
            before,
            "undo reaches the pre-gesture doc"
        );
        h.undone(e.batch, e.inverse);

        let e = rep.edit(h.redo().expect("entry")).expect("redo is valid");
        assert_eq!(
            rep.doc().encode(),
            after,
            "redo reaches the post-gesture doc"
        );
        h.redone(e.batch, e.inverse);

        rep.edit(h.undo().expect("entry")).expect("undo again");
        assert_eq!(rep.doc().encode(), before);
    }

    #[test]
    fn abort_returns_the_revert_and_leaves_no_entry() {
        let mut rep = replica();
        let mut h = History::default();
        h.begin();
        assert_eq!(h.abort(), None, "nothing written, nothing to revert");

        let page = rep.new_id();
        let place = rep.doc().slot(NodeId::ROOT, Position::Last);
        h.begin();
        edit(
            &mut rep,
            &mut h,
            vec![Change::kind(page, Kind::Page), Change::place(page, place)],
        );
        rep.edit(h.abort().expect("revert")).expect("valid");
        assert!(!rep.doc().is_live(page));
        assert_eq!(h.undo(), None);
    }

    #[test]
    fn a_rejected_batch_drops_its_entry() {
        let mut rep = replica();
        let mut h = History::default();
        let page = rep.new_id();
        let place = rep.doc().slot(NodeId::ROOT, Position::Last);
        let e = rep
            .edit(vec![
                Change::kind(page, Kind::Page),
                Change::place(page, place),
            ])
            .expect("valid");
        h.record(e.batch, e.inverse);
        h.rejected(e.batch);
        assert_eq!(h.undo(), None);
    }
}
