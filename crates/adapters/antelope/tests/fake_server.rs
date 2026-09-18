//! Client + adapter against an in-process fake Manager Server: reply
//! correlation, FAIL handling, notifications, and the adapter's
//! read-modify-write + read-back behaviour (which page/strip it touches).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::many_single_char_names,
    clippy::needless_pass_by_value
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use patchbay_antelope::{
    Announce, Call, Client, ClientEvent, FrameDecoder, Galaxy32Adapter, ServerFrame, encode_frame,
};
use patchbay_device::{
    ChannelRef, DeviceAdapter, DeviceError, DeviceEvent, ParamValue, WriteGuard,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

const CYCLIC: &str = include_str!("fixtures/cyclic_state_galaxy32_idle.json");

#[derive(Default)]
struct State {
    routing: HashMap<u64, Vec<[u64; 2]>>,
    mixers: HashMap<u64, Vec<Value>>,
    afx: HashMap<u64, Vec<Value>>,
    dim: u64,
    calls: Vec<Call>,
    initialized: bool,
}

impl State {
    fn new() -> Self {
        let mut s = Self::default();
        for p in 0..19 {
            s.routing.insert(p, (0..64).map(|i| [0, i % 32]).collect());
        }
        for m in 0..4 {
            s.mixers.insert(
                m,
                (0..33)
                    .map(|_| json!({"level": 0, "pan": 32, "mute": 0, "solo": 0, "send": 96}))
                    .collect(),
            );
        }
        // Opto 2A (59) instances 0..=5 in use elsewhere → next free is 6.
        for (strip, inst) in [(24u64, 0u64), (25, 1), (31, 2), (30, 3), (29, 4), (28, 5)] {
            s.afx.insert(strip, vec![json!({"type": 59, "inst": inst})]);
        }
        s
    }
}

fn single(ext2: u64, ext3: u64, contents: Value) -> Value {
    json!({"type": "single", "protocol_version": 1,
           "header": {"cmd": 117, "seq": 1, "ext2": ext2, "ext3": ext3}, "contents": contents})
}

fn cyclic(dim: u64) -> Value {
    let mut v: Value = serde_json::from_str(CYCLIC).unwrap();
    v["contents"]["volumes_and_mutes"][0]["dim"] = json!(dim);
    v
}

/// Handle one call; returns the reply to send, if any.
fn handle(st: &mut State, call: &Call) -> Option<Value> {
    let ext3 = call
        .kwarg_value("ext3")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let arg = |i: usize| call.args.get(i).and_then(Value::as_u64).unwrap();
    match call.method.as_str() {
        "get_routing" => {
            let cfg: Vec<Value> = st.routing[&ext3]
                .iter()
                .map(|[t, c]| json!({"in_periph_id": t, "in_chann": c}))
                .collect();
            Some(single(
                3,
                ext3,
                json!({"bank_idx": ext3, "bank_configs": cfg}),
            ))
        }
        "set_routing" => {
            let page = arg(0);
            let cells = call.args[1].as_array().unwrap();
            let slots = st.routing.get_mut(&page).unwrap();
            for (i, c) in cells.iter().enumerate() {
                slots[i] = [c[0].as_u64().unwrap(), c[1].as_u64().unwrap()];
            }
            None
        }
        "get_mixer" => Some(single(4, ext3, Value::Array(st.mixers[&ext3].clone()))),
        "set_mixer" => {
            let mut strip = json!({});
            for k in ["level", "pan", "mute", "solo", "send"] {
                strip[k] = call.kwarg_value(k).unwrap().clone();
            }
            st.mixers.get_mut(&arg(0)).unwrap()[usize::try_from(arg(1)).unwrap()] = strip;
            None
        }
        "get_afx_strip_order" => {
            let mut slots = st.afx.get(&ext3).cloned().unwrap_or_default();
            slots.resize(8, json!({"type": 0, "inst": 0}));
            Some(single(25, ext3, json!([{"slots": slots}])))
        }
        "set_afx_order" => {
            st.afx
                .insert(arg(0), call.args[1].as_array().unwrap().clone());
            None
        }
        "get_afx_available_instances" => {
            Some(single(12, 0, json!([{"type_id": 59, "inst_count": 2}])))
        }
        "set_dim" => {
            st.dim = arg(1);
            None
        }
        "get_reverb_config" => Some(single(
            10,
            ext3,
            json!({"mixer_id": ext3, "room_size": 81, "color": 100, "predelay": 0, "density": 100,
                   "early_ref_gain": 11, "late_ref_delay": 13, "richness": 24, "reverb_time": 66,
                   "reverb_level": 50, "on": 1}),
        )),
        "get_trim_configs" => {
            let levels: Vec<Value> = (0..64)
                .map(|i| json!({"whole": u64::from(i == 31), "fract": 0}))
                .collect();
            Some(single(
                15,
                ext3,
                json!({"trim_id": ext3, "control": 1, "levels": levels}),
            ))
        }
        "get_bogus" => Some(json!({"type": "single", "contents": "", "COMMAND_STATUS": "FAIL"})),
        _ => None,
    }
}

async fn serve(stream: TcpStream, st: Arc<Mutex<State>>) {
    let (mut r, w) = stream.into_split();
    let w = Arc::new(Mutex::new(w));
    // 10 Hz cyclic, reflecting `dim`.
    let (w2, st2) = (Arc::clone(&w), Arc::clone(&st));
    tokio::spawn(async move {
        loop {
            let dim = st2.lock().await.dim;
            let frame = encode_frame(cyclic(dim).to_string().as_bytes()).unwrap();
            if w2.lock().await.write_all(&frame).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let mut d = FrameDecoder::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = r.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        d.push(&buf[..n]);
        while let Some(body) = d.next_frame().unwrap() {
            let v: Value = serde_json::from_slice(&body).unwrap();
            let call = Call::from_value(&v).unwrap();
            let mut s = st.lock().await;
            if call.method == "initialize_format" {
                s.initialized = true;
                continue;
            }
            assert!(s.initialized, "handshake must come first");
            let reply = handle(&mut s, &call);
            s.calls.push(call.clone());
            drop(s);
            if let Some(reply) = reply {
                let frame = encode_frame(reply.to_string().as_bytes()).unwrap();
                w.lock().await.write_all(&frame).await.unwrap();
            }
            // Rebroadcast like auto_send_notification (to the same client,
            // which is enough to exercise the parsing path).
            if call.method == "set_mixer" {
                let n = json!({"type": "notification", "contents": [call.method, call.args,
                    Value::Object(call.kwargs.iter().cloned().collect())]});
                let frame = encode_frame(n.to_string().as_bytes()).unwrap();
                w.lock().await.write_all(&frame).await.unwrap();
            }
        }
    }
}

async fn fake() -> (SocketAddr, Arc<Mutex<State>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let st = Arc::new(Mutex::new(State::new()));
    let st2 = Arc::clone(&st);
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(serve(s, Arc::clone(&st2)));
        }
    });
    (addr, st)
}

fn announce(addr: SocketAddr) -> Announce {
    serde_json::from_value(json!({
        "ip": addr.ip().to_string(), "port": addr.port(), "uuid": "u", "name": "n",
        "type": patchbay_antelope::CONTROL_SERVICE,
        "properties": {"device_name": "Galaxy32", "serial_number": "4202524000109",
                       "firmware_version": "8.24", "server_version": "1.8.19"}
    }))
    .unwrap()
}

#[tokio::test]
async fn client_correlates_replies_and_failures() {
    let (addr, _st) = fake().await;
    let c = Client::connect(addr).await.unwrap();
    let mut events = c.subscribe();
    let kw = |i: u64| vec![("ext3".to_owned(), json!(i))];
    // Concurrent requests for different keys resolve independently.
    let (a, b) = tokio::join!(
        c.request("get_routing", vec![], kw(6), (3, 6)),
        c.request("get_mixer", vec![], kw(2), (4, 2)),
    );
    assert_eq!(a.unwrap()["bank_idx"], json!(6));
    assert_eq!(b.unwrap().as_array().unwrap().len(), 33);
    // Header-less FAIL fails the oldest pending request.
    let err = c
        .request("get_bogus", vec![], vec![], (99, 0))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("get_bogus"), "{err}");
    // Cyclic state arrives and is cached.
    assert!(
        matches!(events.recv().await.unwrap(), ClientEvent::Frame(f) if matches!(*f, ServerFrame::Cyclic { .. }))
    );
    assert!(c.latest_state().is_some());
}

#[tokio::test]
async fn adapter_route_rmw_touches_only_target_page() {
    let (addr, st) = fake().await;
    let a = Galaxy32Adapter::connect(addr, &announce(addr))
        .await
        .unwrap();
    assert_eq!(
        DeviceAdapter::info(&a).id.as_str(),
        "antelope:galaxy32:4202524000109"
    );
    let mut ev = DeviceAdapter::subscribe(&a);
    st.lock().await.calls.clear();

    a.set_route(
        ChannelRef::new("DIGI_OUT0", 3),
        Some(ChannelRef::new("LINE_IN0", 5)),
    )
    .await
    .unwrap();
    {
        let s = st.lock().await;
        let sets: Vec<&Call> = s
            .calls
            .iter()
            .filter(|c| c.method.starts_with("set_"))
            .collect();
        assert_eq!(sets.len(), 1, "exactly one write");
        assert_eq!(sets[0].method, "set_routing");
        assert_eq!(sets[0].args[0], json!(6));
        let cells = sets[0].args[1].as_array().unwrap();
        assert_eq!(cells.len(), 32);
        assert_eq!(cells[3], json!([0, 5]));
        // Every other slot re-sent unchanged.
        assert_eq!(cells[4], json!([0, 4]));
        assert_eq!(s.routing[&6][3], [0, 5]);
        assert_eq!(s.routing[&7][3], [0, 3], "page 7 untouched");
    }
    let got = loop {
        if let DeviceEvent::RouteChanged(c) = ev.recv().await.unwrap() {
            break c;
        }
    };
    assert_eq!(got.source, Some(ChannelRef::new("LINE_IN0", 5)));

    // Unpatch → type 18.
    a.set_route(ChannelRef::new("DIGI_OUT1", 0), None)
        .await
        .unwrap();
    assert_eq!(st.lock().await.routing[&7][0], [18, 0]);

    // Bad ports are rejected before any I/O.
    assert!(matches!(
        a.set_route(ChannelRef::new("DIGI_OUT0", 32), None).await,
        Err(DeviceError::UnknownPort(_))
    ));
    assert!(matches!(
        a.set_route(
            ChannelRef::new("ADAT_OUT0", 0),
            Some(ChannelRef::new("ADAT_IN0", 8))
        )
        .await,
        Err(DeviceError::UnknownPort(_))
    ));
}

#[tokio::test]
async fn adapter_mixer_monitor_afx_and_guards() {
    let (addr, st) = fake().await;
    let a = Galaxy32Adapter::connect(addr, &announce(addr))
        .await
        .unwrap();

    a.set_param("mixer/1/strip/16/level", ParamValue::Level(-10.0))
        .await
        .unwrap();
    a.set_param("mixer/1/strip/16/pan", ParamValue::Pan(-1.0))
        .await
        .unwrap();
    {
        let s = st.lock().await;
        assert_eq!(
            s.mixers[&0][16],
            json!({"level": 10, "pan": 2, "mute": 0, "solo": 0, "send": 96})
        );
        assert_eq!(s.mixers[&0][15]["level"], json!(0), "neighbour untouched");
        let last = s.calls.iter().rfind(|c| c.method == "set_mixer").unwrap();
        // (The fake re-parses through a sorted map, so compare fields.)
        assert_eq!(last.args, vec![json!(0), json!(16)]);
        assert_eq!(last.kwarg_value("sender"), Some(&json!(16)));
        assert_eq!(last.kwarg_value("level"), Some(&json!(10)));
    }

    a.set_param("monitor/dim", ParamValue::Toggle(true))
        .await
        .unwrap();
    assert_eq!(st.lock().await.dim, 1);
    a.set_param("monitor/dim", ParamValue::Toggle(false))
        .await
        .unwrap();

    // AFX: insert Opto 2A on AFX 16 → lowest free instance (6).
    a.set_param("afx/strip/16/slot/1/effect", ParamValue::Enum(59))
        .await
        .unwrap();
    assert_eq!(
        st.lock().await.afx[&15],
        vec![json!({"type": 59, "inst": 6})]
    );
    // Single-field AFX write needs known values first.
    assert!(matches!(
        a.set_param("afx/strip/16/slot/1/gain", ParamValue::Int(40))
            .await,
        Err(DeviceError::Unsupported(_))
    ));
    a.set_afx_conf(16, 1, &[json!(0), json!(1), json!(31), json!(1)])
        .await
        .unwrap();
    a.set_param("afx/strip/16/slot/1/gain", ParamValue::Int(40))
        .await
        .unwrap();
    // A read round-trip flushes the fire-and-forget writes before it.
    a.device().get_afx_strip(15).await.unwrap();
    {
        let s = st.lock().await;
        let conf: Vec<String> = s
            .calls
            .iter()
            .filter(|c| c.method == "set_opto2a_conf")
            .map(|c| c.to_json().unwrap())
            .collect();
        assert_eq!(
            conf,
            [
                r#"["set_opto2a_conf",[59,6,0,1,31,1],{}]"#,
                r#"["set_opto2a_conf",[59,6,0,1,40,1],{}]"#
            ]
        );
    }
    a.set_param("afx/strip/16/slot/1/effect", ParamValue::Enum(0))
        .await
        .unwrap();
    assert!(st.lock().await.afx[&15].is_empty());

    // Guards.
    assert!(matches!(
        a.set_param("clock/sample_rate", ParamValue::Enum(1)).await,
        Err(DeviceError::DisruptiveWrite(_))
    ));
    assert!(matches!(
        a.apply_param(
            "clock/measured_rate",
            ParamValue::Int(1),
            WriteGuard::AllowDisruptive
        )
        .await,
        Err(DeviceError::ReadOnly(_))
    ));
    assert!(matches!(
        a.set_param("mixer/1/strip/16/level", ParamValue::Toggle(true))
            .await,
        Err(DeviceError::InvalidValue { .. })
    ));
    assert!(matches!(
        a.set_param("mixer/5/strip/1/level", ParamValue::Level(0.0))
            .await,
        Err(DeviceError::UnknownParam(_))
    ));
    let s = st.lock().await;
    assert!(
        !s.calls
            .iter()
            .any(|c| c.method == "set_samp_rate" || c.method == "set_sync_source")
    );
}

#[tokio::test]
async fn adapter_emits_notified_writes() {
    let (addr, _st) = fake().await;
    let a = Galaxy32Adapter::connect(addr, &announce(addr))
        .await
        .unwrap();
    let mut ev = DeviceAdapter::subscribe(&a);
    a.device()
        .raw_call(
            "set_mixer",
            vec![json!(1), json!(3)],
            ["level", "pan", "mute", "solo", "send"]
                .into_iter()
                .zip([json!(6), json!(62), json!(1), json!(0), json!(96)])
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
        .await
        .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 5 {
        if let Ok(Ok(DeviceEvent::ParamChanged { path, value })) =
            tokio::time::timeout(Duration::from_secs(2), ev.recv()).await
        {
            if path.starts_with("mixer/2/strip/3/") {
                seen.push((path, value));
            }
        } else {
            panic!("missing notification events: {seen:?}");
        }
    }
    assert!(seen.contains(&("mixer/2/strip/3/level".into(), ParamValue::Level(-6.0))));
    assert!(seen.contains(&("mixer/2/strip/3/pan".into(), ParamValue::Pan(1.0))));
    assert!(seen.contains(&("mixer/2/strip/3/mute".into(), ParamValue::Toggle(true))));
}

#[tokio::test]
async fn snapshot_maps_to_generic_model() {
    let (addr, _st) = fake().await;
    let a = Galaxy32Adapter::connect(addr, &announce(addr))
        .await
        .unwrap();
    let snap = a.snapshot().await.unwrap();
    assert_eq!(snap.outputs.len(), 19);
    assert_eq!(snap.inputs.len(), 18);
    let expected_routes: u32 = snap.outputs.iter().map(|g| u32::from(g.channels)).sum();
    assert_eq!(u32::try_from(snap.routes.len()).unwrap(), expected_routes);
    // Fake page 6 slot 3 = [0, 3] → LINE IN 4.
    assert_eq!(
        snap.source_of(&ChannelRef::new("DIGI_OUT0", 3)),
        Some(Some(&ChannelRef::new("LINE_IN0", 3)))
    );
    let p = |path: &str| {
        snap.param(path)
            .unwrap_or_else(|| panic!("missing {path}"))
            .clone()
    };
    assert_eq!(p("mixer/1/strip/16/pan").value, ParamValue::Pan(0.0));
    assert_eq!(p("mixer/4/master/send").value, ParamValue::Level(-96.0));
    assert_eq!(p("mixer/1/reverb/room_size").value, ParamValue::Int(81));
    assert_eq!(p("trim/line_in/32").value, ParamValue::Level(21.0));
    assert_eq!(p("trim/line_in/control").value, ParamValue::Enum(1));
    assert_eq!(p("clock/measured_rate").value, ParamValue::Int(48_000));
    assert_eq!(p("clock/sample_rate").value, ParamValue::Enum(2));
    assert!(p("clock/sample_rate").disruptive);
    assert!(!p("clock/locked").writable);
    assert_eq!(p("afx/strip/25/slot/1/effect").value, ParamValue::Enum(59));
    assert_eq!(p("afx/strip/16/slot/1/effect").value, ParamValue::Enum(0));
    assert!(snap.info.online);
}
