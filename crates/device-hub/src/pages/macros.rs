//! マクロ (マウス・キーボード共通の編集画面)。

use crate::app::{Modal, PopItem, Recording, Shell};
use crate::ui::{self, Btn};
use crate::{motion, theme};
use devhub_core::keys;
use devhub_core::*;
use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::rc::Rc;
use std::time::Instant;

fn macros_of(this: &Shell) -> Vec<Option<MacroDef>> {
    let Some(st) = this.cur().and_then(|s| s.state.as_ref()) else {
        return vec![];
    };
    st.mouse
        .as_ref()
        .map(|m| m.macros.clone())
        .or_else(|| st.keyboard.as_ref().map(|k| k.macros.clone()))
        .unwrap_or_default()
}

fn event_label(e: &MacroEvent) -> String {
    match e.input {
        MacroInput::Key(u) => keys::hid_name(u),
        MacroInput::Mouse(m) => keys::mouse_fn_name(m).to_string(),
    }
}

impl Shell {
    pub fn page_macros(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::pal();
        let Some(mcap) = self.macro_caps() else {
            return div().into_any_element();
        };
        let list = macros_of(self);
        self.macro_sel = self.macro_sel.min(list.len().saturating_sub(1));
        let sel = self.macro_sel;

        // ---- スロット一覧
        let mut slots = ui::body().pt(px(4.)).gap(px(2.));
        for (i, m) in list.iter().enumerate() {
            let on = i == sel;
            let (name, sub) = match m {
                Some(m) => (m.name.clone(), format!("{} イベント", m.events.len())),
                None => (format!("スロット {}", i + 1), "空".into()),
            };
            let defined = m.is_some();
            slots = slots.child(
                div()
                    .id(SharedString::from(format!("mslot-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent_soft))
                    .when(!on, |d| d.hover(|s| s.bg(p.ctrl)))
                    .child(
                        div()
                            .size(px(28.))
                            .rounded(px(9.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(if defined {
                                theme::alpha(p.accent, 0.2)
                            } else {
                                p.ctrl
                            })
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(if defined { p.accent } else { p.faint })
                            .child(format!("{}", i + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(if defined { p.fg } else { p.sub })
                                    .child(name),
                            )
                            .child(ui::faint(sub)),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.macro_sel = i;
                        this.macro_draft = None;
                        this.recording = None;
                        this.macro_event_sel = None;
                        let name = macros_of(this)
                            .get(i)
                            .and_then(|m| m.clone())
                            .map(|m| m.name)
                            .unwrap_or_default();
                        this.set_input("macro_name", &name, w, cx);
                        cx.notify();
                    })),
            );
        }
        let slot_card = ui::card_titled("マクロ一覧", "list-video", None)
            .w(px(260.))
            .flex_none()
            .child(slots.pb(px(14.)));

        // ---- 編集
        if self.macro_draft.is_none() {
            if let Some(Some(m)) = list.get(sel) {
                self.macro_draft = Some(m.clone());
                let n = m.name.clone();
                self.input("macro_name", "", "名前", window, cx);
                self.set_input("macro_name", &n, window, cx);
            }
        }
        let editor: AnyElement = match self.macro_draft.clone() {
            None => ui::card()
                .flex_1()
                .child(
                    div()
                        .relative()
                        .p(px(24.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(ui::empty_state(
                            "list-video",
                            "このスロットは空です",
                            "キー操作を記録してマクロを作れます。",
                        ))
                        .child(ui::button(
                            "macro-new",
                            "新規作成",
                            Some("plus"),
                            Btn::Primary,
                            cx,
                            move |this, w, cx| {
                                let mode = this
                                    .macro_caps()
                                    .and_then(|m| m.modes.first().map(|o| o.value as u8))
                                    .unwrap_or(0);
                                let name = format!("マクロ {}", sel + 1);
                                this.macro_draft = Some(MacroDef {
                                    name: name.clone(),
                                    events: vec![],
                                    mode,
                                    repeat: 1,
                                });
                                this.input("macro_name", "", "名前", w, cx);
                                this.set_input("macro_name", &name, w, cx);
                                cx.notify();
                            },
                        )),
                )
                .into_any_element(),
            Some(d) => self
                .macro_editor(&mcap, d, sel, window, cx)
                .into_any_element(),
        };

        div()
            .flex()
            .items_start()
            .gap(px(theme::GAP))
            .child(slot_card)
            .child(div().flex_1().min_w_0().child(editor))
            .into_any_element()
    }

    fn macro_editor(
        &mut self,
        mcap: &MacroCaps,
        d: MacroDef,
        slot: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = theme::pal();
        let recording = self.recording.is_some();
        let name_in = self.input("macro_name", &d.name, "名前", window, cx);

        // モード。
        let items: Vec<PopItem> = mcap
            .modes
            .iter()
            .map(|o| PopItem::new(o.label.clone(), o.value as u8 == d.mode))
            .collect();
        let mode_label = mcap
            .modes
            .iter()
            .find(|o| o.value as u8 == d.mode)
            .map(|o| o.label.clone())
            .unwrap_or_default();
        let modes = mcap.modes.clone();
        let mut props = ui::body().pt(px(4.));
        if mcap.names {
            props = props.child(ui::row_first(
                "名前",
                None,
                div().w(px(260.)).child(Input::new(&name_in)),
            ));
        }
        props = props.child(
            ui::row(
                "実行モード",
                None,
                ui::select(
                    "macro-mode",
                    items,
                    &mode_label,
                    300.,
                    cx,
                    move |this, i, _, cx| {
                        if let Some(d) = this.macro_draft.as_mut() {
                            d.mode = modes[i].value as u8;
                        }
                        cx.notify();
                    },
                ),
            )
            .when(!mcap.names, |r| r.border_t_0()),
        );
        if mcap.repeat_modes.contains(&d.mode) {
            props = props.child(ui::row(
                "繰り返し回数",
                None,
                ui::stepper(
                    "macro-rep",
                    d.repeat as i64,
                    1,
                    255,
                    "回",
                    cx,
                    |this, v, cx| {
                        if let Some(d) = this.macro_draft.as_mut() {
                            d.repeat = v as u16;
                        }
                        cx.notify();
                    },
                ),
            ));
        }

        // 記録ツール。
        let rec_mode = self
            .recording
            .as_ref()
            .map(|r| r.delay_mode)
            .unwrap_or(REC_MODE.with(|c| c.get()));
        let rec_btn = if recording {
            ui::button(
                "rec-stop",
                "記録を停止",
                Some("square"),
                Btn::Danger,
                cx,
                |this, _, cx| {
                    this.recording = None;
                    cx.notify();
                },
            )
        } else {
            ui::button(
                "rec-start",
                "記録を開始",
                Some("circle"),
                Btn::Primary,
                cx,
                move |this, w, cx| {
                    let mode = REC_MODE.with(|c| c.get());
                    this.recording = Some(Recording {
                        started: Instant::now(),
                        last: Instant::now(),
                        delay_mode: mode,
                        fixed_ms: 20,
                        mods: Modifiers::default(),
                    });
                    w.focus(&this.focus, cx);
                    cx.notify();
                },
            )
        };
        let mut insert = div().flex().flex_wrap().gap(px(6.)).child(rec_btn).child(
            ui::segmented(
                "rec-mode",
                &["実時間".into(), "固定 20ms".into(), "遅延なし".into()],
                rec_mode as usize,
                cx,
                |this, i, _| {
                    REC_MODE.with(|c| c.set(i as u8));
                    if let Some(r) = this.recording.as_mut() {
                        r.delay_mode = i as u8;
                    }
                },
            )
            .w(px(260.)),
        );
        insert = insert.child(ui::button(
            "ins-key",
            "キーを挿入",
            Some("keyboard"),
            Btn::Glass,
            cx,
            |this, _, cx| {
                this.modal = Some(Modal::CaptureKey {
                    title: "挿入するキー".into(),
                    got: None,
                    on_key: Rc::new(|this, k, _, cx| {
                        let at = this.macro_event_sel.map(|i| i + 1);
                        if let Some(d) = this.macro_draft.as_mut() {
                            let mut ev = vec![];
                            for (bit, u) in [
                                (mods::LCTRL, 0xE0u16),
                                (mods::LSHIFT, 0xE1),
                                (mods::LALT, 0xE2),
                                (mods::LGUI, 0xE3),
                            ] {
                                if k.mods & bit != 0 {
                                    ev.push(MacroEvent {
                                        input: MacroInput::Key(u),
                                        edge: MacroEdge::Down,
                                        delay_ms: 10,
                                    });
                                }
                            }
                            if k.usage != 0 {
                                ev.push(MacroEvent {
                                    input: MacroInput::Key(k.usage),
                                    edge: MacroEdge::Down,
                                    delay_ms: 20,
                                });
                                ev.push(MacroEvent {
                                    input: MacroInput::Key(k.usage),
                                    edge: MacroEdge::Up,
                                    delay_ms: 10,
                                });
                            }
                            for (bit, u) in [
                                (mods::LGUI, 0xE3u16),
                                (mods::LALT, 0xE2),
                                (mods::LSHIFT, 0xE1),
                                (mods::LCTRL, 0xE0),
                            ] {
                                if k.mods & bit != 0 {
                                    ev.push(MacroEvent {
                                        input: MacroInput::Key(u),
                                        edge: MacroEdge::Up,
                                        delay_ms: 10,
                                    });
                                }
                            }
                            let pos = at.unwrap_or(d.events.len()).min(d.events.len());
                            d.events.splice(pos..pos, ev);
                        }
                        cx.notify();
                    }),
                });
                cx.notify();
            },
        ));
        if mcap.mouse_events {
            let items: Vec<PopItem> = keys::MOUSE_FNS[..5]
                .iter()
                .map(|m| PopItem::new(keys::mouse_fn_name(*m), false))
                .collect();
            insert = insert.child(ui::select(
                "ins-mouse",
                items,
                "マウスクリックを挿入",
                190.,
                cx,
                |this, i, _, cx| {
                    let m = keys::MOUSE_FNS[i];
                    let at = this.macro_event_sel.map(|i| i + 1);
                    if let Some(d) = this.macro_draft.as_mut() {
                        let pos = at.unwrap_or(d.events.len()).min(d.events.len());
                        d.events.splice(
                            pos..pos,
                            [
                                MacroEvent {
                                    input: MacroInput::Mouse(m),
                                    edge: MacroEdge::Down,
                                    delay_ms: 20,
                                },
                                MacroEvent {
                                    input: MacroInput::Mouse(m),
                                    edge: MacroEdge::Up,
                                    delay_ms: 10,
                                },
                            ],
                        );
                    }
                    cx.notify();
                },
            ));
        }

        // イベント列。
        let max_delay = mcap.max_delay_ms;
        let mut events = div().flex().flex_col().gap(px(2.));
        if d.events.is_empty() {
            events = events.child(
                div()
                    .h(px(90.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(if recording { p.danger } else { p.hair })
                    .child(ui::faint(if recording {
                        "キーを押すと記録されます…"
                    } else {
                        "「記録を開始」してキーボードを操作してください"
                    })),
            );
        }
        let total: u32 = d.events.iter().map(|e| e.delay_ms).sum();
        for (i, e) in d.events.iter().enumerate() {
            let on = self.macro_event_sel == Some(i);
            let down = e.edge == MacroEdge::Down;
            let delay = e.delay_ms;
            // 遅延は直接入力もできる (確定で下書きへ反映)。
            let delay_in = self.num_field(
                &format!("mev-d-{i}"),
                delay as f32,
                0.,
                max_delay as f32,
                1.,
                "",
                window,
                cx,
                Rc::new(move |this: &mut Shell, v: f32, cx: &mut Context<Shell>| {
                    if let Some(e) = this.macro_draft.as_mut().and_then(|d| d.events.get_mut(i)) {
                        e.delay_ms = (v.max(0.).round() as u32).min(max_delay);
                    }
                    cx.notify();
                }),
            );
            events = events.child(
                div()
                    .id(SharedString::from(format!("mev-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .h(px(36.))
                    .rounded(px(10.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent_soft))
                    .when(!on, |d| d.hover(|s| s.bg(p.ctrl)))
                    .child(
                        div()
                            .w(px(26.))
                            .text_size(px(11.))
                            .text_color(p.faint)
                            .child(format!("{}", i + 1)),
                    )
                    .child(ui::icon(
                        if down { "arrow-down" } else { "arrow-up" },
                        14.,
                        if down { p.accent } else { p.ok },
                    ))
                    .child(div().flex_1().text_size(px(13.)).child(format!(
                        "{} {}",
                        event_label(e),
                        if down { "押す" } else { "離す" }
                    )))
                    .child(ui::stepper_input(
                        &format!("mev-d-{i}"),
                        &delay_in,
                        "ms",
                        delay > 0,
                        delay < max_delay,
                        cx,
                        move |this, _| {
                            if let Some(e) =
                                this.macro_draft.as_mut().and_then(|d| d.events.get_mut(i))
                            {
                                e.delay_ms = e.delay_ms.saturating_sub(if e.delay_ms > 100 {
                                    10
                                } else {
                                    1
                                });
                            }
                        },
                        move |this, _| {
                            if let Some(e) =
                                this.macro_draft.as_mut().and_then(|d| d.events.get_mut(i))
                            {
                                e.delay_ms = (e.delay_ms + if e.delay_ms >= 100 { 10 } else { 1 })
                                    .min(max_delay);
                            }
                        },
                    ))
                    .child(ui::icon_button(
                        format!("mev-x-{i}"),
                        "x",
                        cx,
                        move |this, _, cx| {
                            if let Some(d) = this.macro_draft.as_mut() {
                                if i < d.events.len() {
                                    d.events.remove(i);
                                }
                            }
                            this.macro_event_sel = None;
                            cx.notify();
                        },
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.macro_event_sel = if this.macro_event_sel == Some(i) {
                            None
                        } else {
                            Some(i)
                        };
                        cx.notify();
                    })),
            );
        }
        if recording {
            motion::keep_alive();
        }

        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(ui::faint(format!(
                "{} / {} イベント · 合計 {:.2} 秒",
                d.events.len(),
                mcap.max_events,
                total as f32 / 1000.
            )))
            .child(div().flex_1())
            .child(ui::button(
                "macro-clear",
                "クリア",
                Some("eraser"),
                Btn::Ghost,
                cx,
                |this, _, cx| {
                    if let Some(d) = this.macro_draft.as_mut() {
                        d.events.clear();
                    }
                    cx.notify();
                },
            ))
            .child(ui::button(
                "macro-del",
                "削除",
                Some("trash"),
                Btn::Danger,
                cx,
                move |this, _, cx| {
                    this.confirm(
                    "マクロを削除",
                    "このスロットのマクロを削除します。割り当て済みのボタンは動作しなくなります。",
                    "削除",
                    true,
                    Rc::new(move |this, _, cx| {
                        this.apply(Change::Macro { slot, def: None }, cx);
                        this.macro_draft = None;
                    }),
                    cx,
                );
                },
            ))
            .child(ui::button(
                "macro-save",
                "デバイスに保存",
                Some("save"),
                Btn::Primary,
                cx,
                move |this, _, cx| {
                    this.recording = None;
                    let name = this.input_value("macro_name", cx);
                    if let Some(mut d) = this.macro_draft.clone() {
                        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
                            d.name = n.trim().to_string();
                        }
                        if d.events.is_empty() {
                            this.toast("イベントが空です", true, cx);
                            return;
                        }
                        // 押したまま終わるキーには離すイベントを補う。
                        let mut held: Vec<MacroInput> = vec![];
                        for e in &d.events {
                            match e.edge {
                                MacroEdge::Down => held.push(e.input),
                                MacroEdge::Up => held.retain(|h| *h != e.input),
                            }
                        }
                        for h in held {
                            d.events.push(MacroEvent {
                                input: h,
                                edge: MacroEdge::Up,
                                delay_ms: 0,
                            });
                        }
                        this.macro_draft = Some(d.clone());
                        this.apply(Change::Macro { slot, def: Some(d) }, cx);
                        this.toast("マクロを保存しました", false, cx);
                    }
                },
            ));

        ui::card_titled(
            &format!("スロット {} を編集", slot + 1),
            "pencil",
            recording.then(|| ui::chip("● 記録中", p.danger).into_any_element()),
        )
        .child(props)
        .child(ui::body().gap(px(10.)).pt(px(6.)).child(insert))
        .child(ui::body().child(events))
        .child(ui::body().pb(px(16.)).child(footer))
    }
}

thread_local! {
    static REC_MODE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
