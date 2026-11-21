//! G HUB からの移行 — `scripts/ghub-export.py` が settings.db から作った
//! `ghub-migration.json` を読み、Logitech ドライバの保存状態へ変換する。
//!
//! 形式 (docs/LOGITECH_HIDPP.md §7):
//! ```json
//! { "format": "devhub-ghub-migration", "version": 1,
//!   "devices": [ { "prefix": "g502wireless",
//!                  "desktop": { "dpi_levels": [400,800], "active_dpi": 800, "report_rate_hz": 1000,
//!                               "lighting": [ { "zone": "logo", "effect": "cycle", "period_ms": 8000, "intensity": 100 } ] },
//!                  "apps": [ { "name": "Valorant", "exe": "", "settings": { … } } ] } ] }
//! ```
//! ここは HID に触らない純粋な変換だけ (テスト対象)。適用はドライバ側。

use crate::logitech::{led, period_to_speed};
use devhub_core::model::{LightingState, Rgb};
use serde::Deserialize;
use std::collections::BTreeMap;

pub const FORMAT: &str = "devhub-ghub-migration";

#[derive(Clone, Debug, Deserialize)]
pub struct Migration {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub devices: Vec<GDevice>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GDevice {
    /// G HUB 内部のデバイス接頭辞 ("g502wireless" / "g502x-lightspeed" …)。
    pub prefix: String,
    #[serde(default)]
    pub desktop: Option<GSettings>,
    #[serde(default)]
    pub apps: Vec<GApp>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GApp {
    pub name: String,
    #[serde(default)]
    pub exe: String,
    pub settings: GSettings,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct GSettings {
    #[serde(default)]
    pub dpi_levels: Vec<u32>,
    #[serde(default)]
    pub active_dpi: Option<u32>,
    #[serde(default)]
    pub default_dpi: Option<u32>,
    /// DPI シフト (スナイパー)。ホストモードでは未対応なので参照しない。
    #[serde(default)]
    pub shift_dpi: Option<u32>,
    #[serde(default)]
    pub report_rate_hz: Option<i64>,
    #[serde(default)]
    pub lighting: Vec<GLight>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GLight {
    /// "primary" / "logo" / "all"
    pub zone: String,
    /// "off" / "fixed" / "cycle" / "breathing" / "unsupported"
    pub effect: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub period_ms: Option<u32>,
    /// 0..100
    #[serde(default)]
    pub intensity: Option<u8>,
}

pub fn parse(bytes: &[u8]) -> Result<Migration, String> {
    let m: Migration =
        serde_json::from_slice(bytes).map_err(|e| format!("移行ファイルを読めません: {e}"))?;
    if m.format != FORMAT {
        return Err("ghub-export.py が出力したファイルではありません".into());
    }
    if m.version != 1 {
        return Err(format!("未対応の版です (version {})", m.version));
    }
    Ok(m)
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// G HUB の接頭辞と、マウスが DeviceName で名乗る名前が同じ機種か。
pub fn prefix_matches(prefix: &str, device_name: &str) -> bool {
    let p = norm(prefix);
    let n = norm(device_name);
    if n.is_empty() {
        return false;
    }
    if p == "g502wireless" {
        // G502 LIGHTSPEED (無印)。G502 X と取り違えない。
        return n.contains("g502") && !n.contains("g502x");
    }
    if p.starts_with("g502x") {
        return n.contains("g502x") && n.contains("plus") == p.contains("plus");
    }
    n.contains(&p) || p.contains(&n)
}

/// 移行ファイルからこのマウスの項目を選ぶ。
pub fn pick_device<'a>(m: &'a Migration, device_name: &str) -> Result<&'a GDevice, String> {
    let hits: Vec<&GDevice> = m
        .devices
        .iter()
        .filter(|d| prefix_matches(&d.prefix, device_name))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] if m.devices.len() == 1 => Ok(&m.devices[0]),
        _ => Err(format!(
            "「{device_name}」に対応する G HUB の設定を特定できません (ファイル内: {})",
            m.devices
                .iter()
                .map(|d| d.prefix.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// デバイスの LED ゾーン (index, location, [(effect index, effect id)])。
pub type ZoneDesc = (u8, u16, Vec<(u8, u16)>);

/// 変換結果 (保存状態の一部)。
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub stages: Vec<u32>,
    pub active: usize,
    pub rate_hz: Option<i64>,
    pub lighting: BTreeMap<u8, LightingState>,
    /// 再現できずに飛ばした項目 (トースト用)。
    pub skipped: Vec<String>,
}

fn parse_color(s: Option<&str>) -> Option<Rgb> {
    let h = s?.trim_start_matches('#');
    u32::from_str_radix(h.get(..6)?, 16).ok().map(Rgb::hex)
}

/// G HUB の 1 プロファイル分 → このマウスで使える値。
/// `dpi` = (min, max, step)、`rates` = 対応 Hz、`zones` = LED ゾーン。
pub fn convert(
    s: &GSettings,
    dpi: (u32, u32, u32),
    rates: &[i64],
    zones: &[ZoneDesc],
) -> Converted {
    let (lo, hi, step) = dpi;
    let step = step.max(1);
    let fit = |d: u32| ((d.clamp(lo, hi) + step / 2) / step * step).clamp(lo, hi);
    let mut skipped = vec![];

    let mut stages: Vec<u32> = vec![];
    for d in &s.dpi_levels {
        let v = fit(*d);
        if !stages.contains(&v) {
            stages.push(v);
        }
    }
    if stages.len() > 5 {
        skipped.push(format!("DPI ステージ {} 段目以降", 6));
        stages.truncate(5);
    }
    if stages.is_empty() {
        stages.push(fit(s.active_dpi.or(s.default_dpi).unwrap_or(800)));
    }
    let want = s.active_dpi.or(s.default_dpi).map(fit).unwrap_or(stages[0]);
    let active = stages
        .iter()
        .enumerate()
        .min_by_key(|(_, d)| d.abs_diff(want))
        .map(|(i, _)| i)
        .unwrap_or(0);
    if s.shift_dpi.is_some_and(|d| d != want) {
        skipped.push("DPI シフト".into());
    }

    let rate_hz = s.report_rate_hz.and_then(|hz| {
        // 対応外 (例: 2000Hz を 1000Hz 機で) は上限側の近い値へ。
        let r = rates
            .iter()
            .copied()
            .filter(|r| *r <= hz)
            .max()
            .or(rates.iter().copied().min());
        if r != Some(hz) {
            skipped.push(format!("ポーリング {hz}Hz"));
        }
        r
    });

    let mut lighting = BTreeMap::new();
    for (index, location, effects) in zones {
        let want_zone = match location {
            0x0001 => "primary",
            0x0002 => "logo",
            _ => "",
        };
        let Some(g) = s
            .lighting
            .iter()
            .find(|l| l.zone == want_zone)
            .or_else(|| s.lighting.iter().find(|l| l.zone == "all"))
        else {
            continue;
        };
        let id = match g.effect.as_str() {
            "off" => led::OFF,
            "fixed" => led::STATIC,
            "cycle" => led::CYCLE,
            "breathing" => led::BREATHE,
            _ => {
                skipped.push(format!("ライティング ({})", g.zone));
                continue;
            }
        };
        let Some(&(effect_idx, _)) = effects.iter().find(|(_, e)| *e == id) else {
            skipped.push(format!("ライティング ({} {})", g.zone, g.effect));
            continue;
        };
        lighting.insert(
            *index,
            LightingState {
                on: id != led::OFF,
                effect: effect_idx,
                color: parse_color(g.color.as_deref()).unwrap_or(Rgb::hex(0x00B4FF)),
                brightness: g.intensity.unwrap_or(100).clamp(1, 100),
                speed: g.period_ms.map(period_to_speed).unwrap_or(5),
                direction: 0,
                per_key: BTreeMap::new(),
                zone_on: vec![],
                palette: vec![],
                palette_sel: 0,
                color_correction: None,
            },
        );
    }
    Converted {
        stages,
        active,
        rate_hz,
        lighting,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実際の G HUB settings.db (G502 LIGHTSPEED / G502 X LIGHTSPEED) を
    /// ghub-export.py に通した出力の抜粋。
    const SAMPLE: &str = r#"{
      "format": "devhub-ghub-migration", "version": 1, "ghub_version": "x",
      "known_devices": [],
      "devices": [
        {"prefix": "g502wireless",
         "desktop": {"dpi_levels": [400, 800, 1600, 3200, 6400], "active_dpi": 800, "default_dpi": 800,
                     "shift_dpi": 1600, "report_rate_hz": 1000,
                     "lighting": [{"zone": "logo", "ghub_id": "OFF", "effect": "off"},
                                  {"zone": "primary", "ghub_id": "OFF", "effect": "off"}]},
         "apps": [{"name": "Genshin Impact", "exe": "",
                   "settings": {"dpi_levels": [400, 800, 1600, 3200, 6400], "active_dpi": 800,
                                "report_rate_hz": 1000,
                                "lighting": [{"zone": "logo", "effect": "cycle", "period_ms": 8000, "intensity": 100},
                                             {"zone": "primary", "effect": "cycle", "period_ms": 8000, "intensity": 100}]}}]},
        {"prefix": "g502x-lightspeed",
         "desktop": {"dpi_levels": [1600], "active_dpi": 1600, "report_rate_hz": 1000, "lighting": []},
         "apps": []}
      ]}"#;

    fn zones() -> Vec<ZoneDesc> {
        vec![
            (
                0,
                0x0001,
                vec![
                    (0, led::OFF),
                    (1, led::STATIC),
                    (2, led::CYCLE),
                    (3, led::BREATHE),
                ],
            ),
            (
                1,
                0x0002,
                vec![(0, led::OFF), (1, led::STATIC), (2, led::BREATHE)],
            ),
        ]
    }

    #[test]
    fn parse_and_pick() {
        let m = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.devices.len(), 2);
        let d = pick_device(&m, "G502 LIGHTSPEED Wireless Gaming Mouse").unwrap();
        assert_eq!(d.prefix, "g502wireless");
        let d = pick_device(&m, "G502 X LIGHTSPEED").unwrap();
        assert_eq!(d.prefix, "g502x-lightspeed");
        assert!(pick_device(&m, "PRO Wireless Gaming Mouse").is_err());
        assert!(parse(br#"{"format":"other","version":1}"#).is_err());
    }

    #[test]
    fn convert_desktop() {
        let m = parse(SAMPLE.as_bytes()).unwrap();
        let s = m.devices[0].desktop.clone().unwrap();
        let c = convert(&s, (100, 25600, 50), &[1000, 500, 250, 125], &zones());
        assert_eq!(c.stages, vec![400, 800, 1600, 3200, 6400]);
        assert_eq!(c.active, 1);
        assert_eq!(c.rate_hz, Some(1000));
        assert_eq!(c.lighting[&0].effect, 0);
        assert!(!c.lighting[&0].on);
        assert_eq!(c.lighting[&1].effect, 0);
        assert!(c.skipped.iter().any(|s| s.contains("シフト")));
    }

    #[test]
    fn convert_cycle_missing_on_logo() {
        let m = parse(SAMPLE.as_bytes()).unwrap();
        let s = m.devices[0].apps[0].settings.clone();
        let c = convert(&s, (100, 25600, 50), &[1000, 500], &zones());
        let p = &c.lighting[&0];
        assert_eq!(p.effect, 2);
        assert_eq!(p.brightness, 100);
        assert_eq!(p.speed, period_to_speed(8000));
        // ロゴ側はサイクルを持たない → 飛ばして報告
        assert!(!c.lighting.contains_key(&1));
        assert!(c.skipped.iter().any(|s| s.contains("logo")));
    }

    #[test]
    fn clamp_and_rates() {
        let s = GSettings {
            dpi_levels: vec![50, 820, 30000],
            active_dpi: Some(830),
            report_rate_hz: Some(2000),
            ..Default::default()
        };
        let c = convert(&s, (100, 25600, 50), &[1000, 500], &[]);
        assert_eq!(c.stages, vec![100, 800, 25600]);
        assert_eq!(c.active, 1);
        assert_eq!(c.rate_hz, Some(1000));
    }
}
