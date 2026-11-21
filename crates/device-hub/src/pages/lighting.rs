//! ライティング (領域ごとのエフェクト + キーごとの色)。

use crate::app::{Shell, Tab};
use crate::pages::keyboard::Cap;
use crate::ui::{self, Btn};
use crate::{overlay, theme};
use devhub_core::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

impl Shell {
    pub fn page_lighting(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = theme::pal();
        let Some(s) = self.cur() else {
            return div().into_any_element();
        };
        let (Some(caps), Some(state)) = (s.caps.clone(), s.state.clone()) else {
            return div().into_any_element();
        };
        let mut col = div().flex().flex_col().gap(px(theme::GAP));
        self.light_anim = false;
        if !caps.lighting.is_empty() {
            let ai = caps
                .lighting
                .iter()
                .position(|a| a.id == self.light_area)
                .unwrap_or(0);
            let area = caps.lighting[ai].clone();
            self.light_area = area.id;
            let ls = state
                .lighting
                .get(&area.id)
                .cloned()
                .unwrap_or(LightingState {
                    on: true,
                    effect: area.effects.first().map(|e| e.id).unwrap_or(0),
                    color: Rgb::hex(0xffffff),
                    brightness: area.brightness_max,
                    speed: area.speed_max / 2,
                    direction: 0,
                    per_key: BTreeMap::new(),
                    zone_on: vec![true; area.zones.len()],
                    palette: vec![Rgb::hex(0xffffff)],
                    palette_sel: 0,
                    color_correction: None,
                });
            let eff = area.effects.iter().find(|e| e.id == ls.effect).cloned();
            let aid = area.id;
            let ls_send = ls.clone();
            let send = move |f: Rc<dyn Fn(&mut LightingState)>| {
                let base = ls_send.clone();
                move |this: &mut Shell, cx: &mut Context<Shell>| {
                    let mut n = base.clone();
                    f(&mut n);
                    this.apply(
                        Change::Lighting {
                            area: aid,
                            state: n,
                        },
                        cx,
                    );
                }
            };

            let area_tabs: Option<AnyElement> = (caps.lighting.len() > 1).then(|| {
                let names: Vec<String> = caps.lighting.iter().map(|a| a.name.clone()).collect();
                let ids: Vec<u8> = caps.lighting.iter().map(|a| a.id).collect();
                ui::segmented("light-area", &names, ai, cx, move |this, i, _| {
                    this.light_area = ids[i]
                })
                .w(px((names.len() as f32 * 150.).min(440.)))
                .into_any_element()
            });

            // エフェクト一覧。
            let mut effects = div().flex().flex_wrap().gap(px(8.));
            for e in &area.effects {
                let on = e.id == ls.effect;
                let id = e.id;
                // isCustom セルは公式同様モード切替でも保持されるオーバーレイ。
                // 消すのはペイント側のクリアから明示的に行う。
                let s2 = send(Rc::new(move |l| l.effect = id));
                effects = effects.child(
                    div()
                        .id(SharedString::from(format!("eff-{}", e.id)))
                        .h(px(34.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(11.))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .border_1()
                        .when(on, |d| {
                            d.bg(p.accent_soft)
                                .border_color(theme::alpha(p.accent, 0.55))
                                .text_color(p.fg)
                        })
                        .when(!on, |d| {
                            d.bg(p.ctrl)
                                .border_color(p.hair)
                                .text_color(p.sub)
                                .hover(|s| s.bg(p.ctrl_hover))
                        })
                        .when(e.per_key, |d| d.child(ui::icon("brush", 13., p.accent)))
                        .child(e.name.clone())
                        .on_click(cx.listener(move |this, _, _, cx| s2(this, cx))),
                );
            }

            let mut body = ui::body().pt(px(6.));
            if area.switchable {
                // 全体トグル: ゾーンがある機種では全ゾーンをまとめて開閉する。
                let s2 = send(Rc::new(|l| {
                    l.on = !l.on;
                    for z in &mut l.zone_on {
                        *z = l.on;
                    }
                }));
                body = body.child(ui::row_first(
                    "点灯",
                    None,
                    ui::toggle(format!("light-on-{aid}"), ls.on, cx, move |this, _, cx| {
                        s2(this, cx)
                    }),
                ));
                // DoubleLighting: 公式と同じ上/下ライトスイッチ (jo enum に分解して送る)。
                for (zi, zname) in area.zones.iter().enumerate() {
                    let cur = ls.zone_on.get(zi).copied().unwrap_or(false);
                    let link = self.light_link;
                    let s2 = send(Rc::new(move |l| {
                        if l.zone_on.len() <= zi {
                            l.zone_on.resize(zi + 1, false);
                        }
                        l.zone_on[zi] = !l.zone_on[zi];
                        // 連動 ON のときは両ゾーンを揃える (公式の link トグルと同じ)。
                        if link {
                            let v = l.zone_on[zi];
                            l.zone_on.fill(v);
                        }
                        l.on = l.zone_on.iter().any(|&z| z);
                    }));
                    body = body.child(ui::row(
                        &format!("{zname}スイッチ"),
                        None,
                        ui::toggle(
                            format!("light-zone-{aid}-{zi}"),
                            cur,
                            cx,
                            move |this, _, cx| s2(this, cx),
                        ),
                    ));
                }
                // ライティング連動 (公式 link/link-unlink と同じ UI ローカルトグル)。
                if area.zones.len() > 1 {
                    let link = self.light_link;
                    body = body.child(ui::row(
                        "ライティング連動",
                        Some("ON のとき上/下スイッチを同時に動かします"),
                        ui::toggle(format!("light-link-{aid}"), link, cx, |this, _, _| {
                            this.light_link = !this.light_link;
                        }),
                    ));
                }
            }
            body = body.child(
                div()
                    .py(px(12.))
                    .when(area.switchable, |d| d.border_t_1().border_color(p.hair))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(ui::faint("エフェクト"))
                    .child(effects),
            );
            if eff.as_ref().is_some_and(|e| e.color) {
                let cur = ls.color;
                let base = ls.clone();
                body = body.child(ui::row(
                    "色",
                    None,
                    div()
                        .id("light-color")
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .cursor_pointer()
                        .child(ui::faint(format!("#{:06X}", cur.to_hex())))
                        .child(ui::swatch(theme::rgb_of(cur), 28., false))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            let base = base.clone();
                            overlay::pick_color(
                                this,
                                "ライティングの色",
                                cur,
                                w,
                                cx,
                                move |this, c, _, cx| {
                                    let mut n = base.clone();
                                    n.color = c;
                                    let sel = n.palette_sel as usize;
                                    if sel < n.palette.len() {
                                        n.palette[sel] = c;
                                    }
                                    this.apply(
                                        Change::Lighting {
                                            area: aid,
                                            state: n,
                                        },
                                        cx,
                                    );
                                },
                            );
                        })),
                ));
            }
            // パレット: 公式のカラー 8 ドット。クリックでスロット選択
            // (そのスロットの色が現在色になる)。
            if !ls.palette.is_empty() {
                let mut dots = div().flex().items_center().gap(px(8.));
                for (pi, pc) in ls.palette.iter().enumerate() {
                    let selected = pi == ls.palette_sel as usize;
                    let c = *pc;
                    let s2 = send(Rc::new(move |l| {
                        l.palette_sel = pi as u8;
                        l.color = c;
                    }));
                    // スロット0はファームの「マルチカラー/虹」固定 (公式の more アイコン相当)。
                    let dot = if pi == 0 {
                        let mut strips = div()
                            .size(px(22.))
                            .rounded(px(11.))
                            .overflow_hidden()
                            .flex()
                            .border_2()
                            .border_color(if selected {
                                p.fg
                            } else {
                                theme::alpha(p.fg, 0.12)
                            });
                        for hc in [0xff5040u32, 0xffd040, 0x40d060, 0x40b0ff, 0xa050ff] {
                            strips = strips
                                .child(div().h_full().flex_1().bg(theme::rgb_of(Rgb::hex(hc))));
                        }
                        strips.into_any_element()
                    } else {
                        ui::swatch(theme::rgb_of(c), 22., selected).into_any_element()
                    };
                    dots = dots.child(
                        div()
                            .id(SharedString::from(format!("pal-{aid}-{pi}")))
                            .cursor_pointer()
                            .child(dot)
                            .on_click(cx.listener(move |this, _, _, cx| s2(this, cx))),
                    );
                }
                body = body.child(ui::row("カラー", None, dots));
            }
            if eff.as_ref().is_none_or(|e| e.brightness) {
                let base = ls.clone();
                let bmax = area.brightness_max as f32;
                let f = Rc::new(move |this: &mut Shell, v: f32, cx: &mut Context<Shell>| {
                    let mut n = base.clone();
                    if n.brightness == v as u8 {
                        return;
                    }
                    n.brightness = v as u8;
                    this.apply(
                        Change::Lighting {
                            area: aid,
                            state: n,
                        },
                        cx,
                    );
                }) as Rc<dyn Fn(&mut Shell, f32, &mut Context<Shell>)>;
                let inp = self.num_field(
                    "light-br",
                    ls.brightness as f32,
                    0.,
                    bmax,
                    1.,
                    "",
                    window,
                    cx,
                    f.clone(),
                );
                body = body.child(ui::row(
                    "明るさ",
                    None,
                    ui::slider_value_edit(
                        "light-br",
                        ls.brightness as f32,
                        0.,
                        bmax,
                        1.,
                        &inp,
                        cx,
                        move |t, v, c| f(t, v, c),
                    ),
                ));
            }
            if eff.as_ref().is_some_and(|e| e.speed) {
                let base = ls.clone();
                let smax = area.speed_max as f32;
                let f = Rc::new(move |this: &mut Shell, v: f32, cx: &mut Context<Shell>| {
                    let mut n = base.clone();
                    if n.speed == v as u8 {
                        return;
                    }
                    n.speed = v as u8;
                    this.apply(
                        Change::Lighting {
                            area: aid,
                            state: n,
                        },
                        cx,
                    );
                }) as Rc<dyn Fn(&mut Shell, f32, &mut Context<Shell>)>;
                let inp = self.num_field(
                    "light-sp",
                    ls.speed as f32,
                    0.,
                    smax,
                    1.,
                    "",
                    window,
                    cx,
                    f.clone(),
                );
                body = body.child(ui::row(
                    "速度",
                    None,
                    ui::slider_value_edit(
                        "light-sp",
                        ls.speed as f32,
                        0.,
                        smax,
                        1.,
                        &inp,
                        cx,
                        move |t, v, c| f(t, v, c),
                    ),
                ));
            }
            if eff.as_ref().is_some_and(|e| e.direction) && !area.directions.is_empty() {
                let base = ls.clone();
                body = body.child(ui::row(
                    "方向",
                    None,
                    ui::segmented(
                        "light-dir",
                        &area.directions,
                        ls.direction as usize,
                        cx,
                        move |this, i, cx| {
                            let mut n = base.clone();
                            n.direction = i as u8;
                            this.apply(
                                Change::Lighting {
                                    area: aid,
                                    state: n,
                                },
                                cx,
                            );
                        },
                    )
                    .w(px((area.directions.len() as f32 * 76.).min(380.))),
                ));
            }
            // 彩度 (公式 LightingSaturation): R/G/B 0-100 の 3 スライダー →
            // [5,2,area,2,R,G,B] ColorCorrection。
            if let Some(cc) = ls.color_correction {
                body = body.child(
                    div()
                        .pt(px(12.))
                        .border_t_1()
                        .border_color(p.hair)
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(ui::faint("彩度")),
                );
                for (ch_i, ch_name, ch_v) in [(0usize, "R", cc.r), (1, "G", cc.g), (2, "B", cc.b)] {
                    let base = ls.clone();
                    let f = Rc::new(move |this: &mut Shell, v: f32, cx: &mut Context<Shell>| {
                        let mut n = base.clone();
                        let mut cc2 = n.color_correction.unwrap_or(Rgb::new(100, 100, 100));
                        match ch_i {
                            0 => cc2.r = v as u8,
                            1 => cc2.g = v as u8,
                            _ => cc2.b = v as u8,
                        }
                        n.color_correction = Some(cc2);
                        this.apply(
                            Change::Lighting {
                                area: aid,
                                state: n,
                            },
                            cx,
                        );
                    })
                        as Rc<dyn Fn(&mut Shell, f32, &mut Context<Shell>)>;
                    let inp = self.num_field(
                        &format!("light-cc{ch_i}"),
                        ch_v as f32,
                        0.,
                        100.,
                        1.,
                        "",
                        window,
                        cx,
                        f.clone(),
                    );
                    body = body.child(ui::row(
                        ch_name,
                        None,
                        ui::slider_value_edit(
                            &format!("light-cc{ch_i}"),
                            ch_v as f32,
                            0.,
                            100.,
                            1.,
                            &inp,
                            cx,
                            move |t, v, c| f(t, v, c),
                        ),
                    ));
                }
            }
            col = col.child(ui::card_titled(&area.name, "lightbulb", area_tabs).child(body));

            // ライティングプレビュー — 公式 HUB と同じ仕組み:
            // ワーカーが [5,3] で読み戻した**実機の現在の点灯色**をそのまま出す
            // (セッション live_rgb, ~6Hz 更新)。シミュレーションではないので
            // 模様・動き・色は常に実機と一致する。取れない間 (非対応ドライバ/
            // 初回読み出し前) だけローカル近似にフォールバックする。
            if let Some(kc) = caps.keyboard.clone() {
                let eff2 = eff.clone();
                let live: Option<std::collections::BTreeMap<usize, Rgb>> = self
                    .device_id()
                    .and_then(|id| self.hub.session(&id).map(|s| s.live_rgb.clone()))
                    .filter(|m| !m.is_empty());
                // 上ゾーン (per-key) が OFF なら描画対象が無いのでアニメーションも止める。
                let keys_lit =
                    ls.on && !(area.zones.len() >= 2 && ls.zone_on.first() == Some(&false));
                let animated = live.is_none()
                    && keys_lit
                    && eff2.as_ref().is_some_and(|e| e.speed && !e.per_key);
                self.light_anim = animated;
                let t = self.anim_t;
                let spd = if area.speed_max > 0 {
                    ls.speed as f32 / area.speed_max as f32
                } else {
                    0.5
                };
                let eff_ref = eff.clone();
                let colors: Vec<Option<Rgb>> = match &live {
                    // 実機そのまま — 消灯セルはマップに無いので None。
                    Some(m) => kc
                        .layout
                        .iter()
                        .enumerate()
                        .map(|(i, _)| m.get(&i).copied())
                        .collect(),
                    None => kc
                        .layout
                        .iter()
                        .enumerate()
                        .map(|(i, k)| preview_rgb(&ls, &area, eff_ref.as_ref(), k, i, t, spd))
                        .collect(),
                };
                // 「実機でプレビュー」ON なら DirectDrive フレームを ~30fps で流す
                // (probe9: 実機は ~87fps 連続送信でも輻輳しない)。
                // ライブライティング中はそちらが DirectDrive を使うので、プレビューは送らない。
                let fx_here = self.fx_on.is_some() && self.fx_on == self.device_id();
                let dd_on = !fx_here
                    && self
                        .device_id()
                        .and_then(|id| self.hub.session(&id))
                        .is_some_and(|s| s.dd_on);
                // ライブ読み取りが効く機種では実機プレビューは不要 — 残っていたら
                // 止めてファームの点灯 (＝ 上の表示と同じもの) に戻す。
                if live.is_some() && dd_on {
                    if let Some(id) = self.device_id() {
                        self.hub.set_dd(&id, false);
                    }
                }
                if live.is_none() && dd_on && self.dd_last.elapsed() >= Duration::from_millis(33) {
                    let cells: BTreeMap<usize, Rgb> = colors
                        .iter()
                        .enumerate()
                        .filter_map(|(i, c)| c.map(|c| (i, c)))
                        .collect();
                    if let Some(id) = self.device_id() {
                        self.hub.dd_frame(&id, cells);
                    }
                    self.dd_last = Instant::now();
                }
                let caps_v: Vec<Cap> = kc
                    .layout
                    .iter()
                    .zip(&colors)
                    .map(|(k, c)| Cap {
                        top: k.label.clone(),
                        tint: c.map(|c| theme::alpha(theme::rgb_of(c), 0.85)),
                        ..Default::default()
                    })
                    .collect();
                let view =
                    self.keyboard_view("lit-preview", &kc, caps_v, cx, Rc::new(|_, _, _, _| {}));
                let mut prev_body = ui::body().pt(px(10.)).pb(px(14.)).gap(px(8.)).child(view);
                // DirectDrive 対応エリアでは「実機でプレビュー」トグルを出す。
                // カードはコンテンツ幅がウィンドウを超え得るので、ヘッダー右端ではなく
                // 本文先頭 (常に見える左側) に置く。
                // ON の間、上の colors を ~30fps で [5,5] フレームとして実機へ流す;
                // OFF で DdStop を送りファーム側のアニメーションに戻す。
                if area.direct_drive && fx_here {
                    prev_body = prev_body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::button("fx-goto", "ライブライティング動作中", Some("zap"), Btn::Ghost, cx, |this, _, cx| {
                                this.tab = Tab::Live;
                                cx.notify();
                            }))
                            .child(ui::faint("実機の LED はライブライティングが駆動しています (停止すると通常の点灯に戻ります)")),
                    );
                } else if area.direct_drive && live.is_none() {
                    let dd_label = if dd_on {
                        "実機プレビュー停止"
                    } else {
                        "実機でプレビュー"
                    };
                    let dd_style = if dd_on { Btn::Primary } else { Btn::Ghost };
                    prev_body = prev_body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::button(
                                "dd-preview",
                                dd_label,
                                Some("monitor"),
                                dd_style,
                                cx,
                                move |this, _, cx| {
                                    if let Some(id) = this.device_id() {
                                        let cur = this.hub.session(&id).is_some_and(|s| s.dd_on);
                                        this.hub.set_dd(&id, !cur);
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(ui::faint(
                                "ON の間、上のイメージを実機の LED に直接流します",
                            )),
                    );
                }
                prev_body = prev_body.child(ui::faint(if live.is_some() {
                    "実機の現在の点灯を読み取って表示しています"
                } else {
                    "設定値をもとにしたイメージです (本体の実際の点灯とは近似です)"
                }));
                // isCustom セルはベースモードに関わらず点灯する — 残っているのに
                // カスタムモードを選んでいない場合、編集経路を案内する。
                if !ls.per_key.is_empty() {
                    prev_body = prev_body.child(ui::faint(format!(
                        "{} キーにカスタム色が設定されています (下の「キーごとの色」で編集・クリアできます)",
                        ls.per_key.len()
                    )));
                }
                // DirectDrive 対応エリアでは「実機でプレビュー」トグルを出す。
                // ON の間、上の colors を ~150ms 間引きで [5,5] フレームとして実機へ流す;
                // OFF で DdStop を送りファーム側のアニメーションに戻す。
                col = col.child(ui::card_titled("プレビュー", "eye", None).child(prev_body));
            }

            // キーごとの色。
            // キーごとのカスタム色はモードではなく常時使えるオーバーレイ
            // (公式のカスタムカラーと同じく、どのモードでも編集できる)。
            if area.per_key {
                if let Some(kc) = caps.keyboard.clone() {
                    let paint = self.paint;
                    let caps_v: Vec<Cap> = kc
                        .layout
                        .iter()
                        .enumerate()
                        .map(|(i, k)| Cap {
                            top: k.label.clone(),
                            tint: ls
                                .per_key
                                .get(&i)
                                .map(|c| theme::alpha(theme::rgb_of(*c), 0.75)),
                            ..Default::default()
                        })
                        .collect();
                    let base = ls.clone();
                    let view = self.keyboard_view(
                        "paint",
                        &kc,
                        caps_v,
                        cx,
                        Rc::new(move |this, i, shift, cx| {
                            let mut n = base.clone();
                            if shift {
                                n.per_key.remove(&i);
                            } else {
                                n.per_key.insert(i, this.paint);
                            }
                            this.apply(
                                Change::Lighting {
                                    area: aid,
                                    state: n,
                                },
                                cx,
                            );
                        }),
                    );
                    let base_all = ls.clone();
                    let base_clear = ls.clone();
                    let nkeys = kc.layout.len();
                    let tools = div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .id("paint-color")
                                .cursor_pointer()
                                .child(ui::swatch(theme::rgb_of(paint), 26., true))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    overlay::pick_color(
                                        this,
                                        "塗る色",
                                        paint,
                                        w,
                                        cx,
                                        |this, c, _, cx| {
                                            this.paint = c;
                                            cx.notify();
                                        },
                                    );
                                })),
                        )
                        .child(ui::button(
                            "paint-all",
                            "全キー",
                            Some("paint-bucket"),
                            Btn::Ghost,
                            cx,
                            move |this, _, cx| {
                                let mut n = base_all.clone();
                                n.per_key = (0..nkeys).map(|i| (i, this.paint)).collect();
                                this.apply(
                                    Change::Lighting {
                                        area: aid,
                                        state: n,
                                    },
                                    cx,
                                );
                            },
                        ))
                        .child(ui::button(
                            "paint-clear",
                            "クリア",
                            Some("eraser"),
                            Btn::Ghost,
                            cx,
                            move |this, _, cx| {
                                let mut n = base_clear.clone();
                                n.per_key.clear();
                                this.apply(
                                    Change::Lighting {
                                        area: aid,
                                        state: n,
                                    },
                                    cx,
                                );
                            },
                        ));
                    col = col.child(
                        ui::card_titled("キーごとの色", "brush", Some(tools.into_any_element()))
                            .child(
                                ui::body()
                                    .pt(px(10.))
                                    .pb(px(14.))
                                    .gap(px(10.))
                                    .child(view)
                                    .child(ui::faint("クリックで塗る · Shift+クリックで消す")),
                            ),
                    );
                }
            }
        }
        let cards = self.section_cards(Tab::Lighting, window, cx);
        for c in cards {
            col = col.child(c);
        }
        col.into_any_element()
    }
}

/// プレビュー用: 設定値 (effect/color/brightness/speed/direction/per_key) と時刻 t から
/// キー i の色を近似計算する。デバイスから LED 状態は読み戻せないため、公式 HUB の
/// 点灯イメージに相当するローカル再現。効果 id はドライバが並べたエフェクト順。
///
/// 色の有無・方向の有無は `LightEffect` の caps フラグに従う — 色を持つ効果は
/// 選択色で、持たない効果は色相が回る。方向つき効果だけ direction で掃引が反転する。
fn preview_rgb(
    ls: &LightingState,
    area: &LightArea,
    eff: Option<&LightEffect>,
    k: &LayoutKey,
    i: usize,
    t: f32,
    spd: f32,
) -> Option<Rgb> {
    if !ls.on {
        return None;
    }
    // DoubleLighting: zone_on = [上=per-key, 下=underglow]。キーボードプレビューは
    // per-key LED (=上ゾーン) を描くので、上ゾーンが OFF なら実機同様キーは消える。
    if area.zones.len() >= 2 && ls.zone_on.first().copied() == Some(false) {
        return None;
    }
    // キーの擬似乱数 (虹の明滅・雨用、index で固定)。
    let rnd = (((i as u32).wrapping_mul(2654435761) >> 8) % 1000) as f32 / 1000.;
    // 方向つきエフェクトだけ direction を見る (0=Forward→右掃引 / 1=Backward→左掃引)。
    let dir_sign = if eff.is_some_and(|e| e.direction) && ls.direction == 1 {
        -1.0f32
    } else {
        1.0
    };
    let axis = k.x * dir_sign;
    // 中心からの距離 (嵐・波形系用)。座標はキーキャップ単位。
    let (dx, dy) = (k.x - 10.5, k.y - 3.);
    let dist = (dx * dx + dy * dy).sqrt();
    let ph = t * (0.25 + spd * 2.0); // 位相速度
    let wave = |x: f32| (x * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let v = ls.brightness as f32 / area.brightness_max.max(1) as f32;
    let dim = |c: Rgb, f: f32| {
        Rgb::new(
            (c.r as f32 * f * v) as u8,
            (c.g as f32 * f * v) as u8,
            (c.b as f32 * f * v) as u8,
        )
    };
    let hsv = |h: f32, s: f32, b: f32| -> Rgb {
        let h = h.rem_euclid(1.) * 6.;
        let (p, q, tt) = (
            b * (1. - s),
            b * (1. - s * h.fract()),
            b * (1. - s * (1. - h.fract())),
        );
        let (r, g, bl) = match h as i32 {
            0 => (b, tt, p),
            1 => (q, b, p),
            2 => (p, b, tt),
            3 => (p, q, b),
            4 => (tt, p, b),
            _ => (b, p, q),
        };
        Rgb::new((r * 255.) as u8, (g * 255.) as u8, (bl * 255.) as u8)
    };
    // 実機検証で分かった構造: 各モードは (色相パターン hp, 明度パターン bp) を持ち、
    // sel=0 (ファーム虹スロット) では hp の虹で、色スロット選択時は選択色の色相で
    // bp の明暗だけが出る (sel=1=赤で全モードが赤系になったのをカメラで確認)。
    // 擬似押下点 (プレス系 17-19 のプレビュー用): 一定間隔で位置を変える。
    let seg = (ph * 1.6).floor() as u32;
    let px_i = (((seg.wrapping_mul(2654435761)) >> 8) % 19 + 1) as f32;
    let py_i = (((seg.wrapping_mul(2246822519)) >> 10) % 5) as f32;
    let press_decay = 1. - (ph * 1.6).fract(); // 押下後に減衰
    let (hp, bp): (f32, f32) = match ls.effect {
        // 固定色: 全面均一。sel0 は固定の横向き虹グラデ (動かない、勾配は
        // 実機撮像の B→C→G→Y→M→B→C に合わせ約1.2周)。
        0 => (0.72 - k.x * 0.068 + k.y * 0.01, 1.),
        // 虹の横ながれ: 全虹グラデが左右にゆっくりスクロール (実機 ~2.6s/周)。
        1 => (
            axis * 0.05 + ph * 0.3,
            0.75 + 0.25 * wave(axis * 0.18 - ph * 0.7),
        ),
        // ゆっくり変色: 行ごとの色相バンドがゆっくり循環。
        2 => (
            k.y * 0.11 + ph * 0.15,
            0.7 + 0.3 * wave(ph * 0.2 + k.y * 0.3),
        ),
        // すばやく変色: 中心対称の放射色相が回転。
        3 => (ph * 0.3 - dist * 0.05, 0.8 + 0.2 * wave(ph * 1.3)),
        // 虹の市松: 斜めの細かい虹チェッカーが流れる。
        4 => (
            axis * 0.07 + k.y * 0.08 + ph * 0.15,
            0.85 + 0.15 * wave(rnd * 4. + ph * 1.5),
        ),
        // 虹のめぐり: 大きな滑らかな横グラデが色相循環。
        5 => (axis * 0.028 + ph * 0.15, 0.8 + 0.2 * wave(ph * 0.8)),
        // 虹の明滅: 行ごとの緩い色相勾配 + キーごとの瞬き。
        6 => (
            k.y * 0.07 + ph * 0.15,
            0.45 + 0.55 * wave(ph * 2.2 + rnd * 6.),
        ),
        // ほのかな窓: ほぼ均一の中を薄い窓がゆっくり横切る
        // (実機は数秒かけて色相もゆっくり変わる)。
        7 => {
            let pos = (axis * 0.05 - ph * 0.15).rem_euclid(1.);
            (ph * 0.05, if pos < 0.25 { 0.75 } else { 1. })
        }
        // ゆるやか変形: ゆっくりした不規則な色相モーフ (パッチが染みのように変わる)。
        8 => (
            ph * 0.05 + (wave(axis * 0.11 + ph * 0.1) - wave(k.y * 0.2 - ph * 0.08)) * 0.1,
            0.55 + 0.45 * wave(dist * 0.3 + ph * 0.15),
        ),
        // 色帯シフト: ほぼ均一フィールドに別色相の帯が周期的にずれて入る
        // (実機は支配色が数秒ごとにゆっくり入れ替わる)。
        9 => {
            let zone = ((axis * 0.05 - ph * 0.1).rem_euclid(1.) - 0.5).abs() < 0.14;
            (
                ph * 0.06 + if zone { 0.14 } else { 0. },
                if zone { 1. } else { 0.85 },
            )
        }
        // 虹のうねり: 斜めのマルチバンド虹波。
        10 => (
            (axis + k.y * 3.) * 0.05 - ph * 0.2,
            0.3 + 0.7 * wave(axis * 0.16 + k.y * 0.14 - ph * 0.5),
        ),
        // 斜めの光帯: 色の塊が流れる (暗部と彩度の高い塊が混在)。
        11 => (
            axis * 0.04 + ph * 0.12 + wave(k.y * 0.2 + ph * 0.15) * 0.12,
            0.12 + 0.88 * wave(axis * 0.09 + k.y * 0.11 + ph * 0.25),
        ),
        // コンフェッティ: キーごとにバラバラの色相で咲く紙吹雪状のパッチ。
        12 => (
            rnd * 0.9 + ph * 0.25,
            0.15 + 0.85 * wave(ph * 1.3 + rnd * 5.),
        ),
        // 虹のモザイク: 細かい虹チェッカーが斜めに流れる。
        13 => (
            axis * 0.09 + k.y * 0.11 + ph * 0.15,
            0.65 + 0.35 * wave(rnd * 3. + ph * 1.1),
        ),
        // 光の雨: 列ごとに色ドットが上から下へ落ちる。
        14 => {
            let col_rnd =
                (((k.x * 37.) as u32).wrapping_mul(2654435761) >> 8) as f32 % 1000. / 1000.;
            let pos = (k.y * 0.14 - ph * (0.9 + col_rnd * 0.9) + col_rnd).rem_euclid(1.);
            (col_rnd * 0.9, if pos < 0.12 { 1. } else { 0.06 })
        }
        // 跳ねる光: 5x3 程度の点灯ブロックが位置と色を跳んで移動。
        15 => {
            let jx = (((seg.wrapping_mul(2654435761)) >> 8) % 15) as f32;
            let jy = (((seg.wrapping_mul(40503)) >> 10) % 4) as f32;
            let inb = (k.x - jx).abs() < 2.5 && (k.y - jy).abs() < 1.8;
            (seg as f32 * 0.13, if inb { 1. } else { 0.05 })
        }
        // カスタム: isCustom セルだけが描いた色で点灯、他は消灯 (末尾の overlay が描く)。
        16 => (0., 0.),
        // 押下・まわり: 押したキー周辺のブロックが点灯 (擬似押下で再現)。
        17 => {
            let lit = (k.x - px_i).abs() <= 1.5 && (k.y - py_i).abs() <= 1.5;
            (k.y * 0.1 + 0.15, if lit { press_decay } else { 0.04 })
        }
        // 押下・行: 押したキーの行が点灯。
        18 => {
            let lit = (k.y - py_i).abs() <= 0.6;
            (k.y * 0.1 + 0.15, if lit { press_decay } else { 0.04 })
        }
        // 押下・広域: 押したキー周囲の広範囲が点灯。
        19 => {
            let lit = ((k.x - px_i).powi(2) + (k.y - py_i).powi(2)).sqrt() <= 4.2;
            (k.y * 0.1 + 0.15, if lit { press_decay } else { 0.04 })
        }
        _ => (0.5, 0.6),
    };
    let base = if ls.palette_sel == 0 {
        hsv(hp, 0.9, v * bp)
    } else {
        dim(ls.color, bp)
    };
    // isCustom セルはベースモードに関わらずカスタム色で点灯する (公式と同じオーバーレイ)。
    Some(
        ls.per_key
            .get(&i)
            .copied()
            .map(|c| dim(c, 1.))
            .unwrap_or(base),
    )
}
