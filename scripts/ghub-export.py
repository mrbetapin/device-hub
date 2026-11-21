#!/usr/bin/env python3
"""G HUB の設定 (settings.db) から、Device HUB への移行用 JSON を取り出す。

    python3 scripts/ghub-export.py <settings.db> [-o ghub-migration.json]

settings.db は G HUB の設定 (SQLite)。Windows では
`%LOCALAPPDATA%\\LGHUB\\settings.db`。G HUB を終了してからコピーしたものを使うこと。

- DB は読み取り専用で開く (書き換えない)。
- `data` テーブルの JSON から、マウスごとに「デスクトップ」プロファイルと
  ゲーム (アプリ) ごとのプロファイルの DPI テーブル・レポートレート・
  本体ライティング (firmware lighting) だけを取り出す。
- シリアル番号・解析 ID・アプリのフルパスは出力しない (exe 名だけ)。

出力は Device HUB の Logitech ドライバの「G HUB の設定を取り込む」で読む
(形式は docs/LOGITECH_HIDPP.md §7)。標準ライブラリのみで動く。
"""

import argparse
import json
import ntpath
import sqlite3
import sys

DESKTOP = "APPLICATION_NAME_DESKTOP"

ZONES = {
    "ZONE_PRIMARY": "primary",
    "ZONE_BRANDING": "logo",
    "ZONE_ALL": "all",
}


def color_hex(c):
    """G HUB の色 ({hex} または {rgba: 0..1, 欠けた成分は 0}) → '#rrggbb'。"""
    if not isinstance(c, dict):
        return None
    if isinstance(c.get("hex"), str):
        h = c["hex"].lstrip("#")
        if len(h) >= 6:
            return "#" + h[:6].lower()
    rgba = c.get("rgba")
    if isinstance(rgba, dict):
        ch = [round(max(0.0, min(1.0, float(rgba.get(k, 0)))) * 255) for k in ("red", "green", "blue")]
        return "#%02x%02x%02x" % tuple(ch)
    return None


def intensity_pct(v):
    """G HUB の intensity (0..1 の割合と思われる。>1 なら % とみなす) → 0..100。"""
    if v is None:
        return None
    v = float(v)
    return int(round(v * 100)) if v <= 1 else int(round(min(v, 100)))


def lighting(fw):
    out = []
    for e in (fw or {}).get("effects", []):
        gid = e.get("id", "")
        item = {"zone": ZONES.get(e.get("zoneType"), e.get("zoneType")), "ghub_id": gid}
        params = next((v for k, v in e.items() if k.endswith("Params") and isinstance(v, dict)), {})
        if gid == "OFF":
            item["effect"] = "off"
        elif gid == "FIXED":
            item["effect"] = "fixed"
            item["color"] = color_hex(params.get("color"))
        elif gid == "CYCLE":
            item["effect"] = "cycle"
        elif gid.startswith("BREATH"):
            item["effect"] = "breathing"
            item["color"] = color_hex(params.get("color"))
        else:
            # G HUB のソフトウェア効果など — 本体の LED では再現できない。
            item["effect"] = "unsupported"
        if "periodInMs" in params:
            item["period_ms"] = int(params["periodInMs"])
        if "intensity" in params:
            item["intensity"] = intensity_pct(params["intensity"])
        out.append(item)
    return out


def mouse(ms):
    dt = (ms or {}).get("dpiTable") or {}
    rr = (ms or {}).get("reportRate") or {}
    return {
        "dpi_levels": [int(x) for x in dt.get("levels", [])],
        "active_dpi": dt.get("activeDpi"),
        "default_dpi": dt.get("defaultDpi"),
        "shift_dpi": dt.get("shiftDpi"),
        "report_rate_hz": rr.get("value"),
    }


def export(db_path):
    con = sqlite3.connect("file:%s?mode=ro" % db_path, uri=True)
    rows = con.execute("SELECT file FROM data ORDER BY _id DESC LIMIT 1").fetchall()
    if not rows:
        raise SystemExit("data テーブルが空です")
    j = json.loads(rows[0][0])

    cards = {c["id"]: c for c in j.get("cards", {}).get("cards", [])}
    apps = {a["applicationId"]: a for a in j.get("applications", {}).get("applications", [])}

    devices = {}
    for p in j.get("profiles", {}).get("profiles", []):
        if not p.get("activeForApplication", True):
            continue
        app = apps.get(p.get("applicationId"), {})
        app_name = app.get("name", "?")
        exe = ntpath.basename(app.get("applicationPath") or "")
        per_dev = {}
        for a in p.get("assignments", []):
            slot, card = a.get("slotId", ""), cards.get(a.get("cardId"))
            if not card:
                continue
            if slot.endswith("_mouse_settings") and card.get("attribute") == "MOUSE_SETTINGS":
                prefix = slot[: -len("_mouse_settings")]
                per_dev.setdefault(prefix, {}).update(mouse(card.get("mouseSettings")))
            elif slot.endswith("_lighting_setting_firmware"):
                prefix = slot[: -len("_lighting_setting_firmware")]
                per_dev.setdefault(prefix, {})["lighting"] = lighting(card.get("firmwareLightingSettings"))
        for prefix, s in per_dev.items():
            d = devices.setdefault(prefix, {"prefix": prefix, "desktop": None, "apps": []})
            if app_name == DESKTOP:
                d["desktop"] = s
            else:
                d["apps"].append({"name": app_name, "exe": exe, "settings": s})

    # マウス (DPI テーブルを持つもの) だけ。
    mice = [d for d in devices.values() if any("dpi_levels" in (x or {}) for x in [d["desktop"]] + [a["settings"] for a in d["apps"]])]
    known = [
        {"model_id": k.get("modelId"), "type": k.get("type")}
        for k in j.get("devices/known", {}).get("knownList", [])
    ]
    return {
        "format": "devhub-ghub-migration",
        "version": 1,
        "ghub_version": j.get("last_run_version"),
        "known_devices": known,
        "devices": sorted(mice, key=lambda d: d["prefix"]),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("db")
    ap.add_argument("-o", "--output", default="ghub-migration.json")
    a = ap.parse_args()
    out = export(a.db)
    with open(a.output, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)
    for d in out["devices"]:
        ds = d["desktop"] or {}
        print(
            "%-20s デスクトップ DPI %s (現在 %s) %sHz / ゲーム別 %d 件"
            % (d["prefix"], ds.get("dpi_levels"), ds.get("active_dpi"), ds.get("report_rate_hz"), len(d["apps"]))
        )
    print("→", a.output, file=sys.stderr)


if __name__ == "__main__":
    main()
