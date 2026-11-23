mod common;

use common::{Net, create, make, room, sized};
use doc::library::ComponentDef;
use doc::{
    Attr, CellVec, Change, Intrinsic, Item, Kind, Label, NodeId, OverridePath, Position, Reject,
};

fn instance(net: &mut Net, i: usize, component: NodeId, parent: NodeId, x: i32) -> Vec<Change> {
    let id = net.id(i);
    let mut out = create(
        net.doc(i),
        id,
        Kind::Instance,
        parent,
        sized(x, 0, 0, 0, 0, 0),
    );
    out.push(Change::intrinsic(id, Intrinsic::Component(component)));
    out
}

fn place_instance(net: &mut Net, i: usize, component: NodeId, parent: NodeId, x: i32) -> NodeId {
    let changes = instance(net, i, component, parent, x);
    let id = changes[0].node;
    net.edit(i, changes).unwrap();
    net.settle();
    id
}

fn component_with_part(net: &mut Net, page: NodeId) -> (NodeId, NodeId) {
    let comp = make(net, 0, Kind::Component, page, sized(0, 0, 0, 2, 2, 2));
    let part = make(net, 0, Kind::Shape, comp, sized(0, 0, 0, 1, 1, 1));
    (comp, part)
}

#[test]
fn deleting_a_component_with_a_live_instance_is_rejected() {
    let (mut net, page) = room(2);
    let (comp, _) = component_with_part(&mut net, page);
    let inst = place_instance(&mut net, 1, comp, page, 5);

    assert_eq!(
        net.edit(0, vec![Change::detach(comp)]).err(),
        Some(Reject::ComponentInUse {
            component: comp,
            instance: inst
        })
    );
    net.peers[0].undo.remove(0);
    let undo_create_component = net.peers[0].undo.remove(0);
    assert_eq!(
        net.edit(0, undo_create_component.1).err(),
        Some(Reject::ComponentInUse {
            component: comp,
            instance: inst
        })
    );
    assert!(net.doc(0).is_live(comp));
    net.assert_converged();
}

#[test]
fn a_concurrent_delete_rejects_whichever_of_delete_or_instance_lands_second() {
    for delete_first in [true, false] {
        let (mut net, page) = room(2);
        let (comp, _) = component_with_part(&mut net, page);
        net.edit(1, vec![Change::detach(comp)]).unwrap();
        let placed = instance(&mut net, 0, comp, page, 5);
        let inst = placed[0].node;
        net.edit(0, placed).unwrap();
        let (first, second) = if delete_first { (1, 0) } else { (0, 1) };
        net.up(first);
        net.up(second);
        net.settle();

        let (loser, why) = if delete_first {
            (
                0,
                Reject::NotAComponent {
                    instance: inst,
                    component: comp,
                },
            )
        } else {
            (
                1,
                Reject::ComponentInUse {
                    component: comp,
                    instance: inst,
                },
            )
        };
        assert_eq!(
            net.peers[loser].rejected.last().map(|r| r.why.clone()),
            Some(why)
        );
        assert_eq!(net.doc(0).is_live(comp), !delete_first);
        assert_eq!(net.doc(0).is_live(inst), !delete_first);
        net.assert_converged();
    }
    let (mut net, page) = room(1);
    let (comp, _) = component_with_part(&mut net, page);
    net.edit(0, vec![Change::detach(comp)]).unwrap();
    let placed = instance(&mut net, 0, comp, page, 5);
    let inst = placed[0].node;
    assert_eq!(
        net.edit(0, placed).err(),
        Some(Reject::NotAComponent {
            instance: inst,
            component: comp
        })
    );
}

#[test]
fn a_component_cannot_contain_itself_through_any_instance_chain() {
    let (mut net, page) = room(2);
    let a = make(&mut net, 0, Kind::Component, page, sized(0, 0, 0, 2, 2, 2));
    let b = make(&mut net, 0, Kind::Component, page, sized(5, 0, 0, 2, 2, 2));
    place_instance(&mut net, 0, b, a, 0);

    let a_in_b = instance(&mut net, 0, a, b, 0);
    let n = a_in_b[0].node;
    assert_eq!(
        net.edit(0, a_in_b).err(),
        Some(Reject::ComponentCycle {
            node: n,
            component: b
        })
    );
    let a_in_a = instance(&mut net, 0, a, a, 1);
    let m = a_in_a[0].node;
    assert_eq!(
        net.edit(0, a_in_a).err(),
        Some(Reject::ComponentCycle {
            node: m,
            component: a
        })
    );

    let c = make(&mut net, 0, Kind::Component, page, sized(9, 0, 0, 2, 2, 2));
    let c_in_b = instance(&mut net, 0, c, b, 1);
    let b_in_c = instance(&mut net, 1, b, c, 1);
    net.edit(0, c_in_b).unwrap();
    net.edit(1, b_in_c.clone()).unwrap();
    net.up(0);
    net.up(1);
    net.settle();
    assert_eq!(
        net.peers[1].rejected.last().map(|r| r.why.clone()),
        Some(Reject::ComponentCycle {
            node: b_in_c[0].node,
            component: c
        })
    );
    net.assert_converged();
}

#[test]
fn an_override_through_one_nested_instance_leaves_its_sibling_alone() {
    let (mut net, page) = room(1);
    let c = make(&mut net, 0, Kind::Component, page, sized(0, 0, 0, 2, 2, 2));
    let p = make(
        &mut net,
        0,
        Kind::Text,
        c,
        vec![Attr::Text("master".into())],
    );
    let m = make(&mut net, 0, Kind::Component, page, sized(0, 5, 0, 8, 2, 2));
    let first = place_instance(&mut net, 0, c, m, 0);
    let second = place_instance(&mut net, 0, c, m, 4);
    let outer = place_instance(&mut net, 0, m, page, 20);

    net.edit(
        0,
        vec![Change::set_override(
            outer,
            OverridePath::through(&[first], p),
            Attr::Text("first only".into()),
        )],
    )
    .unwrap();
    net.settle();

    let doc = net.doc(0);
    let texts: Vec<(Vec<NodeId>, Option<&str>)> = doc
        .occurrences(outer)
        .filter(|o| o.id.node == p)
        .map(|o| match o.item {
            Item::Text(s) => (o.id.via, s.label.map(|l: Label| l.text)),
            other => panic!("expected text, got {other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            (vec![outer, first], Some("first only")),
            (vec![outer, second], Some("master")),
        ]
    );
    assert!(doc.occurrences(c).any(|o| o.id.node == p
        && matches!(&o.item, Item::Text(s) if s.label.map(|l| l.text) == Some("master"))));
}

fn shape_boxes(doc: &doc::Doc, root: NodeId) -> Vec<(CellVec, Option<String>)> {
    doc.occurrences(root)
        .filter_map(|o| match o.item {
            Item::Shape { solid, .. } | Item::Text(solid) => {
                Some((solid.world.min, solid.label.map(|l| l.text.to_string())))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_captured_selection_instantiates_with_the_same_layout() {
    let (mut net, page) = room(2);
    let a = make(
        &mut net,
        0,
        Kind::Shape,
        page,
        [sized(3, 4, 0, 2, 2, 1), vec![Attr::Text("a".into())]].concat(),
    );
    let b = make(&mut net, 0, Kind::Shape, page, sized(6, 4, 0, 1, 1, 1));
    make(&mut net, 0, Kind::Shape, page, sized(50, 50, 0, 1, 1, 1));

    let def = ComponentDef::capture(net.doc(0), "pair", &[a, b]).unwrap();
    assert_eq!(def.size, [4, 2, 1]);
    let library = net.doc(0).slot(page, Position::Last);
    let (master, changes) = def.master_changes(&mut || net.peers[0].rep.new_id(), library);
    net.edit(0, changes).unwrap();
    let place = net.doc(0).slot(page, Position::Last);
    let (inst, changes) = ComponentDef::instance_changes(
        &mut || net.peers[0].rep.new_id(),
        master,
        place,
        CellVec::new(10, 10, 0),
    );
    net.edit(0, changes).unwrap();
    net.settle();

    assert_eq!(
        shape_boxes(net.doc(1), inst),
        vec![
            (CellVec::new(10, 10, 0), Some("a".into())),
            (CellVec::new(13, 10, 0), None),
        ]
    );
    net.assert_converged();
}

#[cfg(feature = "ron")]
mod ron_files {
    use super::*;

    const BUILT_INS: [(&str, &str); 2] = [
        (
            "server_rack",
            include_str!("../../../library/server_rack.ron"),
        ),
        (
            "labeled_card",
            include_str!("../../../library/labeled_card.ron"),
        ),
    ];

    #[test]
    fn a_definition_survives_a_ron_round_trip() {
        for (name, text) in BUILT_INS {
            let def = ComponentDef::from_ron(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                ComponentDef::from_ron(&def.to_ron()).unwrap(),
                def,
                "{name}"
            );
        }
    }

    #[test]
    fn built_in_composites_load_and_instantiate() {
        let rack = ComponentDef::from_ron(BUILT_INS[0].1).unwrap();
        let (mut net, page) = room(1);
        let library = net.doc(0).slot(page, Position::Last);
        let (master, changes) = rack.master_changes(&mut || net.peers[0].rep.new_id(), library);
        net.edit(0, changes).unwrap();
        let place = net.doc(0).slot(page, Position::Last);
        let (inst, changes) = ComponentDef::instance_changes(
            &mut || net.peers[0].rep.new_id(),
            master,
            place,
            CellVec::new(0, 0, 10),
        );
        net.edit(0, changes).unwrap();
        net.settle();

        let labels: Vec<Option<String>> = shape_boxes(net.doc(0), inst)
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        assert_eq!(
            labels,
            vec![
                Some("srv-1".into()),
                Some("srv-2".into()),
                Some("srv-3".into())
            ]
        );
        assert_eq!(shape_boxes(net.doc(0), inst)[2].0, CellVec::new(0, 0, 17));
        net.assert_converged();
    }

    #[test]
    fn a_definition_with_a_dangling_parent_is_refused() {
        let text = "(name: \"bad\", size: (1, 1, 1), parts: [(local: 0, parent: Some(7), kind: Shape, attrs: [])])";
        assert!(
            ComponentDef::from_ron(text)
                .unwrap_err()
                .0
                .contains("not declared")
        );
    }
}
