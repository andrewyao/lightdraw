mod common;

use common::{Net, create, make, room, sized};
use doc::{Attr, Cell, Change, Doc, Intrinsic, Kind, NodeId, OverridePath, Position};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, TestRunner};

const PEERS: usize = 3;

#[derive(Clone, Debug)]
enum What {
    Shape,
    Frame,
    Move,
    Reparent,
    Detach,
    Text,
    Instance,
    Override,
}

#[derive(Clone, Debug)]
enum Op {
    Edit {
        who: usize,
        what: What,
        a: usize,
        b: usize,
    },
    Undo(usize),
    Up(usize),
    Down(usize),
    Disconnect(usize),
    Rejoin(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let who = 0..PEERS;
    let what = prop_oneof![
        Just(What::Shape),
        Just(What::Frame),
        Just(What::Move),
        Just(What::Reparent),
        Just(What::Reparent),
        Just(What::Detach),
        Just(What::Text),
        Just(What::Instance),
        Just(What::Override),
    ];
    prop_oneof![
        6 => (who.clone(), what, any::<usize>(), any::<usize>()).prop_map(|(who, what, a, b)| Op::Edit { who, what, a, b }),
        2 => who.clone().prop_map(Op::Undo),
        5 => who.clone().prop_map(Op::Up),
        5 => who.clone().prop_map(Op::Down),
        1 => who.clone().prop_map(Op::Disconnect),
        1 => who.prop_map(Op::Rejoin),
    ]
}

#[derive(Debug, Default)]
struct Stats {
    applied: usize,
    offline_edits: usize,
    undos: usize,
    server_rejections: usize,
    instances: usize,
}

fn pick<T: Copy>(items: &[T], i: usize) -> Option<T> {
    (!items.is_empty()).then(|| items[i % items.len()])
}

fn of_kind(doc: &Doc, kinds: &[Kind]) -> Vec<NodeId> {
    doc.ids()
        .into_iter()
        .filter(|n| doc.kind(*n).is_some_and(|k| kinds.contains(&k)))
        .collect()
}

/// The changes peer `who` would submit for `what`, read from its own view.
fn changes(net: &mut Net, who: usize, what: &What, a: usize, b: usize) -> Option<Vec<Change>> {
    let doc = net.doc(who);
    let holders = of_kind(doc, &[Kind::Page, Kind::Frame, Kind::Component]);
    let movable = of_kind(
        doc,
        &[Kind::Frame, Kind::Shape, Kind::Instance, Kind::Component],
    );
    let components = of_kind(doc, &[Kind::Component]);
    let instances = of_kind(doc, &[Kind::Instance]);
    let x = Cell((b % 7) as i32);
    Some(match what {
        What::Shape | What::Frame => {
            let kind = if matches!(what, What::Shape) {
                Kind::Shape
            } else {
                Kind::Frame
            };
            let parent = pick(&holders, a)?;
            let id = net.id(who);
            create(net.doc(who), id, kind, parent, vec![Attr::X(x)])
        }
        What::Move => vec![Change::set(pick(&movable, a)?, Attr::X(x))],
        What::Reparent => {
            let node = pick(&movable, a)?;
            let parent = pick(&holders, b)?;
            vec![Change::place(node, doc.slot(parent, Position::First))]
        }
        What::Detach => vec![Change::detach(pick(&movable, a)?)],
        What::Text => vec![Change::set(pick(&movable, a)?, Attr::Text(format!("t{b}")))],
        What::Instance => {
            let component = pick(&components, a)?;
            let parent = pick(&holders, b)?;
            let id = net.id(who);
            let mut out = create(net.doc(who), id, Kind::Instance, parent, vec![Attr::X(x)]);
            out.push(Change::intrinsic(id, Intrinsic::Component(component)));
            out
        }
        What::Override => {
            let inst = pick(&instances, a)?;
            let part = doc.node(inst).and_then(|n| match n {
                doc::Node::Instance { component, .. } => doc.children(component).next(),
                _ => None,
            })?;
            vec![Change::set_override(
                inst,
                OverridePath::to(part),
                Attr::Text(format!("o{b}")),
            )]
        }
    })
}

fn run(ops: &[Op]) -> (Net, Stats) {
    let (mut net, page) = room(PEERS);
    make(&mut net, 0, Kind::Frame, page, sized(0, 0, 0, 4, 4, 0));
    make(&mut net, 0, Kind::Frame, page, sized(5, 0, 0, 4, 4, 0));
    let comp = make(&mut net, 0, Kind::Component, page, sized(0, 9, 0, 2, 2, 2));
    make(&mut net, 0, Kind::Shape, comp, sized(0, 0, 0, 1, 1, 1));
    let mut stats = Stats::default();

    for op in ops {
        match op {
            Op::Edit { who, what, a, b } => {
                let Some(batch) = changes(&mut net, *who, what, *a, *b) else {
                    continue;
                };
                if net.edit(*who, batch).is_ok() {
                    stats.applied += 1;
                    stats.offline_edits += usize::from(!net.peers[*who].online);
                    stats.instances += usize::from(matches!(what, What::Instance));
                }
            }
            Op::Undo(who) => stats.undos += usize::from(matches!(net.undo(*who), Some(Ok(_)))),
            Op::Up(who) => {
                net.up(*who);
            }
            Op::Down(who) => {
                if net.down(*who).is_some_and(|u| u.rejected.is_some()) {
                    stats.server_rejections += 1;
                }
            }
            Op::Disconnect(who) => net.disconnect(*who),
            Op::Rejoin(who) => net.rejoin(*who),
        }
        let who = match op {
            Op::Edit { who, .. } | Op::Undo(who) | Op::Up(who) | Op::Down(who) => *who,
            Op::Disconnect(who) | Op::Rejoin(who) => *who,
        };
        assert_walk_terminates(net.doc(who));
    }
    for who in 0..PEERS {
        net.rejoin(who);
    }
    net.settle();
    (net, stats)
}

/// An optimistic view can hold transient cycles. The full walk must still end.
fn assert_walk_terminates(doc: &Doc) {
    let bound = 50 * (doc.ids().len() + 1);
    let walked = doc.occurrences(NodeId::ROOT).take(bound).count();
    assert!(walked < bound, "occurrences did not terminate");
}

/// What `commit` promises about every state the authority reaches.
fn assert_authority_invariants(doc: &Doc) {
    for n in doc.ids() {
        assert!(
            doc.ancestry(n).last().and_then(|a| doc.parent(a)).is_none(),
            "{n:?} is in a parent cycle"
        );
        if let Some(doc::Node::Instance { component, .. }) = doc.node(n).filter(|_| doc.is_live(n))
        {
            assert!(doc.is_live(component), "{n:?} shows a detached component");
            assert_eq!(doc.kind(component), Some(Kind::Component));
        }
    }
}

proptest! {
    #![proptest_config(Config { cases: 256, ..Config::default() })]

    #[test]
    fn replicas_converge_to_the_authority_under_any_interleaving(ops in prop::collection::vec(op(), 0..120)) {
        let (net, _) = run(&ops);
        net.assert_converged();
        assert_authority_invariants(net.auth.doc());
    }
}

#[test]
fn the_interleaving_generator_reaches_the_hard_cases() {
    let mut runner = TestRunner::deterministic();
    let mut total = Stats::default();
    for _ in 0..64 {
        let ops = prop::collection::vec(op(), 60..120)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let (_, s) = run(&ops);
        total.applied += s.applied;
        total.offline_edits += s.offline_edits;
        total.undos += s.undos;
        total.server_rejections += s.server_rejections;
        total.instances += s.instances;
    }
    assert!(total.server_rejections > 0, "{total:?}");
    assert!(total.offline_edits > 0, "{total:?}");
    assert!(total.undos > 0, "{total:?}");
    assert!(total.instances > 0, "{total:?}");
}
