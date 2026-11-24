use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use client::components;
use client::interaction::{Button, Event, Key, Tool};
use client::look::{self, FIELDS};
use doc::Profile;
use engine::{HoloParams, View};
use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use web_sys::{DragEvent, Element, KeyboardEvent, PointerEvent, WheelEvent};

use crate::collab;
use crate::debug;
use crate::editor::{self, Editor, Shared, Ui, sync};

type Sv = StoredValue<Shared, LocalStorage>;

/// Runs `f` on the editor, then refreshes the chrome with no borrow held.
fn act<R>(sv: Sv, f: impl FnOnce(&mut Editor) -> R) -> R {
    let sh = sv.get_value();
    let r = f(&mut sh.borrow_mut());
    sync(&sh);
    r
}

fn button_of(e: &PointerEvent) -> Button {
    match e.button() {
        1 => Button::Middle,
        2 => Button::Secondary,
        _ => Button::Primary,
    }
}

fn client(e: &web_sys::MouseEvent) -> [f64; 2] {
    [f64::from(e.client_x()), f64::from(e.client_y())]
}

fn typing_into(e: &KeyboardEvent) -> bool {
    e.target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .is_some_and(|el| matches!(el.tag_name().as_str(), "TEXTAREA" | "INPUT"))
}

/// A prevented pointerdown moves no focus, so an open text overlay is
/// committed by hand. Its blur handler borrows the editor, so this runs
/// before any borrow.
fn blur_text() {
    let active = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .and_then(|a| a.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(a) = active.filter(|a| a.tag_name() == "TEXTAREA") {
        let _ = a.blur();
    }
}

const TOOLS: [(Tool, &str, &str); 7] = [
    (Tool::Select, "Select", "V"),
    (Tool::Draw(Profile::Rect), "Rect", "R"),
    (Tool::Draw(Profile::Ellipse), "Ellipse", "E"),
    (Tool::Draw(Profile::Diamond), "Diamond", "D"),
    (Tool::Draw(Profile::Hexagon), "Hexagon", "H"),
    (Tool::PushPull, "Push/Pull", "P"),
    (Tool::Connect, "Connect", "C"),
];

fn on_key(sv: Sv, ui: Ui, e: KeyboardEvent) {
    if typing_into(&e) {
        return;
    }
    let cmd = e.meta_key() || e.ctrl_key();
    let key = e.key();
    let ev = match key.as_str() {
        "z" | "Z" if cmd && e.shift_key() => Some(Key::Redo),
        "z" | "Z" if cmd => Some(Key::Undo),
        "y" if cmd => Some(Key::Redo),
        "Escape" => Some(Key::Escape),
        "Delete" | "Backspace" => Some(Key::Delete),
        "1" => Some(Key::View(View::Front)),
        "2" => Some(Key::View(View::Side)),
        "3" => Some(Key::View(View::Top)),
        _ => None,
    };
    if let Some(k) = ev {
        e.prevent_default();
        act(sv, |ed| ed.dispatch(Event::Key(k)));
        return;
    }
    if cmd || e.alt_key() {
        return;
    }
    if let Some((t, ..)) = TOOLS.iter().find(|(_, _, k)| k.eq_ignore_ascii_case(&key)) {
        act(sv, |ed| ed.set_tool(*t));
        return;
    }
    match key.as_str() {
        " " => {
            e.prevent_default();
            sv.with_value(|sh| sh.borrow_mut().space = true);
        }
        "f" | "F" => act(sv, |ed| ed.toggle_floors()),
        "PageUp" => {
            e.prevent_default();
            act(sv, |ed| ed.step_floor(true));
        }
        "PageDown" => {
            e.prevent_default();
            act(sv, |ed| ed.step_floor(false));
        }
        "l" | "L" => ui.lab.update(|v| *v = !*v),
        "k" | "K" => ui.library.update(|v| *v = !*v),
        _ => {}
    }
}

#[component]
pub fn App() -> impl IntoView {
    let hash = web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .unwrap_or_default();
    let room = collab::room_from_hash(&hash);
    let ui = Ui::new(editor::load_look());
    let sh: Shared = Rc::new(RefCell::new(Editor::new(ui, room.clone())));
    let sv: Sv = StoredValue::new_local(sh);
    let canvas = NodeRef::<html::Canvas>::new();
    let overlay = NodeRef::<html::Div>::new();
    let started = StoredValue::new(false);

    Effect::new(move |_| {
        let (Some(el), Some(ov)) = (canvas.get(), overlay.get()) else {
            return;
        };
        if started.get_value() {
            return;
        }
        started.set_value(true);
        let sh = sv.get_value();
        {
            let mut ed = sh.borrow_mut();
            let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
            let w = (f64::from(el.client_width()) * dpr).max(1.0) as u32;
            let h = (f64::from(el.client_height()) * dpr).max(1.0) as u32;
            el.set_width(w);
            el.set_height(h);
            ed.size = [w, h];
            ed.dpr = dpr;
            ed.canvas = Some(el.clone());
            ed.overlay = Some(Element::from(ov));
        }
        let gpu_sh = sh.clone();
        spawn_local(async move {
            match engine::Gpu::from_canvas(el).await {
                Ok(gpu) => gpu_sh.borrow_mut().gpu = Some(gpu),
                Err(e) => ui.status.set(format!("GPU unavailable: {e}")),
            }
        });
        editor::connect(&sh);
        debug::install(&sh);
        editor::animate(sh);
    });

    let keys = window_event_listener(ev::keydown, move |e| on_key(sv, ui, e));
    let keys_up = window_event_listener(ev::keyup, move |e: KeyboardEvent| {
        if e.key() == " " {
            sv.with_value(|sh| sh.borrow_mut().space = false);
        }
    });
    let hash_change = window_event_listener(ev::hashchange, move |_| {
        if let Some(w) = web_sys::window() {
            let _ = w.location().reload();
        }
    });
    on_cleanup(move || {
        keys.remove();
        keys_up.remove();
        hash_change.remove();
    });

    Effect::new(move |_| editor::save_look(&ui.holo.get()));

    view! {
        <main class="app">
            <canvas
                node_ref=canvas
                class="stage"
                on:pointerdown=move |e: PointerEvent| {
                    e.prevent_default();
                    blur_text();
                    if let Some(t) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) {
                        let _ = t.set_pointer_capture(e.pointer_id());
                    }
                    let b = button_of(&e);
                    act(sv, |ed| {
                        let p = ed.pointer(client(&e), e.shift_key());
                        ed.dispatch(Event::Down(b, p));
                    });
                }
                on:pointermove=move |e: PointerEvent| {
                    act(sv, |ed| {
                        let p = ed.pointer(client(&e), e.shift_key());
                        ed.dispatch(Event::Move(p));
                    });
                }
                on:pointerup=move |e: PointerEvent| {
                    act(sv, |ed| {
                        let p = ed.pointer(client(&e), e.shift_key());
                        ed.dispatch(Event::Up(p));
                    });
                }
                on:dblclick=move |e: web_sys::MouseEvent| {
                    act(sv, |ed| {
                        let p = ed.pointer(client(&e), e.shift_key());
                        ed.dispatch(Event::DoubleClick(p));
                    });
                }
                on:contextmenu=move |e: web_sys::MouseEvent| e.prevent_default()
                on:wheel=move |e: WheelEvent| {
                    e.prevent_default();
                    let scale = if e.delta_mode() == 1 { 16.0 } else { 1.0 };
                    act(sv, |ed| {
                        let px = [client(&e)[0] * ed.dpr, client(&e)[1] * ed.dpr];
                        ed.dispatch(Event::Wheel { px, delta: e.delta_y() * scale });
                    });
                }
                on:dragover=move |e: DragEvent| e.prevent_default()
                on:drop=move |e: DragEvent| {
                    e.prevent_default();
                    let key = e.data_transfer().and_then(|d| d.get_data("text/plain").ok());
                    if let Some(key) = key {
                        act(sv, |ed| ed.place(&key, Some(client(&e))));
                    }
                }
            ></canvas>
            <div node_ref=overlay class="overlay"></div>
            <TextEditor sv ui/>
            <Toolbar sv ui/>
            <aside class="room" data-testid="room">
                <div>
                    <b>{format!("#{room}")}</b>
                    " "
                    <span data-testid="status">{move || ui.status.get()}</span>
                </div>
                <ul class="peers">
                    {move || {
                        ui.peers
                            .get()
                            .into_iter()
                            .map(|p| {
                                let label = if p.me {
                                    format!("you ({})", p.id)
                                } else {
                                    format!("peer {}", p.id)
                                };
                                view! {
                                    <li style=format!("color: hsl({:.0},95%,72%)", p.hue)>
                                        {label}
                                    </li>
                                }
                            })
                            .collect_view()
                    }}
                </ul>
            </aside>
            <Show when=move || ui.library.get()>
                <Library sv ui/>
            </Show>
            <Show when=move || ui.lab.get()>
                <LookLab ui/>
            </Show>
        </main>
    }
}

#[component]
fn Toolbar(sv: Sv, ui: Ui) -> impl IntoView {
    let key = move |k: Key| move |_| act(sv, |ed| ed.dispatch(Event::Key(k)));
    view! {
        <nav class="toolbar">
            {TOOLS
                .iter()
                .map(|&(t, label, k)| {
                    view! {
                        <button
                            class:on=move || ui.tool.get() == t
                            title=format!("{label} ({k})")
                            on:click=move |_| act(sv, |ed| ed.set_tool(t))
                        >
                            {label}
                        </button>
                    }
                })
                .collect_view()}
            <span class="sep"></span>
            <button title="Delete (Del)" on:click=key(Key::Delete)>
                "Delete"
            </button>
            <button title="Undo (Cmd/Ctrl+Z)" on:click=key(Key::Undo)>
                "Undo"
            </button>
            <button title="Redo (Shift+Cmd/Ctrl+Z)" on:click=key(Key::Redo)>
                "Redo"
            </button>
            <span class="sep"></span>
            <button
                class:on=move || ui.floor.get().is_some()
                title="Prototype B: stacked floors (F, PageUp/PageDown)"
                on:click=move |_| act(sv, |ed| ed.toggle_floors())
            >
                {move || match ui.floor.get() {
                    Some(l) => format!("Floors: {l}"),
                    None => "Floors".into(),
                }}
            </button>
            <button
                class:on=move || ui.library.get()
                title="Components (K)"
                on:click=move |_| ui.library.update(|v| *v = !*v)
            >
                "Library"
            </button>
            <button
                class:on=move || ui.lab.get()
                title="Look lab (L)"
                on:click=move |_| ui.lab.update(|v| *v = !*v)
            >
                "Look lab"
            </button>
        </nav>
    }
}

#[component]
fn TextEditor(sv: Sv, ui: Ui) -> impl IntoView {
    let area = NodeRef::<html::Textarea>::new();
    Effect::new(move |_| {
        if let Some(t) = area.get() {
            let _ = t.focus();
        }
    });
    let flush_later = move || {
        set_timeout(
            move || act(sv, |ed| ed.flush_text()),
            Duration::from_millis(120),
        );
    };
    move || {
        ui.text.get().map(|t| {
            view! {
                <textarea
                    node_ref=area
                    class="text-edit"
                    data-testid="text-edit"
                    style=format!(
                        "left:{}px;top:{}px;width:{}px;height:{}px",
                        t.left,
                        t.top,
                        t.width,
                        t.height,
                    )
                    prop:value=t.value.clone()
                    on:input=move |e| {
                        let v = event_target_value(&e);
                        let composing = sv.with_value(|sh| sh.borrow().composing);
                        if !composing && act(sv, |ed| ed.typed(v)) {
                            flush_later();
                        }
                    }
                    on:compositionstart=move |_| {
                        sv.with_value(|sh| sh.borrow_mut().composing = true)
                    }
                    on:compositionend=move |e| {
                        let v = event_target_value(&e);
                        act(sv, |ed| {
                            ed.composing = false;
                            ed.typed(v);
                            ed.flush_text();
                        });
                    }
                    on:blur=move |_| {
                        act(sv, |ed| {
                            ed.flush_text();
                            ed.dispatch(Event::TextDone);
                        })
                    }
                    on:keydown=move |e: KeyboardEvent| {
                        e.stop_propagation();
                        let done = (e.key() == "Enter" && !e.shift_key()) || e.key() == "Escape";
                        if done && !e.is_composing() {
                            e.prevent_default();
                            if let Some(t) = area.get_untracked() {
                                let _ = t.blur();
                            }
                        }
                    }
                ></textarea>
            }
        })
    }
}

#[component]
fn Library(sv: Sv, ui: Ui) -> impl IntoView {
    let item = move |key: String, name: String| {
        let drag_key = key.clone();
        view! {
            <li
                draggable="true"
                title="Click to place at the middle of the view, or drag onto the canvas"
                on:dragstart=move |e: DragEvent| {
                    if let Some(d) = e.data_transfer() {
                        let _ = d.set_data("text/plain", &drag_key);
                    }
                }
                on:click=move |_| act(sv, |ed| ed.place(&key, None))
            >
                {name}
            </li>
        }
    };
    let builtins = components::builtins()
        .into_iter()
        .enumerate()
        .map(|(i, d)| item(format!("builtin:{i}"), d.name))
        .collect_view();
    let room = move || {
        ui.rev.get();
        sv.with_value(|sh| components::room(sh.borrow().doc()))
            .into_iter()
            .enumerate()
            .map(|(i, (_, name))| item(format!("room:{i}"), name))
            .collect_view()
    };
    let save = move |_| {
        let name = web_sys::window()
            .and_then(|w| {
                w.prompt_with_message_and_default("Component name", "Component")
                    .ok()
                    .flatten()
            })
            .filter(|n| !n.trim().is_empty());
        if let Some(name) = name {
            act(sv, |ed| ed.save_component(name.trim()));
        }
    };
    view! {
        <section class="panel library" data-testid="library">
            <h3>"Components"</h3>
            <button disabled=move || ui.selected.get() == 0 on:click=save>
                "Save selection as component"
            </button>
            <h4>"Built-in"</h4>
            <ul>{builtins}</ul>
            <h4>"In this room"</h4>
            <ul>{room}</ul>
        </section>
    }
}

#[component]
fn LookLab(ui: Ui) -> impl IntoView {
    let copy = move |_| {
        let text = look::to_ron(&ui.holo.get_untracked());
        if let Some(w) = web_sys::window() {
            let _ = w.navigator().clipboard().write_text(&text);
        }
    };
    view! {
        <section class="panel lab" data-testid="look-lab">
            <h3>"Look lab"</h3>
            <div class="presets">
                {HoloParams::presets()
                    .into_iter()
                    .map(|(name, p)| {
                        view! { <button on:click=move |_| ui.holo.set(p)>{name}</button> }
                    })
                    .collect_view()}
            </div>
            {FIELDS
                .iter()
                .map(|f| {
                    let f = *f;
                    view! {
                        <label class="slider">
                            <span>{f.name}</span>
                            <input
                                type="range"
                                min=f.min
                                max=f.max
                                step=(f.max - f.min) / 200.0
                                prop:value=move || (f.get)(&ui.holo.get()).to_string()
                                on:input=move |e| {
                                    if let Ok(v) = event_target_value(&e).parse::<f32>() {
                                        ui.holo.update(|p| (f.set)(p, v));
                                    }
                                }
                            />
                            <output>{move || format!("{:.3}", (f.get)(&ui.holo.get()))}</output>
                        </label>
                    }
                })
                .collect_view()}
            <button on:click=copy>"Copy as RON"</button>
        </section>
    }
}
