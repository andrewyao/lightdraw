//! Portable component definitions. One type serves built-in `library/*.ron`
//! files and "save selection as component", so there is one instantiate path.
//! A definition holds local part numbers, never `NodeId`s.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::doc::{Doc, MAX_TEXT};
use crate::model::{
    Attr, CellBox, CellVec, Change, Extent, FracIndex, Intrinsic, Kind, NodeId, Place, Prop,
};
use crate::resolve::Node;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComponentDef {
    pub name: String,
    /// The master's box, W by H by D cells. Instances take this size.
    pub size: [u32; 3],
    /// Parents before children.
    pub parts: Vec<PartDef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartDef {
    pub local: u32,
    /// None means a direct child of the master.
    pub parent: Option<u32>,
    pub kind: Kind,
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryError(pub String);

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LibraryError {}

fn err<T>(msg: impl Into<String>) -> Result<T, LibraryError> {
    Err(LibraryError(msg.into()))
}

/// Parts are plain geometry. Instances, connectors, and scenes refer to other
/// nodes by id, which a portable definition cannot hold.
fn portable(kind: Kind) -> bool {
    matches!(kind, Kind::Frame | Kind::Shape | Kind::Text)
}

impl ComponentDef {
    /// Parses and validates RON text.
    #[cfg(feature = "ron")]
    pub fn from_ron(text: &str) -> Result<ComponentDef, LibraryError> {
        let opts = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::UNWRAP_NEWTYPES);
        let def: ComponentDef = opts
            .from_str(text)
            .map_err(|e| LibraryError(e.to_string()))?;
        def.validate()?;
        Ok(def)
    }

    #[cfg(feature = "ron")]
    pub fn to_ron(&self) -> String {
        let opts = ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::UNWRAP_NEWTYPES);
        opts.to_string_pretty(self, ron::ser::PrettyConfig::default())
            .expect("in-memory encode cannot fail")
    }

    /// Checks what serde cannot: unique part numbers, parents declared first
    /// and able to hold children, portable kinds, and the text limit.
    pub fn validate(&self) -> Result<(), LibraryError> {
        let mut kinds = HashMap::new();
        for p in &self.parts {
            if !portable(p.kind) {
                return err(format!(
                    "part {}: {:?} cannot be part of a definition",
                    p.local, p.kind
                ));
            }
            if let Some(parent) = p.parent {
                match kinds.get(&parent) {
                    None => {
                        return err(format!(
                            "part {}: parent {parent} is not declared before it",
                            p.local
                        ));
                    }
                    Some(k) if !Kind::holds_children(*k) => {
                        return err(format!(
                            "part {}: parent {parent} cannot hold children",
                            p.local
                        ));
                    }
                    Some(_) => {}
                }
            }
            if p.attrs
                .iter()
                .any(|a| matches!(a, Attr::Text(t) if t.len() > MAX_TEXT))
            {
                return err(format!("part {}: text too long", p.local));
            }
            if kinds.insert(p.local, p.kind).is_some() {
                return err(format!("part {}: duplicate number", p.local));
            }
        }
        Ok(())
    }

    /// "Save selection as component". `roots` must be live siblings. The
    /// definition is translated so the selection's bounding box starts at the
    /// master's origin.
    pub fn capture(doc: &Doc, name: &str, roots: &[NodeId]) -> Result<ComponentDef, LibraryError> {
        let Some(&first) = roots.first() else {
            return err("empty selection");
        };
        let parent = doc.parent(first);
        if !doc.is_live(first) || roots.iter().any(|r| doc.parent(*r) != parent) {
            return err("selection must be live siblings");
        }
        let mut boxes = Vec::new();
        for &r in roots {
            match doc.node(r) {
                Some(Node::Frame(b) | Node::Text(b) | Node::Shape { body: b, .. }) => {
                    boxes.push(b.bounds)
                }
                _ => return err("selection holds a node that cannot be captured"),
            }
        }
        let corner = |b: &CellBox| [b.min.x.0, b.min.y.0, b.min.z.0];
        let lo = [0, 1, 2].map(|i| boxes.iter().map(|b| corner(b)[i]).min().expect("non-empty"));
        let hi = [0, 1, 2].map(|i| {
            let ext = |b: &CellBox| [b.w.0, b.h.0, b.d.0][i];
            boxes
                .iter()
                .map(|b| corner(b)[i].saturating_add_unsigned(ext(b)))
                .max()
                .expect("non-empty")
        });
        let origin = CellVec::new(lo[0], lo[1], lo[2]);

        let mut locals: HashMap<NodeId, u32> = HashMap::new();
        let mut parts = Vec::new();
        let mut stack: Vec<(NodeId, Option<u32>)> =
            roots.iter().rev().map(|r| (*r, None)).collect();
        while let Some((n, parent)) = stack.pop() {
            if locals.contains_key(&n) {
                continue;
            }
            let Some(kind) = doc.kind(n).filter(|k| portable(*k)) else {
                return err(format!("cannot capture a {:?}", doc.kind(n)));
            };
            let local = locals.len() as u32;
            locals.insert(n, local);
            let mut attrs: Vec<Attr> = doc
                .registers(n)
                .filter_map(|p| match p {
                    Prop::Attr(a) => Some(a.clone()),
                    _ => None,
                })
                .collect();
            if parent.is_none() {
                let at = boxes[roots.iter().position(|r| *r == n).expect("a root")]
                    .min
                    .minus(origin);
                attrs.retain(|a| !matches!(a, Attr::X(_) | Attr::Y(_) | Attr::Z(_)));
                attrs.extend([Attr::X(at.x), Attr::Y(at.y), Attr::Z(at.z)]);
            }
            parts.push(PartDef {
                local,
                parent,
                kind,
                attrs,
            });
            let kids: Vec<NodeId> = doc.children(n).collect();
            stack.extend(kids.into_iter().rev().map(|k| (k, Some(local))));
        }
        let size = [0, 1, 2].map(|i| hi[i].abs_diff(lo[i]));
        let def = ComponentDef {
            name: name.to_string(),
            size,
            parts,
        };
        def.validate()?;
        Ok(def)
    }

    /// Changes creating the master at `place`. Submit them as one batch.
    pub fn master_changes(
        &self,
        mint: &mut impl FnMut() -> NodeId,
        place: Place,
    ) -> (NodeId, Vec<Change>) {
        let master = mint();
        let [w, h, d] = self.size;
        let mut out = vec![
            Change::kind(master, Kind::Component),
            Change::place(master, place),
            Change::set(master, Attr::W(Extent(w))),
            Change::set(master, Attr::H(Extent(h))),
            Change::set(master, Attr::D(Extent(d))),
            Change::set(master, Attr::Text(self.name.clone())),
        ];
        let mut ids = HashMap::new();
        let mut last: HashMap<NodeId, FracIndex> = HashMap::new();
        for p in &self.parts {
            let id = mint();
            ids.insert(p.local, id);
            let parent = p.parent.map_or(master, |l| ids[&l]);
            let index = last
                .get(&parent)
                .map_or_else(FracIndex::first, FracIndex::after);
            last.insert(parent, index.clone());
            out.push(Change::kind(id, p.kind));
            out.push(Change::place(id, Place { parent, index }));
            out.extend(p.attrs.iter().cloned().map(|a| Change::set(id, a)));
        }
        (master, out)
    }

    /// Changes placing an instance of `master` at `at`, relative to the parent
    /// named in `place`.
    pub fn instance_changes(
        mint: &mut impl FnMut() -> NodeId,
        master: NodeId,
        place: Place,
        at: CellVec,
    ) -> (NodeId, Vec<Change>) {
        let id = mint();
        let changes = vec![
            Change::kind(id, Kind::Instance),
            Change::place(id, place),
            Change::intrinsic(id, Intrinsic::Component(master)),
            Change::set(id, Attr::X(at.x)),
            Change::set(id, Attr::Y(at.y)),
            Change::set(id, Attr::Z(at.z)),
        ];
        (id, changes)
    }
}
