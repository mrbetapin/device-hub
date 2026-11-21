//! Logitech HID++ 解析ツール。
//!
//! ```text
//! cargo run -p devhub-hid --example hidpp_probe            # 046d の全 IF 列挙 + 機能表ダンプ
//! cargo run -p devhub-hid --example hidpp_probe -- --sniff # 受信レポートを全部表示 (Ctrl+C で終了)
//! ```
//!
//! - 列挙: VID 046d の全 HID コレクション (usage page / usage / IF 番号)。
//! - 機能表: `FF00:0002` (long report) のコレクションごとに、応答した device index と
//!   FeatureSet (0x0001) が返す全 feature (ID・index・種別・版) を出す。
//!   docs/LOGITECH_HIDPP.md の表と突き合わせ、registry / ドライバに足す材料にする。
//! - `--sniff`: long/short の両コレクションを開き、届いた生レポートを 16 進で表示する。
//!   ボタンを押す・電源を入れ直す・G HUB の操作をする等で、通知の形を観察できる。
//!   (G HUB を起動したまま sniff すると、G HUB が送った要求への応答も見える。)
//!
//! G HUB は終了してから使うこと (同じ IF で要求を取り合う)。

use devhub_hid::logitech::{feat, probe};

fn main() {
    let api = hidapi::HidApi::new().expect("hidapi");
    let sniff = std::env::args().any(|a| a == "--sniff");

    println!("== Logitech HID インターフェース ==");
    let mut long = vec![];
    let mut short = vec![];
    for d in api.device_list().filter(|d| d.vendor_id() == 0x046D) {
        println!(
            "046d:{:04x} if={} up={:04x} u={:04x} {:?}",
            d.product_id(),
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.product_string().unwrap_or("")
        );
        if d.usage_page() == 0xFF00 && d.usage() == 0x0002 {
            long.push(d.clone());
        }
        if d.usage_page() == 0xFF00 && d.usage() == 0x0001 {
            short.push(d.clone());
        }
    }

    if sniff {
        let mut handles = vec![];
        for d in long.iter().chain(short.iter()) {
            if let Ok(h) = d.open_device(&api) {
                h.set_blocking_mode(false).ok();
                handles.push((d.product_id(), d.usage(), h));
            }
        }
        println!(
            "== sniff ({} コレクション) — Ctrl+C で終了 ==",
            handles.len()
        );
        let t0 = std::time::Instant::now();
        let mut buf = [0u8; 64];
        loop {
            for (pid, usage, h) in &handles {
                if let Ok(n) = h.read_timeout(&mut buf, 5) {
                    if n > 0 {
                        let hex: Vec<String> =
                            buf[..n].iter().map(|b| format!("{b:02x}")).collect();
                        println!(
                            "{:>8.3}s {pid:04x}/u{usage} {}",
                            t0.elapsed().as_secs_f32(),
                            hex.join(" ")
                        );
                    }
                }
            }
        }
    }

    for d in &long {
        let pid = d.product_id();
        // 有線マウスは 0xC0xx、レシーバーは 0xC5xx。
        let wireless = (pid & 0xFF00) == 0xC500;
        println!(
            "\n== 046d:{pid:04x} ({}) ==",
            if wireless {
                "レシーバー"
            } else {
                "有線"
            }
        );
        let Ok(h) = d.open_device(&api) else {
            println!("  開けません (他のアプリが占有?)");
            continue;
        };
        match probe(&h, wireless) {
            Ok(r) => {
                println!(
                    "  device index = {:#04x}, HID++ {}.x",
                    r.dev_idx, r.protocol
                );
                println!(
                    "  {:>5}  {:>6}  {:>4}  {:>3}  名前",
                    "index", "ID", "type", "ver"
                );
                for (id, idx, ty, ver) in r.features {
                    // type: bit7 obsolete, bit6 hidden, bit5 engineering
                    println!(
                        "  {idx:>5}  0x{id:04X}  0x{ty:02x}  {ver:>3}  {}",
                        feat::name(id)
                    );
                }
            }
            Err(e) => println!("  応答なし: {e} (無線ならマウスを動かして起こしてから再実行)"),
        }
    }
}
