//! 常駐エージェント (`devhub-agent.exe`) のログオン時自動起動。
//!
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` の値 `DeviceHubAgent` に
//! エージェントのパスを書く (管理者権限は不要)。エージェントは `device-hub.exe` と
//! 同じフォルダに置く前提。

use devhub_core::backend::{HubError, HubResult};
use std::path::PathBuf;

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE: &str = "DeviceHubAgent";
/// エージェントの実行ファイル名。
pub const AGENT_EXE: &str = if cfg!(windows) {
    "devhub-agent.exe"
} else {
    "devhub-agent"
};

/// このプラットフォームで自動起動を扱えるか。
pub fn supported() -> bool {
    cfg!(windows)
}

/// 実行中の exe と同じフォルダのエージェント。
pub fn agent_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if exe
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(AGENT_EXE))
    {
        return Some(exe);
    }
    let p = exe.parent()?.join(AGENT_EXE);
    p.exists().then_some(p)
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 自動起動が登録されているか。
#[cfg(windows)]
pub fn is_enabled() -> bool {
    use windows_sys::Win32::System::Registry::*;
    let key = wide(RUN_KEY);
    let val = wide(VALUE);
    let mut size: u32 = 0;
    // SAFETY: 文字列は 0 終端。データは受け取らずサイズだけ問い合わせる。
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    r == 0
}

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

/// 自動起動の登録/解除。登録時はエージェントをすぐ起動もする。
#[cfg(windows)]
pub fn set(on: bool) -> HubResult<()> {
    use windows_sys::Win32::System::Registry::*;
    let key = wide(RUN_KEY);
    let val = wide(VALUE);
    if on {
        let agent = agent_path().ok_or_else(|| {
            HubError::Invalid(format!(
                "{AGENT_EXE} が device-hub.exe と同じフォルダにありません"
            ))
        })?;
        let cmd = wide(&format!("\"{}\"", agent.display()));
        // SAFETY: 0 終端の UTF-16 をバイト長付きで渡す。
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                val.as_ptr(),
                REG_SZ,
                cmd.as_ptr() as *const _,
                (cmd.len() * 2) as u32,
            )
        };
        if r != 0 {
            return Err(HubError::Io(format!("レジストリ書き込み失敗 ({r})")));
        }
        // 今すぐ常駐させる (多重起動はエージェント側で弾く)。
        std::process::Command::new(&agent).spawn().ok();
        Ok(())
    } else {
        // SAFETY: 0 終端の UTF-16。
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr()) };
        // 2 = ERROR_FILE_NOT_FOUND (もともと無い) は成功扱い。
        if r != 0 && r != 2 {
            return Err(HubError::Io(format!("レジストリ削除失敗 ({r})")));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn set(_on: bool) -> HubResult<()> {
    Err(HubError::Unsupported("autostart"))
}

/// GUI の起動時に呼ぶ: 自動起動が ON なら、登録先をこの exe と同じフォルダのエージェントに
/// 更新し (別フォルダの古いビルドを指したままにしない)、エージェントを起動する
/// (常駐中なら新しい方はすぐ終了する)。
pub fn ensure_running() {
    if is_enabled() {
        if let Err(e) = set(true) {
            crate::agent_log(&format!("エージェントの起動に失敗: {e}"));
        }
    }
}
