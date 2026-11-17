//! An in-process fake TF1 on a loopback port: the console's param table
//! (the live `prminfo` dump), its values, scenes and NOTIFY lines, and a
//! log of what it was sent. Shared by the tests (`mod common;`).
#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::many_single_char_names
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use patchbay_yamaha_tf::rcp::token::{quote, tokenize};
use patchbay_yamaha_tf::{PrmInfo, PrmType, RcpValue, SceneBank, TF1_PRMINFO_JSON};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast};

// ── fake console ──────────────────────────────────────────────────────

pub struct FakeState {
    pub rows: Vec<PrmInfo>,
    pub lines: Vec<String>,
    pub values: HashMap<(String, u16, u16), RcpValue>,
    pub log: Vec<String>,
    pub scene: (SceneBank, u8),
    pub connections: usize,
}

impl FakeState {
    pub fn row(&self, address: &str) -> Option<&PrmInfo> {
        self.rows.iter().find(|r| r.address == address)
    }

    pub fn value(&self, address: &str, x: u16, y: u16) -> RcpValue {
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

pub fn wire(v: &RcpValue) -> String {
    match v {
        RcpValue::Int(n) => n.to_string(),
        RcpValue::Str(s) => quote(s).unwrap(),
    }
}

pub fn handle(st: &mut FakeState, line: &str) -> String {
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
pub struct Fake {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<FakeState>>,
    pub inject: broadcast::Sender<String>,
    pub kill: broadcast::Sender<()>,
}

impl Fake {
    pub async fn start() -> Self {
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

    pub async fn serve(self, sock: TcpStream) {
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

    pub async fn log(&self) -> Vec<String> {
        self.state.lock().await.log.clone()
    }

    pub async fn clear_log(&self) {
        self.state.lock().await.log.clear();
    }

    pub async fn count(&self, prefix: &str) -> usize {
        self.log()
            .await
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
}
