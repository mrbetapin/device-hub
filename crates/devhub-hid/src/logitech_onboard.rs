//! Logitech OnboardProfiles (0x8100) の本体メモリ形式 — プロファイルとマクロの符号化/復号。
//! 純粋関数のみ (HID I/O は `logitech.rs`)。形式は docs/LOGITECH_HIDPP.md
//! 「OnboardProfiles のプロファイルとマクロ」(G502 X LIGHTSPEED で実機確認) を参照。

use crate::logitech::crc_ccitt;
use devhub_core::model::*;

/// マクロを置く最初のセクタ (ユーザー領域: 0 = ディレクトリ、1..=5 = プロファイル)。
pub const MACRO_FIRST_SECTOR: u16 = 6;
/// マクロ枠の上限 (1 セクタ 1 本)。
pub const MACRO_MAX_SLOTS: usize = 10;

const BUTTONS_AT: usize = 0x20;
const DPI_AT: usize = 0x03;
const DPI_COUNT: usize = 5;

// ---------------------------------------------------------------- セクタ共通

/// 末尾 2B (BE) に CRC を入れる。
pub fn seal(d: &mut [u8]) {
    let n = d.len();
    let c = crc_ccitt(&d[..n - 2]).to_be_bytes();
    d[n - 2] = c[0];
    d[n - 1] = c[1];
}

pub fn crc_ok(d: &[u8]) -> bool {
    let n = d.len();
    n >= 2 && crc_ccitt(&d[..n - 2]) == u16::from_be_bytes([d[n - 2], d[n - 1]])
}

/// ディレクトリ (sector 0) のエントリ `[sector BE16, 有効, 予約]`。`ffff` で終端。
pub fn parse_dir(d: &[u8]) -> Vec<(u16, bool)> {
    d.chunks_exact(4)
        .map(|e| (u16::from_be_bytes([e[0], e[1]]), e[2] != 0))
        .take_while(|(s, _)| *s != 0xFFFF)
        .collect()
}

/// ディレクトリの `index` 番目のエントリを有効にする (変更したら true)。
pub fn enable_dir_entry(d: &mut [u8], index: usize) -> bool {
    let at = index * 4 + 2;
    if at < d.len() && d[at] == 0 {
        d[at] = 1;
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------- プロファイル

/// DPI 段 (LE16 × 5)。0 / 0xFFFF の段は使っていない。
pub fn dpis(d: &[u8]) -> Vec<u32> {
    (0..DPI_COUNT)
        .map(|i| u16::from_le_bytes([d[DPI_AT + i * 2], d[DPI_AT + i * 2 + 1]]))
        .take_while(|v| *v != 0 && *v != 0xFFFF)
        .map(u32::from)
        .collect()
}

/// DPI 段を書く (残りは 0 = 未使用)。既定段 (byte 1) とシフト段 (byte 2) は範囲内に収める。
pub fn set_dpis(d: &mut [u8], stages: &[u32]) {
    for i in 0..DPI_COUNT {
        let v = stages.get(i).map(|v| (*v).min(0xFFFE) as u16).unwrap_or(0);
        d[DPI_AT + i * 2..DPI_AT + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    let last = stages.len().clamp(1, DPI_COUNT) as u8 - 1;
    d[1] = d[1].min(last);
    d[2] = d[2].min(last);
}

pub fn default_stage(d: &[u8]) -> usize {
    d[1] as usize
}

pub fn set_default_stage(d: &mut [u8], i: usize) {
    d[1] = i.min(DPI_COUNT - 1) as u8;
}

/// レポート間隔 (byte 0, ms) → Hz。
pub fn report_hz(d: &[u8]) -> Option<i64> {
    (1..=125).contains(&d[0]).then(|| 1000 / d[0] as i64)
}

pub fn set_report_hz(d: &mut [u8], hz: i64) {
    d[0] = (1000 / hz.clamp(8, 1000)) as u8;
}

pub fn button(d: &[u8], i: usize) -> [u8; 4] {
    let at = BUTTONS_AT + i * 4;
    [d[at], d[at + 1], d[at + 2], d[at + 3]]
}

pub fn set_button(d: &mut [u8], i: usize, b: [u8; 4]) {
    let at = BUTTONS_AT + i * 4;
    d[at..at + 4].copy_from_slice(&b);
}

/// プロファイル名 (0xA0.., UTF-16LE)。
pub fn profile_name(d: &[u8]) -> String {
    if d.len() < 0xD0 {
        return String::new();
    }
    let units: Vec<u16> = d[0xA0..0xD0]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0 && *u != 0xFFFF)
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------- ボタン割り当て

fn mouse_mask(m: MouseFn) -> Option<u16> {
    Some(match m {
        MouseFn::Left => 0x01,
        MouseFn::Right => 0x02,
        MouseFn::Middle => 0x04,
        MouseFn::Back => 0x08,
        MouseFn::Forward => 0x10,
        _ => return None,
    })
}

fn mask_mouse(mask: u16) -> Option<MouseFn> {
    Some(match mask {
        0x01 => MouseFn::Left,
        0x02 => MouseFn::Right,
        0x04 => MouseFn::Middle,
        0x08 => MouseFn::Back,
        0x10 => MouseFn::Forward,
        _ => return None,
    })
}

const MEDIA: &[(MediaFn, u16)] = &[
    (MediaFn::PlayPause, 0x00CD),
    (MediaFn::Stop, 0x00B7),
    (MediaFn::Next, 0x00B5),
    (MediaFn::Prev, 0x00B6),
    (MediaFn::VolumeUp, 0x00E9),
    (MediaFn::VolumeDown, 0x00EA),
    (MediaFn::Mute, 0x00E2),
    (MediaFn::Calculator, 0x0192),
    (MediaFn::MyComputer, 0x0194),
    (MediaFn::Browser, 0x0196),
    (MediaFn::Mail, 0x018A),
    (MediaFn::BrightnessUp, 0x006F),
    (MediaFn::BrightnessDown, 0x0070),
    (MediaFn::Refresh, 0x0227),
];

/// 4B → UI の割り当て。表せないもの (G-Shift・電池表示・他所のマクロ等) は None
/// (UI には「無効」と見えるが、触らなければ本体の値はそのまま残る)。
pub fn decode_binding(b: [u8; 4], profile: &[u8]) -> Option<ButtonAction> {
    Some(match b {
        [0x80, 0x01, hi, lo] => {
            let mask = u16::from_be_bytes([hi, lo]);
            if mask == 0 {
                ButtonAction::Disabled
            } else {
                ButtonAction::Mouse(mask_mouse(mask)?)
            }
        }
        [0x80, 0x02, m, k] => ButtonAction::Key(KeyStroke {
            mods: m,
            usage: k as u16,
        }),
        [0x80, 0x03, hi, lo] => {
            let u = u16::from_be_bytes([hi, lo]);
            ButtonAction::Media(MEDIA.iter().find(|(_, v)| *v == u)?.0)
        }
        [0x90, 0x01, _, _] => ButtonAction::Mouse(MouseFn::ScrollLeft),
        [0x90, 0x02, _, _] => ButtonAction::Mouse(MouseFn::ScrollRight),
        [0x90, 0x03, _, _] => ButtonAction::Dpi(DpiFn::Up),
        [0x90, 0x04, _, _] => ButtonAction::Dpi(DpiFn::Down),
        [0x90, 0x05, _, _] => ButtonAction::Dpi(DpiFn::Cycle),
        [0x90, 0x07, _, _] => {
            let d = dpis(profile);
            ButtonAction::Dpi(DpiFn::Shift(*d.get(profile[2] as usize).or(d.first())?))
        }
        [0x90, 0x0A, _, _] => ButtonAction::ProfileCycle,
        [0x00, sector, 0x00, 0x00] => ButtonAction::Macro {
            slot: macro_slot(sector as u16)? as u8,
            run: MacroRun::Times(1),
        },
        _ => return None,
    })
}

/// UI の割り当て → 4B。シフト DPI はプロファイルのシフト段 (byte 2) も書き換える。
pub fn encode_binding(a: &ButtonAction, profile: &mut [u8]) -> Result<[u8; 4], String> {
    Ok(match a {
        ButtonAction::Disabled => [0x80, 0x01, 0x00, 0x00],
        ButtonAction::Mouse(MouseFn::ScrollLeft) => [0x90, 0x01, 0, 0],
        ButtonAction::Mouse(MouseFn::ScrollRight) => [0x90, 0x02, 0, 0],
        ButtonAction::Mouse(m) => {
            let mask = mouse_mask(*m).ok_or("このマウス動作は本体に割り当てられません")?;
            let b = mask.to_be_bytes();
            [0x80, 0x01, b[0], b[1]]
        }
        ButtonAction::Key(k) => {
            let usage = u8::try_from(k.usage).map_err(|_| "このキーは割り当てられません")?;
            [0x80, 0x02, k.mods, usage]
        }
        ButtonAction::Media(m) => {
            let u = MEDIA
                .iter()
                .find(|(f, _)| f == m)
                .map(|(_, u)| *u)
                .ok_or("このメディア操作は割り当てられません")?;
            let b = u.to_be_bytes();
            [0x80, 0x03, b[0], b[1]]
        }
        ButtonAction::Dpi(DpiFn::Up) => [0x90, 0x03, 0, 0],
        ButtonAction::Dpi(DpiFn::Down) => [0x90, 0x04, 0, 0],
        ButtonAction::Dpi(DpiFn::Cycle) => [0x90, 0x05, 0, 0],
        ButtonAction::Dpi(DpiFn::Shift(v)) => {
            // 押している間の DPI は「DPI 段のどれか」で指定する形式 — いちばん近い段を使う。
            let d = dpis(profile);
            let idx = d
                .iter()
                .enumerate()
                .min_by_key(|(_, x)| x.abs_diff(*v))
                .map(|(i, _)| i)
                .ok_or("DPI 段がありません")?;
            profile[2] = idx as u8;
            [0x90, 0x07, 0, 0]
        }
        ButtonAction::ProfileCycle => [0x90, 0x0A, 0, 0],
        ButtonAction::Macro { slot, .. } => {
            if *slot as usize >= MACRO_MAX_SLOTS {
                return Err("マクロ枠の範囲外です".into());
            }
            [0x00, (MACRO_FIRST_SECTOR + *slot as u16) as u8, 0x00, 0x00]
        }
        _ => return Err("この割り当ては本体に保存できません".into()),
    })
}

/// マクロのセクタ → 枠番号。
pub fn macro_slot(sector: u16) -> Option<usize> {
    let s = sector.checked_sub(MACRO_FIRST_SECTOR)? as usize;
    (s < MACRO_MAX_SLOTS).then_some(s)
}

// ---------------------------------------------------------------- マクロ

const M_NOOP: u8 = 0x00;
const M_DELAY: u8 = 0x40;
const M_BTN_DOWN: u8 = 0x41;
const M_BTN_UP: u8 = 0x42;
const M_KEY_DOWN: u8 = 0x43;
const M_KEY_UP: u8 = 0x44;
const M_END: u8 = 0xFF;

fn mod_bit(usage: u16) -> Option<u8> {
    (0xE0..=0xE7).contains(&usage).then(|| 1u8 << (usage - 0xE0))
}

struct Writer {
    out: Vec<u8>,
    cap: usize,
}

impl Writer {
    /// 3B 命令。16B の区切りをまたぐなら NOOP で詰める (libratbag の読み方に合わせる)。
    fn cmd(&mut self, op: u8, a: u8, b: u8) -> Result<(), String> {
        while self.out.len() % 16 > 13 {
            self.out.push(M_NOOP);
        }
        self.out.extend_from_slice(&[op, a, b]);
        if self.out.len() + 1 > self.cap {
            return Err("マクロが長すぎて本体に入りません".into());
        }
        Ok(())
    }
}

/// マクロ → 本体のバイト列 (末尾 0xFF まで)。`cap` は使える長さ (セクタ長 - CRC 2B)。
///
/// - 修飾キー (usage 0xE0..0xE7) は `43 <bit> 00` / `44 <bit> 00`。
/// - 通常キーは押下時に押している修飾を付けて `43 <修飾> <キー>`、離すときは `44 00 <キー>`
///   (キーだけ離す — 修飾は自分の Up で離す)。
/// - 待ち時間は 1 命令 65535 ms まで (超えたら分割)。
pub fn encode_macro(def: &MacroDef, cap: usize) -> Result<Vec<u8>, String> {
    let mut w = Writer {
        out: Vec::new(),
        cap,
    };
    let mut held: u8 = 0;
    for ev in &def.events {
        match (ev.input, ev.edge) {
            (MacroInput::Key(u), edge) => {
                if let Some(bit) = mod_bit(u) {
                    match edge {
                        MacroEdge::Down => {
                            held |= bit;
                            w.cmd(M_KEY_DOWN, bit, 0)?;
                        }
                        MacroEdge::Up => {
                            held &= !bit;
                            w.cmd(M_KEY_UP, bit, 0)?;
                        }
                    }
                } else {
                    let k = u8::try_from(u).map_err(|_| format!("キー {u:#x} はマクロに使えません"))?;
                    match edge {
                        MacroEdge::Down => w.cmd(M_KEY_DOWN, held, k)?,
                        MacroEdge::Up => w.cmd(M_KEY_UP, 0, k)?,
                    }
                }
            }
            (MacroInput::Mouse(m), edge) => {
                let mask = mouse_mask(m).ok_or("このマウス動作はマクロに使えません")?;
                let b = mask.to_be_bytes();
                let op = if edge == MacroEdge::Down {
                    M_BTN_DOWN
                } else {
                    M_BTN_UP
                };
                w.cmd(op, b[0], b[1])?;
            }
        }
        let mut left = ev.delay_ms;
        while left > 0 {
            let d = left.min(0xFFFF) as u16;
            let b = d.to_be_bytes();
            w.cmd(M_DELAY, b[0], b[1])?;
            left -= d as u32;
        }
    }
    w.out.push(M_END);
    Ok(w.out)
}

/// 本体のバイト列 → マクロのイベント列。空 (先頭 0xFF) なら None。
/// 知らない命令が出たらそこまでを返す。
pub fn decode_macro(b: &[u8]) -> Option<Vec<MacroEvent>> {
    if b.first().is_none_or(|x| *x == M_END) {
        return None;
    }
    let mut ev: Vec<MacroEvent> = vec![];
    let mut held: u8 = 0;
    let push = |ev: &mut Vec<MacroEvent>, input, edge| {
        ev.push(MacroEvent {
            input,
            edge,
            delay_ms: 0,
        })
    };
    let mut i = 0;
    while i < b.len() {
        let op = b[i];
        if op == M_END {
            break;
        }
        if op == M_NOOP {
            i += 1;
            continue;
        }
        if i + 3 > b.len() {
            break;
        }
        let (a, c) = (b[i + 1], b[i + 2]);
        match op {
            M_DELAY => {
                if let Some(last) = ev.last_mut() {
                    last.delay_ms += u16::from_be_bytes([a, c]) as u32;
                }
            }
            M_KEY_DOWN => {
                for bit in 0..8 {
                    if a & (1 << bit) != 0 && held & (1 << bit) == 0 {
                        held |= 1 << bit;
                        push(&mut ev, MacroInput::Key(0xE0 + bit), MacroEdge::Down);
                    }
                }
                if c != 0 {
                    push(&mut ev, MacroInput::Key(c as u16), MacroEdge::Down);
                }
            }
            M_KEY_UP => {
                if c != 0 {
                    push(&mut ev, MacroInput::Key(c as u16), MacroEdge::Up);
                }
                for bit in 0..8 {
                    if a & (1 << bit) != 0 && held & (1 << bit) != 0 {
                        held &= !(1 << bit);
                        push(&mut ev, MacroInput::Key(0xE0 + bit), MacroEdge::Up);
                    }
                }
            }
            M_BTN_DOWN | M_BTN_UP => {
                let Some(m) = mask_mouse(u16::from_be_bytes([a, c])) else {
                    break;
                };
                let edge = if op == M_BTN_DOWN {
                    MacroEdge::Down
                } else {
                    MacroEdge::Up
                };
                push(&mut ev, MacroInput::Mouse(m), edge);
            }
            _ => break,
        }
        i += 3;
    }
    Some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G502 X の ROM プロファイル 1 (実機ダンプ) の先頭 0x50 バイト + 0xFF 埋め。
    fn g502_profile() -> Vec<u8> {
        let mut d = vec![0xFFu8; 255];
        let head: [u8; 0x50] = [
            0x01, 0x02, 0x00, 0x20, 0x03, 0xb0, 0x04, 0x40, 0x06, 0x60, 0x09, 0x80, 0x0c, 0xff,
            0xff, 0xff, 0xff, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0x3c, 0x00, 0x2c, 0x01, 0x80, 0x01, 0x00, 0x01, 0x80, 0x01, 0x00, 0x02, 0x80, 0x01,
            0x00, 0x04, 0x80, 0x01, 0x00, 0x08, 0x90, 0x07, 0x00, 0x00, 0x80, 0x01, 0x00, 0x10,
            0x90, 0x01, 0x00, 0x00, 0x90, 0x02, 0x00, 0x00, 0x90, 0x0a, 0x00, 0x00, 0x90, 0x03,
            0x00, 0x00, 0x90, 0x04, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        ];
        d[..0x50].copy_from_slice(&head);
        d
    }

    #[test]
    fn profile_fields() {
        let mut d = g502_profile();
        assert_eq!(dpis(&d), vec![800, 1200, 1600, 2400, 3200]);
        assert_eq!(default_stage(&d), 2);
        assert_eq!(report_hz(&d), Some(1000));
        set_dpis(&mut d, &[400, 800]);
        assert_eq!(dpis(&d), vec![400, 800]);
        assert_eq!(default_stage(&d), 1, "既定段は範囲内に収まる");
        set_report_hz(&mut d, 500);
        assert_eq!(d[0], 2);
    }

    #[test]
    fn decode_g502_buttons() {
        let d = g502_profile();
        let got: Vec<Option<ButtonAction>> = (0..11).map(|i| decode_binding(button(&d, i), &d)).collect();
        assert_eq!(got[0], Some(ButtonAction::Mouse(MouseFn::Left)));
        assert_eq!(got[3], Some(ButtonAction::Mouse(MouseFn::Back)));
        assert_eq!(got[4], Some(ButtonAction::Dpi(DpiFn::Shift(800))));
        assert_eq!(got[6], Some(ButtonAction::Mouse(MouseFn::ScrollLeft)));
        assert_eq!(got[8], Some(ButtonAction::ProfileCycle));
        assert_eq!(got[9], Some(ButtonAction::Dpi(DpiFn::Up)));
        assert_eq!(got[10], Some(ButtonAction::Dpi(DpiFn::Down)));
    }

    #[test]
    fn binding_roundtrip() {
        let mut d = g502_profile();
        for a in [
            ButtonAction::Disabled,
            ButtonAction::Mouse(MouseFn::Forward),
            ButtonAction::Key(KeyStroke {
                mods: mods::LALT | mods::LSHIFT,
                usage: 0x05,
            }),
            ButtonAction::Media(MediaFn::VolumeUp),
            ButtonAction::Dpi(DpiFn::Cycle),
            ButtonAction::Dpi(DpiFn::Shift(1200)),
            ButtonAction::Macro {
                slot: 0,
                run: MacroRun::Times(1),
            },
        ] {
            let b = encode_binding(&a, &mut d).unwrap();
            assert_eq!(decode_binding(b, &d), Some(a));
        }
        // 実機で確認した値。
        assert_eq!(
            encode_binding(&ButtonAction::Disabled, &mut d).unwrap(),
            [0x80, 0x01, 0x00, 0x00]
        );
        let m = ButtonAction::Macro {
            slot: 0,
            run: MacroRun::Times(1),
        };
        assert_eq!(encode_binding(&m, &mut d).unwrap(), [0x00, 0x06, 0x00, 0x00]);
    }

    fn ev(input: MacroInput, edge: MacroEdge, delay_ms: u32) -> MacroEvent {
        MacroEvent {
            input,
            edge,
            delay_ms,
        }
    }

    #[test]
    fn macro_encode_double_back() {
        // 実機で動作確認したバイト列と同じになる。
        let def = MacroDef {
            name: String::new(),
            events: vec![
                ev(MacroInput::Mouse(MouseFn::Back), MacroEdge::Down, 0),
                ev(MacroInput::Mouse(MouseFn::Back), MacroEdge::Up, 50),
                ev(MacroInput::Mouse(MouseFn::Back), MacroEdge::Down, 0),
                ev(MacroInput::Mouse(MouseFn::Back), MacroEdge::Up, 0),
            ],
            mode: 0,
            repeat: 1,
        };
        let b = encode_macro(&def, 253).unwrap();
        assert_eq!(
            b,
            [
                0x41, 0x00, 0x08, 0x42, 0x00, 0x08, 0x40, 0x00, 0x32, 0x41, 0x00, 0x08, 0x42,
                0x00, 0x08, 0xff
            ]
        );
        assert_eq!(decode_macro(&b), Some(def.events));
    }

    #[test]
    fn macro_keys_with_modifiers_roundtrip() {
        let def = MacroDef {
            name: String::new(),
            events: vec![
                ev(MacroInput::Key(0xE2), MacroEdge::Down, 0), // LAlt
                ev(MacroInput::Key(0xE1), MacroEdge::Down, 0), // LShift
                ev(MacroInput::Key(0x05), MacroEdge::Down, 0), // B
                ev(MacroInput::Key(0x05), MacroEdge::Up, 0),
                ev(MacroInput::Key(0xE1), MacroEdge::Up, 0),
                ev(MacroInput::Key(0xE2), MacroEdge::Up, 100),
                ev(MacroInput::Key(0x28), MacroEdge::Down, 0), // Enter
                ev(MacroInput::Key(0x28), MacroEdge::Up, 0),
            ],
            mode: 0,
            repeat: 1,
        };
        let b = encode_macro(&def, 253).unwrap();
        // B は押している修飾 (Alt+Shift) を付けて押す。
        assert_eq!(&b[6..9], &[0x43, 0x06, 0x05]);
        // 16B の区切りをまたぐ命令は無い。
        let mut i = 0;
        while b[i] != 0xFF {
            if b[i] == 0 {
                i += 1;
                continue;
            }
            assert!(i % 16 <= 13, "offset {i} の命令が区切りをまたぐ");
            i += 3;
        }
        assert_eq!(decode_macro(&b), Some(def.events));
    }

    #[test]
    fn decode_tested_bookmark_macro() {
        // 実機で動いた手書きの列 (修飾と同時押し) も読める。
        let b = [
            0x43, 0x06, 0x05, 0x44, 0x06, 0x05, 0x40, 0x00, 0x64, 0x43, 0x00, 0x28, 0x44, 0x00,
            0x28, 0xff,
        ];
        let e = decode_macro(&b).unwrap();
        assert_eq!(e.len(), 8);
        assert_eq!(e[2], ev(MacroInput::Key(0x05), MacroEdge::Down, 0));
        assert_eq!(e[5].delay_ms, 100);
        assert_eq!(decode_macro(&[0xFF; 16]), None);
    }

    #[test]
    fn macro_too_long() {
        let events = (0..200)
            .map(|_| ev(MacroInput::Key(0x04), MacroEdge::Down, 10))
            .collect();
        let def = MacroDef {
            name: String::new(),
            events,
            mode: 0,
            repeat: 1,
        };
        assert!(encode_macro(&def, 253).is_err());
    }

    #[test]
    fn directory() {
        let mut d = vec![0xFFu8; 255];
        d[..20].copy_from_slice(&[
            0, 1, 1, 0xff, 0, 2, 1, 0xff, 0, 3, 0, 0xff, 0, 4, 0, 0xff, 0, 5, 0, 0xff,
        ]);
        assert_eq!(parse_dir(&d).len(), 5);
        assert_eq!(parse_dir(&d)[2], (3, false));
        assert!(enable_dir_entry(&mut d, 2));
        assert!(!enable_dir_entry(&mut d, 2));
        seal(&mut d);
        assert!(crc_ok(&d));
    }
}
