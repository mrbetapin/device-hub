//! `SettingSpec` / `ActionSpec` を自動でカードに並べる (どのデバイスでも同じ描き方)。

use crate::app::{tab_of_section, PopItem, Shell, Tab};
use crate::ui::{self, Btn};
use devhub_core::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::rc::Rc;

/// section の表示順と見出し。
pub const SECTIONS: &[(&str, &str, &str)] = &[
    (section::PERFORMANCE, "パフォーマンス", "gauge"),
    (section::SENSOR, "センサー", "radar"),
    (section::CLICK, "クリック / ホイール", "mouse-pointer-click"),
    (section::MAGNETIC, "磁気スイッチ", "magnet"),
    (section::POWER, "電源 / インジケーター", "battery-charging"),
    (section::KEYBOARD, "キーボード", "keyboard"),
    (section::LIGHTING, "ライティング全般", "lightbulb"),
    (section::CONNECTION, "接続", "radio"),
    (section::AUDIO, "オーディオ", "volume-2"),
    (section::SYSTEM, "メンテナンス", "wrench"),
];

pub fn section_meta(id: &str) -> (&'static str, &'static str) {
    SECTIONS
        .iter()
        .find(|s| s.0 == id)
        .map(|s| (s.1, s.2))
        .unwrap_or(("その他", "sliders-horizontal"))
}

impl Shell {
    /// そのタブに属する section のカード (設定 + 操作) を並べて返す。
    pub fn section_cards(
        &mut self,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(s) = self.cur() else { return vec![] };
        let (Some(caps), Some(state)) = (s.caps.clone(), s.state.clone()) else {
            return vec![];
        };
        let kb = caps.keyboard.is_some();
        // 既知の順 → 未知の section の順。
        let mut order: Vec<String> = SECTIONS.iter().map(|s| s.0.to_string()).collect();
        for sp in &caps.settings {
            if !order.contains(&sp.section) {
                order.push(sp.section.clone());
            }
        }
        for a in &caps.actions {
            if !order.contains(&a.section) {
                order.push(a.section.clone());
            }
        }
        let mut out = vec![];
        for sec in order {
            if tab_of_section(&sec, kb) != tab {
                continue;
            }
            let specs: Vec<SettingSpec> = caps
                .settings
                .iter()
                .filter(|sp| sp.section == sec && visible(sp, &state))
                // EQ ボリュームは EQ カード (プリアンプ) に出す。
                .filter(|sp| !(caps.peq.is_some() && sp.id == "eq_volume"))
                .cloned()
                .collect();
            let actions: Vec<ActionSpec> = caps
                .actions
                .iter()
                .filter(|a| a.section == sec && !a.id.ends_with("_finish"))
                // DEF は EQ カードのプリセットに含める。
                .filter(|a| !(caps.peq.is_some() && a.id == "eq_def"))
                .cloned()
                .collect();
            if specs.is_empty() && actions.is_empty() {
                continue;
            }
            let (title, icon) = section_meta(&sec);
            let mut body = ui::body();
            for (i, sp) in specs.iter().enumerate() {
                let mut r = self.setting_row(sp, state.settings.get(&sp.id), i == 0, window, cx);
                // enabled_when を満たさない設定は薄く (理由はツールチップ代わりに下へ)。
                if let Err(why) = crate::pages::perf::enabled(sp, &state) {
                    r = div()
                        .flex()
                        .flex_col()
                        .child(r.opacity(0.4))
                        .child(ui::faint(why).pb(px(6.)));
                }
                body = body.child(r);
            }
            for (i, a) in actions.iter().enumerate() {
                body = body.child(self.action_row(a, specs.is_empty() && i == 0, cx));
            }
            // 磁気スイッチのカードにライブの押下深さバー (作動点を決めながら見られる)。
            if sec == section::MAGNETIC
                && caps
                    .mouse
                    .as_ref()
                    .is_some_and(|m| !m.live_buttons.is_empty())
            {
                if self.cur().is_some_and(|s| !s.travel_on) {
                    if let Some(id) = self.device_id() {
                        self.hub.set_travel(&id, true);
                    }
                }
                body = body.child(self.btn_depth_row());
            }
            out.push(
                ui::card_titled(title, icon, None)
                    .child(body)
                    .into_any_element(),
            );
        }
        out
    }

    pub fn setting_row(
        &mut self,
        sp: &SettingSpec,
        value: Option<&SettingValue>,
        first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = crate::theme::pal();
        let help = sp.help.as_deref();
        let row = |ctl: AnyElement| {
            if first {
                ui::row_first(&sp.label, help, ctl)
            } else {
                ui::row(&sp.label, help, ctl)
            }
        };
        let Some(v) = value.cloned() else {
            return row(ui::faint("読み取れません").into_any_element()).opacity(0.6);
        };
        let id = sp.id.clone();
        let confirm = sp.confirm.clone();
        // 確認が要る設定は、確認後に送る。
        let send = Rc::new(
            move |this: &mut Shell, nv: SettingValue, cx: &mut Context<Shell>| {
                let change = Change::Setting {
                    id: id.clone(),
                    value: nv,
                };
                if let Some(msg) = &confirm {
                    let c2 = change.clone();
                    this.confirm(
                        "確認",
                        msg,
                        "変更する",
                        true,
                        Rc::new(move |this, _, cx| this.apply(c2.clone(), cx)),
                        cx,
                    );
                } else {
                    this.apply(change, cx);
                }
            },
        );
        match &sp.kind {
            SettingKind::Toggle => {
                let on = v.as_bool();
                let s2 = send.clone();
                row(
                    ui::toggle(format!("set-{}", sp.id), on, cx, move |this, nv, cx| {
                        s2(this, SettingValue::Bool(nv), cx)
                    })
                    .into_any_element(),
                )
            }
            SettingKind::Slider {
                min,
                max,
                step,
                unit,
            } => {
                let (min, max, step) = (*min, *max, *step);
                let cur = v.as_num();
                let id = sp.id.clone();
                let has_confirm = sp.confirm.is_some();
                let s2 = send.clone();
                let fid = format!("set-{}", sp.id);
                // 直接入力した値はそのまま (必要なら確認を挟んで) 送る。
                let s3 = send.clone();
                let inp = self.num_field(
                    &fid,
                    cur,
                    min,
                    max,
                    step,
                    unit,
                    window,
                    cx,
                    Rc::new(move |this, nv, cx| s3(this, SettingValue::Num(nv), cx)),
                );
                row(ui::slider_value_edit(
                    &fid,
                    cur,
                    min,
                    max,
                    step,
                    &inp,
                    cx,
                    move |this, nv, cx| {
                        if (nv - cur).abs() < step * 0.5 {
                            return;
                        }
                        if has_confirm && nv <= min + step * 0.5 {
                            // 最小値 (0ms 等) だけ確認する。ドラッグ中は確認せずに止める。
                            if !crate::motion::is_down() {
                                s2(this, SettingValue::Num(nv), cx);
                            }
                            return;
                        }
                        this.apply(
                            Change::Setting {
                                id: id.clone(),
                                value: SettingValue::Num(nv),
                            },
                            cx,
                        );
                    },
                )
                .into_any_element())
            }
            SettingKind::Choice { options } => {
                let cur = v.as_choice();
                let sel = options.iter().position(|o| o.value == cur);
                // セグメント化は収まるものだけ: ≤4 個なら中程度のラベルまで、
                // ≤8 個なら短いラベルのみ。それ以上はドロップダウンに逃がす
                // (DAC フィルター 5 択のようにカードからはみ出すのを防ぐ)。
                let short = (options.len() <= 4
                    && options.iter().all(|o| o.label.chars().count() <= 8))
                    || (options.len() <= 8 && options.iter().all(|o| o.label.chars().count() <= 4));
                let opts = options.clone();
                if short {
                    let labels: Vec<String> = options.iter().map(|o| o.label.clone()).collect();
                    let w = (labels.len() as f32 * if labels.len() > 5 { 46. } else { 64. })
                        .clamp(140., 360.);
                    let s2 = send.clone();
                    row(ui::segmented(
                        &format!("set-{}", sp.id),
                        &labels,
                        sel.unwrap_or(usize::MAX),
                        cx,
                        move |this, i, cx| {
                            if Some(i) != sel {
                                s2(this, SettingValue::Choice(opts[i].value), cx);
                            }
                        },
                    )
                    .w(px(w))
                    .into_any_element())
                } else {
                    let items: Vec<PopItem> = options
                        .iter()
                        .map(|o| PopItem::new(o.label.clone(), o.value == cur))
                        .collect();
                    let label = sel
                        .map(|i| options[i].label.clone())
                        .unwrap_or_else(|| format!("{cur}"));
                    let s2 = send.clone();
                    let _ = p;
                    row(ui::select(
                        &format!("set-{}", sp.id),
                        items,
                        &label,
                        180.,
                        cx,
                        move |this, i, _, cx| {
                            s2(this, SettingValue::Choice(opts[i].value), cx);
                        },
                    )
                    .into_any_element())
                }
            }
        }
    }

    pub fn action_row(&mut self, a: &ActionSpec, first: bool, cx: &mut Context<Self>) -> Div {
        let running = self.cur().and_then(|s| s.running.clone());
        let busy = running.as_deref() == Some(a.label.as_str());
        let a2 = a.clone();
        let btn = ui::button(
            format!("act-{}", a.id),
            if busy { "実行中…" } else { "実行" },
            Some(if a.file.is_some() {
                "file-up"
            } else if a.danger {
                "rotate-ccw"
            } else {
                "play"
            }),
            if a.danger { Btn::Danger } else { Btn::Glass },
            cx,
            move |this, w, cx| this.start_action(&a2, w, cx),
        );
        let label = a.label.clone();
        let help = a.help.clone();
        if first {
            ui::row_first(&label, help.as_deref(), btn)
        } else {
            ui::row(&label, help.as_deref(), btn)
        }
    }

    pub fn start_action(&mut self, a: &ActionSpec, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.device_id() else { return };
        if self.cur().and_then(|s| s.running.as_ref()).is_some() {
            self.toast("別の操作を実行中です", true, cx);
            return;
        }
        // キャリブレーションはウィザードで行う (押下深さを見ながら全キーを確認する)。
        if a.id == "calibrate" {
            let live = self
                .cur()
                .and_then(|s| s.caps.as_ref())
                .and_then(|c| c.keyboard.as_ref())
                .and_then(|k| k.perf.as_ref())
                .is_some_and(|p| p.live_travel);
            if live {
                self.modal = Some(crate::app::Modal::Calibrate {
                    started: false,
                    maxes: vec![],
                });
                cx.notify();
                return;
            }
        }
        let a = a.clone();
        let (title, confirm, danger) = (a.label.clone(), a.confirm.clone(), a.danger);
        let run = Rc::new(
            move |this: &mut Shell, _: &mut Window, cx: &mut Context<Shell>| {
                if a.file.is_none() {
                    this.hub.run_action(&id, &a.id, &a.label, None);
                    cx.notify();
                    return;
                }
                // ファイル選択は gpui の非同期ダイアログで開く。クリック処理の中で
                // rfd の同期ダイアログを開くと、Windows ではダイアログのメッセージループが
                // gpui のウィンドウ処理に再入して落ちる。
                let picked: futures::future::LocalBoxFuture<'static, Option<std::path::PathBuf>> =
                    if a.save {
                        let dir = std::env::var_os("USERPROFILE")
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        let rx = cx.prompt_for_new_path(&dir, Some("device-hub-config.json"));
                        Box::pin(async move { rx.await.ok().and_then(|r| r.ok()).flatten() })
                    } else {
                        let rx = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: None,
                        });
                        Box::pin(async move {
                            rx.await.ok().and_then(|r| r.ok()).flatten().and_then(|mut v| v.pop())
                        })
                    };
                let (id, a) = (id.clone(), a.clone());
                cx.spawn(async move |this: WeakEntity<Shell>, cx: &mut AsyncApp| {
                    let file = picked.await;
                    let Some(file) = file else { return };
                    let _ = this.update(cx, |this, cx| {
                        this.hub.run_action(&id, &a.id, &a.label, Some(file));
                        cx.notify();
                    });
                })
                .detach();
            },
        );
        if let Some(c) = &confirm {
            self.confirm(&title, c, "実行", danger, run, cx);
        } else {
            run(self, window, cx);
        }
    }
}

/// visible_when の判定。
pub fn visible(sp: &SettingSpec, st: &DeviceState) -> bool {
    match &sp.visible_when {
        None => true,
        Some((id, v)) => st.settings.get(id).is_some_and(|cur| match (cur, v) {
            (SettingValue::Bool(a), SettingValue::Bool(b)) => a == b,
            (a, b) => a.as_choice() == b.as_choice(),
        }),
    }
}

