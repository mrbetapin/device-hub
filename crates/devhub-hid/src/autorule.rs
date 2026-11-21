//! 常駐エージェント用のプロセス連動 — GUI の `procmon.rs` の簡易版。
//!
//! GUI (device-hub) が起動している間は GUI がルールを評価し、閉じている間だけ
//! エージェントが `%APPDATA%\DeviceHub\prefs.json` の `proc_rules` を評価する
//! (`gui_running` で判定。二重に適用しない)。
//!
//! GUI と同じ規則: exe 名の部分一致、前面が 2 tick 連続で同じときだけ評価、
//! 離脱 (on_leave) を先に・新規 (on_fg) を後に適用。ルール名が `◆本体プロファイル N`
//! なら本体の枠の切替 (`Change::Profile(N-1)`)、それ以外は設定プロファイルの変更列。

use devhub_core::Change;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// ルールの適用先に「本体の枠」を直接選ぶための予約名。
pub const SLOT_PREFIX: &str = "◆本体プロファイル ";

pub fn slot_name(n: u8) -> String {
    format!("{SLOT_PREFIX}{n}")
}

/// 予約名なら枠番号 (1 始まり)。
pub fn slot_of(name: &str) -> Option<u8> {
    name.strip_prefix(SLOT_PREFIX)?
        .trim()
        .parse()
        .ok()
        .filter(|n| *n >= 1)
}

// ---------------------------------------------------------------- GUI の在否

fn gui_lock_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("DeviceHub")
        .join("gui.lock")
}

/// GUI が起動時に呼ぶ: 終了までロックを持ち続ける (エージェントはルール評価を止める)。
pub fn hold_gui_lock() {
    let p = gui_lock_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).ok();
    }
    // エージェントが在否確認で一瞬開いているところに当たることがあるので数回試す。
    for _ in 0..20 {
        if let Some(f) = crate::owner::open_exclusive(&p) {
            std::mem::forget(f);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// GUI が起動しているか (ロックを取れなければ起動中)。
pub fn gui_running() -> bool {
    let p = gui_lock_path();
    if !p.exists() {
        return false;
    }
    crate::owner::open_exclusive(&p).is_none()
}

// ---------------------------------------------------------------- 前面ウィンドウ

/// 前面ウィンドウの exe 名 (小文字・拡張子つき)。
#[cfg(windows)]
pub fn foreground_exe() -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    // SAFETY: Win32 API をドキュメント通りの引数で呼ぶ。ハンドルは必ず閉じる。
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 || len == 0 {
            return None;
        }
        String::from_utf16_lossy(&buf[..len as usize])
            .rsplit(['\\', '/'])
            .next()
            .map(|s| s.to_lowercase())
    }
}

#[cfg(not(windows))]
pub fn foreground_exe() -> Option<String> {
    None
}

// ---------------------------------------------------------------- ルール評価

#[derive(Clone, Debug, Deserialize)]
struct Rule {
    process: String,
    device: String,
    on_fg: String,
    #[serde(default)]
    on_leave: String,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
struct Target {
    device: String,
    #[serde(default)]
    changes: Vec<Change>,
}

#[derive(Clone, Debug, Deserialize)]
struct Profile {
    name: String,
    #[serde(default)]
    devices: Vec<Target>,
    // 旧形式 (単一 device + changes)。
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    changes: Vec<Change>,
}

#[derive(Default, Deserialize)]
struct PrefsFile {
    /// 1 件ずつ読む (知らない Change があっても他は使えるように)。
    #[serde(default)]
    profiles: Vec<serde_json::Value>,
    #[serde(default)]
    proc_rules: Vec<Rule>,
}

/// 1 デバイスへ流す変更。
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub device: String,
    /// ログ用 (プロファイル名)。
    pub label: String,
    pub changes: Vec<Change>,
}

fn rule_key(r: &Rule) -> String {
    format!("{}|{}", r.process.trim().to_lowercase(), r.device)
}

fn rule_matches(r: &Rule, fg: &str) -> bool {
    let pat = r.process.trim().to_lowercase();
    r.enabled && !pat.is_empty() && fg.contains(&pat)
}

pub struct AutoRules {
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
    rules: Vec<Rule>,
    profiles: Vec<Profile>,
    last_fg: Option<String>,
    /// アクティブなキー → (デバイス, 離脱時のプロファイル名)。
    active: HashMap<String, (String, String)>,
}

impl Default for AutoRules {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoRules {
    pub fn new() -> Self {
        let path = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|b| b.join("DeviceHub").join("prefs.json"));
        Self {
            path,
            mtime: None,
            rules: vec![],
            profiles: vec![],
            last_fg: None,
            active: HashMap::new(),
        }
    }

    /// GUI に担当が移ったとき: 状態を捨てる (戻ってきたら前面から評価し直す)。
    pub fn reset(&mut self) {
        self.last_fg = None;
        self.active.clear();
    }

    pub fn has_rules(&self) -> bool {
        self.rules.iter().any(|r| r.enabled)
    }

    /// prefs.json が変わっていれば読み直す。
    pub fn reload(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if mtime.is_none() || mtime == self.mtime {
            return;
        }
        self.mtime = mtime;
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        match serde_json::from_slice::<PrefsFile>(&bytes) {
            Ok(p) => {
                self.set(p);
                crate::agent_log(&format!(
                    "プロセス連動: ルール {} 件 (有効 {} 件)・プロファイル {} 件を読み込み",
                    self.rules.len(),
                    self.rules.iter().filter(|r| r.enabled).count(),
                    self.profiles.len()
                ));
            }
            Err(e) => crate::agent_log(&format!("prefs.json を読めません: {e}")),
        }
    }

    fn set(&mut self, p: PrefsFile) {
        self.rules = p.proc_rules;
        self.profiles = p
            .profiles
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Profile>(v).ok())
            .map(|mut pr| {
                if let Some(d) = pr.device.take() {
                    if !pr.devices.iter().any(|t| t.device == d) {
                        pr.devices.push(Target {
                            device: d,
                            changes: std::mem::take(&mut pr.changes),
                        });
                    }
                }
                pr
            })
            .collect();
    }

    fn resolve(&self, device: &str, name: &str) -> Option<Action> {
        if let Some(n) = slot_of(name) {
            return Some(Action {
                device: device.to_string(),
                label: name.to_string(),
                changes: vec![Change::Profile(n - 1)],
            });
        }
        let t = self
            .profiles
            .iter()
            .filter(|p| p.name == name)
            .find_map(|p| p.devices.iter().find(|t| t.device == device))?;
        Some(Action {
            device: device.to_string(),
            label: name.to_string(),
            changes: t.changes.clone(),
        })
    }

    /// 1 tick (~1 秒) の評価。適用する変更を返す (離脱 → 新規の順)。
    /// `fg` は前面 exe 名 (解決できなければ None — その tick は遷移しない)。
    pub fn tick(&mut self, fg: Option<String>) -> Vec<Action> {
        let Some(fg) = fg else {
            return vec![];
        };
        let stable = self.last_fg.as_deref() == Some(fg.as_str());
        if !stable {
            crate::agent_log(&format!("プロセス連動: 前面 = {fg}"));
        }
        self.last_fg = Some(fg.clone());
        if !stable {
            return vec![];
        }
        // 一致したルールをキーで集約 (並び順を保つ)。
        let mut order: Vec<String> = vec![];
        let mut by_key: HashMap<String, Vec<&Rule>> = HashMap::new();
        for r in &self.rules {
            if rule_matches(r, &fg) {
                let k = rule_key(r);
                if !order.contains(&k) {
                    order.push(k.clone());
                }
                by_key.entry(k).or_default().push(r);
            }
        }
        let mut out = vec![];
        // 離脱。
        let mut left: Vec<String> = self
            .active
            .keys()
            .filter(|k| !by_key.contains_key(*k))
            .cloned()
            .collect();
        left.sort();
        for k in left {
            if let Some((dev, leave)) = self.active.remove(&k) {
                if !leave.is_empty() {
                    out.extend(self.resolve(&dev, &leave));
                }
            }
        }
        // 新規。
        for k in &order {
            let rs = &by_key[k];
            if !self.active.contains_key(k) {
                let mut last: Option<&str> = None;
                for r in rs {
                    if !r.on_fg.is_empty() && last != Some(r.on_fg.as_str()) {
                        out.extend(self.resolve(&r.device, &r.on_fg));
                        last = Some(r.on_fg.as_str());
                    }
                }
            }
            let leave = rs
                .iter()
                .rev()
                .find(|r| !r.on_leave.is_empty())
                .map(|r| r.on_leave.clone())
                .unwrap_or_default();
            self.active
                .insert(k.clone(), (rs[0].device.clone(), leave));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(json: &str) -> AutoRules {
        let mut a = AutoRules {
            path: None,
            ..AutoRules::new()
        };
        a.set(serde_json::from_str(json).unwrap());
        a
    }

    #[test]
    fn slot_names() {
        assert_eq!(slot_of(&slot_name(2)), Some(2));
        assert_eq!(slot_of("◆本体プロファイル 0"), None);
        assert_eq!(slot_of("fps"), None);
    }

    #[test]
    fn enter_and_leave_with_slots() {
        let mut a = rules(
            r#"{"proc_rules":[{"process":"valorant","device":"m","on_fg":"◆本体プロファイル 2","on_leave":"◆本体プロファイル 1"}],
                "other": 1}"#,
        );
        // 1 tick 目は安定待ち。
        assert!(a.tick(Some("valorant.exe".into())).is_empty());
        let e = a.tick(Some("valorant.exe".into()));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].changes, vec![Change::Profile(1)]);
        // 継続中は何もしない。
        assert!(a.tick(Some("valorant.exe".into())).is_empty());
        // 前面が変わる → 安定待ち → 離脱。
        assert!(a.tick(Some("explorer.exe".into())).is_empty());
        let l = a.tick(Some("explorer.exe".into()));
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].changes, vec![Change::Profile(0)]);
        // 解決できない tick は遷移しない。
        assert!(a.tick(None).is_empty());
    }

    #[test]
    fn named_profiles_and_old_format() {
        let mut a = rules(
            r#"{"profiles":[
                    {"name":"fps","devices":[{"device":"m","changes":[{"ActiveStage":0}]}]},
                    {"name":"old","device":"m","changes":[{"ActiveStage":2}]},
                    {"name":"broken","devices":[{"device":"m","changes":[{"NoSuchChange":1}]}]}],
                "proc_rules":[{"process":"apex","device":"m","on_fg":"fps","on_leave":"old"}]}"#,
        );
        a.tick(Some("r5apex.exe".into()));
        let e = a.tick(Some("r5apex.exe".into()));
        assert_eq!(e[0].changes, vec![Change::ActiveStage(0)]);
        a.tick(Some("x.exe".into()));
        let l = a.tick(Some("x.exe".into()));
        assert_eq!(l[0].changes, vec![Change::ActiveStage(2)]);
        assert_eq!(a.profiles.len(), 2, "壊れたプロファイルだけ読み飛ばす");
    }
}
