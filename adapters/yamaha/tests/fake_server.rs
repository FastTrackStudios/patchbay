//! Client + adapter against an in-process fake TF1: reply correlation,
//! split reads, write confirmation, NOTIFY → events, scene-recall re-read,
//! guards, keepalive pings and reconnect.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    clippy::float_cmp,
    clippy::needless_pass_by_value,
    clippy::many_single_char_names
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use patchbay_device::{
    ChannelRef, DeviceAdapter, DeviceError, DeviceEvent, ParamValue, WriteGuard,
};
use patchbay_yamaha::rcp::token::{quote, tokenize};
use patchbay_yamaha::{
    Client, ClientEvent, ClientOptions, Command, PrmInfo, PrmType, RcpValue, SceneBank,
    TF1_PRMINFO_JSON, TfAdapter, TfError, TfOptions,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast, mpsc};

// ── fake console ──────────────────────────────────────────────────────

struct FakeState {
    rows: Vec<PrmInfo>,
    lines: Vec<String>,
    values: HashMap<(String, u16, u16), RcpValue>,
    log: Vec<String>,
    scene: (SceneBank, u8),
    connections: usize,
}

impl FakeState {
    fn row(&self, address: &str) -> Option<&PrmInfo> {
        self.rows.iter().find(|r| r.address == address)
    }

    fn value(&self, address: &str, x: u16, y: u16) -> RcpValue {
        if let Some(v) = self.values.get(&(address.to_owned(), x, y)) {
            return v.clone();
        }
        let row = self.row(address).unwrap();
        match row.ty {
            PrmType::Integer => row.default.clone(),
            _ => RcpValue::Str(if address.ends_with("/Color") {
                "Blue".to_owned()
            } else if address.ends_with("/Icon") {
                "Blank".to_owned()
            } else if address.ends_with("/Category") {
                "Others".to_owned()
            } else if address.ends_with("/Name") {
                format!("ch{}", x + 1)
            } else {
                "Mono".to_owned()
            }),
        }
    }
}

fn wire(v: &RcpValue) -> String {
    match v {
        RcpValue::Int(n) => n.to_string(),
        RcpValue::Str(s) => quote(s).unwrap(),
    }
}

fn handle(st: &mut FakeState, line: &str) -> String {
    st.log.push(line.to_owned());
    let t = tokenize(line);
    let verb = t.first().map_or("", |t| t.text.as_str());
    let arg = |i: usize| t.get(i).map_or("", |t| t.text.as_str()).to_owned();
    let bank = |s: &str| match s {
        "scene_a" => Some(SceneBank::A),
        "scene_b" => Some(SceneBank::B),
        _ => None,
    };
    match verb {
        "devinfo" => {
            let v = match arg(1).as_str() {
                "productname" => "TF1",
                "version" => "V4.55",
                _ => "",
            };
            format!("OK devinfo {} \"{v}\"", arg(1))
        }
        "devstatus" => "OK devstatus runmode \"normal\"".to_owned(),
        "scpmode" => format!("OK scpmode {} {}", arg(1), arg(2)),
        "prminfo" => {
            let i: usize = arg(1).parse().unwrap();
            st.lines
                .get(i)
                .cloned()
                .unwrap_or_else(|| "ERROR prminfo InternalError".to_owned())
        }
        "get" | "set" => {
            let (a, x, y) = (arg(1), arg(2).parse::<u16>(), arg(3).parse::<u16>());
            let (Ok(x), Ok(y)) = (x, y) else {
                return format!("ERROR {verb} WrongFormat");
            };
            let Some(row) = st.row(&a) else {
                return format!("ERROR {verb} UnknownAddress");
            };
            if x >= row.x_count.max(1) || y >= row.y_count.max(1) {
                return format!("ERROR {verb} InvalidArgument");
            }
            if verb == "set" {
                let v = RcpValue::from_token(&t[4]);
                // The fake refuses the `Kick` icon, like a console that
                // doesn't know a name.
                if v == RcpValue::Str("Kick".to_owned()) {
                    return "ERROR set InvalidArgument".to_owned();
                }
                // Clamp levels like the console does.
                let v = match v {
                    RcpValue::Int(n) if row.unit == "dB" => RcpValue::Int(n.min(row.max)),
                    other => other,
                };
                st.values.insert((a.clone(), x, y), v.clone());
                return format!("OK set {a} {x} {y} {} \"disp\"", wire(&v));
            }
            format!("OK get {a} {x} {y} {}", wire(&st.value(&a, x, y)))
        }
        "sscurrent_ex" => match bank(&arg(1)) {
            Some(b) if b == st.scene.0 => format!("OK sscurrent_ex {} {}", arg(1), st.scene.1),
            _ => "ERROR sscurrent_ex InvalidArgument".to_owned(),
        },
        "ssinfo_ex" => format!(
            "OK ssinfo_ex {} {} \"X\" \"Title {}\" \"\" user",
            arg(1),
            arg(2),
            arg(2)
        ),
        "ssrecall_ex" => {
            let b = bank(&arg(1)).unwrap();
            let n: u8 = arg(2).parse().unwrap();
            st.scene = (b, n);
            // A recall changes params without per-param NOTIFYs.
            st.values.insert(
                ("MIXER:Current/InCh/Fader/Level".to_owned(), 9, 0),
                RcpValue::Int(-600),
            );
            format!("OK ssrecall_ex {} {n}", arg(1))
        }
        other => format!("ERROR {other} UnknownCommand"),
    }
}

#[derive(Clone)]
struct Fake {
    addr: SocketAddr,
    state: Arc<Mutex<FakeState>>,
    inject: broadcast::Sender<String>,
    kill: broadcast::Sender<()>,
}

impl Fake {
    async fn start() -> Self {
        let lines: Vec<String> = serde_json::from_str(TF1_PRMINFO_JSON).unwrap();
        let rows = lines
            .iter()
            .map(|l| PrmInfo::parse_line(l).unwrap())
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Self {
            addr: listener.local_addr().unwrap(),
            state: Arc::new(Mutex::new(FakeState {
                rows,
                lines,
                values: HashMap::new(),
                log: Vec::new(),
                scene: (SceneBank::B, 22),
                connections: 0,
            })),
            inject: broadcast::channel(64).0,
            kill: broadcast::channel(4).0,
        };
        let f = fake.clone();
        tokio::spawn(async move {
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                f.state.lock().await.connections += 1;
                tokio::spawn(f.clone().serve(sock));
            }
        });
        fake
    }

    async fn serve(self, sock: TcpStream) {
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        let mut inject = self.inject.subscribe();
        let mut kill = self.kill.subscribe();
        loop {
            tokio::select! {
                l = lines.next_line() => {
                    let Ok(Some(l)) = l else { return };
                    if l.is_empty() { continue; }
                    let reply = handle(&mut *self.state.lock().await, &l);
                    if w.write_all(format!("{reply}\n").as_bytes()).await.is_err() { return; }
                }
                Ok(l) = inject.recv() => {
                    if w.write_all(format!("{l}\n").as_bytes()).await.is_err() { return; }
                }
                _ = kill.recv() => return,
            }
        }
    }

    async fn log(&self) -> Vec<String> {
        self.state.lock().await.log.clone()
    }

    async fn clear_log(&self) {
        self.state.lock().await.log.clear();
    }

    async fn count(&self, prefix: &str) -> usize {
        self.log()
            .await
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
}

fn opts() -> TfOptions {
    TfOptions {
        client: ClientOptions {
            ping_interval: None,
            backoff_min: Duration::from_millis(20),
            backoff_max: Duration::from_millis(100),
            ..ClientOptions::default()
        },
        rescan_debounce: Duration::from_millis(30),
        ..TfOptions::default()
    }
}

async fn next_event(
    rx: &mut broadcast::Receiver<DeviceEvent>,
    pred: impl Fn(&DeviceEvent) -> bool + Send + Sync,
) -> DeviceEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ev = rx.recv().await.unwrap();
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .expect("event not seen in time")
}

fn changed(path: &'static str) -> impl Fn(&DeviceEvent) -> bool + Send + Sync {
    move |e| matches!(e, DeviceEvent::ParamChanged { path: p, .. } if p == path)
}

fn value_of(ev: DeviceEvent) -> ParamValue {
    match ev {
        DeviceEvent::ParamChanged { value, .. } => value,
        other => panic!("not a ParamChanged: {other:?}"),
    }
}

// ── adapter ───────────────────────────────────────────────────────────

#[tokio::test]
async fn connect_reads_everything_read_only() {
    let fake = Fake::start().await;
    let a = TfAdapter::connect_with(fake.addr, opts()).await.unwrap();
    let info = DeviceAdapter::info(&a);
    assert_eq!(info.id.as_str(), "yamaha:tf1:127.0.0.1");
    assert_eq!(info.model, "TF1");
    assert_eq!(info.firmware.as_deref(), Some("V4.55"));
    assert!(info.online);

    let snap = a.cached_snapshot();
    assert_eq!(snap.params.len(), 4376);
    assert!(snap.inputs.is_empty() && snap.outputs.is_empty() && snap.routes.is_empty());
    assert_eq!(
        snap.param("in/1/name").unwrap().value,
        ParamValue::Text("ch1".into())
    );
    assert_eq!(
        snap.param("in/1/level").unwrap().value,
        ParamValue::Level(f64::NEG_INFINITY)
    );
    assert_eq!(
        snap.param("dca/1/level").unwrap().value,
        ParamValue::Level(0.0)
    );
    assert_eq!(
        snap.param("scene/current").unwrap().value,
        ParamValue::Text("B22".into())
    );
    assert_eq!(
        snap.param("scene/title").unwrap().value,
        ParamValue::Text("Title 22".into())
    );

    let log = fake.log().await;
    assert_eq!(
        log.first().map(String::as_str),
        Some("scpmode keepalive 10000")
    );
    let read_only = [
        "scpmode keepalive ",
        "devinfo ",
        "prminfo ",
        "get ",
        "sscurrent_ex ",
        "ssinfo_ex ",
    ];
    for l in &log {
        assert!(
            read_only.iter().any(|p| l.starts_with(p)),
            "non-read command: {l}"
        );
    }
    assert!(
        !log.iter().any(|l| l.contains("MonitorMix")),
        "never touch the password"
    );
    assert!(
        !log.iter()
            .any(|l| l.starts_with("get MIXER:Current/InCh/Fader/Level 32 "))
    );
    // prminfo enumerated until the ERROR at 108.
    assert!(log.iter().any(|l| l == "prminfo 108"));

    // snapshot() re-reads from the device.
    fake.clear_log().await;
    let s2 = a.snapshot().await.unwrap();
    assert_eq!(s2.params.len(), 4376);
    assert_eq!(fake.count("get ").await, 4372);

    assert!(matches!(
        a.set_route(ChannelRef::new("x", 0), None).await,
        Err(DeviceError::Unsupported(_))
    ));
}

#[tokio::test]
async fn set_is_confirmed_and_emitted() {
    let fake = Fake::start().await;
    let a = TfAdapter::connect_with(fake.addr, opts()).await.unwrap();
    let mut rx = a.subscribe();
    fake.clear_log().await;

    a.set_param("in/1/level", ParamValue::Level(-10.0))
        .await
        .unwrap();
    let ev = next_event(&mut rx, changed("in/1/level")).await;
    assert_eq!(value_of(ev), ParamValue::Level(-10.0));
    assert_eq!(
        fake.log().await,
        [
            "set MIXER:Current/InCh/Fader/Level 0 0 -1000",
            "get MIXER:Current/InCh/Fader/Level 0 0",
        ]
    );

    // Quoting/escaping reaches the wire exactly and round-trips.
    fake.clear_log().await;
    let name = r#"VOX "1" \ L"#;
    a.set_param("in/2/name", ParamValue::Text(name.into()))
        .await
        .unwrap();
    assert_eq!(
        fake.log().await[0],
        r#"set MIXER:Current/InCh/Label/Name 1 0 "VOX \"1\" \\ L""#
    );
    let ev = next_event(&mut rx, changed("in/2/name")).await;
    assert_eq!(value_of(ev), ParamValue::Text(name.into()));

    // Colour by enum index → the TF wire name.
    fake.clear_log().await;
    let opts_list = match &a.cached_snapshot().param("in/1/color").unwrap().kind {
        patchbay_device::ParamKind::Enum { options } => options.clone(),
        k => panic!("{k:?}"),
    };
    let sky = u32::try_from(opts_list.iter().position(|o| o == "SkyBlue").unwrap()).unwrap();
    a.set_param("in/1/color", ParamValue::Enum(sky))
        .await
        .unwrap();
    assert_eq!(
        fake.log().await[0],
        r#"set MIXER:Current/InCh/Label/Color 0 0 "SkyBlue""#
    );

    // -inf, pan, toggle, pre/post.
    fake.clear_log().await;
    a.set_param("in/1/level", ParamValue::Level(f64::NEG_INFINITY))
        .await
        .unwrap();
    a.set_param("in/1/send/aux/3/pan", ParamValue::Pan(-1.0))
        .await
        .unwrap();
    a.set_param("in/1/on", ParamValue::Toggle(false))
        .await
        .unwrap();
    a.set_param("in/1/send/aux/3/prepost", ParamValue::Enum(0))
        .await
        .unwrap();
    let sets: Vec<String> = fake
        .log()
        .await
        .into_iter()
        .filter(|l| l.starts_with("set"))
        .collect();
    assert_eq!(
        sets,
        [
            "set MIXER:Current/InCh/Fader/Level 0 0 -32768",
            "set MIXER:Current/InCh/ToMix/Pan 0 2 -63",
            "set MIXER:Current/InCh/Fader/On 0 0 0",
            "set MIXER:Current/InCh/ToMix/PrePost 0 2 0",
        ]
    );
    let snap = a.cached_snapshot();
    assert_eq!(
        snap.param("in/1/on").unwrap().value,
        ParamValue::Toggle(false)
    );
    assert_eq!(
        snap.param("in/1/send/aux/3/pan").unwrap().value,
        ParamValue::Pan(-1.0)
    );
}

#[tokio::test]
async fn invalid_readonly_and_rejected_writes() {
    let fake = Fake::start().await;
    let a = TfAdapter::connect_with(fake.addr, opts()).await.unwrap();
    fake.clear_log().await;

    assert!(matches!(
        a.set_param("mutegroup/1/name", ParamValue::Text("X".into()))
            .await,
        Err(DeviceError::ReadOnly(_))
    ));
    assert!(matches!(
        a.set_param("in/1/role", ParamValue::Text("Mono".into()))
            .await,
        Err(DeviceError::ReadOnly(_))
    ));
    assert!(matches!(
        a.set_param("in/99/level", ParamValue::Level(0.0)).await,
        Err(DeviceError::UnknownParam(_))
    ));
    assert!(matches!(
        a.set_param("in/1/name", ParamValue::Text("Bühne".into()))
            .await,
        Err(DeviceError::InvalidValue { .. })
    ));
    assert!(matches!(
        a.set_param("in/1/name", ParamValue::Text("x".repeat(65)))
            .await,
        Err(DeviceError::InvalidValue { .. })
    ));
    assert!(matches!(
        a.set_param("in/1/level", ParamValue::Level(10.5)).await,
        Err(DeviceError::InvalidValue { .. })
    ));
    assert!(matches!(
        a.set_param("in/1/level", ParamValue::Toggle(true)).await,
        Err(DeviceError::InvalidValue { .. })
    ));
    assert!(
        fake.log().await.is_empty(),
        "invalid writes must not reach the wire"
    );

    // Console ERROR → mapped error; nothing emitted as confirmed.
    let kick = 0; // TF_ICONS[0] == "Kick", which the fake refuses
    let r = a.set_param("in/1/icon", ParamValue::Enum(kick)).await;
    assert!(matches!(r, Err(DeviceError::InvalidValue { .. })), "{r:?}");
    assert_eq!(
        fake.log().await,
        [r#"set MIXER:Current/InCh/Label/Icon 0 0 "Kick""#]
    );
}

#[tokio::test]
async fn notify_becomes_param_changed() {
    let fake = Fake::start().await;
    let a = TfAdapter::connect_with(fake.addr, opts()).await.unwrap();
    let mut rx = a.subscribe();

    fake.inject
        .send("NOTIFY set MIXER:Current/InCh/Fader/On 4 0 0 \"OFF\"".into())
        .unwrap();
    let ev = next_event(&mut rx, changed("in/5/on")).await;
    assert_eq!(value_of(ev), ParamValue::Toggle(false));

    fake.inject
        .send(r#"NOTIFY set MIXER:Current/InCh/Label/Name 2 0 "LEAD \"V\"""#.into())
        .unwrap();
    let ev = next_event(&mut rx, changed("in/3/name")).await;
    assert_eq!(value_of(ev), ParamValue::Text(r#"LEAD "V""#.into()));

    // Alias address form.
    fake.inject
        .send("NOTIFY set MIXER:Current/DcaCh/Fader/Level 1 0 -500 \"-5.00\"".into())
        .unwrap();
    let ev = next_event(&mut rx, changed("dca/2/level")).await;
    assert_eq!(value_of(ev), ParamValue::Level(-5.0));

    // Unknown colour from other firmware: appended to the vocabulary.
    fake.inject
        .send(r#"NOTIFY set MIXER:Current/Mix/Label/Color 0 0 "Off""#.into())
        .unwrap();
    let ev = next_event(&mut rx, changed("aux/1/color")).await;
    let ParamValue::Enum(i) = value_of(ev) else {
        panic!()
    };
    let snap = a.cached_snapshot();
    let p = snap.param("aux/1/color").unwrap();
    let patchbay_device::ParamKind::Enum { options } = &p.kind else {
        panic!()
    };
    assert_eq!(options[usize::try_from(i).unwrap()], "Off");
    assert_eq!(
        snap.param("in/5/on").unwrap().value,
        ParamValue::Toggle(false)
    );
}

#[tokio::test]
async fn scene_notify_triggers_full_reread() {
    let fake = Fake::start().await;
    let a = TfAdapter::connect_with(fake.addr, opts()).await.unwrap();
    let mut rx = a.subscribe();
    {
        // The console recalls a scene elsewhere: values change silently.
        let mut st = fake.state.lock().await;
        st.values.insert(
            ("MIXER:Current/InCh/Fader/Level".into(), 2, 0),
            RcpValue::Int(250),
        );
        st.scene = (SceneBank::A, 5);
        st.log.clear();
    }
    fake.inject
        .send("NOTIFY sscurrent_ex scene_a 5".into())
        .unwrap();

    let ev = next_event(&mut rx, changed("in/3/level")).await;
    assert_eq!(value_of(ev), ParamValue::Level(2.5));
    next_event(&mut rx, |e| matches!(e, DeviceEvent::SnapshotReplaced)).await;
    let snap = a.cached_snapshot();
    assert_eq!(
        snap.param("scene/current").unwrap().value,
        ParamValue::Text("A05".into())
    );
    assert_eq!(
        snap.param("scene/title").unwrap().value,
        ParamValue::Text("Title 5".into())
    );
    assert!(fake.count("get ").await >= 4372, "full re-read");
}

#[tokio::test]
async fn scene_recall_is_guarded_then_rereads() {
    let fake = Fake::start().await;
    let a = TfAdapter::connect_with(fake.addr, opts()).await.unwrap();
    let mut rx = a.subscribe();
    fake.clear_log().await;

    let r = a
        .set_param("scene/recall", ParamValue::Text("A05".into()))
        .await;
    assert!(matches!(r, Err(DeviceError::DisruptiveWrite(_))), "{r:?}");
    assert!(matches!(
        a.recall_scene(SceneBank::A, 5, WriteGuard::Normal).await,
        Err(DeviceError::DisruptiveWrite(_))
    ));
    assert!(matches!(
        a.apply_param(
            "scene/recall",
            ParamValue::Text("C01".into()),
            WriteGuard::AllowDisruptive
        )
        .await,
        Err(DeviceError::InvalidValue { .. })
    ));
    assert!(matches!(
        a.set_param("scene/current", ParamValue::Text("A01".into()))
            .await,
        Err(DeviceError::ReadOnly(_))
    ));
    assert!(fake.log().await.is_empty());

    a.apply_param(
        "scene/recall",
        ParamValue::Text("a5".into()),
        WriteGuard::AllowDisruptive,
    )
    .await
    .unwrap();
    let log = fake.log().await;
    assert_eq!(log[0], "ssrecall_ex scene_a 5");
    assert!(log.iter().filter(|l| l.starts_with("get ")).count() >= 4372);
    let ev = next_event(&mut rx, changed("in/10/level")).await;
    assert_eq!(value_of(ev), ParamValue::Level(-6.0));
    next_event(&mut rx, |e| matches!(e, DeviceEvent::SnapshotReplaced)).await;
    assert_eq!(
        a.cached_snapshot().param("scene/current").unwrap().value,
        ParamValue::Text("A05".into())
    );
}

#[tokio::test]
async fn keepalive_pings_and_reconnect() {
    let fake = Fake::start().await;
    let mut o = opts();
    o.client.ping_interval = Some(Duration::from_millis(40));
    o.client.keepalive = Some(Duration::from_millis(2000));
    let a = TfAdapter::connect_with(fake.addr, o).await.unwrap();
    let mut rx = a.subscribe();
    assert_eq!(fake.log().await[0], "scpmode keepalive 2000");

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake.count("devstatus runmode").await >= 2, "pings");

    fake.kill.send(()).unwrap();
    next_event(&mut rx, |e| matches!(e, DeviceEvent::Offline)).await;
    assert!(!DeviceAdapter::info(&a).online);
    next_event(&mut rx, |e| matches!(e, DeviceEvent::Online)).await;
    next_event(&mut rx, |e| matches!(e, DeviceEvent::SnapshotReplaced)).await;
    assert!(DeviceAdapter::info(&a).online);
    assert_eq!(fake.state.lock().await.connections, 2);
    assert_eq!(fake.count("scpmode keepalive 2000").await, 2, "per session");
    a.set_param("in/1/on", ParamValue::Toggle(false))
        .await
        .unwrap();
}

// ── client ────────────────────────────────────────────────────────────

/// A scripted server: answers each request line with the given chunks.
async fn scripted(
    replies: Vec<Vec<&'static [u8]>>,
) -> (SocketAddr, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        for chunks in replies {
            let Ok(Some(l)) = lines.next_line().await else {
                return;
            };
            tx.send(l).unwrap();
            for c in chunks {
                w.write_all(c).await.unwrap();
                w.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        // Keep the socket open until the client goes away.
        while let Ok(Some(_)) = lines.next_line().await {}
    });
    (addr, rx)
}

fn bare_opts() -> ClientOptions {
    ClientOptions {
        keepalive: None,
        ping_interval: None,
        reconnect: false,
        ..ClientOptions::default()
    }
}

#[tokio::test]
async fn client_handles_split_and_batched_lines() {
    let (addr, _seen) = scripted(vec![
        // A reply split mid-token across three reads.
        vec![b"OK get MIXER:Current/InCh/Fa", b"der/Level 0 0 -10", b"00\r\n"],
        // A NOTIFY and the reply in one read, then a partial next line.
        vec![b"NOTIFY set MIXER:Current/InCh/Fader/On 1 0 0 \"OFF\"\nOK devinfo productname \"TF1\"\nNOTIFY sscur", b"rent_ex scene_a 3\n"],
    ])
    .await;
    let c = Client::connect_with(addr, bare_opts()).await.unwrap();
    let mut rx = c.subscribe();
    let p = c.get("MIXER:Current/InCh/Fader/Level", 0, 0).await.unwrap();
    assert_eq!(p.value, RcpValue::Int(-1000));
    assert_eq!(c.devinfo("productname").await.unwrap(), "TF1");
    let mut notes = Vec::new();
    while notes.len() < 2 {
        if let ClientEvent::Notify(n) = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            notes.push(n);
        }
    }
    let p = notes[0].param().unwrap();
    assert_eq!(
        (p.x, p.value.clone(), p.display.as_deref()),
        (1, RcpValue::Int(0), Some("OFF"))
    );
    assert!(notes[1].is_scene_change());
}

#[tokio::test]
async fn client_correlates_errors_in_order_and_times_out() {
    let (addr, _seen) = scripted(vec![
        vec![b"ERROR get InvalidArgument\n"],
        vec![b"OK get MIXER:Current/InCh/Fader/Level 1 0 5\n"],
        vec![], // no answer → timeout
        vec![b"OK devstatus runmode \"normal\"\n"],
    ])
    .await;
    let mut o = bare_opts();
    o.request_timeout = Duration::from_millis(300);
    let c = Client::connect_with(addr, o).await.unwrap();
    let (a, b) = tokio::join!(
        c.get("MIXER:Current/InCh/Fader/Level", 40, 0),
        c.get("MIXER:Current/InCh/Fader/Level", 1, 0),
    );
    // Pipelined: whichever was written first got the ERROR.
    let (err, ok) = if a.is_err() { (a, b) } else { (b, a) };
    assert!(
        matches!(err, Err(TfError::Rejected { ref reason, .. }) if reason == "InvalidArgument")
    );
    assert_eq!(ok.unwrap().value, RcpValue::Int(5));
    assert!(matches!(
        c.request(&Command::devinfo("version")).await,
        Err(TfError::Timeout { .. })
    ));
    c.request(&Command::devstatus("runmode")).await.unwrap();
    c.close().await;
    assert!(!c.is_connected());
    assert!(matches!(c.devinfo("version").await, Err(TfError::Closed)));
}

#[tokio::test]
async fn client_spaces_writes() {
    let fake = Fake::start().await;
    let mut o = bare_opts();
    o.write_gap = Duration::from_millis(30);
    let c = Client::connect_with(fake.addr, o).await.unwrap();
    let t0 = tokio::time::Instant::now();
    for i in 0..4 {
        c.set("MIXER:Current/InCh/Fader/On", i, 0, RcpValue::Int(1))
            .await
            .unwrap();
    }
    assert!(
        t0.elapsed() >= Duration::from_millis(90),
        "{:?}",
        t0.elapsed()
    );
    // Reads are not spaced.
    let t1 = tokio::time::Instant::now();
    for i in 0..4 {
        c.get("MIXER:Current/InCh/Fader/On", i, 0).await.unwrap();
    }
    assert!(t1.elapsed() < Duration::from_millis(90));
}
