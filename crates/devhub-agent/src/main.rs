//! Device HUB 常駐エージェント (G HUB の常駐部分の代わり)。
//!
//! ウィンドウもトレイも持たない小さな常駐プロセス。2 秒ごとに HID を列挙し、
//! Logitech (HID++) マウスのドライバを開いたままにする。再適用そのものは
//! ドライバの監視スレッドがオーナー (= このプロセス) として行う:
//! 電源投入・スリープ復帰・レシーバー再接続のたびに、Device HUB で保存した
//! DPI / ポーリング / LED / DPI ボタン設定をマウスへ書き戻す。
//!
//! 使い方:
//!   devhub-agent              常駐 (多重起動は 2 本目がすぐ終了)
//!   devhub-agent --install    ログオン時の自動起動に登録して常駐を開始
//!   devhub-agent --uninstall  自動起動を解除
//!   devhub-agent --once       1 回だけ列挙・再適用して終了 (動作確認用)
//!
//! ログ: `%TEMP%\devhub_agent.log`

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use devhub_core::model::{DeviceId, DriverKind};
use devhub_hid::agent_log;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    if std::env::var_os("DEVHUB_AGENT_LOG").is_none() {
        let p = std::env::temp_dir().join("devhub_agent.log");
        // SAFETY: スレッドを起こす前の起動直後に 1 回だけ設定する。
        unsafe { std::env::set_var("DEVHUB_AGENT_LOG", p) };
    }
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "--install" => {
            // set(true) がレジストリ登録とエージェント起動 (別プロセス) まで行う。
            match devhub_hid::autostart::set(true) {
                Ok(()) => agent_log("自動起動を登録しました"),
                Err(e) => agent_log(&format!("自動起動の登録に失敗: {e}")),
            }
            return;
        }
        "--uninstall" => {
            match devhub_hid::autostart::set(false) {
                Ok(()) => agent_log("自動起動を解除しました"),
                Err(e) => agent_log(&format!("自動起動の解除に失敗: {e}")),
            }
            return;
        }
        _ => {}
    }
    let once = arg == "--once";
    if !single_instance() {
        // 既に常駐中。
        return;
    }
    agent_log(&format!("起動 (pid {})", std::process::id()));
    // 先にオーナーを取る (GUI が先に取っていれば、GUI 終了後に引き継ぐ)。
    devhub_hid::owner::is_owner();

    let hub = loop {
        match devhub_hid::connect() {
            Ok(h) => break h,
            Err(e) => {
                agent_log(&format!("HID 初期化失敗: {e} — 再試行"));
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };

    let mut open: HashMap<DeviceId, Arc<dyn devhub_core::DeviceDriver>> = HashMap::new();
    let mut warned_ghub = false;
    loop {
        devhub_hid::owner::is_owner();
        match hub.scan() {
            Ok(list) => {
                let ids: Vec<DeviceId> = list
                    .iter()
                    .filter(|d| d.driver == DriverKind::Logitech && d.supported)
                    .map(|d| d.id.clone())
                    .collect();
                // 抜かれたものを閉じる (ドライバの Drop で監視スレッドが止まる)。
                open.retain(|id, _| {
                    let keep = ids.contains(id);
                    if !keep {
                        hub.invalidate(id);
                        agent_log(&format!("切断: {id}"));
                    }
                    keep
                });
                for id in ids {
                    if open.contains_key(&id) {
                        continue;
                    }
                    match hub.driver(&id) {
                        Ok(d) => {
                            agent_log(&format!("接続: {} ({id})", d.info().name));
                            open.insert(id, d);
                        }
                        Err(e) => agent_log(&format!("open {id} 失敗: {e}")),
                    }
                }
            }
            Err(e) => agent_log(&format!("scan 失敗: {e}")),
        }
        if !warned_ghub && ghub_running() {
            warned_ghub = true;
            agent_log("警告: G HUB (lghub) が動作中です。設定を取り合うので終了/アンインストールしてください");
        }
        if once {
            // 監視スレッドの初回再適用を待ってから終わる。
            std::thread::sleep(Duration::from_secs(4));
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// 多重起動防止 (`%TEMP%\devhub_agent.lock` を共有禁止で開き続ける)。
fn single_instance() -> bool {
    let p = std::env::temp_dir().join("devhub_agent.lock");
    let f = {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(&p)
        }
        #[cfg(not(windows))]
        {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&p)
        }
    };
    match f {
        Ok(f) => {
            // プロセス終了まで保持する。
            std::mem::forget(f);
            true
        }
        Err(_) => false,
    }
}

/// G HUB の常駐 (lghub_agent.exe / lghub_updater.exe など) が動いているか。
fn ghub_running() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_ascii_lowercase()
                    .contains("lghub")
            })
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}
