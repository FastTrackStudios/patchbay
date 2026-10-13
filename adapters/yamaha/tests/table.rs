//! Parameter table from the live TF1 `prminfo` dump, value mapping, and
//! label vocabulary against the live label capture.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::float_cmp
)]

use std::collections::{HashMap, HashSet};

use patchbay_yamaha::{
    ModelLimits, ParamTable, SceneField, Target, ValueKind, Vocab, VocabKind, canonical_address,
    db_to_raw, embedded_prminfo, raw_to_db,
};

const LABELS: &str = include_str!("fixtures/tf1_labels.json");

fn tf1() -> ParamTable {
    ParamTable::build(&embedded_prminfo().unwrap(), &ModelLimits::TF1)
}

fn count(t: &ParamTable, prefix: &str) -> usize {
    t.defs()
        .iter()
        .filter(|d| d.path.starts_with(prefix))
        .count()
}

#[test]
fn tf1_counts() {
    let t = tf1();
    // 97 per input: 7 strip/label + 2×3 FX + 20×4 AUX + 2 SUB + pan + panmode.
    assert_eq!(count(&t, "in/"), 32 * 97);
    assert_eq!(count(&t, "stin/"), 4 * 97);
    // No FX sends; category is read-only but mapped.
    assert_eq!(count(&t, "fxrtn/"), 4 * 91);
    assert_eq!(count(&t, "aux/"), 20 * 19);
    assert_eq!(count(&t, "matrix/"), 4 * 7);
    assert_eq!(count(&t, "stereo/"), 2 * 17);
    assert_eq!(count(&t, "sub/"), 14);
    assert_eq!(count(&t, "dca/"), 8 * 6);
    assert_eq!(count(&t, "mutegroup/"), 6 * 2);
    assert_eq!(count(&t, "scene/"), 4);
    assert_eq!(t.len(), 4376);
    assert_eq!(t.skipped().len(), 10);
    assert!(
        t.skipped()
            .iter()
            .any(|s| s == "MIXER:Setup/MonitorMix/Password")
    );
    // Paths are unique.
    let mut seen = HashSet::new();
    for d in t.defs() {
        assert!(seen.insert(d.path.clone()), "dup {}", d.path);
    }
}

#[test]
fn tf1_clamps_to_physical_channels() {
    let t = tf1();
    assert!(t.by_path("in/32/level").is_some());
    assert!(
        t.by_path("in/33/level").is_none(),
        "InCh x=32..39 are not TF1 channels"
    );
    assert!(t.by_rcp("MIXER:Current/InCh/Fader/Level", 32, 0).is_none());
    assert!(t.by_path("in/32/send/aux/20/prepost").is_some());
    assert!(t.by_path("in/1/send/aux/21/level").is_none());
    assert!(t.by_path("in/1/send/fx/2/level").is_some());
    assert!(t.by_path("in/1/send/fx/3/level").is_none());
    assert!(t.by_path("dca/8/name").is_some());
    assert!(t.by_path("dca/9/name").is_none());
    assert!(t.by_path("mutegroup/6/on").is_some());

    // Unclamped: prminfo counts at face value (InCh 40).
    let raw = ParamTable::build(&embedded_prminfo().unwrap(), &ModelLimits::UNLIMITED);
    assert!(raw.by_path("in/40/level").is_some());
    assert!(raw.by_path("in/41/level").is_none());
}

#[test]
fn paths_map_to_wire_addresses() {
    let t = tf1();
    let rcp = |p: &str| match &t.by_path(p).unwrap_or_else(|| panic!("{p}")).target {
        Target::Rcp { address, x, y } => (address.clone(), *x, *y),
        Target::Scene(_) => panic!("{p} is a scene param"),
    };
    let a = |s: &str| format!("MIXER:Current/{s}");
    assert_eq!(rcp("in/1/level"), (a("InCh/Fader/Level"), 0, 0));
    assert_eq!(rcp("in/32/on"), (a("InCh/Fader/On"), 31, 0));
    assert_eq!(rcp("in/3/name"), (a("InCh/Label/Name"), 2, 0));
    assert_eq!(rcp("in/3/pan"), (a("InCh/ToSt/Pan"), 2, 0));
    assert_eq!(rcp("in/5/send/aux/20/pan"), (a("InCh/ToMix/Pan"), 4, 19));
    assert_eq!(
        rcp("in/5/send/fx/2/prepost"),
        (a("InCh/ToFx/PrePost"), 4, 1)
    );
    assert_eq!(rcp("in/5/send/sub/level"), (a("InCh/ToMono/Level"), 4, 0));
    assert_eq!(rcp("stin/4/color"), (a("StInCh/Label/Color"), 3, 0));
    assert_eq!(rcp("fxrtn/2/send/aux/1/on"), (a("FxRtnCh/ToMix/On"), 1, 0));
    assert_eq!(rcp("aux/9/balance"), (a("Mix/Out/Balance"), 8, 0));
    assert_eq!(
        rcp("aux/9/send/matrix/4/level"),
        (a("Mix/ToMtrx/Level"), 8, 3)
    );
    assert_eq!(rcp("matrix/2/level"), (a("Mtrx/Fader/Level"), 1, 0));
    assert_eq!(rcp("stereo/l/level"), (a("St/Fader/Level"), 0, 0));
    assert_eq!(rcp("stereo/r/send/matrix/1/on"), (a("St/ToMtrx/On"), 1, 0));
    assert_eq!(rcp("sub/name"), (a("Mono/Label/Name"), 0, 0));
    assert_eq!(
        rcp("sub/send/matrix/2/level"),
        (a("Mono/ToMtrx/Level"), 0, 1)
    );
    assert_eq!(rcp("dca/1/level"), (a("DCA/Fader/Level"), 0, 0));
    assert_eq!(rcp("mutegroup/6/on"), (a("MuteMaster/On"), 5, 0));
    assert_eq!(rcp("mutegroup/1/name"), (a("MuteMaster/Label/Name"), 0, 0));

    // Aliases resolve to the canonical param.
    assert_eq!(
        canonical_address("MIXER:Current/DcaCh/Fader/Level"),
        "MIXER:Current/DCA/Fader/Level"
    );
    assert_eq!(
        t.by_rcp("MIXER:Current/DcaCh/Label/Name", 7, 0)
            .unwrap()
            .path,
        "dca/8/name"
    );
    assert_eq!(
        t.by_rcp("MIXER:Current/InCh/ToStereo/Pan", 0, 0)
            .unwrap()
            .path,
        "in/1/pan"
    );
}

#[test]
fn kinds_and_writability() {
    let t = tf1();
    let d = |p: &str| t.by_path(p).unwrap_or_else(|| panic!("{p}"));
    assert_eq!(d("in/1/level").kind, ValueKind::Level);
    assert_eq!(d("in/1/on").kind, ValueKind::Toggle);
    assert_eq!(d("in/1/pan").kind, ValueKind::Pan);
    assert_eq!(d("in/1/send/aux/1/prepost").kind, ValueKind::PrePost);
    assert_eq!(d("in/1/name").kind, ValueKind::Text { max_len: 64 });
    assert_eq!(d("mutegroup/1/name").kind, ValueKind::Text { max_len: 8 });
    assert_eq!(d("in/1/color").kind, ValueKind::Label(VocabKind::Color));
    assert_eq!(d("in/1/icon").kind, ValueKind::Label(VocabKind::Icon));
    assert_eq!(
        d("dca/1/category").kind,
        ValueKind::Label(VocabKind::Category)
    );
    assert!(matches!(d("aux/1/panmode").kind, ValueKind::Int { .. }));
    assert_eq!(d("aux/1/bustype").kind, ValueKind::Opaque);

    for p in [
        "in/1/level",
        "in/1/name",
        "in/1/send/aux/1/level",
        "dca/1/color",
        "mutegroup/1/on",
        "aux/1/panlink",
    ] {
        assert!(d(p).writable, "{p}");
        assert!(!d(p).disruptive, "{p}");
    }
    // Read-only per the live prminfo (rw column "r") or forced (Role).
    for p in [
        "mutegroup/1/name",
        "fxrtn/1/category",
        "in/1/role",
        "in/1/panmode",
        "aux/1/bustype",
        "scene/current",
        "scene/title",
    ] {
        assert!(!d(p).writable, "{p}");
    }
    let recall = d("scene/recall");
    assert!(recall.writable && recall.disruptive);
    assert_eq!(recall.target, Target::Scene(SceneField::Recall));
}

#[test]
fn level_conversion() {
    assert_eq!(raw_to_db(-32768), f64::NEG_INFINITY);
    assert_eq!(raw_to_db(-1000), -10.0);
    assert_eq!(raw_to_db(670), 6.7);
    assert_eq!(raw_to_db(1000), 10.0);
    assert_eq!(db_to_raw(f64::NEG_INFINITY), Some(-32768));
    assert_eq!(db_to_raw(-200.0), Some(-32768));
    assert_eq!(db_to_raw(-138.0), Some(-13800));
    assert_eq!(db_to_raw(-10.0), Some(-1000));
    assert_eq!(db_to_raw(-0.054), Some(-5));
    assert_eq!(db_to_raw(10.0), Some(1000));
    assert_eq!(db_to_raw(10.01), None);
    assert_eq!(db_to_raw(f64::NAN), None);
}

#[test]
fn live_label_vocabulary_decodes() {
    // Every label value on the live console decodes to a vocabulary entry
    // without growing the vocabulary (so the enum options are complete
    // for this console).
    let labels: HashMap<String, HashMap<String, String>> = serde_json::from_str(LABELS).unwrap();
    let mut vocab = Vocab::default();
    let sizes = |v: &Vocab| {
        [VocabKind::Color, VocabKind::Icon, VocabKind::Category].map(|k| v.options(k).len())
    };
    let before = sizes(&vocab);
    for ch in labels.values() {
        for (field, kind) in [
            ("Color", VocabKind::Color),
            ("Icon", VocabKind::Icon),
            ("Category", VocabKind::Category),
        ] {
            let raw = ch[field].trim_matches('"');
            let i = vocab.intern(kind, raw).unwrap();
            assert_eq!(vocab.name(kind, i), Some(raw));
        }
    }
    assert_eq!(sizes(&vocab), before, "live values outside the vocabulary");

    // Unknown values are appended and round-trip.
    let i = vocab.intern(VocabKind::Color, "Off").unwrap();
    assert_eq!(vocab.name(VocabKind::Color, i), Some("Off"));
    // Case-insensitive match keeps the canonical spelling.
    let sky = vocab.index_of(VocabKind::Color, "skyblue").unwrap();
    assert_eq!(vocab.name(VocabKind::Color, sky), Some("SkyBlue"));
}

#[test]
fn live_label_capture_matches_table_paths() {
    // tf1_labels.json keys (`InCh/0`, `St/1`, `Mono/0`, `DCA/7`…) all map.
    let t = tf1();
    let labels: HashMap<String, HashMap<String, String>> = serde_json::from_str(LABELS).unwrap();
    assert_eq!(labels.len(), 32 + 4 + 4 + 20 + 4 + 2 + 1 + 8);
    for (key, fields) in &labels {
        let (block, x) = key.split_once('/').unwrap();
        let x: u16 = x.parse().unwrap();
        for leaf in ["Label/Name", "Fader/Level", "Fader/On"] {
            let addr = format!("MIXER:Current/{block}/{leaf}");
            assert!(t.by_rcp(&addr, x, 0).is_some(), "{addr} {x}");
        }
        assert!(fields.contains_key("Level"));
    }
    // Spot-check a live row: CH 7 fader at 670 = +6.70 dB.
    let def = t.by_rcp("MIXER:Current/InCh/Fader/Level", 6, 0).unwrap();
    assert_eq!(def.path, "in/7/level");
    let raw: i64 = labels["InCh/6"]["Level"].parse().unwrap();
    assert_eq!(raw_to_db(raw), 6.7);
}
