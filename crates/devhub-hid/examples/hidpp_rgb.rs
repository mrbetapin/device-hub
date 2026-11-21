//! Logitech キーボード照明の実験ツール (RgbEffects 0x8071 / PerKeyLightingV2 0x8081 /
//! BrightnessControl 0x8040)。書き込み形式を実機で確かめ、ドライバへ組み込む材料にする。
//!
//! ```text
//! hidpp_rgb info                          # 機種名・SW 制御・明るさ・クラスタとエフェクト一覧
//! hidpp_rgb static  <cluster> <RRGGBB>    # 固定色
//! hidpp_rgb breathe <cluster> <RRGGBB> [周期ms=3000] [強さ=100]
//! hidpp_rgb cycle   <cluster> [周期ms=5000] [強さ=100]
//! hidpp_rgb off     <cluster>
//! hidpp_rgb effect  <cluster> <effect index> [10B の16進...]   # 生パラメータで設定
//! hidpp_rgb brightness [0-100]            # 引数なしで読み取り
//! hidpp_rgb swctl [16進...]               # 引数なしで読み取り。例: swctl 01 01
//! hidpp_rgb zones                         # キー単位照明のゾーン ID 一覧
//! hidpp_rgb perkey <RRGGBB> all|<zone ID...>   # キー単位の色 (最後に frameEnd を送る)
//! hidpp_rgb raw <feature 16進> <関数番号> [16進...]           # 任意の要求
//! ```
//!
//! 共通オプション:
//! - `--name <部分文字列>`: 複数の機器があるとき、名前で選ぶ (既定は 0x8071 を持つ最初の 1 台)
//! - `--persist <0|1>`: エフェクト設定の最後の 1B (既定 1 = 本体に保存)
//!
//! G HUB・Device HUB・devhub-agent は終了してから使うこと (同じ IF で要求を取り合う)。

use devhub_hid::logitech::{feat, raw_request};
use std::process::exit;

const SW: u8 = 0x0D;

struct Kb {
    h: hidapi::HidDevice,
    d: u8,
}

impl Kb {
    fn rq(&self, fi: u8, func: u8, params: &[u8]) -> Result<[u8; 16], String> {
        raw_request(&self.h, self.d, fi, func, SW, params).map_err(|e| e.to_string())
    }

    /// Root.GetFeature → index (無ければ None)。
    fn fidx(&self, id: u16) -> Option<u8> {
        let b = id.to_be_bytes();
        self.rq(0, 0, &[b[0], b[1]]).ok().map(|p| p[0]).filter(|i| *i != 0)
    }

    fn need(&self, id: u16) -> u8 {
        self.fidx(id).unwrap_or_else(|| die(&format!("この機器は 0x{id:04X} を持っていません")))
    }

    fn name(&self) -> String {
        let Some(fi) = self.fidx(feat::DEVICE_NAME) else {
            return "?".into();
        };
        let Ok(c) = self.rq(fi, 0, &[]) else {
            return "?".into();
        };
        let len = c[0] as usize;
        let mut name = Vec::new();
        while name.len() < len {
            let Ok(chunk) = self.rq(fi, 1, &[name.len() as u8]) else {
                break;
            };
            let take = (len - name.len()).min(16);
            name.extend_from_slice(&chunk[..take]);
        }
        String::from_utf8_lossy(&name).trim_matches('\0').to_string()
    }

    /// 送って結果を表示する。
    fn show(&self, label: &str, fi: u8, func: u8, params: &[u8]) -> Option<[u8; 16]> {
        print!("{label:<28} -> ");
        match self.rq(fi, func, params) {
            Ok(r) => {
                println!("{}", hex(&r));
                Some(r)
            }
            Err(e) => {
                println!("ERR {e}");
                None
            }
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn nth(a: &[String], i: usize) -> &str {
    a.get(i)
        .map(String::as_str)
        .unwrap_or_else(|| die("引数が足りません"))
}

fn parse_u8(s: &str) -> u8 {
    let t = s.trim_start_matches("0x");
    u8::from_str_radix(t, 16).unwrap_or_else(|_| die(&format!("16進の 1B ではありません: {s}")))
}

fn parse_dec<T: std::str::FromStr>(s: &str) -> T {
    s.parse()
        .unwrap_or_else(|_| die(&format!("数値ではありません: {s}")))
}

fn parse_color(s: &str) -> [u8; 3] {
    let t = s.trim_start_matches('#');
    let v = u32::from_str_radix(t, 16)
        .ok()
        .filter(|_| t.len() == 6)
        .unwrap_or_else(|| die(&format!("色は RRGGBB で指定してください: {s}")));
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// 0x8071 を持つ (または名前が一致する) ペア先を探す。
fn find(api: &hidapi::HidApi, name_filter: Option<&str>) -> Kb {
    for info in api
        .device_list()
        .filter(|d| d.vendor_id() == 0x046D && d.usage_page() == 0xFF00 && d.usage() == 0x0002)
    {
        let wireless = (info.product_id() & 0xFF00) == 0xC500;
        let cands: Vec<u8> = if wireless { (1..=6).collect() } else { vec![0xFF] };
        let Ok(mut h) = info.open_device(api) else {
            continue;
        };
        h.set_blocking_mode(false).ok();
        for d in cands {
            let kb = Kb { h, d };
            let alive = kb.rq(0, 1, &[0, 0, 0x5A]).is_ok();
            let ok = alive
                && match name_filter {
                    Some(f) => kb.name().to_lowercase().contains(&f.to_lowercase()),
                    None => kb.fidx(feat::RGB_EFFECTS).is_some(),
                };
            if ok {
                return kb;
            }
            h = kb.h;
        }
    }
    die("対象の機器が見つかりません (キーを押して起こしてから再実行してください)")
}

/// クラスタ c のエフェクト一覧 (index, ID, 生応答)。
fn effects(kb: &Kb, fi: u8, c: u8) -> Vec<(u8, u16, [u8; 16])> {
    let Ok(ci) = kb.rq(fi, 0, &[c, 0xFF, 0x00]) else {
        die(&format!("クラスタ {c} がありません"));
    };
    (0..ci[4].min(16))
        .filter_map(|e| {
            kb.rq(fi, 0, &[c, e, 0x00])
                .ok()
                .map(|r| (e, u16::from_be_bytes([r[2], r[3]]), r))
        })
        .collect()
}

fn effect_index(kb: &Kb, fi: u8, c: u8, ids: &[u16]) -> (u8, u16) {
    let list = effects(kb, fi, c);
    for id in ids {
        if let Some((e, id, _)) = list.iter().find(|(_, i, _)| i == id) {
            return (*e, *id);
        }
    }
    die(&format!(
        "クラスタ {c} にエフェクト {ids:04X?} がありません (info で一覧を確認)"
    ))
}

fn set_effect(kb: &Kb, fi: u8, c: u8, e: u8, p: &[u8], persist: u8) {
    let mut params = vec![c, e];
    let mut p10 = [0u8; 10];
    let n = p.len().min(10);
    p10[..n].copy_from_slice(&p[..n]);
    params.extend_from_slice(&p10);
    params.push(persist);
    kb.show(&format!("8071 fn1 {}", hex(&params)), fi, 1, &params);
}

/// 0x8081 getInfo(0, page) のビットマップからゾーン ID を集める (LSB から数える前提)。
fn zones(kb: &Kb, fi: u8) -> Vec<u16> {
    let mut out = vec![];
    for page in 0..8u8 {
        let Ok(r) = kb.rq(fi, 0, &[0x00, page]) else {
            break;
        };
        for (i, byte) in r[2..].iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    out.push(page as u16 * 112 + i as u16 * 8 + bit);
                }
            }
        }
    }
    out
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut opt = |name: &str| -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        let v = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let name_filter = opt("--name");
    let persist = opt("--persist").map(|s| parse_u8(&s)).unwrap_or(1);
    let Some(cmd) = args.first().cloned() else {
        die("使い方は hidpp_rgb.rs 冒頭のコメントを参照 (例: hidpp_rgb info)");
    };
    let a = &args[1..];
    let arg = |i: usize| nth(a, i);

    let api = hidapi::HidApi::new().expect("hidapi");
    let kb = find(&api, name_filter.as_deref());
    println!("対象: {:?} (device index {:#04x})", kb.name(), kb.d);

    match cmd.as_str() {
        "info" => {
            if let Some(fi) = kb.fidx(feat::RGB_EFFECTS) {
                kb.show("8071 swctl get (fn5)", fi, 5, &[]);
                for c in 0..8u8 {
                    if kb.rq(fi, 0, &[c, 0xFF, 0x00]).is_err() {
                        break;
                    }
                    kb.show(&format!("8071 cluster {c}"), fi, 0, &[c, 0xFF, 0x00]);
                    for (e, id, r) in effects(&kb, fi, c) {
                        println!(
                            "    effect {e}: ID 0x{id:04X}  caps {:02x}{:02x}  period {} ms",
                            r[4],
                            r[5],
                            u16::from_be_bytes([r[6], r[7]])
                        );
                    }
                    // 現在のエフェクトの読み出し (関数番号は未確定なので候補を試す)。
                    kb.show(&format!("8071 fn14 [{c}] (現在値?)"), fi, 14, &[c]);
                }
            }
            if let Some(fi) = kb.fidx(feat::BRIGHTNESS_CONTROL) {
                kb.show("8040 getInfo (fn0)", fi, 0, &[]);
                kb.show("8040 getBrightness (fn1)", fi, 1, &[]);
            }
            if let Some(fi) = kb.fidx(feat::PER_KEY_LIGHTING_V2) {
                println!("8081 ゾーン数: {}", zones(&kb, fi).len());
            }
        }
        "static" | "breathe" | "cycle" | "off" => {
            let fi = kb.need(feat::RGB_EFFECTS);
            let c = parse_dec::<u8>(arg(0));
            let mut p = [0u8; 10];
            let (e, id) = match cmd.as_str() {
                "static" => {
                    p[..3].copy_from_slice(&parse_color(arg(1)));
                    effect_index(&kb, fi, c, &[0x0001])
                }
                "breathe" => {
                    p[..3].copy_from_slice(&parse_color(arg(1)));
                    let period: u16 = a.get(2).map(|s| parse_dec(s)).unwrap_or(3000);
                    p[3..5].copy_from_slice(&period.to_be_bytes());
                    p[5] = 0; // 波形
                    p[6] = a.get(3).map(|s| parse_dec(s)).unwrap_or(100);
                    effect_index(&kb, fi, c, &[0x000A])
                }
                "cycle" => {
                    let period: u16 = a.get(1).map(|s| parse_dec(s)).unwrap_or(5000);
                    let intensity: u8 = a.get(2).map(|s| parse_dec(s)).unwrap_or(100);
                    let (e, id) = effect_index(&kb, fi, c, &[0x0015, 0x0003]);
                    if id == 0x0015 {
                        // CycleS: saturation@1, period@6, intensity@8
                        p[1] = 0xFF;
                        p[6..8].copy_from_slice(&period.to_be_bytes());
                        p[8] = intensity;
                    } else {
                        p[5..7].copy_from_slice(&period.to_be_bytes());
                        p[7] = intensity;
                    }
                    (e, id)
                }
                _ => effect_index(&kb, fi, c, &[0x0000]),
            };
            println!("cluster {c} effect index {e} (ID 0x{id:04X})");
            set_effect(&kb, fi, c, e, &p, persist);
        }
        "effect" => {
            let fi = kb.need(feat::RGB_EFFECTS);
            let c = parse_dec::<u8>(arg(0));
            let e = parse_dec::<u8>(arg(1));
            let p: Vec<u8> = a[2..].iter().map(|s| parse_u8(s)).collect();
            set_effect(&kb, fi, c, e, &p, persist);
        }
        "brightness" => {
            let fi = kb.need(feat::BRIGHTNESS_CONTROL);
            if let Some(v) = a.first() {
                let v: u16 = parse_dec(v);
                let b = v.to_be_bytes();
                kb.show(&format!("8040 setBrightness {v}"), fi, 2, &[b[0], b[1]]);
            }
            kb.show("8040 getBrightness", fi, 1, &[]);
        }
        "swctl" => {
            let fi = kb.need(feat::RGB_EFFECTS);
            let p: Vec<u8> = a.iter().map(|s| parse_u8(s)).collect();
            if !p.is_empty() {
                kb.show(&format!("8071 fn5 {}", hex(&p)), fi, 5, &p);
            }
            kb.show("8071 fn5 (get)", fi, 5, &[]);
        }
        "zones" => {
            let fi = kb.need(feat::PER_KEY_LIGHTING_V2);
            let z = zones(&kb, fi);
            println!("{} 個: {z:?}", z.len());
        }
        "perkey" => {
            let fi = kb.need(feat::PER_KEY_LIGHTING_V2);
            let [r, g, b] = parse_color(arg(0));
            let ids: Vec<u8> = if arg(1) == "all" {
                zones(&kb, fi)
                    .into_iter()
                    .filter_map(|z| u8::try_from(z).ok())
                    .collect()
            } else {
                a[1..].iter().map(|s| parse_dec(s)).collect()
            };
            if arg(1) == "all" {
                // setRgbZonesSingleValue (fn6): 色 3B + ゾーン最大 13 個
                for chunk in ids.chunks(13) {
                    let mut p = vec![r, g, b];
                    p.extend_from_slice(chunk);
                    kb.show(&format!("8081 fn6 ({} 個)", chunk.len()), fi, 6, &p);
                }
            } else {
                // setIndividualRgbZones (fn1): [zone, r, g, b] × 最大 4
                for chunk in ids.chunks(4) {
                    let p: Vec<u8> = chunk.iter().flat_map(|z| [*z, r, g, b]).collect();
                    kb.show(&format!("8081 fn1 {chunk:?}"), fi, 1, &p);
                }
            }
            // frameEnd (fn7)
            kb.show("8081 fn7 frameEnd", fi, 7, &[0x00]);
        }
        "raw" => {
            let id = u16::from_str_radix(arg(0).trim_start_matches("0x"), 16)
                .unwrap_or_else(|_| die("feature は 16進 (例: 8071)"));
            let fi = kb.need(id);
            let func = parse_dec::<u8>(arg(1));
            let p: Vec<u8> = a[2..].iter().map(|s| parse_u8(s)).collect();
            kb.show(&format!("{id:04X} fn{func} {}", hex(&p)), fi, func, &p);
        }
        _ => die(&format!("不明なコマンド: {cmd}")),
    }
}
