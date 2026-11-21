# Logitech G (HID++ 2.0) — PRO Wireless 対応と G HUB の置き換え

Device HUB で Logitech G HUB を使わずに PRO Wireless などを設定し、常駐エージェントで
設定を維持するためのメモ。実装は `crates/devhub-hid/src/logitech.rs`、常駐は `crates/devhub-agent`。

> **状態:** LIGHTSPEED レシーバー C547 経由の **G502 X LIGHTSPEED** (DPI・ポーリング・電池) と
> **G915 X LS TKL** (RgbEffects 0x8071 の照明・明るさ 0x8040・SW 制御の再適用) は実機確認済み。
> PRO Wireless・有線接続・他のレシーバー PID、オンボードメモリモード/常駐エージェントの長期動作は未検証。
> プロトコルは HID++ 2.0 の公開資料 (Solaar / libratbag の実装) と実機の観測に基づく。
> 新しい機種は下の「実機での確認手順」で確かめ、この文書と `registry.rs` の `verified` に反映すること。

## 1. 使い方

1. **G HUB を終了・アンインストールする** (常駐の `lghub_agent` がホストモードを取り合う)。
2. `device-hub.exe` と `devhub-agent.exe` を同じフォルダに置き、Device HUB を起動。
3. マウスを選ぶ → DPI / ポーリング / ライティングを変更すると、自動で **ホストモード** に入る。
4. 「システム」タブの **常駐エージェント (ログオン時に起動)** を ON にする。
   (コマンドなら `devhub-agent.exe --install`、解除は `--uninstall`。)
5. Device HUB のウィンドウを閉じても、エージェントが電源投入・スリープ復帰・
   レシーバー差し直しのたびに設定を書き戻す。ログは `%TEMP%\devhub_agent.log`。

本体のプロファイル (G HUB でオンボードメモリに保存したもの) に戻すときは
「オンボードメモリモード」を ON にする。

## 2. なぜ常駐が要るか

PRO Wireless は既定で **オンボードメモリモード** (本体のプロファイルで動作) で起動する。
PC から DPI・レポートレートを変えるには OnboardProfiles (0x8100) で **ホストモード** に
切り替える。ホストモードの値は RAM 上だけで、電源を切ると消えてオンボードモードに戻る。
G HUB はこれを常駐プロセスが毎回書き戻している。Device HUB も同じ方式:

- 変更は `%APPDATA%\DeviceHub\logitech\<デバイス名>.json` に保存 (有線/無線で共有)。
- 再適用は **オーナー** プロセスだけが行う。`%APPDATA%\DeviceHub\logitech\owner.lock` を
  共有禁止で開けたプロセスがオーナー (エージェント優先、居なければ GUI)。
  GUI とエージェントが二重に再適用・二重に DPI 順送りしないため。
- 監視は 1.5 秒周期の Ping + オンボードモードへの戻り検知 + 通知
  (WirelessDeviceStatus 0x1D4B「再設定が必要」/ レシーバーの接続通知 0x41)。

## 3. 接続とフレーム

| 経路 | VID:PID | コレクション | device index |
|---|---|---|---|
| LIGHTSPEED レシーバー | 046D:C539 (他 C53A/C53F/C547/C54D) | FF00:0002 (long) | ペア先 1..6 (PRO Wireless は 1) |
| 有線 PRO Wireless | 046D:C088 | FF00:0002 (long) | 0xFF |

Windows は 1 つの vendor IF を usage ごとに別コレクションへ分ける: short (report 0x10, 7B) は
`FF00:0001`、long (report 0x11, 20B) は `FF00:0002`。このドライバは long だけで要求・応答する
(HID++ 2.0 機器は long を常に受け付ける)。

```
要求/応答  [0x11, dev, featIdx, fn<<4 | swId, params × 16]
エラー     [0x11, dev, 0xFF,   featIdx, fn<<4 | swId, code, …]
通知       swId = 0 (デバイス発)
HID++1.0 エラー (レシーバー)  [0x10, dev, 0x8F, subId, addr, code, 0]  — 8/9 は未接続・スリープ
```

`swId` は 1..15 をプロセスごとに変えて (`pid % 15 + 1`)、GUI とエージェントが同じ
コレクションを開いていても他方の応答を取り違えないようにしている。

## 4. 使っている feature

feature の index は機種ごとに違うので、毎回 `Root.GetFeature(featureId)` (index 0, fn 0) で引く。

| ID | 名前 | 使う関数 | 用途 |
|---|---|---|---|
| 0x0000 | Root | fn0 GetFeature, fn1 Ping | index 解決・生存確認 |
| 0x0001 | FeatureSet | fn0 count, fn1 GetFeatureId | 解析ツールの全 feature 列挙 |
| 0x0003 | DeviceFwVersion | fn0 count, fn1 GetFwInfo | ファームウェア表示 (`U1 39.02.B0021` 形式) |
| 0x0005 | DeviceName | fn0 length, fn1 GetName(offset) | 表示名・保存ファイル名 |
| 0x1001 | BatteryVoltage | fn0 → `[mV hi, mV lo, flags]` | 電圧→% (放電曲線を補間)、flags bit7 = 充電中。0x1000 / 0x1004 にもフォールバック |
| 0x1B04 | ReprogControlsV4 | fn0 count, fn1 CidInfo, fn3 SetCidReporting | DPI ボタン (CID 0xFD 等) を一時ダイバートしてソフト順送り |
| 0x1D4B | WirelessDeviceStatus | 通知 fn0 `[status=1 …]` | 再接続 → 再適用 |
| 0x2201 | AdjustableDpi | fn1 DpiList, fn2 GetDpi, fn3 SetDpi `[sensor, dpi BE]` | DPI。リストの `0xE000|step` は範囲の刻み (PRO Wireless は 100–25600 / 50 の想定) |
| 0x8060 | AdjustableReportRate | fn0 list (bit n = n+1 ms), fn1 get, fn2 set(ms) | ポーリング 125/250/500/1000 Hz。**ホストモードでのみ変更可** |
| 0x8070 | ColorLedEffects | fn0 info, fn1 ZoneInfo, fn2 ZoneEffectInfo, fn3 SetZoneEffect | ロゴ LED。効果 0x00 消灯 / 0x01 固定 / 0x02 パルス / 0x03 サイクル / 0x0A ブリージング |
| 0x8071 | RgbEffects | fn0 getInfo, fn1 SetRgbClusterEffect, fn5 SW 制御 | キーボード (G915 X LS TKL で実機確認)。下記 |
| 0x8040 | BrightnessControl | fn0 info `[max BE16, steps, flags]`, fn1 get, fn2 set(BE16) | 照明全体の明るさ (G915 X: max 100) |
| 0x8081 | PerKeyLightingV2 | fn0 getInfo(0, page), fn1 個別, fn6 単色一括, fn7 frameEnd | キー単位の色 (解析ツールのみ、ドライバ未実装) |
| 0x8100 | OnboardProfiles | fn1 SetMode (1=オンボード, 2=ホスト), fn2 GetMode | モード切替 |

`SetZoneEffect` は `[zone, effectIdx, params × 10, persist]`。params の並び:

| 効果 | params |
|---|---|
| 固定 0x01 | R G B ramp |
| パルス 0x02 | R G B speed |
| サイクル 0x03 | 0 0 0 0 0 period(BE16) intensity |
| ブリージング 0x0A | R G B period(BE16) waveform intensity |

### RgbEffects (0x8071) — G915 X LS TKL での実測

- `getInfo(cluster, effect, 0)`。`0xFF` は「全体/クラスタ自身」。
  - 全体 `[FF, 00, クラスタ数, …]`、クラスタ `[c, 00, location BE16, エフェクト数]`
    (location 1 = 本体、2 = ロゴ)、エフェクト `[c, e, ID BE16, caps BE16, period BE16]`。
- **SW 制御 (fn5)**: `[01, 01]` で ON、`[01, 00]` で OFF。get (引数なし) は `[00, 状態, …]`
  (OFF 時 `00 00 06`、ON 時 `00 01 00`)。**OFF の間は fn1 が NotAllowed (0x05) で拒否される**。
  ドライバは「本体の照明設定を使う」OFF = SW 制御 ON として、マウスのホストモードと同じ扱いにする。
- `SetRgbClusterEffect` (fn1) は 0x8070 の SetZoneEffect と同じ `[cluster, effectIdx, params × 10, persist]`。
- 現在のエフェクトの読み出し関数は未確認 (fn14 は 0x07 InvalidFunction)。表示は保存値から。

| 効果 | params | 確認 |
|---|---|---|
| 消灯 0x00 | — | ○ |
| 固定 0x01 | R G B | ○ |
| ブリージング 0x0A | R G B period(BE16) waveform intensity | ○ (指定周期どおり) |
| サイクル 0x15 | 0 saturation 0 0 0 0 period(BE16) intensity | ○ |
| 0x16 | 不明 (全 0 で白点灯) | UI 非表示 |
| 0x17 | 全 0 でキー入力に波紋 | ○ |

PerKeyLightingV2 (0x8081): getInfo(0, page) は `[0, page, ビットマップ 14B]` (page × 112 + bit)。
G915 X は 118 ゾーン、210 はロゴ。fn6 `[R G B, zone × 13]`・fn1 `[zone R G B] × 4` の後に
fn7 frameEnd `[00]` で反映。キー入力リアクティブ系のエフェクトが残っていると上書きされる。

### OnboardProfiles (0x8100) のプロファイルとマクロ — G502 X LIGHTSPEED での実測

`hidpp_onboard` (読み取り・書き込み実験) で確認。G502 X: format 3、セクタ長 255、16 セクタ、ユーザー枠 5、ROM 2、ボタン 11。

| fn | 内容 |
|---|---|
| 0 | getInfo `[memory_model, profile_format, macro_format, 枠数, ROM 数, ボタン数, セクタ数, セクタ長 BE16, layout, various]` |
| 1 / 2 | setMode / getMode (1 = オンボード、2 = ホスト) |
| 3 / 4 | setCurrentProfile `[00, 枠 1..]` / get → `[00, 枠]` (ホストモード中は 0) |
| 5 | memoryRead `[sector BE16, offset BE16]` → 16B (末尾は `セクタ長-16` から) |
| 6 / 7 / 8 | memoryAddrWrite `[sector BE16, 0, 0, 長さ BE16]` → memoryWrite 16B × n → memoryWriteEnd |
| 11 / 12 | getCurrentDpiIndex / setCurrentDpiIndex (0 始まり) |

- セクタ 0x0000 = ユーザーのディレクトリ (`[sector BE16, 有効, ff]` × n、`ffff` 終端)、0x0001.. = 各枠、
  0x0100 = ROM ディレクトリ、0x0101.. = ROM プロファイル (CRC なし)。
  ユーザーセクタは末尾 2B に CRC-16/CCITT (init 0xFFFF) を BE で持つ。
- **オンボードへ切り替えても DPI 段は既定段 (プロファイルの byte 1) に戻らない** — 切替後に
  setCurrentDpiIndex で明示する。

プロファイル (ユーザーセクタ) の並び:

| offset | 内容 |
|---|---|
| 0x00 | レポート間隔 (ms) |
| 0x01 / 0x02 | 既定 DPI 段 / シフト時 DPI 段 |
| 0x03..0x0C | DPI × 5 (**LE16**) |
| 0x0D..0x0F | 色 |
| 0x1C / 0x1E | 省電力 / 電源オフまでの時間 |
| 0x20..0x5F | ボタン 4B × 16 |
| 0x60..0x9F | G-Shift 時のボタン 4B × 16 (ff ff ff ff = 未設定) |
| 0xA0..0xCF | 名前 (UTF-16LE) |
| 0xD0.. | LED |
| 末尾 2B | CRC |

ボタン割り当て 4B:

| 形式 | 意味 |
|---|---|
| `80 01 <マスク BE16>` | マウスボタン (1 左, 2 右, 4 中, 8 戻る, 10 進む)。**`80 01 00 00` = 無効** (実機確認) |
| `80 02 <修飾> <キー>` | キー (修飾は HID の bit: 01 Ctrl, 02 Shift, 04 Alt, 08 Win)。実機確認 |
| `80 03 <usage BE16>` | コンシューマ (音量など) |
| `90 <番号> 00 00` | 特殊: 01/02 チルト左右, 03/04 DPI 上げ/下げ, 05 DPI 循環, 06 既定 DPI, 07 DPI シフト, 08/09 プロファイル次/前, 0A プロファイル循環, 0B G-Shift, 0C 電池表示 |
| `00 <sector> 00 <offset>` | マクロ (offset はセクタ内のバイト位置)。実機確認 |

G502 X のボタン番号: #0 左, #1 右, #2 中, #3 戻る, #4 スナイパー, #5 進む, #6/#7 チルト, #8 G9, #9 G8 (DPI 上げ), #10 G7 (DPI 下げ)。

マクロ (各命令 3B、`00` だけ 1B。16B の区切りをまたがないように置く):

| 命令 | 意味 |
|---|---|
| `43 <修飾> <キー>` / `44 <修飾> <キー>` | キー押す / 離す (実機確認) |
| `41 <ボタン BE16>` / `42 <ボタン BE16>` | マウスボタン押す / 離す (実機確認) |
| `40 <ms BE16>` | 待つ (実機確認) |
| `45` / `46 <usage BE16>` | コンシューマ押す / 離す |
| `20` / `21` / `61` | ホイール / 横スクロール / カーソル移動 (誤って使うとカーソルが飛ぶ) |
| `60 <offset> <sector>` | ジャンプ |
| `00` | 1 ステップ待つ |
| `ff` | 終わり |

例 (セクタ 6 に置き、ボタンへ `00 06 00 00` / `00 06 00 10`):
`43 06 05 44 06 05 40 00 64 43 00 28 44 00 28 ff` = Alt+Shift+B → 100ms → Enter、
`41 00 08 42 00 08 40 00 32 41 00 08 42 00 08 ff` = 戻る → 50ms → 戻る。

### ドライバでの扱い (本体プロファイル編集)

0x8100 と AdjustableDpi を持ち、プロファイル形式が既知 (format 1–3、セクタ長 ≥ 0xD0) のマウスは
**本体プロファイルを直接編集する** (`logitech_onboard.rs` が符号化、`logitech.rs` の `ob_*` が I/O)。

- 常にオンボードモードで動かす。DPI 段・ポーリング・ボタン割り当て・マクロは使用中の枠のセクタに
  書き、書いたら枠を選び直して本体に読み込ませる (DPI 段は fn12 で戻す)。
  内容が同じなら書かない。常駐エージェントの再適用はオンボードモードを保つだけ。
- プロファイル切替 = 本体の枠 (1..5) の切替。無効な枠はディレクトリで有効にしてから選ぶ。
- マクロ: 枠 n = セクタ `6 + n` (最大 10 本、1 本 1 セクタ)。修飾キーは `43 <bit> 00`、
  通常キーは押している修飾を付けて `43 <修飾> <キー>`、離すときは `44 00 <キー>`。
  命令が 16B の区切りをまたぐときは `00` (NOOP) で詰める。
- UI に出せない割り当て (G-Shift・電池表示など) は「無効」と表示されるが、
  そのボタンを触らない限り本体の値はそのまま残る。
- 未対応: G-Shift 時の割り当て、プロファイル名、本体プロファイル内の LED、マクロの繰り返し実行。

## 5. 実機での確認手順 (プロトコル解析)

```sh
# 1) 046d の全コレクションと、応答したデバイスの全 feature 表
cargo run -p devhub-hid --example hidpp_probe

# 2) 生レポートを見る (ボタン・電源投入・スリープ復帰の通知の形を観察)
cargo run -p devhub-hid --example hidpp_probe -- --sniff
# 3) キーボード照明の書き込み実験 (0x8071 / 0x8040 / 0x8081)。コマンドは hidpp_rgb.rs 冒頭
cargo run -p devhub-hid --example hidpp_rgb -- info
```

確認すること:

- [ ] `FF00:0002` のコレクションが列挙され、c539 で device index 1 が Ping に応答する
- [ ] feature 表に 0x2201 / 0x8060 / 0x8070 / 0x8100 / 0x1001 がある (index を記録)
- [ ] DPI リストの値 (範囲・刻み) — ドライバの `parse_dpi_list` の想定と合うか
- [ ] ホストモードで DPI・ポーリング・LED を変えて反映されるか
- [ ] 電源を入れ直した後、エージェントが数秒以内に再適用するか (`devhub_agent.log`)
- [ ] 0x1B04 に DPI ボタンの CID があるか (無ければ「DPI ボタンでステージ切替」は出ない)
- [ ] `--sniff` で電源投入時に `11 01 xx 00 01 …` (0x1D4B の status=1) が来るか

**G HUB の通信を記録する場合**: G HUB を起動したまま `--sniff` すると、G HUB の要求に対する
応答 (swId ≠ 0) も見える。G HUB で 1 項目だけ変えて差分を見るのが速い (ADD_DEVICE.md §2.3 と同じ考え方)。
USBPcap + Wireshark なら要求側も取れる。

## 6. 未対応・今後

- ボタン割り当て・マクロ: 本体への保存は OnboardProfiles のプロファイル (セクタ読み書き + CRC) の
  解析が必要。現状はホストモード + ソフトの DPI 順送りのみ。
- 8K 機種の拡張レポートレート (0x8061)、ExtendedAdjustableDpi (0x2202)。
- G502 LIGHTSPEED (有線 C08D)・G502 X LIGHTSPEED (有線 C098)・POWERPLAY / 他の LIGHTSPEED 受信機は PID 登録のみ (同じ HID++ 2.0 なので動く想定)。

## 7. G HUB からの移行

G HUB の設定は `%LOCALAPPDATA%\LGHUB\settings.db` (SQLite)。`data` テーブルの 1 行に
設定全体が JSON で入っている。

```sh
# G HUB を終了してから settings.db をコピーし、移行用 JSON を作る (DB は読み取り専用で開く)
python3 scripts/ghub-export.py settings.db -o ghub-migration.json
```

Device HUB でマウスを選び、「システム」→ **G HUB の設定を取り込む** で `ghub-migration.json` を選ぶ。

- マウスの DeviceName と G HUB のデバイス接頭辞 (`g502wireless`, `g502x-lightspeed` …) を照合して項目を選ぶ。
- **デスクトップ** のプロファイル → 現在の設定 (ホストモード) として保存・適用 → 常駐エージェントが維持。
- **ゲーム別** のプロファイル → プリセット `G HUB - <ゲーム名>` (`%APPDATA%\DeviceHub\presets\<model>\`)。
  フォアグラウンド連動にするには、Device HUB の「プロセス連動」でルールを作る
  (G HUB の DB はゲームの exe 名を持たないことが多い — `exe` が空なら手で指定)。

### settings.db の JSON で使っている部分

| キー | 中身 |
|---|---|
| `profiles.profiles[]` | `applicationId`・`activeForApplication`・`assignments[] {slotId, cardId}` |
| `applications.applications[]` | `applicationId` → `name` (`APPLICATION_NAME_DESKTOP` = デスクトップ)・`applicationPath` |
| `cards.cards[]` (attribute `MOUSE_SETTINGS`) | `mouseSettings.dpiTable {levels, activeDpi, defaultDpi, shiftDpi}`・`reportRate.value` |
| `cards.cards[]` (attribute `FIRMWARE_LIGHTING_SETTINGS`) | `firmwareLightingSettings.effects[] {id: OFF/FIXED/CYCLE/…, zoneType: ZONE_PRIMARY/ZONE_BRANDING/ZONE_ALL, *Params}` |

slotId は `<デバイス接頭辞>_mouse_settings` / `<接頭辞>_lighting_setting_firmware`。
色は `{hex}` か `{rgba: 0..1}` (欠けた成分は 0)。`intensity` は 0..1 の割合と解釈している (要実機確認)。

再現しない (飛ばして報告する) もの: DPI シフト、6 段目以降の DPI、本体 LED に無い効果
(G HUB のソフトウェア効果 `SIGNATURE_*` など)、マクロ・ボタン割り当て。
