//! `window.__lightdraw`: read-only getters for automated browser checks.
//!
//! - `occurrences()` every occurrence on the canvas page: `id`, `kind`,
//!   `box` (world cells: x, y, z, w, h, d) or `from`/`to` for connectors,
//!   and `text`.
//! - `camera()` target, eye, yaw, pitch, distance.
//! - `peers()` the other clients: id, cursor, selection size.
//! - `me()` this client's id, the room, and whether it is online.

use doc::{CellBox, CellVec, End, Item};
use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use crate::editor::Shared;

fn obj(fields: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in fields {
        let _ = Reflect::set(&o, &JsValue::from_str(k), v);
    }
    o.into()
}

fn num(v: impl Into<f64>) -> JsValue {
    JsValue::from_f64(v.into())
}

fn cell(c: CellVec) -> JsValue {
    obj(&[("x", num(c.x.0)), ("y", num(c.y.0)), ("z", num(c.z.0))])
}

fn cellbox(b: CellBox) -> JsValue {
    obj(&[
        ("x", num(b.min.x.0)),
        ("y", num(b.min.y.0)),
        ("z", num(b.min.z.0)),
        ("w", num(b.w.0)),
        ("h", num(b.h.0)),
        ("d", num(b.d.0)),
    ])
}

fn end(e: &End) -> JsValue {
    match e {
        End::At(c) => obj(&[("at", cell(*c))]),
        End::On { id, world } => obj(&[
            ("on", JsValue::from_str(&format!("{:?}", id.node))),
            ("box", cellbox(*world)),
        ]),
    }
}

fn occurrences(sh: &Shared) -> JsValue {
    let ed = sh.borrow();
    let out = Array::new();
    let Some(page) = ed.page else {
        return out.into();
    };
    for occ in ed.doc().occurrences(page) {
        let id = JsValue::from_str(&format!("{:?}", occ.id.node));
        let kind = JsValue::from_str(&format!("{:?}", ed.doc().kind(occ.id.node)));
        let text = |l: Option<doc::Label>| l.map_or(JsValue::NULL, |l| JsValue::from_str(l.text));
        let v = match &occ.item {
            Item::Group(s) | Item::Text(s) | Item::Shape { solid: s, .. } => obj(&[
                ("id", id),
                ("kind", kind),
                ("instanced", JsValue::from_bool(!occ.id.via.is_empty())),
                ("box", cellbox(s.world)),
                ("text", text(s.label)),
            ]),
            Item::Connector {
                from, to, label, ..
            } => obj(&[
                ("id", id),
                ("kind", kind),
                ("from", end(from)),
                ("to", end(to)),
                ("text", text(*label)),
            ]),
            Item::Scene(_) => continue,
        };
        out.push(&v);
    }
    out.into()
}

fn camera(sh: &Shared) -> JsValue {
    let ed = sh.borrow();
    let c = &ed.camera;
    let v3 = |v: glam::DVec3| obj(&[("x", num(v.x)), ("y", num(v.y)), ("z", num(v.z))]);
    obj(&[
        ("target", v3(c.target)),
        ("eye", v3(c.eye())),
        ("yaw", num(c.yaw)),
        ("pitch", num(c.pitch)),
        ("distance", num(c.distance)),
    ])
}

fn peers(sh: &Shared) -> JsValue {
    let ed = sh.borrow();
    let out = Array::new();
    if let Some(r) = ed.collab.replica() {
        for (c, p) in r.peers() {
            if *c == r.client() {
                continue;
            }
            let cursor = p.cursor.map_or(JsValue::NULL, |pt| {
                let [x, y, z] = pt.get();
                obj(&[("x", num(x)), ("y", num(y)), ("z", num(z))])
            });
            out.push(&obj(&[
                ("id", num(c.get())),
                ("cursor", cursor),
                ("selection", num(p.selection.len() as f64)),
            ]));
        }
    }
    out.into()
}

fn me(sh: &Shared) -> JsValue {
    let ed = sh.borrow();
    obj(&[
        (
            "id",
            ed.collab
                .replica()
                .map_or(JsValue::NULL, |r| num(r.client().get())),
        ),
        ("room", JsValue::from_str(&ed.collab.room)),
        ("online", JsValue::from_bool(ed.collab.online())),
    ])
}

type Getter = fn(&Shared) -> JsValue;

pub fn install(sh: &Shared) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let hook = Object::new();
    let getters: [(&str, Getter); 4] = [
        ("occurrences", occurrences),
        ("camera", camera),
        ("peers", peers),
        ("me", me),
    ];
    for (name, f) in getters {
        let sh = sh.clone();
        let c = Closure::<dyn Fn() -> JsValue>::new(move || f(&sh));
        let _ = Reflect::set(&hook, &JsValue::from_str(name), c.as_ref());
        c.forget();
    }
    let _ = Reflect::set(&window, &JsValue::from_str("__lightdraw"), &hook);
}
