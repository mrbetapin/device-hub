//! 「オーナー」= 再適用・DPI ボタン処理を担当するプロセス (1 台につき 1 つ)。
//!
//! GUI (device-hub) と常駐エージェント (devhub-agent) は同じ HID を同時に開くので、
//! 両方が再適用や DPI 順送りをすると二重に動く。`%APPDATA%\DeviceHub\logitech\owner.lock`
//! を共有禁止で開けたプロセスだけがオーナーになる。オーナーが終了すると OS がロックを
//! 外すので、残ったプロセスが次の周期で引き継ぐ。

use std::fs::File;
use std::sync::Mutex;

static LOCK: Mutex<Option<File>> = Mutex::new(None);

fn lock_path() -> std::path::PathBuf {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("DeviceHub")
        .join("logitech")
        .join("owner.lock")
}

/// このプロセスがオーナーか (まだなら取得を試みる)。
pub fn is_owner() -> bool {
    let mut g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if g.is_some() {
        return true;
    }
    let path = lock_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    match open_exclusive(&path) {
        Some(f) => {
            *g = Some(f);
            true
        }
        None => false,
    }
}

#[cfg(windows)]
pub(crate) fn open_exclusive(path: &std::path::Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .ok()
}

#[cfg(not(windows))]
pub(crate) fn open_exclusive(path: &std::path::Path) -> Option<File> {
    // Windows 以外は単一プロセス運用 (開発用)。
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .ok()
}
