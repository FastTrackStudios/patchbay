//! Wire-level tests: framing, call encoding (byte-exact vs. the panel),
//! reply/cyclic parsing from captured fixtures, tables, AFX encoder,
//! discovery endpoint ordering.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use patchbay_antelope::tables::{
    self, OUTPUT_PAGES, SOURCE_NONE, SOURCE_TYPES, sample_rate_from_bytes, sample_rate_index,
};
use patchbay_antelope::{
    AfxCatalog, AfxSlot, Announce, Call, DeviceState, FrameDecoder, MixerStrip, MonitorToggle,
    RouteSlot, ServerFrame, TrimConfig, TrimLevel, afx_order_call, control_endpoints, encode_frame,
    mixer_call, monitor_toggle_call, parse_afx_strip_reply, parse_routing_reply, routing_call,
    trim_call,
};
use serde_json::{Value, json};

const CYCLIC: &str = include_str!("fixtures/cyclic_state_galaxy32_idle.json");
const ROUTING_REPLY: &str = include_str!("fixtures/get_routing_reply_page1.json");
const MIXER_REPLY: &str = include_str!("fixtures/get_mixer_reply_mixer0.json");
const TRIM_REPLY: &str = include_str!("fixtures/get_trim_configs_reply_line_in.json");
const FAIL_REPLY: &str = include_str!("fixtures/single_fail_reply.json");

// ── Framing ───────────────────────────────────────────────────────────

#[test]
fn frame_length_includes_header() {
    let f = encode_frame(br#"{"a":1}"#).unwrap();
    assert_eq!(f.len(), 7 + 4);
    assert_eq!(&f[..4], &11u32.to_be_bytes());
    assert_eq!(&f[4..], br#"{"a":1}"#);
}

#[test]
fn frame_roundtrip() {
    let bodies: [&[u8]; 3] = [br#"{"type":"cyclic"}"#, b"[]", br#"["set_dim",[0,1],{}]"#];
    let mut d = FrameDecoder::new();
    for b in bodies {
        d.push(&encode_frame(b).unwrap());
    }
    for b in bodies {
        assert_eq!(d.next_frame().unwrap().unwrap(), b);
    }
    assert!(d.next_frame().unwrap().is_none());
    assert_eq!(d.buffered(), 0);
}

#[test]
fn incremental_decode_across_splits() {
    let big = CYCLIC.as_bytes();
    let mut stream = encode_frame(big).unwrap();
    stream.extend(encode_frame(b"{}").unwrap());
    // Every split point, and byte-at-a-time.
    for split in [1, 2, 3, 4, 5, 100, big.len(), big.len() + 4, big.len() + 5] {
        let mut d = FrameDecoder::new();
        let mut out = Vec::new();
        for chunk in [&stream[..split], &stream[split..]] {
            d.push(chunk);
            while let Some(f) = d.next_frame().unwrap() {
                out.push(f);
            }
        }
        assert_eq!(out.len(), 2, "split {split}");
        assert_eq!(out[0], big);
        assert_eq!(out[1], b"{}");
    }
    let mut d = FrameDecoder::new();
    let mut n = 0;
    for b in &stream {
        d.push(std::slice::from_ref(b));
        while d.next_frame().unwrap().is_some() {
            n += 1;
        }
    }
    assert_eq!(n, 2);
}

#[test]
fn impossible_length_is_an_error() {
    let mut d = FrameDecoder::new();
    d.push(&2u32.to_be_bytes());
    assert!(d.next_frame().is_err());
    let mut d = FrameDecoder::new();
    d.push(&u32::MAX.to_be_bytes());
    assert!(d.next_frame().is_err());
}

// ── Call encoding: byte-exact vs. the panel's calls ──────────────────

#[test]
fn set_routing_matches_panel() {
    // Captured panel call (client_rpc_log_*.json): HDX OUT 1-32.
    let expected = r#"["set_routing",[6,[[1,0],[1,1],[3,2],[3,3],[3,4],[3,5],[3,6],[3,7],[3,8],[3,9],[3,10],[3,11],[3,12],[3,13],[3,14],[3,15],[3,16],[3,17],[3,18],[3,19],[3,20],[3,21],[3,22],[3,23],[3,24],[3,25],[3,26],[3,27],[3,28],[3,29],[3,30],[3,31]]],{}]"#;
    let mut slots = vec![RouteSlot { ty: 1, ch: 0 }, RouteSlot { ty: 1, ch: 1 }];
    slots.extend((2..32).map(|ch| RouteSlot { ty: 3, ch }));
    assert_eq!(
        routing_call(6, &slots).unwrap().to_json().unwrap(),
        expected
    );
    assert!(routing_call(6, &slots[..31]).is_err());
    assert!(routing_call(19, &slots).is_err());
}

#[test]
fn set_mixer_matches_panel() {
    let s = MixerStrip {
        level: 26,
        pan: 32,
        mute: 1,
        solo: 0,
        send: 96,
    };
    assert_eq!(
        mixer_call(0, 16, &s).unwrap().to_json().unwrap(),
        r#"["set_mixer",[0,16],{"sender":16,"level":26,"pan":32,"mute":1,"solo":0,"send":96}]"#
    );
    assert!(mixer_call(4, 1, &s).is_err());
    assert!(mixer_call(0, 33, &s).is_err());
}

#[test]
fn other_calls_match_panel() {
    assert_eq!(
        monitor_toggle_call(MonitorToggle::Dim, 0, true)
            .to_json()
            .unwrap(),
        r#"["set_dim",[0,1],{}]"#
    );
    // Object keys inside args go through serde_json's (sorted) map, so
    // compare this one structurally; the server is key-order agnostic.
    let afx = afx_order_call(
        15,
        &[AfxSlot {
            effect: 59,
            inst: 6,
        }],
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&afx.to_json().unwrap()).unwrap(),
        json!(["set_afx_order", [15, [{"type": 59, "inst": 6}]], {}])
    );
    assert_eq!(
        afx_order_call(15, &[]).unwrap().to_json().unwrap(),
        r#"["set_afx_order",[15,[]],{}]"#
    );
    assert_eq!(
        Call::new("get_routing").kwarg("ext3", 6).to_json().unwrap(),
        r#"["get_routing",[],{"ext3":6}]"#
    );
    let mut levels = vec![TrimLevel { whole: 0, fract: 0 }; 64];
    levels[31] = TrimLevel { whole: 1, fract: 0 };
    let trim = trim_call(&TrimConfig {
        trim_id: 2,
        control: 1,
        levels,
    })
    .unwrap()
    .to_json()
    .unwrap();
    assert!(trim.starts_with(r#"["set_trim_config",[2,1,[[0,0],"#));
    assert!(trim.ends_with("[0,0],[1,0]]],{}]"));
}

#[test]
fn call_frame_is_length_prefixed_json() {
    let f = monitor_toggle_call(MonitorToggle::Dim, 0, false)
        .to_frame()
        .unwrap();
    let mut d = FrameDecoder::new();
    d.push(&f);
    assert_eq!(d.next_frame().unwrap().unwrap(), br#"["set_dim",[0,0],{}]"#);
}

// ── Server frames ────────────────────────────────────────────────────

#[test]
fn parse_get_routing_reply() {
    let f = ServerFrame::parse(ROUTING_REPLY.as_bytes()).unwrap();
    let ServerFrame::Single {
        header, contents, ..
    } = &f
    else {
        panic!("not single: {f:?}")
    };
    assert_eq!(header.unwrap().key(), (3, 1));
    assert_eq!(header.unwrap().cmd, 117);
    let page = parse_routing_reply(1, contents).unwrap();
    assert_eq!(page.slots.len(), 32);
    // MONITOR page is fed by SURROUND OUT 1-2.
    assert_eq!(page.slots[0], RouteSlot { ty: 17, ch: 0 });
    assert_eq!(page.slots[1], RouteSlot { ty: 17, ch: 1 });
    assert_eq!(tables::source_type(17).unwrap().name, "SURROUND OUT");
}

#[test]
fn parse_cyclic_state() {
    let f = ServerFrame::parse(CYCLIC.as_bytes()).unwrap();
    let ServerFrame::Cyclic {
        header, contents, ..
    } = &f
    else {
        panic!("not cyclic")
    };
    assert_eq!(header.cmd, 115);
    let s = DeviceState::from_cyclic(contents).unwrap();
    assert_eq!(s.sample_rate, 48_000);
    assert_eq!(s.sync_source, 11);
    assert!(s.locked);
    assert!(!s.monitor.dim);
    assert_eq!(s.monitor.volume, 33);
}

#[test]
fn parse_mixer_and_trim_replies() {
    let ServerFrame::Single { contents, .. } = ServerFrame::parse(MIXER_REPLY.as_bytes()).unwrap()
    else {
        panic!()
    };
    let strips: Vec<MixerStrip> = serde_json::from_value(contents).unwrap();
    assert_eq!(strips.len(), 33);
    assert_eq!(strips[0].pan, 32);

    let ServerFrame::Single {
        header, contents, ..
    } = ServerFrame::parse(TRIM_REPLY.as_bytes()).unwrap()
    else {
        panic!()
    };
    assert_eq!(header.unwrap().key(), (15, 2));
    let t: TrimConfig = serde_json::from_value(contents).unwrap();
    assert_eq!(t.trim_id, 2);
    assert_eq!(t.levels.len(), 64);
    assert!((t.levels[0].dbu() - 22.0).abs() < 1e-9);
}

#[test]
fn parse_headerless_failure() {
    let f = ServerFrame::parse(FAIL_REPLY.as_bytes()).unwrap();
    assert!(f.is_failure());
    assert!(f.header().is_none());
}

#[test]
fn parse_notification_call() {
    let raw = json!({
        "type": "notification",
        "contents": ["set_mixer", [0, 16], {"sender": 16, "level": 26, "pan": 32, "mute": 1, "solo": 0, "send": 96}]
    });
    let f = ServerFrame::parse(raw.to_string().as_bytes()).unwrap();
    let call = f.notified_call().unwrap();
    assert_eq!(call.method, "set_mixer");
    assert_eq!(call.args, vec![json!(0), json!(16)]);
    assert_eq!(call.kwarg_value("level"), Some(&json!(26)));
    // Admin-port notifications carry text and don't parse as calls.
    let admin = ServerFrame::parse(
        br#"{"type":"notification","contents":"Status: running\n","state":"running"}"#,
    )
    .unwrap();
    assert!(admin.notified_call().is_none());
}

#[test]
fn parse_afx_strip() {
    let v = json!([{"slots": [{"type": 45, "inst": 0}, {"type": 59, "inst": 0}, {"type": 0, "inst": 0}]}]);
    assert_eq!(
        parse_afx_strip_reply(&v).unwrap(),
        vec![
            AfxSlot {
                effect: 45,
                inst: 0
            },
            AfxSlot {
                effect: 59,
                inst: 0
            }
        ]
    );
}

// ── Tables ────────────────────────────────────────────────────────────

#[test]
fn sample_rate_decode() {
    assert_eq!(sample_rate_from_bytes(0, 187, 128), 48_000);
    assert_eq!(sample_rate_from_bytes(0, 172, 68), 44_100);
    assert_eq!(sample_rate_from_bytes(2, 238, 0), 192_000);
    assert_eq!(sample_rate_index(48_000), Some(2));
    assert_eq!(sample_rate_index(47_999), None);
}

#[test]
fn page_and_type_tables() {
    assert_eq!(OUTPUT_PAGES.len(), 19);
    for (i, p) in OUTPUT_PAGES.iter().enumerate() {
        assert_eq!(usize::from(p.page), i, "pages are dense and ordered");
    }
    for (i, s) in SOURCE_TYPES.iter().enumerate() {
        assert_eq!(usize::from(s.ty), i, "types are dense and ordered");
    }
    let p6 = tables::output_page(6).unwrap();
    assert_eq!(
        (p6.id, p6.name, p6.channels),
        ("DIGI_OUT0", "HDX OUT 1-32", 32)
    );
    assert_eq!(tables::output_page(7).unwrap().name, "HDX OUT 33-64");
    assert_eq!(
        tables::output_page(1).unwrap().channels,
        tables::MONITOR_CHANNELS
    );
    assert_eq!(tables::output_page_by_id("MIXER_IN0").unwrap().page, 13);
    assert_eq!(tables::source_type(0).unwrap().name, "LINE IN 1-32");
    assert_eq!(tables::source_type(12).unwrap().name, "MIX1 OUT");
    assert_eq!(tables::source_type(12).unwrap().channels, 2);
    assert!(tables::source_type(SOURCE_NONE).is_none());
    assert_eq!(tables::source_type_by_id("DANTE_IN1").unwrap().ty, 4);
    assert_eq!(tables::SYNC_SOURCES[0], "INTERNAL");
    assert_eq!(tables::SYNC_SOURCES[11], "DANTE");
}

// ── AFX ───────────────────────────────────────────────────────────────

#[test]
fn afx_catalog_from_schema() {
    let cat = AfxCatalog::load().unwrap();
    let opto = cat.by_name("opto2a").unwrap();
    assert_eq!(opto.type_id, Some(59));
    let names: Vec<&str> = opto.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["meter", "limit", "gain", "peak"]);
    // Cross-checks against captured set_afx_order / set_*_conf traffic.
    assert_eq!(cat.by_name("neve_1073").unwrap().type_id, Some(7));
    assert_eq!(cat.by_name("api_550").unwrap().type_id, Some(8));
    assert_eq!(cat.by_name("altec_436c").unwrap().type_id, Some(35));
    assert_eq!(cat.by_name("antelope_tremolo").unwrap().type_id, Some(53));
    assert_eq!(cat.by_name("antares_autotune").unwrap().type_id, Some(52));
    assert_eq!(cat.by_type(59).unwrap().name, "opto2a");
    assert!(!cat.by_name("guitar_cab").unwrap().generic);
    assert!(cat.insertable().count() > 60);
}

#[test]
fn afx_encode_matches_panel() {
    let cat = AfxCatalog::load().unwrap();
    let opto = cat.by_name("opto2a").unwrap();
    let vals: Vec<Value> = [0, 1, 31, 1].into_iter().map(Value::from).collect();
    assert_eq!(
        opto.encode(6, &vals).unwrap().to_json().unwrap(),
        r#"["set_opto2a_conf",[59,6,0,1,31,1],{}]"#
    );
    let named = json!({"meter": 0, "limit": 1, "gain": 31, "peak": 1});
    assert_eq!(
        opto.encode_named(6, named.as_object().unwrap()).unwrap(),
        opto.encode(6, &vals).unwrap()
    );
    assert!(opto.encode(6, &vals[..3]).is_err(), "arity");
    let bad: Vec<Value> = [0, 1, 300, 1].into_iter().map(Value::from).collect();
    assert!(opto.encode(6, &bad).is_err(), "u8 range");
    assert!(
        opto.encode_named(6, json!({"gain": 1}).as_object().unwrap())
            .is_err()
    );
    // Captured: ["set_rd_47_conf", [47, 1, 57, 55], {}]
    let rd = cat.by_name("rd_47").unwrap();
    assert_eq!(
        rd.encode(1, &[json!(57), json!(55)])
            .unwrap()
            .to_json()
            .unwrap(),
        r#"["set_rd_47_conf",[47,1,57,55],{}]"#
    );
}

// ── Discovery ─────────────────────────────────────────────────────────

fn announce(ip: &str, port: u16, service: &str, serial: &str) -> Announce {
    serde_json::from_value(json!({
        "ip": ip, "port": port, "uuid": format!("u{port}"), "name": "x", "type": service,
        "protocol": "TCP", "interval": 500.0,
        "properties": {"device_name": "Galaxy32", "serial_number": serial, "firmware_version": "8.24",
                       "server_version": "1.8.19", "mode": "app"}
    }))
    .unwrap()
}

#[test]
fn control_endpoint_ordering() {
    let c = patchbay_antelope::CONTROL_SERVICE;
    let a = vec![
        announce("127.0.0.1", 2020, patchbay_antelope::ADMIN_SERVICE, ""),
        announce("127.0.0.1", 2022, c, "4202524000109"),
        announce("127.0.0.1", 2023, c, "4202524000109"),
        announce("192.168.1.116", 2023, c, "4202524000109"),
        announce("127.0.0.1", 2030, c, "other"),
    ];
    let eps: Vec<String> = control_endpoints(&a, Some("4202524000109"))
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        eps,
        ["127.0.0.1:2023", "127.0.0.1:2022", "192.168.1.116:2023"]
    );
    assert_eq!(control_endpoints(&a, None).len(), 4);
    assert_eq!(a[1].properties.extra.get("mode"), Some(&json!("app")));
}
