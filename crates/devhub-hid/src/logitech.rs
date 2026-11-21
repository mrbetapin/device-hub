//! Logitech G 系マウス — HID++ 2.0 (G HUB の代替)。
//!
//! 対象: PRO Wireless (LIGHTSPEED レシーバー `046d:c539` / 有線 `046d:c088`) と
//! 同じ HID++ 2.0 の機能 (feature) を持つ G シリーズ。プロトコルの詳細と出典は
//! `docs/LOGITECH_HIDPP.md`。
//!
//! # フレーム
//!
//! - long report `[0x11, dev, featIdx, fn<<4 | swId, params[16]]` (20B) で要求・応答する。
//!   Windows では long は `FF00:0002` コレクションに分かれているので、そこだけ開く。
//! - `dev` = 有線 0xFF、レシーバー経由はペア先の index (1..6、PRO Wireless は 1)。
//! - 応答は同じ `featIdx` と `fn|swId` のエコー。エラーは `[0x11, dev, 0xFF, featIdx, fn|swId, code]`。
//! - `swId` = 0 はデバイス発の通知。要求には 1..15 を使う (GUI と常駐エージェントが
//!   同じ IF を開くので、プロセスごとに変えて他方の応答を取り違えないようにする)。
//!
//! # ホストモードと常駐
//!
//! PRO Wireless は既定で **オンボードメモリモード** (本体のプロファイルで動く)。
//! DPI・レポートレートを PC から変えるには OnboardProfiles (0x8100) で **ホストモード**
//! に切り替える必要があり、ホストモードの値は電源を切ると消える。G HUB はこれを常駐で
//! 再設定している。このドライバも同じ方式で、変更は `%APPDATA%\DeviceHub\logitech\` に
//! 保存し、**オーナー** (常駐エージェント `devhub-agent`、居なければ GUI) が
//! 接続・電源投入・スリープ復帰のたびに再適用する (`Monitor`)。

use crate::owner;
use devhub_core::backend::*;
use devhub_core::model::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

// ====================================================================== 定数

const REPORT_SHORT: u8 = 0x10;
const REPORT_LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
/// 有線 (デバイス直結) の device index。
const DEV_WIRED: u8 = 0xFF;

/// HID++ 2.0 feature ID (公開ドキュメント / Solaar / libratbag の名前に合わせる)。
pub mod feat {
    pub const ROOT: u16 = 0x0000;
    pub const FEATURE_SET: u16 = 0x0001;
    pub const FW_VERSION: u16 = 0x0003;
    pub const DEVICE_NAME: u16 = 0x0005;
    pub const BATTERY_STATUS: u16 = 0x1000;
    pub const BATTERY_VOLTAGE: u16 = 0x1001;
    pub const UNIFIED_BATTERY: u16 = 0x1004;
    pub const REPROG_CONTROLS_V4: u16 = 0x1B04;
    pub const WIRELESS_STATUS: u16 = 0x1D4B;
    pub const ADJUSTABLE_DPI: u16 = 0x2201;
    pub const REPORT_RATE: u16 = 0x8060;
    pub const COLOR_LED_EFFECTS: u16 = 0x8070;
    pub const ONBOARD_PROFILES: u16 = 0x8100;

    /// 名前 (プローブ表示用)。
    pub fn name(id: u16) -> &'static str {
        match id {
            0x0000 => "Root",
            0x0001 => "FeatureSet",
            0x0002 => "FeatureInfo",
            0x0003 => "DeviceFwVersion",
            0x0005 => "DeviceName",
            0x0007 => "DeviceFriendlyName",
            0x0020 => "ConfigChange",
            0x00C2 => "DfuControlSigned",
            0x00D0 => "Dfu",
            0x1000 => "BatteryStatus",
            0x1001 => "BatteryVoltage",
            0x1004 => "UnifiedBattery",
            0x1814 => "ChangeHost",
            0x1815 => "HostsInfo",
            0x1B04 => "ReprogControlsV4",
            0x1D4B => "WirelessDeviceStatus",
            0x1802 => "DeviceReset",
            0x1803 => "GpioAccess",
            0x1806 => "ConfigurableDeviceProperties",
            0x1811 => "Oobstate",
            0x1830 => "PowerModes",
            0x18A1 => "LedSwControl",
            0x1DF3 => "EquadDjDebugInfo",
            0x1E00 => "EnableHiddenFeatures",
            0x1E02 => "ManageDeactivatableFeatures",
            0x1EB0 => "TdeAccessToNvm",
            0x2201 => "AdjustableDpi",
            0x2202 => "ExtendedAdjustableDpi",
            0x8060 => "AdjustableReportRate",
            0x8061 => "ExtendedAdjustableReportRate",
            0x8070 => "ColorLedEffects",
            0x8071 => "RgbEffects",
            0x8090 => "ModeStatus",
            0x8100 => "OnboardProfiles",
            0x8110 => "MouseButtonSpy",
            _ => "?",
        }
    }
}

/// ReprogControlsV4 の DPI 切替系コントロール ID (公開の CID 表から)。
const DPI_CIDS: &[u16] = &[0x00FD, 0x00ED, 0x00EE];

/// 応答待ち (レシーバー中継は数十 ms)。
const REPLY_TIMEOUT: Duration = Duration::from_millis(600);
/// 監視スレッドの周期。
const MONITOR_PERIOD: Duration = Duration::from_millis(1500);

// ====================================================================== 純粋な符号化/復号 (テスト対象)

/// HID++ のエラー。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidppErr {
    /// HID++ 2.0 のエラーコード (2=InvalidArgument, 3=OutOfRange, 5=NotAllowed, 7=Busy …)。
    V2(u8),
    /// HID++ 1.0 (レシーバー) のエラー (8=UnknownDevice, 9=ResourceError …) — 多くは未接続/スリープ。
    V1(u8),
}

/// long 要求フレーム (report ID 込み 20B)。
pub fn build_long(dev: u8, feat_idx: u8, func: u8, sw: u8, params: &[u8]) -> [u8; LONG_LEN] {
    let mut b = [0u8; LONG_LEN];
    b[0] = REPORT_LONG;
    b[1] = dev;
    b[2] = feat_idx;
    b[3] = ((func & 0x0F) << 4) | (sw & 0x0F);
    let n = params.len().min(16);
    b[4..4 + n].copy_from_slice(&params[..n]);
    b
}

/// 受信レポートが (dev, feat_idx, func, sw) の要求への応答なら Some。
/// `Ok(params[16])` / `Err(HidppErr)`。関係ないレポート (通知・他プロセスの応答) は None。
pub fn match_reply(
    r: &[u8],
    dev: u8,
    feat_idx: u8,
    func: u8,
    sw: u8,
) -> Option<Result<[u8; 16], HidppErr>> {
    let fsw = ((func & 0x0F) << 4) | (sw & 0x0F);
    if r.len() < 5 || r[1] != dev {
        return None;
    }
    match r[0] {
        REPORT_LONG => {
            if r[2] == feat_idx && r[3] == fsw {
                let mut p = [0u8; 16];
                let n = (r.len() - 4).min(16);
                p[..n].copy_from_slice(&r[4..4 + n]);
                Some(Ok(p))
            } else if r[2] == 0xFF && r[3] == feat_idx && r[4] == fsw {
                Some(Err(HidppErr::V2(r.get(5).copied().unwrap_or(0))))
            } else {
                None
            }
        }
        // HID++ 1.0 エラー `[0x10, dev, 0x8F, subId, addr, code, 0]`。
        // (Windows では short は別コレクションなので通常ここには来ない。)
        REPORT_SHORT if r.len() >= 6 && r[2] == 0x8F && r[3] == feat_idx && r[4] == fsw => {
            Some(Err(HidppErr::V1(r[5])))
        }
        _ => None,
    }
}

/// `getSensorDpiList` の語列 (BE u16、0 終端) → (min, max, step)。
/// 上位 3 bit が 111 の語は「前後の値の間をこの刻みで」(range 表現)。
pub fn parse_dpi_list(bytes: &[u8]) -> Option<(u32, u32, u32)> {
    let mut vals: Vec<u32> = vec![];
    let mut step: Option<u32> = None;
    for w in bytes.chunks_exact(2) {
        let v = u16::from_be_bytes([w[0], w[1]]);
        if v == 0 {
            break;
        }
        if v >> 13 == 0b111 {
            step = Some((v & 0x1FFF) as u32);
        } else {
            vals.push(v as u32);
        }
    }
    let min = *vals.first()?;
    let max = *vals.last()?;
    let step = step.unwrap_or_else(|| {
        // 離散リスト: 隣り合う差の最大公約数。
        vals.windows(2)
            .map(|w| w[1].saturating_sub(w[0]))
            .filter(|d| *d > 0)
            .fold(0, gcd)
            .max(1)
    });
    Some((min, max, step.max(1)))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `getReportRateList` のビットマスク (bit n = (n+1) ms) → Hz (降順)。
pub fn parse_rate_list(mask: u8) -> Vec<i64> {
    let mut v: Vec<i64> = (0..8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| 1000 / (i as i64 + 1))
        .collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

/// 電池電圧 (mV) → 残量 % (リチウムイオンの放電曲線を線形補間)。
pub fn voltage_to_percent(mv: u16) -> u8 {
    const T: &[(i32, i32)] = &[
        (4186, 100),
        (4067, 90),
        (3989, 80),
        (3922, 70),
        (3859, 60),
        (3811, 50),
        (3778, 40),
        (3751, 30),
        (3717, 20),
        (3671, 10),
        (3646, 5),
        (3579, 2),
        (3500, 0),
    ];
    let v = mv as i32;
    if v >= T[0].0 {
        return 100;
    }
    for w in T.windows(2) {
        let (hi_v, hi_p) = w[0];
        let (lo_v, lo_p) = w[1];
        if v >= lo_v {
            return (lo_p + (v - lo_v) * (hi_p - lo_p) / (hi_v - lo_v)) as u8;
        }
    }
    0
}

/// BatteryVoltage (0x1001) の `[volt_hi, volt_lo, flags]` → Battery。
pub fn decode_battery_voltage(p: &[u8]) -> Battery {
    let mv = u16::from_be_bytes([p[0], p[1]]);
    let flags = p[2];
    Battery {
        percent: Some(voltage_to_percent(mv)),
        charging: flags & 0x80 != 0,
    }
}

/// DeviceFwVersion (0x0003) `getFwInfo` → "U1 39.02.B0021" 形式。
pub fn format_fw(p: &[u8]) -> Option<String> {
    let prefix: String = p[1..4]
        .iter()
        .filter(|c| c.is_ascii_graphic())
        .map(|&c| c as char)
        .collect();
    let build = u16::from_be_bytes([p[6], p[7]]);
    if prefix.is_empty() && p[4] == 0 && p[5] == 0 && build == 0 {
        return None;
    }
    let mut s = format!("{} {:02X}.{:02X}", prefix, p[4], p[5]);
    if build != 0 {
        s.push_str(&format!(".B{build:04X}"));
    }
    Some(s.trim().to_string())
}

/// ColorLedEffects (0x8070) の既知エフェクト ID。
pub mod led {
    pub const OFF: u16 = 0x00;
    pub const STATIC: u16 = 0x01;
    pub const PULSE: u16 = 0x02;
    pub const CYCLE: u16 = 0x03;
    pub const BREATHE: u16 = 0x0A;
}

/// UI の速度 0..=10 → 周期 ms (10 が最速 1 秒、0 が 20 秒)。
pub fn speed_to_period(speed: u8) -> u16 {
    let s = speed.min(10) as u16;
    1000 + (10 - s) * 1900
}

/// `setZoneEffect` の 10B パラメータ。
pub fn led_params(effect_id: u16, st: &LightingState) -> [u8; 10] {
    let mut p = [0u8; 10];
    let c = st.color;
    let period = speed_to_period(st.speed).to_be_bytes();
    let intensity = st.brightness.clamp(1, 100);
    match effect_id {
        led::STATIC => {
            p[0..3].copy_from_slice(&[c.r, c.g, c.b]);
            // p[3] = ramp (0 = 既定)
        }
        led::PULSE => {
            p[0..3].copy_from_slice(&[c.r, c.g, c.b]);
            p[3] = st.speed.min(10) * 25;
        }
        led::CYCLE => {
            p[5..7].copy_from_slice(&period);
            p[7] = intensity;
        }
        led::BREATHE => {
            p[0..3].copy_from_slice(&[c.r, c.g, c.b]);
            p[3..5].copy_from_slice(&period);
            p[5] = 0; // 波形 (0 = 既定)
            p[6] = intensity;
        }
        _ => {}
    }
    p
}

fn led_effect_label(id: u16) -> Option<LightEffect> {
    Some(match id {
        led::OFF => LightEffect::new(0, "消灯").still(),
        led::STATIC => LightEffect::new(0, "固定").color().still(),
        led::PULSE => LightEffect::new(0, "パルス").color(),
        led::CYCLE => LightEffect::new(0, "カラーサイクル"),
        led::BREATHE => LightEffect::new(0, "ブリージング").color(),
        _ => return None,
    })
}

// ====================================================================== 保存状態

/// ホストモードでアプリが管理する値 (電源断で消えるので再適用する)。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// true = ホストモード (このアプリが管理)。false = オンボードメモリモードに任せる。
    #[serde(default)]
    pub host_mode: bool,
    #[serde(default)]
    pub stages: Vec<u32>,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub rate_hz: Option<i64>,
    /// ゾーン index → 点灯設定。
    #[serde(default)]
    pub lighting: BTreeMap<u8, LightingState>,
    /// 本体の DPI ボタンでステージを順送り (ソフトウェア処理)。
    #[serde(default)]
    pub dpi_button_cycle: bool,
}

fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("DeviceHub")
        .join("logitech")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ====================================================================== 発見した機能

#[derive(Clone, Debug)]
struct Zone {
    index: u8,
    location: u16,
    /// (effect index, effect id)
    effects: Vec<(u8, u16)>,
}

#[derive(Clone, Debug, Default)]
struct Features {
    /// feature ID → index
    idx: BTreeMap<u16, u8>,
    dpi: Option<(u32, u32, u32)>,
    rates_hz: Vec<i64>,
    zones: Vec<Zone>,
    dpi_cid: Option<u16>,
    name: Option<String>,
    fw: Option<String>,
}

impl Features {
    fn has(&self, id: u16) -> bool {
        self.idx.contains_key(&id)
    }
    fn battery(&self) -> bool {
        self.has(feat::BATTERY_VOLTAGE)
            || self.has(feat::BATTERY_STATUS)
            || self.has(feat::UNIFIED_BATTERY)
    }
}

// ====================================================================== ドライバ本体

type Watcher = Arc<dyn Fn(HubEvent) + Send + Sync>;

struct Inner {
    io: Mutex<hidapi::HidDevice>,
    path: CString,
    dev_idx: AtomicU8,
    sw: u8,
    wireless: bool,
    feats: Mutex<Option<Features>>,
    /// None = まだ一度も設定していない (オンボードのまま触らない)。
    saved: Mutex<Option<Saved>>,
    saved_mtime: Mutex<Option<SystemTime>>,
    info: Mutex<DeviceInfo>,
    events: Option<Watcher>,
    stop: AtomicBool,
    online: AtomicBool,
    need_reapply: AtomicBool,
}

pub struct LogiDriver {
    inner: Arc<Inner>,
}

impl Drop for LogiDriver {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
    }
}

/// ドライバを開く。`path` は通知受信用に同じコレクションをもう 1 本開くのに使う。
pub fn open(
    dev: hidapi::HidDevice,
    path: CString,
    info: DeviceInfo,
    events: Option<Watcher>,
) -> HubResult<LogiDriver> {
    dev.set_blocking_mode(false).ok();
    let wireless = info.transport != Transport::Wired;
    let inner = Arc::new(Inner {
        io: Mutex::new(dev),
        path,
        dev_idx: AtomicU8::new(if wireless { 1 } else { DEV_WIRED }),
        sw: (std::process::id() % 15 + 1) as u8,
        wireless,
        feats: Mutex::new(None),
        saved: Mutex::new(None),
        saved_mtime: Mutex::new(None),
        info: Mutex::new(info),
        events,
        stop: AtomicBool::new(false),
        online: AtomicBool::new(false),
        need_reapply: AtomicBool::new(false),
    });
    // 開いた時点で起きていれば機能表を取る (寝ていれば監視スレッドが後で取る)。
    if inner.discover().is_ok() {
        inner.online.store(true, Ordering::Relaxed);
        inner.load_saved();
    } else {
        inner.lock_info().asleep = wireless;
    }
    spawn_listener(inner.clone());
    spawn_monitor(inner.clone());
    Ok(LogiDriver { inner })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn proto_err(e: HidppErr) -> HubError {
    match e {
        HidppErr::V1(c) => HubError::Protocol(format!(
            "マウスが応答しません (スリープ中/未接続? HID++1.0 err {c:#04x})"
        )),
        HidppErr::V2(5) => HubError::Protocol("デバイスが変更を拒否しました (NotAllowed)".into()),
        HidppErr::V2(c) => HubError::Protocol(format!("HID++ エラー {c:#04x}")),
    }
}

impl Inner {
    fn lock_info(&self) -> std::sync::MutexGuard<'_, DeviceInfo> {
        lock(&self.info)
    }

    /// 要求 1 件の往復。
    fn request(&self, feat_idx: u8, func: u8, params: &[u8]) -> HubResult<[u8; 16]> {
        let dev = self.dev_idx.load(Ordering::Relaxed);
        self.request_to(dev, feat_idx, func, params)
    }

    fn request_to(&self, dev: u8, feat_idx: u8, func: u8, params: &[u8]) -> HubResult<[u8; 16]> {
        let io = lock(&self.io);
        // 古い入力 (通知・他プロセス宛の応答) を捨ててから送る。
        let mut buf = [0u8; 64];
        while matches!(io.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
        let frame = build_long(dev, feat_idx, func, self.sw, params);
        io.write(&frame)
            .map_err(|e| HubError::Io(format!("hid++ write: {e}")))?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(HubError::Protocol(
                    "マウスが応答しません (スリープ中？マウスを動かして起こしてください)".into(),
                ));
            }
            let n = io
                .read_timeout(&mut buf, left.as_millis().max(1) as i32)
                .map_err(|e| HubError::Io(format!("hid++ read: {e}")))?;
            if n == 0 {
                continue;
            }
            if let Some(r) = match_reply(&buf[..n], dev, feat_idx, func, self.sw) {
                return r.map_err(proto_err);
            }
        }
    }

    /// Root.GetFeature(id) → index (0 = 無し)。
    fn get_feature_index(&self, dev: u8, id: u16) -> HubResult<u8> {
        let b = id.to_be_bytes();
        let p = self.request_to(dev, 0, 0, &[b[0], b[1]])?;
        Ok(p[0])
    }

    fn fidx(&self, id: u16) -> HubResult<u8> {
        lock(&self.feats)
            .as_ref()
            .and_then(|f| f.idx.get(&id).copied())
            .ok_or(HubError::Unsupported("feature"))
    }

    fn call(&self, id: u16, func: u8, params: &[u8]) -> HubResult<[u8; 16]> {
        let i = self.fidx(id)?;
        self.request(i, func, params)
    }

    /// Root.Ping。応答があればプロトコル版 (major) を返す。
    fn ping(&self, dev: u8) -> HubResult<u8> {
        let p = self.request_to(dev, 0, 1, &[0, 0, 0x5A])?;
        Ok(p[0])
    }

    /// ペア先 index を探し、機能表と詳細を読む。
    fn discover(&self) -> HubResult<()> {
        let dev = if self.wireless {
            let cur = self.dev_idx.load(Ordering::Relaxed);
            let order = std::iter::once(cur).chain((1..=6).filter(move |d| *d != cur));
            let mut found = None;
            for d in order {
                if matches!(self.ping(d), Ok(v) if v >= 2) {
                    found = Some(d);
                    break;
                }
            }
            found.ok_or_else(|| HubError::Protocol("ペア済みのマウスが応答しません".into()))?
        } else {
            self.ping(DEV_WIRED)?;
            DEV_WIRED
        };
        self.dev_idx.store(dev, Ordering::Relaxed);

        let mut idx: BTreeMap<u16, u8> = BTreeMap::new();
        for id in [
            feat::FW_VERSION,
            feat::DEVICE_NAME,
            feat::BATTERY_STATUS,
            feat::BATTERY_VOLTAGE,
            feat::UNIFIED_BATTERY,
            feat::REPROG_CONTROLS_V4,
            feat::WIRELESS_STATUS,
            feat::ADJUSTABLE_DPI,
            feat::REPORT_RATE,
            feat::COLOR_LED_EFFECTS,
            feat::ONBOARD_PROFILES,
        ] {
            if let Ok(i) = self.get_feature_index(dev, id) {
                if i != 0 {
                    idx.insert(id, i);
                }
            }
        }
        let call = |id: u16, func: u8, params: &[u8]| -> HubResult<[u8; 16]> {
            let i = *idx.get(&id).ok_or(HubError::Unsupported("feature"))?;
            self.request(i, func, params)
        };
        let mut f = Features {
            idx: idx.clone(),
            ..Features::default()
        };

        // 名前 (DeviceName: getCount → getName(offset) を 16B ずつ)。
        if let Ok(p) = call(feat::DEVICE_NAME, 0, &[]) {
            let len = p[0] as usize;
            let mut name = Vec::new();
            while name.len() < len {
                let Ok(c) = call(feat::DEVICE_NAME, 1, &[name.len() as u8]) else {
                    break;
                };
                let take = (len - name.len()).min(16);
                name.extend_from_slice(&c[..take]);
            }
            let s = String::from_utf8_lossy(&name)
                .trim_matches('\0')
                .trim()
                .to_string();
            if !s.is_empty() {
                f.name = Some(s);
            }
        }
        // ファームウェア (エンティティのうち type 0 = 主アプリ)。
        if let Ok(p) = call(feat::FW_VERSION, 0, &[]) {
            for e in 0..p[0].min(8) {
                if let Ok(fw) = call(feat::FW_VERSION, 1, &[e]) {
                    if fw[0] & 0x0F == 0 {
                        f.fw = format_fw(&fw);
                        break;
                    }
                }
            }
        }
        // DPI 範囲 (センサー 0)。
        if let Ok(p) = call(feat::ADJUSTABLE_DPI, 1, &[0]) {
            f.dpi = parse_dpi_list(&p[1..]);
        }
        // レポートレート一覧。
        if let Ok(p) = call(feat::REPORT_RATE, 0, &[]) {
            f.rates_hz = parse_rate_list(p[0]);
        }
        // LED ゾーンとエフェクト。
        if let Ok(p) = call(feat::COLOR_LED_EFFECTS, 0, &[]) {
            for z in 0..p[0].min(4) {
                let Ok(zi) = call(feat::COLOR_LED_EFFECTS, 1, &[z]) else {
                    continue;
                };
                let location = u16::from_be_bytes([zi[1], zi[2]]);
                let count = zi[3].min(16);
                let mut effects = vec![];
                for e in 0..count {
                    if let Ok(ei) = call(feat::COLOR_LED_EFFECTS, 2, &[z, e]) {
                        effects.push((e, u16::from_be_bytes([ei[2], ei[3]])));
                    }
                }
                f.zones.push(Zone {
                    index: z,
                    location,
                    effects,
                });
            }
        }
        // DPI ボタン (一時ダイバート可能なもの)。
        if let Ok(p) = call(feat::REPROG_CONTROLS_V4, 0, &[]) {
            for i in 0..p[0].min(32) {
                if let Ok(c) = call(feat::REPROG_CONTROLS_V4, 1, &[i]) {
                    let cid = u16::from_be_bytes([c[0], c[1]]);
                    let divertable = c[4] & 0x20 != 0;
                    if divertable && DPI_CIDS.contains(&cid) {
                        f.dpi_cid = Some(cid);
                        break;
                    }
                }
            }
        }

        {
            let mut info = self.lock_info();
            if let Some(n) = &f.name {
                info.name = n.clone();
            }
            info.firmware = f.fw.clone();
            info.asleep = false;
        }
        *lock(&self.feats) = Some(f);
        Ok(())
    }

    fn saved_path(&self) -> PathBuf {
        let key = {
            let info = self.lock_info();
            let k = sanitize(&info.name);
            if k.is_empty() {
                sanitize(&info.id.0)
            } else {
                k
            }
        };
        data_dir().join(format!("{key}.json"))
    }

    fn load_saved(&self) {
        let p = self.saved_path();
        let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let s = std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok());
        *lock(&self.saved) = s;
        *lock(&self.saved_mtime) = mtime;
    }

    fn store_saved(&self, s: &Saved) {
        let p = self.saved_path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        if let Ok(json) = serde_json::to_vec_pretty(s) {
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                std::fs::rename(&tmp, &p).ok();
            }
        }
        *lock(&self.saved_mtime) = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        *lock(&self.saved) = Some(s.clone());
    }

    /// 他プロセス (GUI ↔ エージェント) が保存ファイルを書き換えたら読み直す。
    fn saved_changed_on_disk(&self) -> bool {
        let p = self.saved_path();
        let now = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let prev = *lock(&self.saved_mtime);
        if now.is_some() && now != prev {
            self.load_saved();
            return true;
        }
        false
    }

    // ---------------------------------------------------------------- 個別の操作

    fn onboard_mode(&self) -> HubResult<u8> {
        Ok(self.call(feat::ONBOARD_PROFILES, 2, &[])?[0])
    }

    /// 1 = オンボード、2 = ホスト。
    fn set_onboard_mode(&self, mode: u8) -> HubResult<()> {
        self.call(feat::ONBOARD_PROFILES, 1, &[mode]).map(|_| ())
    }

    fn current_dpi(&self) -> HubResult<u32> {
        let p = self.call(feat::ADJUSTABLE_DPI, 2, &[0])?;
        Ok(u16::from_be_bytes([p[1], p[2]]) as u32)
    }

    fn set_dpi(&self, dpi: u32) -> HubResult<()> {
        let (lo, hi, step) = lock(&self.feats)
            .as_ref()
            .and_then(|f| f.dpi)
            .unwrap_or((100, 25600, 50));
        let v = (dpi.clamp(lo, hi) / step * step).clamp(lo, hi) as u16;
        let b = v.to_be_bytes();
        self.call(feat::ADJUSTABLE_DPI, 3, &[0, b[0], b[1]])
            .map(|_| ())
    }

    fn current_rate_hz(&self) -> HubResult<i64> {
        let ms = self.call(feat::REPORT_RATE, 1, &[])?[0].max(1);
        Ok(1000 / ms as i64)
    }

    fn set_rate_hz(&self, hz: i64) -> HubResult<()> {
        let ms = (1000 / hz.clamp(125, 1000)) as u8;
        self.call(feat::REPORT_RATE, 2, &[ms]).map(|_| ())
    }

    fn set_led(&self, zone: u8, st: &LightingState) -> HubResult<()> {
        let effect_id = lock(&self.feats)
            .as_ref()
            .and_then(|f| f.zones.iter().find(|z| z.index == zone).cloned())
            .and_then(|z| z.effects.iter().find(|(i, _)| *i == st.effect).map(|e| e.1))
            .ok_or_else(|| HubError::Invalid("LED エフェクト".into()))?;
        let mut params = vec![zone, st.effect];
        params.extend_from_slice(&led_params(effect_id, st));
        params.push(0x01); // 不揮発に保存
        self.call(feat::COLOR_LED_EFFECTS, 3, &params).map(|_| ())
    }

    fn set_divert(&self, on: bool) -> HubResult<()> {
        let cid = lock(&self.feats)
            .as_ref()
            .and_then(|f| f.dpi_cid)
            .ok_or(HubError::Unsupported("dpi button"))?;
        let b = cid.to_be_bytes();
        // flags: bit0 divert, bit1 divert 値が有効。
        let flags = if on { 0x03 } else { 0x02 };
        self.call(feat::REPROG_CONTROLS_V4, 3, &[b[0], b[1], flags, 0, 0])
            .map(|_| ())
    }

    fn battery(&self) -> HubResult<Battery> {
        let f = lock(&self.feats).clone();
        let f = f.ok_or(HubError::Unsupported("battery"))?;
        if f.has(feat::UNIFIED_BATTERY) {
            let p = self.call(feat::UNIFIED_BATTERY, 1, &[])?;
            return Ok(Battery {
                percent: Some(p[0].min(100)),
                charging: matches!(p[2], 1 | 2),
            });
        }
        if f.has(feat::BATTERY_VOLTAGE) {
            let p = self.call(feat::BATTERY_VOLTAGE, 0, &[])?;
            return Ok(decode_battery_voltage(&p));
        }
        if f.has(feat::BATTERY_STATUS) {
            let p = self.call(feat::BATTERY_STATUS, 0, &[])?;
            return Ok(Battery {
                percent: Some(p[0].min(100)),
                charging: matches!(p[2], 1..=3),
            });
        }
        Err(HubError::Unsupported("battery"))
    }

    // ---------------------------------------------------------------- ホストモード管理

    /// 初めて設定を変えるとき: ホストモードへ切り替え、今の値を保存の初期値にする。
    fn ensure_host(&self) -> HubResult<Saved> {
        let has_onboard = lock(&self.feats)
            .as_ref()
            .is_some_and(|f| f.has(feat::ONBOARD_PROFILES));
        let mut s = lock(&self.saved).clone().unwrap_or_default();
        let fresh = lock(&self.saved).is_none();
        if fresh {
            // 切替前に今の値を控える (オンボードプロファイルの値)。
            if let Ok(d) = self.current_dpi() {
                s.stages = vec![d];
                s.active = 0;
            }
            s.rate_hz = self.current_rate_hz().ok();
        }
        let was_host = s.host_mode;
        s.host_mode = true;
        if has_onboard && (!was_host || self.onboard_mode().ok() != Some(2)) {
            self.set_onboard_mode(2)?;
            // 切替で既定値に戻るので、控えた値を入れ直す。
            self.reapply(&s);
        }
        Ok(s)
    }

    /// 保存値をすべて書き込む。個々の失敗は続行 (ログだけ)。
    fn reapply(&self, s: &Saved) {
        let f = lock(&self.feats).clone().unwrap_or_default();
        if !s.host_mode {
            if f.has(feat::ONBOARD_PROFILES) {
                log_err("onboard", self.set_onboard_mode(1));
            }
            return;
        }
        if f.has(feat::ONBOARD_PROFILES) && self.onboard_mode().ok() != Some(2) {
            log_err("host mode", self.set_onboard_mode(2));
        }
        if let Some(hz) = s.rate_hz {
            if f.has(feat::REPORT_RATE) {
                log_err("rate", self.set_rate_hz(hz));
            }
        }
        if let Some(d) = s.stages.get(s.active).or(s.stages.first()) {
            log_err("dpi", self.set_dpi(*d));
        }
        for (zone, st) in &s.lighting {
            log_err("led", self.set_led(*zone, st));
        }
        if s.dpi_button_cycle && f.dpi_cid.is_some() {
            log_err("divert", self.set_divert(true));
        }
        crate::agent_log(&format!(
            "{}: 設定を再適用しました (DPI {:?}, {:?}Hz)",
            self.lock_info().name,
            s.stages.get(s.active),
            s.rate_hz
        ));
    }

    /// 本体の DPI ボタンで次のステージへ。
    fn cycle_stage(&self) {
        let Some(mut s) = lock(&self.saved).clone() else {
            return;
        };
        if s.stages.is_empty() {
            return;
        }
        s.active = (s.active + 1) % s.stages.len();
        if self.set_dpi(s.stages[s.active]).is_ok() {
            self.store_saved(&s);
            self.emit_state();
        }
    }

    fn emit_state(&self) {
        if let Some(ev) = &self.events {
            ev(HubEvent::StateChanged(self.lock_info().id.clone()));
        }
    }

    fn emit_info(&self) {
        if let Some(ev) = &self.events {
            let i = self.lock_info().clone();
            ev(HubEvent::Info(i.id.clone(), Box::new(i)));
        }
    }

    /// 生存確認。起きていれば true。
    fn heartbeat(&self) -> bool {
        if lock(&self.feats).is_none() {
            return self.discover().is_ok();
        }
        let dev = self.dev_idx.load(Ordering::Relaxed);
        self.ping(dev).is_ok()
    }
}

fn log_err(what: &str, r: HubResult<()>) {
    if let Err(e) = r {
        crate::agent_log(&format!("再適用 {what} 失敗: {e}"));
    }
}

/// 接続・電源投入・スリープ復帰を見張り、オーナーなら保存値を再適用する。
fn spawn_monitor(inner: Arc<Inner>) {
    std::thread::spawn(move || {
        let mut first = true;
        while !inner.stop.load(Ordering::Relaxed) {
            let had_feats = lock(&inner.feats).is_some();
            let alive = inner.heartbeat();
            let was = inner.online.swap(alive, Ordering::Relaxed);
            if alive && !had_feats {
                // 寝ていたマウスが起きて初めて機能表が取れた — caps が変わる。
                inner.load_saved();
                inner.emit_state();
            }
            if alive != was {
                inner.lock_info().asleep = !alive && inner.wireless;
                inner.emit_info();
            }
            if alive && owner::is_owner() {
                let saved = lock(&inner.saved).clone();
                if let Some(s) = saved {
                    // 電源を入れ直すとオンボードモードへ戻る — それも再接続とみなす。
                    let reverted = s.host_mode
                        && lock(&inner.feats)
                            .as_ref()
                            .is_some_and(|f| f.has(feat::ONBOARD_PROFILES))
                        && inner.onboard_mode().ok() == Some(1);
                    let wake = !was || first;
                    if wake || reverted || inner.need_reapply.swap(false, Ordering::Relaxed) {
                        inner.reapply(&s);
                    }
                }
                first = false;
            }
            if inner.saved_changed_on_disk() {
                inner.emit_state();
            }
            std::thread::sleep(MONITOR_PERIOD);
        }
    });
}

/// 同じコレクションをもう 1 本開いて、デバイス発の通知 (swId=0) を読む。
fn spawn_listener(inner: Arc<Inner>) {
    std::thread::spawn(move || {
        let Ok(api) = hidapi::HidApi::new() else {
            return;
        };
        let Ok(dev) = api.open_path(&inner.path) else {
            return;
        };
        let mut buf = [0u8; 64];
        let mut pressed: Vec<u16> = vec![];
        while !inner.stop.load(Ordering::Relaxed) {
            let n = match dev.read_timeout(&mut buf, 250) {
                Ok(n) if n >= 5 => n,
                Ok(_) => continue,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let r = &buf[..n];
            let my_dev = inner.dev_idx.load(Ordering::Relaxed);
            // レシーバーの接続通知 (short 0x41)。flags bit6 = リンク無し。
            if r[0] == REPORT_SHORT && r[2] == 0x41 {
                if r[4] & 0x40 == 0 {
                    inner.need_reapply.store(true, Ordering::Relaxed);
                }
                continue;
            }
            if r[0] != REPORT_LONG || n < LONG_LEN || r[1] != my_dev || r[3] & 0x0F != 0 {
                continue;
            }
            let fi = r[2];
            let func = r[3] >> 4;
            let p = &r[4..];
            let (reprog, wstatus, batt_v, batt_u, batt_s) = {
                let f = lock(&inner.feats);
                let g = |id| f.as_ref().and_then(|f| f.idx.get(&id).copied());
                (
                    g(feat::REPROG_CONTROLS_V4),
                    g(feat::WIRELESS_STATUS),
                    g(feat::BATTERY_VOLTAGE),
                    g(feat::UNIFIED_BATTERY),
                    g(feat::BATTERY_STATUS),
                )
            };
            if Some(fi) == reprog && func == 0 {
                // divertedButtonsEvent: 押されている CID 最大 4 個。
                let now: Vec<u16> = p[..8]
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .filter(|c| *c != 0)
                    .collect();
                let dpi_cid = lock(&inner.feats).as_ref().and_then(|f| f.dpi_cid);
                let cycle = lock(&inner.saved)
                    .as_ref()
                    .is_some_and(|s| s.dpi_button_cycle);
                if let Some(cid) = dpi_cid {
                    if cycle && now.contains(&cid) && !pressed.contains(&cid) && owner::is_owner() {
                        inner.cycle_stage();
                    }
                }
                pressed = now;
            } else if Some(fi) == wstatus && func == 0 {
                // status 1 = 再設定が必要 (電源投入・再接続)。
                if p[0] == 1 {
                    inner.need_reapply.store(true, Ordering::Relaxed);
                }
            } else if Some(fi) == batt_v && func == 0 {
                if let Some(ev) = &inner.events {
                    ev(HubEvent::Battery(
                        inner.lock_info().id.clone(),
                        decode_battery_voltage(p),
                    ));
                }
            } else if (Some(fi) == batt_u && func == 0) || (Some(fi) == batt_s && func == 0) {
                if let Some(ev) = &inner.events {
                    let b = if Some(fi) == batt_u {
                        Battery {
                            percent: Some(p[0].min(100)),
                            charging: matches!(p[2], 1 | 2),
                        }
                    } else {
                        Battery {
                            percent: Some(p[0].min(100)),
                            charging: matches!(p[2], 1..=3),
                        }
                    };
                    ev(HubEvent::Battery(inner.lock_info().id.clone(), b));
                }
            }
        }
    });
}

// ====================================================================== 契約の実装

fn zone_name(location: u16, index: u8) -> String {
    match location {
        0x0001 => "本体".into(),
        0x0002 => "ロゴ".into(),
        _ => format!("ゾーン {}", index + 1),
    }
}

fn default_lighting(z: &Zone) -> LightingState {
    let effect = z
        .effects
        .iter()
        .find(|(_, id)| *id == led::STATIC)
        .or(z.effects.first())
        .map(|e| e.0)
        .unwrap_or(0);
    LightingState {
        on: true,
        effect,
        color: Rgb::hex(0x00B4FF),
        brightness: 100,
        speed: 5,
        direction: 0,
        per_key: BTreeMap::new(),
        zone_on: vec![],
        palette: vec![],
        palette_sel: 0,
        color_correction: None,
    }
}

impl DeviceDriver for LogiDriver {
    fn info(&self) -> DeviceInfo {
        let inner = &self.inner;
        if lock(&inner.feats).is_none() && inner.discover().is_ok() {
            inner.online.store(true, Ordering::Relaxed);
            inner.load_saved();
        }
        let mut i = inner.lock_info().clone();
        i.asleep = inner.wireless && !inner.online.load(Ordering::Relaxed);
        if let Ok(b) = inner.battery() {
            i.battery = Some(b);
        }
        i
    }

    fn caps(&self) -> Capabilities {
        use devhub_core::model::section::*;
        let f = lock(&self.inner.feats).clone().unwrap_or_default();
        let mut c = Capabilities::empty();
        let (lo, hi, step) = f.dpi.unwrap_or((100, 25600, 50));
        if f.has(feat::ADJUSTABLE_DPI) || f.idx.is_empty() {
            c.mouse = Some(MouseCaps {
                dpi_min: lo,
                dpi_max: hi,
                dpi_step: step,
                min_stages: 1,
                max_stages: 5,
                xy_split: false,
                stage_colors: false,
                stage_toggle: false,
                buttons: vec![],
                button_kinds: vec![],
                require_left_click: false,
                macros: None,
                live_buttons: vec![],
                adaptive_dpi: None,
            });
        }
        if f.has(feat::REPORT_RATE) && !f.rates_hz.is_empty() {
            c.settings.push(
                SettingSpec::choice(
                    "polling",
                    PERFORMANCE,
                    "ポーリングレート",
                    f.rates_hz
                        .iter()
                        .map(|hz| ChoiceOpt::new(*hz, format!("{hz} Hz")))
                        .collect(),
                )
                .help("変更するとホストモード (このアプリで管理) に切り替わります"),
            );
        }
        if f.dpi_cid.is_some() {
            c.settings.push(
                SettingSpec::toggle("dpi_button_cycle", PERFORMANCE, "DPI ボタンでステージ切替")
                    .help(
                        "ホストモードでは本体の DPI ボタンが効かないため、アプリ側で順送りします",
                    ),
            );
        }
        if f.has(feat::ONBOARD_PROFILES) {
            c.settings.push(
                SettingSpec::toggle("onboard_mode", SYSTEM, "オンボードメモリモード")
                    .help("ON: 本体に保存されたプロファイルで動作 (G HUB で保存した設定)。OFF: このアプリが設定を管理し、常駐エージェントが再接続時に再適用"),
            );
        }
        if crate::autostart::supported() {
            c.settings.push(
                SettingSpec::toggle("agent_autostart", SYSTEM, "常駐エージェント (ログオン時に起動)")
                    .help("ウィンドウを閉じてもバックグラウンドで電源投入・スリープ復帰時に設定を再適用します (G HUB の代わり)"),
            );
        }
        c.actions.push(
            ActionSpec::new("reapply", SYSTEM, "設定を今すぐ再適用")
                .help("保存されている DPI・ポーリング・LED をマウスへ書き込み直します"),
        );
        for z in &f.zones {
            let effects: Vec<LightEffect> = z
                .effects
                .iter()
                .filter_map(|(idx, id)| {
                    led_effect_label(*id).map(|mut e| {
                        e.id = *idx;
                        e
                    })
                })
                .collect();
            if effects.is_empty() {
                continue;
            }
            c.lighting.push(LightArea {
                id: z.index,
                name: zone_name(z.location, z.index),
                effects,
                brightness_max: 100,
                speed_max: 10,
                directions: vec![],
                switchable: false,
                zones: vec![],
                per_key: false,
                direct_drive: false,
                live_readback: false,
            });
        }
        c.battery = f.battery() || (f.idx.is_empty() && self.inner.wireless);
        c
    }

    fn read(&self) -> HubResult<DeviceState> {
        let inner = &self.inner;
        if lock(&inner.feats).is_none() {
            inner.discover()?;
            inner.load_saved();
        }
        let f = lock(&inner.feats).clone().unwrap_or_default();
        let saved = lock(&inner.saved).clone();
        let mut settings = BTreeMap::new();

        let onboard = if f.has(feat::ONBOARD_PROFILES) {
            inner.onboard_mode().ok().map(|m| m == 1)
        } else {
            None
        };
        if let Some(on) = onboard {
            settings.insert("onboard_mode".into(), SettingValue::Bool(on));
        }
        if f.has(feat::REPORT_RATE) {
            if let Ok(hz) = inner.current_rate_hz() {
                settings.insert("polling".into(), SettingValue::Choice(hz));
            }
        }
        if f.dpi_cid.is_some() {
            settings.insert(
                "dpi_button_cycle".into(),
                SettingValue::Bool(saved.as_ref().is_some_and(|s| s.dpi_button_cycle)),
            );
        }
        if crate::autostart::supported() {
            settings.insert(
                "agent_autostart".into(),
                SettingValue::Bool(crate::autostart::is_enabled()),
            );
        }

        let mouse = if f.has(feat::ADJUSTABLE_DPI) {
            let cur = inner.current_dpi().ok();
            let host = saved.as_ref().is_some_and(|s| s.host_mode) && onboard != Some(true);
            let (dpis, active) = match (&saved, host) {
                (Some(s), true) if !s.stages.is_empty() => {
                    (s.stages.clone(), s.active.min(s.stages.len() - 1))
                }
                // オンボードモード (または未設定): 実機の今の DPI だけを見せる。
                _ => (vec![cur.unwrap_or(800)], 0),
            };
            Some(MouseState {
                stages: dpis
                    .iter()
                    .map(|d| DpiStage {
                        x: *d,
                        y: None,
                        color: Rgb::default(),
                        enabled: true,
                    })
                    .collect(),
                active_stage: active,
                buttons: vec![],
                macros: vec![],
                adaptive: None,
            })
        } else {
            None
        };

        let mut lighting = BTreeMap::new();
        for z in &f.zones {
            let st = saved
                .as_ref()
                .and_then(|s| s.lighting.get(&z.index).cloned())
                .unwrap_or_else(|| default_lighting(z));
            lighting.insert(z.index, st);
        }

        Ok(DeviceState {
            profile: ProfileState::default(),
            settings,
            mouse,
            keyboard: None,
            lighting,
            peq: None,
        })
    }

    fn apply(&self, change: &Change) -> HubResult<()> {
        let inner = &self.inner;
        if lock(&inner.feats).is_none() {
            inner.discover()?;
            inner.load_saved();
        }
        match change {
            Change::Setting { id, value } => match id.as_str() {
                "onboard_mode" => {
                    let mut s = lock(&inner.saved).clone().unwrap_or_default();
                    if value.as_bool() {
                        inner.set_onboard_mode(1)?;
                        s.host_mode = false;
                        inner.store_saved(&s);
                    } else {
                        let s = inner.ensure_host()?;
                        inner.store_saved(&s);
                    }
                    Ok(())
                }
                "polling" => {
                    let mut s = inner.ensure_host()?;
                    let hz = value.as_choice();
                    inner.set_rate_hz(hz)?;
                    s.rate_hz = Some(hz);
                    inner.store_saved(&s);
                    Ok(())
                }
                "dpi_button_cycle" => {
                    let mut s = inner.ensure_host()?;
                    let on = value.as_bool();
                    inner.set_divert(on)?;
                    s.dpi_button_cycle = on;
                    inner.store_saved(&s);
                    Ok(())
                }
                "agent_autostart" => crate::autostart::set(value.as_bool()),
                _ => Err(HubError::Unsupported("setting")),
            },
            Change::DpiStages { stages, active } => {
                if stages.is_empty() {
                    return Err(HubError::Invalid("DPI ステージが空です".into()));
                }
                let mut s = inner.ensure_host()?;
                s.stages = stages.iter().map(|st| st.x).collect();
                s.active = (*active).min(s.stages.len() - 1);
                inner.set_dpi(s.stages[s.active])?;
                inner.store_saved(&s);
                Ok(())
            }
            Change::ActiveStage(a) => {
                let mut s = inner.ensure_host()?;
                let d = *s
                    .stages
                    .get(*a)
                    .ok_or_else(|| HubError::Invalid("DPI ステージ".into()))?;
                inner.set_dpi(d)?;
                s.active = *a;
                inner.store_saved(&s);
                Ok(())
            }
            Change::Lighting { area, state } => {
                let mut s = inner.ensure_host()?;
                inner.set_led(*area, state)?;
                s.lighting.insert(*area, state.clone());
                inner.store_saved(&s);
                Ok(())
            }
            _ => Err(HubError::Unsupported("change")),
        }
    }

    fn run_action(&self, id: &str, _file: Option<&std::path::Path>) -> HubResult<ActionOutcome> {
        match id {
            "reapply" => {
                let s = lock(&self.inner.saved)
                    .clone()
                    .ok_or_else(|| HubError::Invalid("まだ保存された設定がありません".into()))?;
                self.inner.reapply(&s);
                Ok(ActionOutcome {
                    message: Some("設定を再適用しました".into()),
                    reload: true,
                })
            }
            _ => Err(HubError::Unsupported("action")),
        }
    }

    fn battery(&self) -> HubResult<Battery> {
        self.inner.battery()
    }
}

// ====================================================================== プローブ (解析用)

/// 1 デバイス分の解析結果 (example `hidpp_probe` 用)。
pub struct ProbeReport {
    pub dev_idx: u8,
    pub protocol: u8,
    /// (feature ID, index, type flags, version)
    pub features: Vec<(u16, u8, u8, u8)>,
}

/// FeatureSet (0x0001) で全機能表を列挙する。
pub fn probe(dev: &hidapi::HidDevice, wireless: bool) -> HubResult<ProbeReport> {
    let sw = 0x0E;
    let rq = |d: u8, fi: u8, func: u8, params: &[u8]| -> HubResult<[u8; 16]> {
        let mut buf = [0u8; 64];
        while matches!(dev.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
        dev.write(&build_long(d, fi, func, sw, params))
            .map_err(|e| HubError::Io(e.to_string()))?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while Instant::now() < deadline {
            let n = dev
                .read_timeout(&mut buf, 100)
                .map_err(|e| HubError::Io(e.to_string()))?;
            if let Some(r) = match_reply(&buf[..n], d, fi, func, sw) {
                return r.map_err(proto_err);
            }
        }
        Err(HubError::Protocol("timeout".into()))
    };
    let cands: Vec<u8> = if wireless {
        (1..=6).collect()
    } else {
        vec![DEV_WIRED]
    };
    for d in cands {
        let Ok(p) = rq(d, 0, 1, &[0, 0, 0x5A]) else {
            continue;
        };
        let mut out = ProbeReport {
            dev_idx: d,
            protocol: p[0],
            features: vec![(0, 0, 0, 0)],
        };
        let fs = rq(d, 0, 0, &[0x00, 0x01])?;
        if fs[0] == 0 {
            return Ok(out);
        }
        let count = rq(d, fs[0], 0, &[])?[0];
        for i in 1..=count {
            if let Ok(e) = rq(d, fs[0], 1, &[i]) {
                out.features
                    .push((u16::from_be_bytes([e[0], e[1]]), i, e[2], e[3]));
            }
        }
        return Ok(out);
    }
    Err(HubError::Protocol("応答するデバイスがありません".into()))
}

// ====================================================================== テスト

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_frame_layout() {
        let f = build_long(0x01, 0x0A, 3, 0x0B, &[0x00, 0x03, 0x20]);
        assert_eq!(&f[..7], &[0x11, 0x01, 0x0A, 0x3B, 0x00, 0x03, 0x20]);
        assert_eq!(f.len(), 20);
    }

    #[test]
    fn reply_matching() {
        let mut ok = [0u8; 20];
        ok[..6].copy_from_slice(&[0x11, 0x01, 0x0A, 0x2B, 0x00, 0x03]);
        assert_eq!(
            match_reply(&ok, 1, 0x0A, 2, 0x0B).unwrap().unwrap()[1],
            0x03
        );
        // 別の swId (他プロセス) は無視
        assert!(match_reply(&ok, 1, 0x0A, 2, 0x0C).is_none());
        // 通知 (swId 0) も無視
        ok[3] = 0x20;
        assert!(match_reply(&ok, 1, 0x0A, 2, 0x0B).is_none());
        let mut err = [0u8; 20];
        err[..6].copy_from_slice(&[0x11, 0x01, 0xFF, 0x0A, 0x2B, 0x05]);
        assert_eq!(
            match_reply(&err, 1, 0x0A, 2, 0x0B),
            Some(Err(HidppErr::V2(5)))
        );
        let e1 = [0x10, 0x01, 0x8F, 0x0A, 0x2B, 0x09, 0x00];
        assert_eq!(
            match_reply(&e1, 1, 0x0A, 2, 0x0B),
            Some(Err(HidppErr::V1(9)))
        );
    }

    #[test]
    fn dpi_list_range_and_discrete() {
        // 100..25600 刻み 50 (PRO Wireless / HERO の公開値)
        let b = [0x00, 0x64, 0xE0, 0x32, 0x64, 0x00, 0x00, 0x00];
        assert_eq!(parse_dpi_list(&b), Some((100, 25600, 50)));
        // 離散 400/800/1600
        let b = [0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0, 0];
        assert_eq!(parse_dpi_list(&b), Some((400, 1600, 400)));
        assert_eq!(parse_dpi_list(&[0, 0]), None);
    }

    #[test]
    fn rate_list() {
        // bit0=1ms, bit1=2ms, bit3=4ms, bit7=8ms
        assert_eq!(parse_rate_list(0x8B), vec![1000, 500, 250, 125]);
    }

    #[test]
    fn battery_curve() {
        assert_eq!(voltage_to_percent(4200), 100);
        assert_eq!(voltage_to_percent(3811), 50);
        assert_eq!(voltage_to_percent(3400), 0);
        let mid = voltage_to_percent(3835);
        assert!((50..=60).contains(&mid));
        let b = decode_battery_voltage(&[0x0F, 0x1F, 0x80]);
        assert!(b.charging);
    }

    #[test]
    fn fw_format() {
        let p = [
            0x00, b'U', b'1', b' ', 0x39, 0x02, 0x00, 0x21, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(format_fw(&p).as_deref(), Some("U1 39.02.B0021"));
    }

    #[test]
    fn led_param_layouts() {
        let st = LightingState {
            on: true,
            effect: 1,
            color: Rgb::new(1, 2, 3),
            brightness: 80,
            speed: 10,
            direction: 0,
            per_key: BTreeMap::new(),
            zone_on: vec![],
            palette: vec![],
            palette_sel: 0,
            color_correction: None,
        };
        assert_eq!(&led_params(led::STATIC, &st)[..4], &[1, 2, 3, 0]);
        let b = led_params(led::BREATHE, &st);
        assert_eq!(&b[..7], &[1, 2, 3, 0x03, 0xE8, 0, 80]);
        let c = led_params(led::CYCLE, &st);
        assert_eq!(&c[5..8], &[0x03, 0xE8, 80]);
        assert_eq!(led_params(led::OFF, &st), [0u8; 10]);
    }

    #[test]
    fn saved_roundtrip() {
        let s = Saved {
            host_mode: true,
            stages: vec![400, 800, 1600],
            active: 1,
            rate_hz: Some(1000),
            lighting: BTreeMap::new(),
            dpi_button_cycle: true,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Saved>(&j).unwrap(), s);
        // 古い/欠けたファイルでも読める
        assert_eq!(
            serde_json::from_str::<Saved>("{}").unwrap(),
            Saved::default()
        );
    }

    #[test]
    fn sanitize_names() {
        assert_eq!(
            sanitize("PRO Wireless Gaming Mouse"),
            "pro-wireless-gaming-mouse"
        );
    }
}
