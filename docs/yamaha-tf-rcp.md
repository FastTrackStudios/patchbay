# Yamaha TF1 — Remote Control Protocol (RCP) reference for the patchbay adapter

Status: research compiled 2026-09-18 from public sources only. **Nothing in this
document has been checked against a live console.** A live TF1 (V4.55) exists on the
network, but the original research brief said not to contact a mixer. Any parameter
marked *unverified* needs a read-only `get` check before we depend on it
(see [§8 Verification plan](#8-verification-plan-read-only)).

TL;DR for adapter design:

* TF RCP is a **small, fixed parameter set** (108 `prminfo` entries + scenes + meters).
  It covers faders, on/mute, names, colors, icons, sends, pan, DCA masters, mute-group
  masters, matrix, scenes and meters.
* It does **not** cover head-amp gain, phantom power, phase, HPF, EQ, dynamics, DCA
  assign, mute-group assign, input patch or source select, output patch, or Dante.
  No public source documents a TF path for any of these. The TF datasheet also says
  "Dante Patch from Console: No".
* Settable over RCP on TF: **channel name, color, icon/category, fader, on, sends,
  pan, DCA master level/on/name/color, mute-group master on/name, scene recall/store**.
  **Not settable:** gain and grouping (DCA or mute-group *membership*).

---

## 0. Sources

| Tag | Source | What it gave us |
|---|---|---|
| [COMP] | bitfocus/companion-module-yamaha-rcp @ `7cf172d` (v3.6.0, MIT) — https://github.com/bitfocus/companion-module-yamaha-rcp | `TF Parameters-1.txt` (a raw `prminfo`/`scninfo`/`mtrinfo` dump from a TF, last changed 2024-01-16, commit `7aca92f`), `index.js`, `paramFuncs.js`, `variables.js`, `actions.js`, `rcpNames.json` |
| [BRE-DOCS] | BrenekH/yamaha-rcp-docs @ `6ca522d` — https://github.com/BrenekH/yamaha-rcp-docs | TF-focused notes: grammar, dB encoding, fader min −138.00 dB. Per-command pages are stubs. **No license**, so we cite facts only and copy no text. |
| [RUST] | `yamaha-rcp` crate 0.1.0 (2025-01-12, MIT per Cargo.toml) — https://docs.rs/yamaha-rcp, https://github.com/BrenekH/yamaha-rcp-rs | TF1-tested client, TF color enum, scene syntax |
| [YAM-PY] | Yamaha *Python Script Template V1.00* — https://usa.yamaha.com/files/download/other_assets/0/1266290/Python_Script_Template_V100.zip (`command_list.pdf`, `Readme_EN.pdf`, `recall_a.py`, `TFxQLab/*`) | Official: TF needs **V2.00 or later**; TF1 InCh index **0–31**; `ssrecall_ex scene_a/scene_b 0–99` |
| [YAM-QLAB] | Yamaha *QLab Setup Guide for CL/QL/TF* — https://download.yamaha.com/files/tcm:39-1286035 | Official examples: `set MIXER:Current/InCh/Fader/On`, `.../ToSt/Pan`, `.../Fader/Level` |
| [YAM-MTX] | Yamaha *MTX/MRX/XMV/EX RCP Spec V4.0.0 rev14* — https://usa.yamaha.com/files/download/other_assets/5/1343735/200330_mtx_mrx_xmv_ex_rcps_v400_rev14_en.pdf | The only official full grammar spec (same SCP family): escaping, `scpmode keepalive`, error codes, NOTIFY forms |
| [TF-FW] | Yamaha *TF firmware version information up to V4.55* — https://download.yamaha.com/files/tcm:39-1189926 | Matrix added in V2.50, mute groups 3–6 added in V4.00, NY64-D history |
| [TF-DS] | Yamaha *TF1 technical data sheet* — https://www.la-bs.com/objetsmultimedia/37025/FR/TF1_YAMAHA_dt.pdf | TF1 mixing capacity, Dante 32 in / 24 out, "Dante Patch from Console: No", 200 scenes |
| [TF-RM] | Yamaha *TF5/TF3/TF1 Reference Manual* — https://www.strumentimusicali.net/manuali/YAMAHA_TFSERIES_ENG.pdf | CH NAME screen (name/icon/category/color), network, MonitorMix PIN |
| [CHAT] | vk0eppel/Yamaha-RCP-Chataigne-Module (GPL-3.0) — https://github.com/vk0eppel/Yamaha-RCP-Chataigne-Module | Hardware-derived parser notes: trailing display-string token, keepalive, ERROR noise, scene NOTIFY. **No TF table.** |
| [CLQL-CHAT] | l-r-r/Yamaha-CLQL-Chataigne-Module (GPL-3.0) — https://github.com/l-r-r/Yamaha-CLQL-Chataigne-Module | `CLQLProtocol.txt`: OK/NOTIFY semantics |
| [DOMTC] | Dom-TC/Yamaha-TF-Control — https://github.com/Dom-TC/Yamaha-TF-Control | Real TF reply shape: `OK set MIXER:Current/MuteMaster/On 0 0 1 "ON"`; use of `DcaCh` alias |
| [ISSUES] | Companion issues #25, #42, #4 — https://github.com/bitfocus/companion-module-yamaha-rcp/issues | `InCh/ToMono/On` is TF-only; the TF "to ST" assign button has no RCP command |
| [FORUM] | CheckCheckOneTwo discourse — https://discourse.checkcheckonetwo.com/t/yamaha-rcp-module-for-companion/2232 and https://discourse.checkcheckonetwo.com/t/help-about-rcp-yamaha-tf5-command/3595 | "TF Editor does not support RCP", early off-by-one send bug in the Companion module (since fixed) |
| [QLAB-GRP] | QLab Google group "Controlling Yamaha TF consoles" — https://groups.google.com/g/qlab/c/jnAQAvgLCTU | netcat usage, and that every command must end in a newline |

Local clones (read-only research) are in
`/private/tmp/claude-501/-Volumes-build-disk-development-patchbay/f4b3ceb9-c643-4076-baeb-8efacbe12422/scratchpad/tf-research/`.

---

## 1. Transport and session

### 1.1 Transport
* **TCP port 49280**, console is the server. [YAM-PY `command.py`: "Port must be 49280"]
* Connect to the console's **NETWORK (LAN) port** IP, set in *SETUP → NETWORK* (DHCP or
  Static IP). Yamaha's default examples use `192.168.0.128`. [YAM-QLAB §5–6, TF-RM "NETWORK screen"]
  Do **not** use the NY64-D Dante port for control. The firmware notes also warn against
  putting the NETWORK port on a busy Dante network [TF-FW "Operational precaution"].
* Line-based **ASCII**. Every command and reply ends with **LF (0x0A)**. A bare LF is a legal
  heartbeat. [YAM-MTX §3.1; QLAB-GRP "the line break … is important"]. Accept and strip a
  trailing CR defensively [CHAT `stripEnds`].
* Tokens are separated by one or more spaces. String arguments are **double-quoted**.
  Inside quotes, **backslash escapes `"` and `\`** (`\"`, `\\`). [YAM-MTX §3.1; CHAT `quote()`]
* Character set: ASCII by default. The MTX spec defines `scpmode encoding utf8`
  [YAM-MTX 2-6]. Whether TF supports it is *unverified*, so send ASCII-only names.
* **Firmware:** TF V2.00 or later is required for RCP [YAM-PY Readme; YAM-QLAB §3]. Matrix
  paths need V2.50 or later [TF-FW V2.50-2]. Mute groups 3–6 need V4.00 or later [TF-FW V4.00]. The
  house TF1 reportedly runs V4.55 (identical console firmware to V4.50; V4.55 adds only
  Brooklyn3 NY64-D support) [TF-FW].
* **Max concurrent RCP connections on TF:** *not documented anywhere public.* MTX/MRX allow 8–9
  controllers [YAM-MTX §1.3]. The Rust crate defaults to a pool of **1** connection
  [RUST `connection_limit: 1`]. TF Editor, StageMix and MonitorMix use a **different
  proprietary protocol** ([FORUM] "The Editor does not support the RCP commands"), so they
  do not consume RCP slots as far as anyone knows. **Recommendation:** use exactly one
  persistent connection per console.

### 1.2 Client → console commands

| Command | Syntax | TF support | Notes / source |
|---|---|---|---|
| get | `get <address> <x> <y>` | yes | x, y are **0-based** integers. y is `0` for non-send params [YAM-QLAB, COMP] |
| set | `set <address> <x> <y> <value>` | yes | value is an int, or a quoted string for `string`/`binary` types [COMP `fmtCmd`] |
| Scene recall | `ssrecall_ex scene_a <n>` / `ssrecall_ex scene_b <n>` | yes | n = 0–99 [YAM-PY command_list] |
| Scene store | `ssupdate_ex scene_a <n>` | yes (per COMP) | **Overwrites without confirmation** [COMP README 3.4.0]. Companion maps TF "Store" → `ssupdate_ex scene_x n` |
| Current scene | `sscurrent_ex scene_a` / `sscurrent_ex scene_b` | yes | Reply for the active bank carries the number. The inactive bank answers **ERROR**, which is the only way to tell the active bank [COMP `variables.js` comment] |
| Scene info | `ssinfo_ex scene_a <n>` | yes | Reply fields per COMP: `OK ssinfo_ex <bank> <n> <?> "<name>" "<comment>" <type>`. Exact TF field order *unverified* |
| Scene inc/dec | `event MIXER:Lib/Scene/RecallInc` / `…/RecallDec` | yes (per COMP) | No bank argument on TF [COMP `fmtCmd` index 1010–1011] |
| devinfo | `devinfo productname` \| `devicename` \| `version` \| `deviceid` \| `serialno` \| `protocolver` \| `paramsetver` | productname and devicename used by COMP on TF. Others are from YAM-MTX and *unverified on TF* | Reply `OK devinfo productname "TF1"` |
| devstatus | `devstatus runmode` | yes (COMP keepalive ping) | `devstatus error` is **skipped for TF** by COMP (unsupported) [COMP `variables.js`] |
| scpmode | `scpmode keepalive <ms>` | used by COMP on TF | ms > 1000. The console drops the connection if nothing (not even LF) arrives within the window. Reply `OK scpmode keepalive <ms>` [YAM-MTX 2-9]. CHAT says the desk treats the value as roughly 2× the ping interval. `scpmode sstype "text"` is DM7/Rivage only. `valuetype normalized` / `resolution` are MTX features, *unverified on TF* |
| prminfo | `prminfo <index>` | yes — `TF Parameters-1.txt` **is** a TF `prminfo` dump | Enumerate index 0,1,2… until ERROR for runtime discovery. Reply: `OK prminfo <i> "<addr>" <X> <Y> <min> <max> <default> "<unit>" <type> <ui> <rw> <scale>` |
| scninfo / mtrinfo | `scninfo <1000..>` / `mtrinfo <2000..>` | yes (same dump) | Scene and meter descriptor tables |
| prmlist | — | **no evidence** it exists on TF | Use `prminfo` enumeration instead |
| Meters | `mtrstart <address>/<pickoff> <interval_ms>` | yes (COMP metering) | e.g. `mtrstart MIXER:Current/InCh/PreFader 100`. The stream apparently expires: COMP re-issues it every 10 s. Replies are `NOTIFY mtr <address> mtr <hex> <hex> …`, 0..127, where **dB = value − 126** [COMP `index.js`, `variables.js`]. The interval range 40–1000 ms is COMP's UI limit |

### 1.3 Console → client

| Prefix | Meaning |
|---|---|
| `OK get <addr> <x> <y> <value> ["<display>"]` | Answer to a get |
| `OK set <addr> <x> <y> <value> ["<display>"]` | Echo of **your** set with the value actually applied (it may be clamped) |
| `OKm …` | Seen by several parsers (COMP, CHAT). The meaning is not officially documented; probably "OK, value modified". Treat it exactly like `OK` |
| `NOTIFY set <addr> <x> <y> <value> ["<display>"]` | **Unsolicited change** made elsewhere: console surface, TF Editor, StageMix, MonitorMix, or another RCP client. The originating client gets `OK set`, not NOTIFY [CLQL-CHAT protocol notes]. This is our subscription mechanism: **there is no subscribe command**. Every open RCP session receives NOTIFY for every parameter change |
| `NOTIFY sscurrent_ex scene_a <n>` / `NOTIFY ssrecall_ex …` | Scene changed or recall started [YAM-MTX 1-10/1-11; COMP handles `sscurrent_ex` NOTIFY] |
| `NOTIFY devstatus runmode "normal"` | Run-mode change [YAM-MTX] |
| `NOTIFY mtr …` | Meter stream |
| `ERROR <command> <code>` | Codes (YAM-MTX §3.5.1): `UnknownCommand`, `WrongFormat`, `InvalidArgument` (out of range, bad case, or index out of range), `UnknownAddress`, `UnknownEventID`, `TooLongCommand`, `AccessDenied`, `Busy`, `ReadOnly`, `NoPermission`, `InternalError`. CHAT reports `InvalidArgument` as the normal reply when polling an index a model does not have |

**Trailing display token.** Set, NOTIFY and (on at least CL/DM) get replies carry a quoted
human-readable rendering *after* the value: `… 0 0 1 "ON"`, `… -4760 "-47.60"`,
`… -32768 "-Inf"` [CHAT parser comment; DOMTC expects `OK set MIXER:Current/MuteMaster/On 0 0 1 "ON"`].
**The value is always token 5, not the last token.** The Rust crate takes the last token.
That is a latent bug for numeric params.

**Framing.** TCP chunks can split lines. Buffer until LF [COMP had a bug fix for exactly this, v3.4.10].

### 1.4 Value encoding
* **Level (dB):** integer = dB × 100. Range −32768 … 1000 (+10.00 dB). **−32768 = −∞.**
  The fader's lowest finite step is −138.00 dB (−13800) [BRE-DOCS; RUST `min_fader_val`].
  Yamaha's QLab guide shows −23285 still being in the −∞ region [YAM-QLAB table].
  Clamp outgoing values to {−32768} ∪ [−13800, 1000].
* **On:** 1 = ON (unmuted), 0 = OFF (muted). "On" is the channel ON key, not a mute flag.
* **Pan / Balance:** −63 (L63) … 0 (C) … +63 (R63) [YAM-QLAB].
* **PrePost:** 0/1. Which value means Pre is *unverified*. Defaults are ToMix = 1 and ToFx = 0.
* **Strings (`string`/`binary` type):** always sent quoted. Max length is the `prminfo`
  "max" column (e.g. Name 64, Color 8, Icon 12, Category 16).

---

## 2. TF parameter table

Transcribed from `TF Parameters-1.txt` [COMP]. That file is literal console `prminfo`
output. **X/Y columns are counts** (exclusive upper bound of the 0-based index). Y = 0 means
"pass 0". rw = read/write, r = read-only, w = write-only. "Scale" is the divisor to get
display units. Lines prefixed `--` in the source were disabled by the Companion author
(they are `DcaCh/*` aliases of `DCA/*`, see §5).

Important: the dump reports **TF5/TF3-sized** counts (InCh 40). On **TF1, valid InCh is 0–31**
([YAM-PY command_list]: "0 - 31: TF1"; [TF-DS]: 32 mono inputs). Every row is
**unverified on TF1 V4.55**.

Index legend: `ch` = channel index; `mix` = AUX 0–19 (AUX1–8 mono, AUX9/10 … 19/20 stereo
pairs); `fx` = FX bus 0–1; `mtx` = matrix 0–3.

### 2.1 Input channels — `MIXER:Current/InCh/…` (TF1: x = 0–31 = CH1–32)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale | Meaning |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | `MIXER:Current/InCh/Fader/Level` | 40 | 0 | -32768 | 1000 | -32768 | dB | integer | rw | 100 | Channel fader |
| 1 | `MIXER:Current/InCh/Fader/On` | 40 | 0 | 0 | 1 | 1 | | integer | rw | 1 | Channel ON key |
| 2 | `MIXER:Current/InCh/Label/Color` | 40 | 0 | 0 | 8 | 0 | | string | rw | 1 | Channel color name (§3.1) |
| 3 | `MIXER:Current/InCh/Label/Icon` | 40 | 0 | 0 | 12 | 0 | | binary | rw | 1 | Icon name (§3.2) |
| 4 | `MIXER:Current/InCh/Label/Category` | 40 | 0 | 0 | 16 | 0 | | binary | rw | 1 | Icon category (§3.2) |
| 5 | `MIXER:Current/InCh/Label/Name` | 40 | 0 | 0 | 64 | "ch 1" | | string | rw | 1 | Channel name |
| 6 | `MIXER:Current/InCh/Role` | 40 | 0 | 0 | 255 | 0 | | integer | r | 1 | Mono vs stereo-linked role (values undocumented) |
| 7 | `MIXER:Current/InCh/ToFx/Level` | 40 | 2 | -32768 | 1000 | -32768 | dB | integer | rw | 100 | Send to FX1/FX2 (y = fx) |
| 8 | `MIXER:Current/InCh/ToFx/On` | 40 | 2 | 0 | 1 | 1 | | integer | rw | 1 | FX send on |
| 9 | `MIXER:Current/InCh/ToFx/PrePost` | 40 | 2 | 0 | 1 | 0 | | integer | rw | 1 | FX send pre/post |
| 10 | `MIXER:Current/InCh/ToMix/Level` | 40 | 20 | -32768 | 1000 | -32768 | dB | integer | rw | 100 | Send to AUX1–20 (y = mix) |
| 11 | `MIXER:Current/InCh/ToMix/On` | 40 | 20 | 0 | 1 | 1 | | integer | rw | 1 | AUX send on |
| 12 | `MIXER:Current/InCh/ToMix/Pan` | 40 | 20 | -63 | 63 | 0 | | integer | rw | 1 | Send pan (stereo AUX) |
| 13 | `MIXER:Current/InCh/ToMix/PrePost` | 40 | 20 | 0 | 1 | 1 | | integer | rw | 1 | AUX send pre/post |
| 14 | `MIXER:Current/InCh/ToMono/Level` | 40 | 1 | -32768 | 1000 | -32768 | dB | integer | rw | 100 | Send to **SUB** bus |
| 15 | `MIXER:Current/InCh/ToMono/On` | 40 | 1 | 0 | 1 | 1 | | integer | rw | 1 | SUB send on (TF-only path [ISSUES #25]) |
| 16 | `MIXER:Current/InCh/ToSt/Pan` | 40 | 0 | -63 | 63 | 0 | | integer | rw | 1 | Pan to Stereo (official name [YAM-QLAB]) |
| 17 | `MIXER:Current/InCh/ToStereo/Pan` | 40 | 0 | -63 | 63 | 0 | | integer | rw | 1 | Alias of #16 |
| 98 | `MIXER:Current/InCh/PanMode` | 32 | 0 | 0 | 1 | 0 | | integer | r | 1 | Pan mode (note X = 32 here, not 40) |

### 2.2 Stereo input channels — `MIXER:Current/StInCh/…` (x = 0–3 = ST IN 1L, 1R, 2L, 2R — *mapping unverified*)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale |
|---|---|---|---|---|---|---|---|---|---|---|
| 18 | `MIXER:Current/StInCh/Fader/Level` | 4 | 0 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 19 | `MIXER:Current/StInCh/Fader/On` | 4 | 0 | 0 | 1 | 1 | | integer | rw | 1 |
| 20 | `MIXER:Current/StInCh/Label/Color` | 4 | 0 | 0 | 8 | 0 | | string | rw | 1 |
| 21 | `MIXER:Current/StInCh/Label/Icon` | 4 | 0 | 0 | 12 | 0 | | binary | rw | 1 |
| 22 | `MIXER:Current/StInCh/Label/Category` | 4 | 0 | 0 | 16 | 0 | | binary | rw | 1 |
| 23 | `MIXER:Current/StInCh/Label/Name` | 4 | 0 | 0 | 64 | "Rt1L" | | string | rw | 1 |
| 24 | `MIXER:Current/StInCh/ToFx/Level` | 4 | 2 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 25 | `MIXER:Current/StInCh/ToFx/On` | 4 | 2 | 0 | 1 | 1 | | integer | rw | 1 |
| 26 | `MIXER:Current/StInCh/ToFx/PrePost` | 4 | 2 | 0 | 1 | 0 | | integer | rw | 1 |
| 27 | `MIXER:Current/StInCh/ToMix/Level` | 4 | 20 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 28 | `MIXER:Current/StInCh/ToMix/On` | 4 | 20 | 0 | 1 | 1 | | integer | rw | 1 |
| 29 | `MIXER:Current/StInCh/ToMix/Pan` | 4 | 20 | -63 | 63 | 0 | | integer | rw | 1 |
| 30 | `MIXER:Current/StInCh/ToMix/PrePost` | 4 | 20 | 0 | 1 | 1 | | integer | rw | 1 |
| 31 | `MIXER:Current/StInCh/ToMono/Level` | 4 | 1 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 32 | `MIXER:Current/StInCh/ToMono/On` | 4 | 1 | 0 | 1 | 1 | | integer | rw | 1 |
| 33 | `MIXER:Current/StInCh/ToSt/Pan` | 4 | 0 | -63 | 63 | 0 | | integer | rw | 1 |
| 34 | `MIXER:Current/StInCh/ToStereo/Pan` | 4 | 0 | -63 | 63 | 0 | | integer | rw | 1 |
| 99 | `MIXER:Current/StInCh/PanMode` | 4 | 0 | 0 | 1 | 0 | | integer | r | 1 |
| 103 | `MIXER:Current/StInCh/Role` | 4 | 0 | 0 | 7 | 0 | | binary | r | 1 |

### 2.3 FX return channels — `MIXER:Current/FxRtnCh/…` (x = 0–3 = FX1 L/R, FX2 L/R — default name "Fx1L")

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale |
|---|---|---|---|---|---|---|---|---|---|---|
| 35 | `MIXER:Current/FxRtnCh/Fader/Level` | 4 | 0 | -32768 | 1000 | 0 | dB | integer | rw | 100 |
| 36 | `MIXER:Current/FxRtnCh/Fader/On` | 4 | 0 | 0 | 1 | 1 | | integer | rw | 1 |
| 37 | `MIXER:Current/FxRtnCh/Label/Color` | 4 | 0 | 0 | 8 | 0 | | string | rw | 1 |
| 38 | `MIXER:Current/FxRtnCh/Label/Icon` | 4 | 0 | 0 | 12 | 0 | | binary | rw | 1 |
| 39 | `MIXER:Current/FxRtnCh/Label/Name` | 4 | 0 | 0 | 64 | "Fx1L" | | string | rw | 1 |
| 40 | `MIXER:Current/FxRtnCh/ToMix/Level` | 4 | 20 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 41 | `MIXER:Current/FxRtnCh/ToMix/On` | 4 | 20 | 0 | 1 | 1 | | integer | rw | 1 |
| 42 | `MIXER:Current/FxRtnCh/ToMix/Pan` | 4 | 20 | -63 | 63 | 0 | | integer | rw | 1 |
| 43 | `MIXER:Current/FxRtnCh/ToMix/PrePost` | 4 | 20 | 0 | 1 | 1 | | integer | rw | 1 |
| 44 | `MIXER:Current/FxRtnCh/ToMono/Level` | 4 | 1 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 45 | `MIXER:Current/FxRtnCh/ToMono/On` | 4 | 1 | 0 | 1 | 1 | | integer | rw | 1 |
| 46 | `MIXER:Current/FxRtnCh/ToSt/Pan` | 4 | 0 | -63 | 63 | 0 | | integer | rw | 1 |
| 47 | `MIXER:Current/FxRtnCh/ToStereo/Pan` | 4 | 0 | -63 | 63 | 0 | | integer | rw | 1 |
| 100 | `MIXER:Current/FxRtnCh/PanMode` | 4 | 0 | 0 | 1 | 0 | | integer | r | 1 |
| 104 | `MIXER:Current/FxRtnCh/Role` | 4 | 0 | 0 | 7 | 0 | | binary | r | 1 |
| 106 | `MIXER:Current/FxRtnCh/Label/Category` | 4 | 0 | 0 | 6 | 0 | | binary | rw | 1 |

(FxRtnCh has no ToFx send, which is expected.)

### 2.4 DCA masters — `MIXER:Current/DCA/…` (x = 0–7 = DCA1–8)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale |
|---|---|---|---|---|---|---|---|---|---|---|
| 48 | `MIXER:Current/DCA/Fader/Level` | 8 | 0 | -32768 | 1000 | 0 | dB | integer | rw | 100 |
| 50 | `MIXER:Current/DCA/Fader/On` | 8 | 0 | 0 | 1 | 1 | | integer | rw | 1 |
| 52 | `MIXER:Current/DCA/Label/Color` | 8 | 0 | 0 | 8 | 0 | | string | rw | 1 |
| 54 | `MIXER:Current/DCA/Label/Icon` | 8 | 0 | 0 | 12 | 0 | | binary | rw | 1 |
| 56 | `MIXER:Current/DCA/Label/Category` | 8 | 0 | 0 | 16 | 0 | | binary | rw | 1 |
| 58 | `MIXER:Current/DCA/Label/Name` | 8 | 0 | 0 | 64 | "DCA 1" | | string | rw | 1 |
| 49,51,53,55,57,59 | `MIXER:Current/DcaCh/…` (same leaves) | 8 | 0 | same | | | | | rw | |

The `DcaCh` aliases are marked `--` (disabled) in COMP. [DOMTC] uses
`get/set MIXER:Current/DcaCh/Fader/Level` against a TF. **Use `DCA/`** and treat
`DcaCh/` as an alias whose NOTIFY form must be verified.
**There is no DCA *assign* path on TF**, unlike CL's `MIXER:Current/InCh/DCA/Assign` (x, 16).

### 2.5 AUX / mix buses — `MIXER:Current/Mix/…` (x = 0–19 = AUX1–20)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale | Meaning |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 60 | `MIXER:Current/Mix/Fader/Level` | 20 | 0 | -32768 | 1000 | 0 | dB | integer | rw | 100 | AUX master |
| 61 | `MIXER:Current/Mix/Fader/On` | 20 | 0 | 0 | 1 | 1 | | integer | rw | 1 | |
| 62 | `MIXER:Current/Mix/Label/Color` | 20 | 0 | 0 | 8 | 0 | | string | rw | 1 | |
| 63 | `MIXER:Current/Mix/Label/Icon` | 20 | 0 | 0 | 12 | 0 | | binary | rw | 1 | |
| 64 | `MIXER:Current/Mix/Label/Category` | 20 | 0 | 0 | 16 | 0 | | binary | rw | 1 | |
| 65 | `MIXER:Current/Mix/Label/Name` | 20 | 0 | 0 | 64 | "MX 1" | | string | rw | 1 | |
| 66 | `MIXER:Current/Mix/ToMtrx/Level` | 20 | 4 | -32768 | 1000 | -32768 | dB | integer | rw | 100 | Send to Matrix1–4 (y = mtx) |
| 67 | `MIXER:Current/Mix/ToMtrx/On` | 20 | 4 | 0 | 1 | 1 | | integer | rw | 1 | |
| 68 | `MIXER:Current/Mix/Out/Balance` | 20 | 0 | -63 | 63 | 0 | | integer | rw | 1 | Stereo AUX balance |
| 69 | `MIXER:Current/Mix/PanLink` | 20 | 0 | 0 | 1 | 0 | | integer | rw | 1 | |
| 70 | `MIXER:Current/Mix/Role` | 20 | 0 | 0 | 255 | 0 | | binary | r | 1 | |
| 97 | `MIXER:Current/Mix/BusType` | 20 | 0 | 0 | 4 | 0 | | binary | r | 1 | Read-only on TF (rw on CL) |
| 101 | `MIXER:Current/Mix/PanMode` | 20 | 0 | 0 | 1 | 0 | | integer | r | 1 | |

### 2.6 Matrix — `MIXER:Current/Mtrx/…` (x = 0–3 = MATRIX1–4; needs V2.50 or later)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale |
|---|---|---|---|---|---|---|---|---|---|---|
| 71 | `MIXER:Current/Mtrx/Fader/Level` | 4 | 0 | -32768 | 1000 | 0 | dB | integer | rw | 100 |
| 72 | `MIXER:Current/Mtrx/Fader/On` | 4 | 0 | 0 | 1 | 1 | | integer | rw | 1 |
| 73 | `MIXER:Current/Mtrx/Label/Color` | 4 | 0 | 0 | 8 | 0 | | string | rw | 1 |
| 74 | `MIXER:Current/Mtrx/Label/Icon` | 4 | 0 | 0 | 12 | 0 | | binary | rw | 1 |
| 75 | `MIXER:Current/Mtrx/Label/Category` | 4 | 0 | 0 | 16 | 0 | | binary | rw | 1 |
| 76 | `MIXER:Current/Mtrx/Label/Name` | 4 | 0 | 0 | 64 | "MT 1" | | string | rw | 1 |
| 77 | `MIXER:Current/Mtrx/Role` | 4 | 0 | 0 | 255 | 0 | | binary | r | 1 |

Inputs cannot send to matrix on TF: there is no `InCh/ToMtrx`. Matrices are fed from AUX, ST and SUB only [TF-FW V2.50-2].

### 2.7 Stereo master — `MIXER:Current/St/…` (x = 0–1 = ST L, ST R)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale |
|---|---|---|---|---|---|---|---|---|---|---|
| 78 | `MIXER:Current/St/Fader/Level` | 2 | 0 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 79 | `MIXER:Current/St/Fader/On` | 2 | 0 | 0 | 1 | 1 | | integer | rw | 1 |
| 80 | `MIXER:Current/St/Label/Color` | 2 | 0 | 0 | 8 | 0 | | string | rw | 1 |
| 81 | `MIXER:Current/St/Label/Icon` | 2 | 0 | 0 | 12 | 0 | | binary | rw | 1 |
| 82 | `MIXER:Current/St/Label/Category` | 2 | 0 | 0 | 16 | 0 | | binary | rw | 1 |
| 83 | `MIXER:Current/St/Label/Name` | 2 | 0 | 0 | 64 | "ST L" | | string | rw | 1 |
| 84 | `MIXER:Current/St/ToMtrx/Level` | 2 | 4 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 85 | `MIXER:Current/St/ToMtrx/On` | 2 | 4 | 0 | 1 | 1 | | integer | rw | 1 |
| 86 | `MIXER:Current/St/Out/Balance` | 2 | 0 | -63 | 63 | 0 | | integer | rw | 1 |
| 102 | `MIXER:Current/St/PanMode` | 2 | 0 | 0 | 1 | 0 | | integer | r | 1 |
| 105 | `MIXER:Current/St/Role` | 2 | 0 | 0 | 7 | 0 | | binary | r | 1 |

The master fader is normally operated at x = 0. Whether L/R are linked, so that setting x = 0 moves both, is *unverified*.

### 2.8 SUB (mono) bus — `MIXER:Current/Mono/…` (x = 0)

| # | Address | X | Y | Min | Max | Default | Unit | Type | RW | Scale |
|---|---|---|---|---|---|---|---|---|---|---|
| 87 | `MIXER:Current/Mono/Fader/Level` | 1 | 0 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 88 | `MIXER:Current/Mono/Fader/On` | 1 | 0 | 0 | 1 | 1 | | integer | rw | 1 |
| 89 | `MIXER:Current/Mono/Label/Color` | 1 | 0 | 0 | 8 | 0 | | string | rw | 1 |
| 90 | `MIXER:Current/Mono/Label/Icon` | 1 | 0 | 0 | 12 | 0 | | binary | rw | 1 |
| 91 | `MIXER:Current/Mono/Label/Category` | 1 | 0 | 0 | 16 | 0 | | binary | rw | 1 |
| 92 | `MIXER:Current/Mono/Label/Name` | 1 | 0 | 0 | 64 | "MONO" | | string | rw | 1 |
| 93 | `MIXER:Current/Mono/ToMtrx/Level` | 1 | 4 | -32768 | 1000 | -32768 | dB | integer | rw | 100 |
| 94 | `MIXER:Current/Mono/ToMtrx/On` | 1 | 4 | 0 | 1 | 1 | | integer | rw | 1 |

### 2.9 Mute groups — `MIXER:Current/MuteMaster/…` (x = 0–5 = MUTE1–6)

| # | Address | X | Y | Min | Max | Default | Type | RW | Meaning |
|---|---|---|---|---|---|---|---|---|---|
| 96 | `MIXER:Current/MuteMaster/On` | 6 | 0 | 0 | 1 | 0 | integer | rw | Mute group master engaged (1 = muting) |
| 107 | `MIXER:Current/MuteMaster/Label/Name` | 6 | 0 | 0 | 8 | "MUTE 1" | string | rw | Mute group name (max 8) |

Reply seen on a TF: `OK set MIXER:Current/MuteMaster/On 0 0 1 "ON"` [DOMTC]. Groups 3–6
exist from V4.00 [TF-FW]. **No mute-group assign path.**

### 2.10 Setup

| # | Address | X | Y | Max | Type | RW | Meaning |
|---|---|---|---|---|---|---|---|
| 95 | `MIXER:Setup/MonitorMix/Password` | 0 | 0 | 24 | binary | rw | MonitorMix app PIN [TF-RM]. **Security-sensitive: never read or write it from patchbay.** |

### 2.11 Scenes — `scninfo` [COMP]

| # | Descriptor | X | Y | Range | RW | Wire command actually sent |
|---|---|---|---|---|---|---|
| 1000 | `MIXER:Lib/Bank/Scene/Recall` | 1 | 2 (bank A/B) | 0–100 | w | `ssrecall_ex scene_a <n>` / `scene_b` |
| 1001 | `MIXER:Lib/Bank/Scene/Store` | 1 | 2 | 0–100 | w | `ssupdate_ex scene_a <n>` |
| 1010 | `MIXER:Lib/Scene/RecallInc` | 0 | 0 | — | w | `event MIXER:Lib/Scene/RecallInc` |
| 1011 | `MIXER:Lib/Scene/RecallDec` | 0 | 0 | — | w | `event MIXER:Lib/Scene/RecallDec` |

TF1 has 200 scenes = 2 banks × 100 (A00–A99, B00–B99) [TF-DS; YAM-PY: 0–99]. Scene 00 is the
read-only initial scene in each bank on the console UI. The descriptors are not wire
addresses: Companion rewrites them in `fmtCmd`.

### 2.12 Meters — `mtrinfo` [COMP]

| # | Descriptor | X | Y (pickoffs) | Range | Pickoffs |
|---|---|---|---|---|---|
| 2000 | `MIXER:Current/Meter/InCh` | 40 | 3 | 0–127 | PreHPF, PreFader, PostOn |
| 2001 | `MIXER:Current/Meter/StInCh` | 4 | 3 | 0–127 | PreEQ, PreFader, PostOn |
| 2002 | `MIXER:Current/Meter/FxRtnCh` | 4 | 2 | 0–127 | PreFader, PostOn |
| 2100 | `MIXER:Current/Meter/Mix` | 20 | 3 | 0–127 | PreEQ, PreFader, PostOn |
| 2101 | `MIXER:Current/Meter/Mtrx` | 4 | 3 | 0–127 | PreEQ, PreFader, PostOn |
| 2102 | `MIXER:Current/Meter/St` | 2 | 3 | 0–127 | PreEQ, PreFader, PostOn |
| 2103 | `MIXER:Current/Meter/Mono` | 1 | 3 | 0–127 | PreEQ, PreFader, PostOn |

Wire form: `mtrstart MIXER:Current/InCh/PreFader <ms>` (strip `/Meter`, append the pickoff).
Readings: dB ≈ value − 126.

### 2.13 Explicitly **not** available on TF via RCP (no path in the TF `prminfo` dump or any public source)

| Function | CL/QL path (for contrast) | TF status |
|---|---|---|
| Head-amp (analog) gain | `MIXER:Current/InCh/Port/HA/Gain` (−600…6600) | **absent** |
| Digital gain | — | **absent** |
| Phantom +48 V, phase/polarity | — | **absent** |
| HPF on/freq, EQ, Gate, Comp | `…/Dyna1/Threshold`, `…/Dyna2/Threshold` on CL | **absent** |
| DCA assign | `MIXER:Current/InCh/DCA/Assign` (x, 16) | **absent** |
| Mute-group assign | — | **absent** |
| Channel → ST assign on/off ("ST" button) | — | **absent** [ISSUES #42] |
| Input patch / source select (INPUT / USB / SLOT) | `MIXER:Current/InCh/Patch` (read-only on CL) | **absent** |
| Output patch (OMNI OUT, slot, USB) | `…/OmniOutPort/Patch`, `…/DanteOutPort/Patch`, `…/SlotOut*Port/Patch` | **absent** |
| Cue/solo | `MIXER:Current/Cue/*` | **absent** (COMP removed TF cue variables in 3.0.2) |
| Monitor section, oscillator, GEQ, FX params, recorder | various | **absent** |
| Stereo link, channel library, recall safe | — | **absent** |

These may exist as undocumented addresses that `prminfo` does not enumerate. If so, a
`get` on a guessed path would return OK rather than `UnknownAddress`. Probe this read-only
(§8) before ruling it out entirely. The only full-control channel on TF is the proprietary
TF Editor / StageMix protocol, which is undocumented. Reverse-engineering it is a separate project.

### 2.14 Settable summary (TF)

| Attribute | Input ch | ST IN | FX RTN | AUX | Matrix | ST | SUB | DCA | Mute grp |
|---|---|---|---|---|---|---|---|---|---|
| Name | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (8 chars) |
| Color | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| Icon | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| Category | ✅ | ✅ | ✅ (max 6) | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| Fader | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | — |
| On | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (master) |
| Gain | ❌ | ❌ | — | — | — | — | — | — | — |
| Grouping (DCA/mute membership) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | — | — |

---

## 3. Enumerations

### 3.1 Colors (`…/Label/Color`, string, ≤ 8 chars)
* **TF:** `Blue`, `Orange`, `Yellow`, `Purple`, `SkyBlue`, `Pink`, `Red`, `Green`
  [RUST `LabelColor` (TF1-tested); COMP `rcpNames.chColorsTF`]. COMP also offers `Off`. Whether
  TF accepts `Off` is *unverified* (the Rust crate omits it). Case sensitivity is also
  *unverified*: send exactly as listed. Parse case-insensitively, as the Rust crate does.
* For contrast, CL/QL/DM3 use `Cyan`/`Magenta` where TF uses `SkyBlue`/`Pink`. DM7/Rivage wire
  names were observed as `SkyBlue`, `LightGreen`, `White`, `OFF` [CHAT README]. **Do not share
  a palette table across models.**
* The prminfo default is `0` (a number), not a name. Expect `get` to return a quoted name, *unverified*.

### 3.2 Icons and categories (`…/Label/Icon` ≤ 12 chars, `…/Label/Category` ≤ 16 chars, type `binary`)
* On TF, the CH NAME screen has an icon **Category** selector and an icon list per category.
  Available categories depend on channel type [TF-RM "CH NAME screen"]. That is why TF exposes
  both `Icon` and `Category`, while CL exposes only `Icon`.
* **No public source lists the TF icon or category wire strings.** COMP reuses the CL/QL icon
  list for every model (`Kick`, `Snare`, `Hi-Hat`, `FloorTom`, `Drumkit`, `Perc.`, `A.Bass`,
  `E.Bass`, `BassAmp`, `A.Guitar`, `E.Guitar`, `GuitarAmp`, `Trumpet`, `Trombone`, `Saxophone`,
  `Strings`, `Piano`, `Organ`, `Keyboard`, `Male`, `Female`, `Choir`, `DynamicMic`,
  `CondenserMic`, `WirelessMic`, `SpeechMic`, `Speaker`, `Wedge `, `In-Ear`, `Effect`,
  `Processor`, `Media1`, `Media2`, `Video`, `Mixer`, `PC`, `Audience`, `Star1`, `Star2`, `Blank`).
  Note the trailing space in `Wedge ` in their JSON. **This list is not validated on TF.**
* Plan: build the TF icon/category vocabulary by reading `get …/Label/Icon` and
  `…/Label/Category` for all channels after setting them on the console UI. Until then, treat
  them as opaque strings: round-trip what you read and do not offer a picker.

### 3.3 Other enums
* `Role`, `BusType`, `PanMode`: read-only. Values are undocumented for TF (CL uses strings like
  `"Mono"`, `"StereoL"`, `"VARI"`). Use `Role` to detect stereo pairing of AUX 9/10 … 19/20.
* Scene bank tokens: `scene_a`, `scene_b`.
* `devstatus runmode` values (from MTX spec): `"normal"`, `"emergency"`, etc.

---

## 4. How existing libraries handle feedback, reconnect, rate limiting and scenes

### 4.1 Companion module [COMP `index.js`]
* **Subscriptions:** none needed. It parses every `OK`/`OKm`/`NOTIFY` line into a data store
  keyed by (address, x, y) and fires feedbacks on change. Values it needs but does not have are
  fetched with a queued `get`.
* **Own sets:** it ignores `OK set` echoes for the store and writes its own value optimistically.
* **Rate limiting:** a single command queue with **5 ms spacing** (`MSG_DELAY`). A queued command
  with the same (prefix, address, x, y) is **replaced**, not appended, which coalesces fader
  drags. The next command is released when a matching `get` reply arrives or after 5 ms.
  Yamaha's own readme warns that back-to-back commands with zero wait can be dropped
  [YAM-PY Readme: "set Pre Wait … to 1"].
* **Reconnect:** delegated to Companion's `TCPHelper`, which auto-reconnects. On connect it sends
  `devinfo productname`, `devinfo devicename`, `devstatus runmode`, `sscurrent_ex scene_a`,
  `sscurrent_ex scene_b`, then re-polls.
* **Keepalive (optional, off by default):** `scpmode keepalive 10000`, then `devstatus runmode`
  every 10 s.
* **Scene recall side effects:** on `NOTIFY sscurrent_ex` it **clears the whole data store and
  re-gets every subscribed value**. The console does not reliably emit per-parameter NOTIFYs
  for a recall. With V4.00+ FADE TIME, values can also be mid-fade when re-read [TF-FW V4.00 note].
* **Meters:** `mtrstart … <ms>` re-issued every 10 s.

### 4.2 Chataigne RCP module [CHAT `Yam-RCP.js`] (no TF support, but hardware-tested parser)
* Tokenizer honours quotes and backslash escapes. The value is `tokens[5]`, never the last token.
* ERROR `InvalidArgument` during bulk polling is expected noise. Log it at debug level.
* Keepalive: `scpmode keepalive (2 × ping)`, then `devstatus runmode` pings. Their note: the
  desk closes idle RCP connections, which silently kills NOTIFY feedback.
* After `NOTIFY sscurrent*`: re-get the whole tree. Assumes the recalling client does **not**
  receive its own sscurrent NOTIFY, so it must re-poll itself after its own recall.

### 4.3 Rust crate `yamaha-rcp` 0.1.0 [RUST]
* **License:** MIT (declared in Cargo.toml; the repo has no LICENSE file). MIT is compatible with
  our **GPL-3.0-or-later** (`patchbay/Cargo.toml`), so depending on it would be legal.
* **API:** `TFMixer::new("ip:49280")`, `fader_level/set_fader_level(ch, i32)`,
  `muted/set_muted`, `color/set_color(LabelColor)`, `label/set_label`,
  `recall_scene(SceneList::A|B, u8)`, `fade(ch, from, to, ms)` (50 ms steps),
  `set_connection_limit`. Input channels only. Tokio-based.
* **Problems found reading `src/lib.rs` (commit `9d58684`):**
  1. The reader forwards only lines starting with `OK`/`ERROR`, so **NOTIFY lines are dropped**.
     There is no change feed.
  2. The reader ignores the byte count from `read()` and iterates the whole 512-byte buffer,
     pushing `0x00` bytes into the line. It also allocates a new line buffer per read, so
     **lines split across TCP reads are corrupted**.
  3. Replies are matched FIFO. Any unsolicited `OK` line (such as a scene NOTIFY-adjacent
     reply) desynchronizes callers.
  4. `muted()` returns `true` when `Fader/On != 0`, which is **inverted**.
  5. `request_int` parses the *last* token, which breaks when the display string is present.
  6. `unwrap()` on UTF-8 in the reader task can panic. There is no reconnect and no keepalive.
  7. Pre-production (the README says the API "will change"). One release, about 900 downloads.
* **Recommendation:** **write our own** small client in `crates/adapters/yamaha-tf`. Take the
  crate's `LabelColor` names and scene syntax as reference only. Design: a single connection
  task, an LF-framed codec, a quote-aware tokenizer, a typed `Reply` enum, a request map keyed
  by (verb, address, x, y) rather than FIFO, a NOTIFY broadcast channel, a coalescing send queue
  (~5 ms spacing, last-write-wins per key), keepalive, and exponential-backoff reconnect with
  full re-sync.

### 4.4 Other
* Yamaha's own scripts are fire-and-forget: connect, send one line, `recv(1500)`, close [YAM-PY].
* [BRE-DOCS] `notify_saver.py` just dumps raw socket output. That confirms NOTIFY arrives
  without any subscription.

---

## 5. TF vs CL/QL/DM differences that bite

| Topic | TF | CL/QL | DM3/DM7/Rivage |
|---|---|---|---|
| Index base | x, y **0-based** on the wire (UI CH1 = x 0) | same | same |
| Y for non-send params | `0` | `0` (their prminfo shows Y = 1 = count) | same |
| Scene verb | `ssrecall_ex scene_a|scene_b <int>` | `ssrecall_ex MIXER:Lib/Scene <int>` | DM3: like TF. DM7: `ssrecallt_ex scene_a "1.00"`. Rivage: `ssrecallt_ex MIXER:Lib/Scene "N.MM"` |
| Active scene bank | Only discoverable by `sscurrent_ex` on both banks (one errors) | n/a | DM: same trick |
| Names | ≤ 64 chars (prminfo) | ≤ 8 chars | DM: 64, type binary |
| Colors | 8 names incl. `SkyBlue`, `Pink` | `Cyan`, `Magenta` | DM7: `SkyBlue`, `LightGreen`, `White`, `OFF` |
| Icon | `Icon` + `Category` | `Icon` only | DM: `Icon` + `Category` |
| DCA path | `DCA/…` (+ `DcaCh/…` alias) | `DCA/…` | DM3 has no DCA |
| DCA/mute assign | **none** | `InCh/DCA/Assign` | DM3/DM7 vary |
| Mute master | `MuteMaster/On` (6) | `MuteMaster/On` (8) | DM: `MuteGrpCtrl/On` |
| HA gain | **none** | `InCh/Port/HA/Gain` | DM3: `IO:Current/InCh/HAGain` |
| Patch | **none** | InCh/Patch (r), Dante/Omni/Slot out patch (rw) | varies |
| SUB send | `InCh/ToMono/*` (TF-only) | none | — |
| Matrix sends from inputs | none | `InCh/ToMtrx/*` | yes |
| `devstatus error` | not supported | yes | not on DM |
| Cue paths | none | `Cue/*` | yes |

**TF1 counts** [TF-DS; YAM-PY]:

| Block | Count | RCP x range |
|---|---|---|
| Mono input channels | **32** (CH1–32; CH1–16 local XLR/TRS, CH17–32 via USB/slot or the same jacks) | InCh 0–31 (**not** 0–39 as the TF5 dump says) |
| Stereo inputs | 2 (ST IN 1, 2) | StInCh 0–3 (L/R pairs, *unverified*) |
| FX returns | 2 stereo | FxRtnCh 0–3 |
| AUX buses | 20 (AUX1–8 mono, 9/10–19/20 stereo) | Mix 0–19, send y 0–19 |
| FX buses | 2 | ToFx y 0–1 |
| Stereo | 1 (L/R) | St 0–1 |
| SUB | 1 | Mono 0 |
| Matrix | 4 | Mtrx 0–3, ToMtrx y 0–3 |
| DCA | 8 | DCA 0–7 |
| Mute groups | 6 | MuteMaster 0–5 |
| Scenes | 200 (A 0–99, B 0–99) | — |
| Local I/O | 16 mic/line + 2 stereo line in, 16 analog out, 1 NY slot | not addressable |
| USB audio | 34 in / 34 out (DAW multitrack) | not addressable |

Behaviour of x = 32–39 on a TF1 (ERROR vs. silent accept) is *unverified*.

---

## 6. TF and Dante (NY64-D)

* NY64-D gives a TF1 **32 in / 24 out** Dante channels [TF-DS]. Supported since TF V2.00
  [TF-FW V2.00]. Brooklyn3-based cards need TF V4.55 [TF-FW V4.55].
* **"Dante Patch from Console: No"** [TF-DS]. Dante subscriptions are **not** made on the TF at
  all. They are made in Dante Controller or any Dante control API, or automatically by *Quick
  Config* (fixed patch, [TF-FW V2.00]).
* **No RCP path touches the slot or Dante on TF.** The CL-only `DanteOutPort/Patch` and
  `InCh/Patch` do not exist in the TF dump. Which input source a channel uses (INPUT, USB or
  SLOT) is also not exposed.
* For patchbay, Dante routing therefore belongs in a **separate Dante adapter** using the Dante
  control protocol (e.g. reverse-engineered implementations like `netaudio`, or Audinate's
  DDM/API). The TF adapter can only name and level the channels those subscriptions feed.
* Caveat from the firmware notes: labelling NY64-D transmit channels can cause "Dante Setting
  Error" with DDM. Keep the NETWORK (control) port off the Dante network [TF-FW Known Issues].

---

## 7. Adapter implementation notes

1. On connect: `devinfo productname`. Expect `"TF1"`; refuse or limit on mismatch. Then
   `devinfo version`, `scpmode keepalive 10000` (then ping `devstatus runmode` every ~4 s),
   `sscurrent_ex scene_a`, `sscurrent_ex scene_b`, then a bulk `get` of the model tree
   (about 32×(6 + 20×4 + 2×3 + 4) + … roughly 3.5k gets; throttle it).
2. Optionally enumerate `prminfo 0..` until ERROR to build the tree dynamically and to detect
   firmware differences.
3. Treat `NOTIFY set` as authoritative state. Treat `OK set` as confirmation (use its value; it
   may be clamped).
4. On `NOTIFY sscurrent_ex` / `ssrecall_ex` **and after our own recall**, debounce about 500 ms
   (longer if fade time is used), then re-read everything.
5. Coalesce outgoing sets per key. Keep at least ~5 ms between lines. Cap in-flight gets.
6. Escape `"` and `\` in names. Reject non-ASCII unless `scpmode encoding utf8` is verified.
   Truncate to the prminfo max (64; 8 for mute-group names).
7. Never send `set MIXER:Setup/MonitorMix/Password`. Scene store (`ssupdate_ex`) must be
   behind an explicit user confirmation.

---

## 8. Verification plan (read-only)

The house TF1 is a live production console. Verification must use **only** `devinfo`,
`devstatus`, `get`, `sscurrent_ex`, `ssinfo_ex` and `prminfo`/`scninfo`/`mtrinfo`. It must
never use `set`, `ssrecall_ex`, `ssupdate_ex`, `event`, `mtrstart` or `scpmode`, and it
must hold one short-lived connection. Checklist to fill in when authorised:

- [ ] `devinfo productname`, `devinfo version`, `devinfo protocolver`, `devinfo paramsetver`
- [ ] `prminfo 0..N` until ERROR, then diff against §2 (confirms the TF1 X counts)
- [ ] `get MIXER:Current/InCh/Fader/Level 31 0` → OK; `… 32 0` → ERROR? (TF1 channel count)
- [ ] `get` on each row of §2 at x = 0. Record the reply shape, including the display token
- [ ] `get …/Label/Color|Icon|Category` for all channels to capture the TF vocabulary (§3)
- [ ] `get MIXER:Current/InCh/Port/HA/Gain 0 0`, `get IO:Current/InCh/HAGain 0 0`,
      `get MIXER:Current/InCh/DCA/Assign 0 0` → expect `ERROR … UnknownAddress` (confirms §2.13)
- [ ] `sscurrent_ex scene_a` / `scene_b`, then `ssinfo_ex` on the active one (confirms reply field order)

Each row of §2 should then be annotated "verified on TF1 V4.55" or corrected.


### 8.1 Results — run 2026-09-18 against TF1 V4.55 (read-only)

- `devinfo`: productname "TF1", version "V4.55", protocolver "1.0",
  paramsetver "TF:1.0.0,MIXER:1.0.0", devstatus runmode "normal".
- `prminfo 0..107` all OK, `prminfo 108` → `ERROR prminfo InternalError`:
  **exactly 108 parameters**, matching §2. Reply shape:
  `OK prminfo <i> "<path>" <xcount> <ycount> <min> <max> <default> "<unit>" <type> <ui> <rw> <scale>`,
  e.g. `OK prminfo 0 "MIXER:Current/InCh/Fader/Level" 40 0 -32768 1000 -32768 "dB" integer any rw 100`.
  Full dump: `docs/fixtures/yamaha-tf1/tf1_prminfo.json`.
- InCh x-count is **40 on TF1 too** (shared TF table): `get …/InCh/Fader/Level 32 0` answers OK
  (-32768). Only 0–31 are physical channels — clamp in the adapter.
- `get MIXER:Current/InCh/Port/HA/Gain`, `IO:Current/InCh/HAGain`, `MIXER:Current/InCh/DCA/Assign`
  → `ERROR get UnknownAddress` (confirms §2.13: no gain, no DCA membership).
- `sscurrent_ex scene_b` → `OK sscurrent_ex scene_b 22 modified`; `scene_a` → `ERROR … InvalidArgument`
  (only the active bank answers).
- `get` reply shape for these paths is `OK get <path> <x> <y> <value>` (no trailing display token
  observed on Label/Fader gets).
- Live vocabulary seen (all channels' labels: `docs/fixtures/yamaha-tf1/tf1_labels.json`):
  - Colors: "Blue", "Green", "Orange", "Purple", "Red", "SkyBlue", "Yellow"
  - Icons: "Blank", "Drumkit", "DynamicMic", "E.Bass", "E.Guitar", "Effect", "In-Ear", "Keyboard", "Media2", "Media3", "Organ", "PC", "Piano", "Speaker", "SubWoofer", "Wedge", "WirelessMic"
  - Categories: "FX RTN", "Guitars", "Others", "Output", "Vocal"
- `Fader/On` 1 = channel ON (not muted).
