//! An in-process fake Antelope Manager Server on a loopback port: the
//! Galaxy32's routing pages, mixers, AFX and cyclic state, answering the
//! client's calls and logging them. Shared by the tests (`mod common;`).
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::many_single_char_names,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use patchbay_antelope::{Announce, Call, FrameDecoder, encode_frame};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub const CYCLIC: &str = include_str!("../fixtures/antelope/cyclic_state_galaxy32_idle.json");

#[derive(Default)]
pub struct State {
    pub routing: HashMap<u64, Vec<[u64; 2]>>,
    pub mixers: HashMap<u64, Vec<Value>>,
    pub afx: HashMap<u64, Vec<Value>>,
    pub dim: u64,
    pub calls: Vec<Call>,
    pub initialized: bool,
}

impl State {
    pub fn new() -> Self {
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

pub fn single(ext2: u64, ext3: u64, contents: Value) -> Value {
    json!({"type": "single", "protocol_version": 1,
           "header": {"cmd": 117, "seq": 1, "ext2": ext2, "ext3": ext3}, "contents": contents})
}

pub fn cyclic(dim: u64) -> Value {
    let mut v: Value = serde_json::from_str(CYCLIC).unwrap();
    v["contents"]["volumes_and_mutes"][0]["dim"] = json!(dim);
    v
}

/// Handle one call; returns the reply to send, if any.
pub fn handle(st: &mut State, call: &Call) -> Option<Value> {
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

pub async fn serve(stream: TcpStream, st: Arc<Mutex<State>>) {
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

pub async fn fake() -> (SocketAddr, Arc<Mutex<State>>) {
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

pub fn announce(addr: SocketAddr) -> Announce {
    serde_json::from_value(json!({
        "ip": addr.ip().to_string(), "port": addr.port(), "uuid": "u", "name": "n",
        "type": patchbay_antelope::CONTROL_SERVICE,
        "properties": {"device_name": "Galaxy32", "serial_number": "4202524000109",
                       "firmware_version": "8.24", "server_version": "1.8.19"}
    }))
    .unwrap()
}
