//! OnboardProfiles (0x8100) の本体メモリを **読むだけ** の解析ツール。
//! ボタン割り当て・マクロを本体プロファイルへ書く実装の材料にする。
//!
//! ```text
//! hidpp_onboard            # 情報・ディレクトリ・各プロファイルの要約 (ボタン割り当て)
//! hidpp_onboard --raw      # 上に加えて各セクタの全バイトを 16 進で出す
//! hidpp_onboard --name G502
//!
//! # ---- 書き込み (実験用) ----
//! hidpp_onboard set <枠 1-5> <ボタン#> <4B の16進> [--gshift]  # 1 ボタンの割り当てを書き換える
//! hidpp_onboard restore <枠 1-5>          # ROM の既定値で上書き (枠 2 は ROM 2、他は ROM 1)
//! hidpp_onboard mode <1|2>                # 1 = オンボード、2 = ホスト
//! hidpp_onboard profile <枠 1-5>          # 使う枠を切り替える (オンボードモード中)
//! hidpp_onboard dump <sector 16進>        # 1 セクタを読む
//! hidpp_onboard write <sector 16進> <16進...> [--no-crc]  # 先頭から書き、残り 0xFF、末尾 CRC
//! ```
//!
//! 引数なしの解析は getInfo (fn0) / getMode (fn2) / getCurrentProfile (fn4) / memoryRead (fn5)
//! だけを呼ぶ。書き込みは memoryAddrWrite (fn6) / memoryWrite (fn7) / memoryWriteEnd (fn8)。
//! ユーザー枠 (sector 0x0001..) だけを書く想定。壊れても `restore` で ROM の既定値に戻せる。
//! G HUB・Device HUB・devhub-agent は終了してから使うこと。

use devhub_hid::logitech::{crc_ccitt, feat, raw_request};
use std::process::exit;

const SW: u8 = 0x0C;

struct Dev {
    h: hidapi::HidDevice,
    d: u8,
}

impl Dev {
    fn rq(&self, fi: u8, func: u8, params: &[u8]) -> Result<[u8; 16], String> {
        raw_request(&self.h, self.d, fi, func, SW, params).map_err(|e| e.to_string())
    }
    fn fidx(&self, id: u16) -> Option<u8> {
        let b = id.to_be_bytes();
        self.rq(0, 0, &[b[0], b[1]]).ok().map(|p| p[0]).filter(|i| *i != 0)
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
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1)
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn find(api: &hidapi::HidApi, name_filter: Option<&str>) -> Dev {
    for info in api
        .device_list()
        .filter(|d| d.vendor_id() == 0x046D && d.usage_page() == 0xFF00 && d.usage() == 0x0002)
    {
        let wireless = (info.product_id() & 0xFF00) == 0xC500;
        let cands: Vec<u8> = if wireless {
            (1..=6).collect()
        } else {
            vec![0xFF]
        };
        let Ok(mut h) = info.open_device(api) else {
            continue;
        };
        h.set_blocking_mode(false).ok();
        for d in cands {
            let dev = Dev { h, d };
            let ok = dev.rq(0, 1, &[0, 0, 0x5A]).is_ok()
                && match name_filter {
                    Some(f) => dev.name().to_lowercase().contains(&f.to_lowercase()),
                    None => dev.fidx(feat::ONBOARD_PROFILES).is_some(),
                };
            if ok {
                return dev;
            }
            h = dev.h;
        }
    }
    die("0x8100 を持つ機器が見つかりません (マウスを動かして起こしてから再実行してください)")
}

/// memoryRead (fn5) [sector BE16, offset BE16] → 16B。末尾は size-16 から読んで詰める。
fn read_sector(dev: &Dev, fi: u8, sector: u16, size: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; size];
    let mut off = 0usize;
    while off < size {
        let at = off.min(size.saturating_sub(16));
        let s = sector.to_be_bytes();
        let o = (at as u16).to_be_bytes();
        let r = dev.rq(fi, 5, &[s[0], s[1], o[0], o[1]])?;
        let n = (size - at).min(16);
        out[at..at + n].copy_from_slice(&r[..n]);
        off = at + 16;
    }
    Ok(out)
}

/// セクタ末尾 2B (BE) の CRC が合うか。
fn crc_ok(d: &[u8]) -> (bool, u16, u16) {
    let n = d.len();
    let stored = u16::from_be_bytes([d[n - 2], d[n - 1]]);
    let calc = crc_ccitt(&d[..n - 2]);
    (stored == calc, stored, calc)
}

/// ボタン割り当て 4B の解釈 (libratbag の HID++ 2.0 onboard profile 実装に基づく推定)。
fn binding(b: &[u8]) -> String {
    match b[0] {
        0x80 => match b[1] {
            0x01 => {
                let mask = u16::from_be_bytes([b[2], b[3]]);
                let names = [
                    "左", "右", "中", "戻る", "進む", "B6", "B7", "B8", "B9", "B10", "B11", "B12",
                    "B13", "B14", "B15", "B16",
                ];
                let on: Vec<&str> = (0..16)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| names[i])
                    .collect();
                format!("マウスボタン {}", on.join("+"))
            }
            0x02 => format!("キー mod={:#04x} usage={:#04x}", b[2], b[3]),
            0x03 => format!("コンシューマ {:#06x}", u16::from_be_bytes([b[2], b[3]])),
            _ => "HID ?".into(),
        },
        0x90 => {
            // G502 X の ROM プロファイルと物理ボタンの対応から確定 (libratbag の表とも一致)。
            let name = match b[1] {
                0x01 => "ホイール左",
                0x02 => "ホイール右",
                0x03 => "DPI 上げ",
                0x04 => "DPI 下げ",
                0x05 => "DPI 循環",
                0x06 => "DPI 既定へ",
                0x07 => "DPI シフト (スナイパー)",
                0x08 => "プロファイル次",
                0x09 => "プロファイル前",
                0x0A => "プロファイル循環",
                0x0B => "G-Shift",
                0x0C => "電池残量表示",
                _ => "特殊 ?",
            };
            name.into()
        }
        0x00 => format!(
            "マクロ sector={:#06x} offset={:#06x}",
            u16::from_be_bytes([b[0], b[1]]),
            u16::from_be_bytes([b[2], b[3]])
        ),
        0xFF => "未割り当て/無効".into(),
        _ => "?".into(),
    }
}

/// 末尾 2B に CRC を入れる。
fn with_crc(mut d: Vec<u8>) -> Vec<u8> {
    let n = d.len();
    let c = crc_ccitt(&d[..n - 2]).to_be_bytes();
    d[n - 2] = c[0];
    d[n - 1] = c[1];
    d
}

/// memoryAddrWrite (fn6) [sector BE16, offset BE16, 長さ BE16] → memoryWrite (fn7) 16B ずつ
/// → memoryWriteEnd (fn8)。
fn write_sector(dev: &Dev, fi: u8, sector: u16, data: &[u8]) -> Result<(), String> {
    let s = sector.to_be_bytes();
    let len = (data.len() as u16).to_be_bytes();
    dev.rq(fi, 6, &[s[0], s[1], 0, 0, len[0], len[1]])
        .map_err(|e| format!("memoryAddrWrite: {e}"))?;
    for (i, chunk) in data.chunks(16).enumerate() {
        let mut c = [0xFFu8; 16];
        c[..chunk.len()].copy_from_slice(chunk);
        dev.rq(fi, 7, &c)
            .map_err(|e| format!("memoryWrite #{i}: {e}"))?;
    }
    dev.rq(fi, 8, &[])
        .map_err(|e| format!("memoryWriteEnd: {e}"))?;
    Ok(())
}

/// 書いて読み直し、一致を確かめる。
fn write_verify(dev: &Dev, fi: u8, sector: u16, data: &[u8]) {
    if let Err(e) = write_sector(dev, fi, sector, data) {
        die(&format!("書き込み失敗 (sector {sector:#06x}): {e}"));
    }
    match read_sector(dev, fi, sector, data.len()) {
        Ok(back) if back == data => println!("sector {sector:#06x} を書き込み、読み直して一致を確認しました"),
        Ok(back) => {
            println!("警告: 読み直した内容が一致しません");
            for (i, (a, b)) in data.chunks(16).zip(back.chunks(16)).enumerate() {
                if a != b {
                    println!("  {:04x}: 書いた {}\n        読んだ {}", i * 16, hex(a), hex(b));
                }
            }
        }
        Err(e) => println!("警告: 読み直せません: {e}"),
    }
}

fn parse_hex_u8(s: &str) -> u8 {
    u8::from_str_radix(s.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| die(&format!("16進の 1B ではありません: {s}")))
}

fn parse_hex_u16(s: &str) -> u16 {
    u16::from_str_radix(s.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| die(&format!("16進ではありません: {s}")))
}

fn parse_slot(s: &str) -> u16 {
    match s.parse::<u16>() {
        Ok(n @ 1..=5) => n,
        _ => die("枠は 1〜5 で指定してください"),
    }
}

/// 書き込み系コマンド。
fn run_cmd(dev: &Dev, fi: u8, sector_size: usize, buttons: usize, cmd: &str, a: &[String]) {
    let arg = |i: usize| -> String {
        a.get(i)
            .cloned()
            .unwrap_or_else(|| die("引数が足りません"))
    };
    let no_crc = a.iter().any(|x| x == "--no-crc");
    let gshift = a.iter().any(|x| x == "--gshift");
    let pos: Vec<String> = a.iter().filter(|x| !x.starts_with("--")).cloned().collect();
    match cmd {
        "set" => {
            let slot = parse_slot(&arg(0));
            let btn: usize = arg(1).parse().unwrap_or_else(|_| die("ボタン番号は 10 進"));
            if btn >= buttons.min(16) {
                die(&format!("ボタン番号は 0〜{}", buttons.min(16) - 1));
            }
            if pos.len() < 6 {
                die("割り当ては 4B (例: 80 02 06 05)");
            }
            let bind: Vec<u8> = pos[2..6].iter().map(|x| parse_hex_u8(x)).collect();
            let mut d = read_sector(dev, fi, slot, sector_size)
                .unwrap_or_else(|e| die(&format!("読めません: {e}")));
            let at = if gshift { 0x60 } else { 0x20 } + btn * 4;
            println!(
                "枠 {slot} ボタン #{btn}{}: {} ({}) → {} ({})",
                if gshift { " (G-Shift)" } else { "" },
                hex(&d[at..at + 4]),
                binding(&d[at..at + 4]),
                hex(&bind),
                binding(&bind)
            );
            d[at..at + 4].copy_from_slice(&bind);
            write_verify(dev, fi, slot, &with_crc(d));
        }
        "restore" => {
            let slot = parse_slot(&arg(0));
            let rom = if slot == 2 { 0x0102 } else { 0x0101 };
            let d = read_sector(dev, fi, rom, sector_size)
                .unwrap_or_else(|e| die(&format!("ROM が読めません: {e}")));
            println!("ROM {rom:#06x} → 枠 {slot}");
            write_verify(dev, fi, slot, &with_crc(d));
        }
        "mode" => {
            let m = parse_hex_u8(&arg(0));
            if !matches!(m, 1 | 2) {
                die("mode は 1 (オンボード) か 2 (ホスト)");
            }
            match dev.rq(fi, 1, &[m]) {
                Ok(_) => println!("setMode {m} OK"),
                Err(e) => die(&format!("setMode 失敗: {e}")),
            }
            if let Ok(r) = dev.rq(fi, 2, &[]) {
                println!("getMode: {}", r[0]);
            }
        }
        "profile" => {
            let slot = parse_slot(&arg(0));
            match dev.rq(fi, 3, &[0x00, slot as u8]) {
                Ok(_) => println!("setCurrentProfile {slot} OK"),
                Err(e) => die(&format!("setCurrentProfile 失敗: {e}")),
            }
            if let Ok(r) = dev.rq(fi, 4, &[]) {
                println!("getCurrentProfile: {}", hex(&r[..4]));
            }
        }
        "dump" => {
            let sec = parse_hex_u16(&arg(0));
            let d = read_sector(dev, fi, sec, sector_size)
                .unwrap_or_else(|e| die(&format!("読めません: {e}")));
            let (ok, stored, calc) = crc_ok(&d);
            println!(
                "sector {sec:#06x} CRC {} (格納 {stored:04x} / 計算 {calc:04x})",
                if ok { "OK" } else { "NG" }
            );
            dump_raw(&d);
        }
        "write" => {
            let sec = parse_hex_u16(&pos.first().cloned().unwrap_or_else(|| die("sector を指定")));
            if sec == 0 || sec >= 0x0100 {
                die("書けるのはユーザー領域 (0x0001〜0x00FF) だけにしています");
            }
            let bytes: Vec<u8> = pos[1..].iter().map(|x| parse_hex_u8(x)).collect();
            if bytes.len() > sector_size - 2 {
                die("長すぎます");
            }
            let mut d = vec![0xFFu8; sector_size];
            d[..bytes.len()].copy_from_slice(&bytes);
            let d = if no_crc { d } else { with_crc(d) };
            write_verify(dev, fi, sec, &d);
        }
        _ => die(&format!("不明なコマンド: {cmd}")),
    }
}

fn utf16_name(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0 && *u != 0xFFFF)
        .collect();
    String::from_utf16_lossy(&units)
}

fn dump_raw(d: &[u8]) {
    for (i, row) in d.chunks(16).enumerate() {
        println!("      {:04x}: {}", i * 16, hex(row));
    }
}

fn show_profile(label: &str, d: &[u8], buttons: usize, raw: bool, macros: &mut Vec<u16>) {
    let (ok, stored, calc) = crc_ok(d);
    println!(
        "\n  == {label} (CRC {}: 格納 {stored:04x} / 計算 {calc:04x}) ==",
        if ok { "OK" } else { "NG" }
    );
    if d.iter().all(|b| *b == 0xFF) {
        println!("    (空 — 全 0xFF)");
        return;
    }
    let dpis: Vec<u16> = (0..5)
        .map(|i| u16::from_le_bytes([d[3 + i * 2], d[4 + i * 2]]))
        .collect();
    println!(
        "    report_rate={:#04x} default_dpi_idx={} switched_dpi_idx={} dpi(LE?)={:?}",
        d[0], d[1], d[2], dpis
    );
    println!(
        "    色={} power={:#04x} angle_snap={:#04x} 0x12..0x20={}",
        hex(&d[0x0D..0x10]),
        d[0x10],
        d[0x11],
        hex(&d[0x12..0x20])
    );
    if d.len() >= 0xD0 {
        println!("    名前: {:?}", utf16_name(&d[0xA0..0xD0]));
    }
    for (base, title) in [(0x20usize, "ボタン"), (0x60, "G-Shift 時")] {
        println!("    [{title}]");
        for i in 0..buttons.min(16) {
            let at = base + i * 4;
            if at + 4 > d.len() {
                break;
            }
            let b = &d[at..at + 4];
            if b[0] == 0x00 && b != [0, 0, 0, 0] {
                macros.push(u16::from_be_bytes([b[0], b[1]]));
            }
            println!("      #{i:<2} {}  {}", hex(b), binding(b));
        }
    }
    if d.len() >= 0xFC {
        println!("    LED 0xD0..0xFC: {}", hex(&d[0xD0..0xFC]));
    }
    if raw {
        dump_raw(d);
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let raw = args.iter().any(|a| a == "--raw");
    let name_filter = args.iter().position(|a| a == "--name").map(|i| {
        let v = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        v.unwrap_or_default()
    });
    let cmd = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .cloned();

    let api = hidapi::HidApi::new().expect("hidapi");
    let dev = find(&api, name_filter.as_deref());
    println!("対象: {:?} (device index {:#04x})", dev.name(), dev.d);
    let Some(fi) = dev.fidx(feat::ONBOARD_PROFILES) else {
        die("0x8100 がありません");
    };

    let info = dev
        .rq(fi, 0, &[])
        .unwrap_or_else(|e| die(&format!("getInfo 失敗: {e}")));
    let profiles = info[3] as usize;
    let rom_profiles = info[4] as u16;
    let buttons = info[5] as usize;
    let sector_size = u16::from_be_bytes([info[7], info[8]]) as usize;
    println!("getInfo: {}", hex(&info));
    println!(
        "  memory_model={} profile_format={} macro_format={} プロファイル数={} ROM={} ボタン数={} セクタ数={} セクタ長={} layout={:#04x} various={:#04x}",
        info[0], info[1], info[2], profiles, rom_profiles, buttons, info[6], sector_size, info[9], info[10]
    );
    match dev.rq(fi, 2, &[]) {
        Ok(m) => println!("getMode: {} (1=オンボード 2=ホスト)", m[0]),
        Err(e) => println!("getMode: ERR {e}"),
    }
    match dev.rq(fi, 4, &[]) {
        Ok(p) => println!("getCurrentProfile: {}", hex(&p[..4])),
        Err(e) => println!("getCurrentProfile: ERR {e}"),
    }
    if sector_size < 32 {
        die("セクタ長が不正です");
    }
    if let Some(cmd) = cmd {
        run_cmd(&dev, fi, sector_size, buttons, &cmd, &args[1..]);
        return;
    }

    let mut macros = vec![];
    // ディレクトリ: ユーザー 0x0000 と ROM 0x0100。エントリ [sector BE16, 有効, 予約]、0xFFFF で終端。
    let mut sectors: Vec<(String, u16)> = vec![];
    for (dir, title) in [(0x0000u16, "ユーザー"), (0x0100, "ROM")] {
        print!("\n== ディレクトリ {title} ({dir:#06x}) ");
        match read_sector(&dev, fi, dir, sector_size) {
            Ok(d) => {
                let (ok, stored, calc) = crc_ok(&d);
                println!(
                    "CRC {} (格納 {stored:04x} / 計算 {calc:04x}) ==",
                    if ok { "OK" } else { "NG" }
                );
                println!("  先頭: {}", hex(&d[..32]));
                for e in d.chunks_exact(4).take(16) {
                    let sec = u16::from_be_bytes([e[0], e[1]]);
                    if sec == 0xFFFF {
                        break;
                    }
                    println!("  エントリ: sector {sec:#06x} 有効={} 予約={:#04x}", e[2], e[3]);
                    sectors.push((format!("{title} sector {sec:#06x} (有効={})", e[2]), sec));
                }
                if raw {
                    dump_raw(&d);
                }
            }
            Err(e) => println!("読めません: {e}"),
        }
    }
    // ディレクトリが読めなかった場合に備え、ROM プロファイルも直接読む。
    for i in 1..=rom_profiles {
        let sec = 0x0100 + i;
        if !sectors.iter().any(|(_, s)| *s == sec) {
            sectors.push((format!("ROM sector {sec:#06x} (直接)"), sec));
        }
    }
    for (label, sec) in &sectors {
        match read_sector(&dev, fi, *sec, sector_size) {
            Ok(d) => show_profile(label, &d, buttons, raw, &mut macros),
            Err(e) => println!("\n  == {label}: 読めません: {e}"),
        }
    }

    macros.sort();
    macros.dedup();
    for sec in macros {
        println!("\n== マクロ sector {sec:#06x} ==");
        match read_sector(&dev, fi, sec, sector_size) {
            Ok(d) => dump_raw(&d),
            Err(e) => println!("  読めません: {e}"),
        }
    }
}
