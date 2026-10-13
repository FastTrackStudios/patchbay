//! GUARDED live write round-trips against a real Galaxy32.
//!
//! Touches ONLY the targets cleared as unused on the live rig, and
//! restores each one (read → write → verify → restore → verify):
//!
//! - routing pages 6 and 7 (HDX OUT 1-32 / 33-64),
//! - MIXER 1 strip 16 (level/pan/mute/send — never solo, which would
//!   affect the whole mix),
//! - monitor dim (output 0), toggled and put back,
//! - AFX strip 16 (index 15): insert Opto 2A, write its config, clear.
//!
//! Never writes clock/sample rate, trims, any other routing page or strip.
//! Preconditions (strip 16 unpatched, AFX 16 empty) are checked first and
//! the step is skipped if they don't hold.
//!
//! ```bash
//! cargo run -p patchbay-antelope --example galaxy_write_test
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use patchbay_antelope::tables::{self, SOURCE_NONE};
use patchbay_antelope::{
    AfxSlot, Client, ClientEvent, Galaxy32, Galaxy32Adapter, MixerStrip, RouteSlot, RoutingPage,
    ServerFrame,
};
use patchbay_device::{ChannelRef, DeviceAdapter, ParamValue};
use serde_json::{Value, json};

/// The only routing pages this program may write.
const ALLOWED_PAGES: [u8; 2] = [6, 7];
/// MIXER 1 (wire 0), strip 16.
const MIXER: u8 = 0;
const STRIP: u8 = 16;
/// AFX 16 (wire index 15).
const AFX_STRIP_INDEX: u8 = 15;
const OPTO_2A: u8 = 59;

type Res<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Report {
    rows: Vec<(String, bool, String)>,
}

impl Report {
    fn check(&mut self, step: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!(
            "  [{}] {step}{}",
            if ok { "PASS" } else { "FAIL" },
            if detail.is_empty() {
                String::new()
            } else {
                format!(" — {detail}")
            }
        );
        self.rows.push((step.to_owned(), ok, detail));
    }

    fn result<T, E: std::fmt::Display>(&mut self, step: &str, r: Result<T, E>) -> Option<T> {
        match r {
            Ok(v) => {
                self.check(step, true, "");
                Some(v)
            }
            Err(e) => {
                self.check(step, false, e.to_string());
                None
            }
        }
    }
}

/// Poll a read until it matches (fire-and-forget writes land asynchronously).
async fn settle<T, F, Fut>(read: F, want: &T) -> Res<T>
where
    T: PartialEq + Clone + Send + Sync,
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = patchbay_antelope::Result<T>> + Send,
{
    let mut last = read().await?;
    for _ in 0..10 {
        if &last == want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = read().await?;
    }
    Ok(last)
}

fn slot(s: RouteSlot) -> String {
    tables::source_type(s.ty).map_or_else(
        || "none".to_owned(),
        |t| format!("{} {}", t.name, u16::from(s.ch).saturating_add(1)),
    )
}

async fn restore_page(dev: &Galaxy32, orig: &RoutingPage, rep: &mut Report) -> Res {
    if !ALLOWED_PAGES.contains(&orig.page) {
        return Err(format!("refusing to write routing page {}", orig.page).into());
    }
    dev.set_routing(orig.page, &orig.slots).await?;
    let back = settle(|| dev.get_routing(orig.page), orig).await?;
    rep.check(
        &format!("page {} restored to original (read-back)", orig.page),
        back == *orig,
        "",
    );
    Ok(())
}

async fn routing_step(adapter: &Galaxy32Adapter, rep: &mut Report) -> Res {
    println!("\n== routing: HDX OUT pages 6 / 7");
    let dev = adapter.device();
    for (group, page_no, source) in [
        ("DIGI_OUT0", 6u8, Some(ChannelRef::new("LINE_IN0", 31))),
        ("DIGI_OUT1", 7u8, None),
    ] {
        let page = tables::output_page_by_id(group).ok_or("unknown group")?;
        if page.page != page_no || !ALLOWED_PAGES.contains(&page.page) {
            return Err(format!("{group} maps to page {} — not allowed", page.page).into());
        }
        let orig = dev.get_routing(page_no).await?;
        let cur = orig.slots.first().copied().ok_or("empty page")?;
        // Pick a source that differs from the current one.
        let source = match (&source, cur) {
            (Some(_), RouteSlot { ty: 0, ch: 31 }) => Some(ChannelRef::new("LINE_IN0", 30)),
            (
                None,
                RouteSlot {
                    ty: SOURCE_NONE, ..
                },
            ) => Some(ChannelRef::new("LINE_IN0", 31)),
            _ => source,
        };
        println!(
            "  {group}:1 was {}; setting {:?}",
            slot(cur),
            source.as_ref().map(ToString::to_string)
        );
        let set = adapter
            .set_route(ChannelRef::new(group, 0), source.clone())
            .await;
        let label = format!("set_route {group}:1 confirmed by adapter read-back");
        let ok = rep.result(&label, set).is_some();
        if ok {
            let now = dev.get_routing(page_no).await?;
            let want = source.as_ref().map_or(RouteSlot::NONE, |s| RouteSlot {
                ty: tables::source_type_by_id(&s.group).map_or(SOURCE_NONE, |t| t.ty),
                ch: u8::try_from(s.channel).unwrap_or(0),
            });
            let first_ok = now.slots.first() == Some(&want);
            let rest_ok = now.slots.get(1..) == orig.slots.get(1..);
            rep.check(
                &format!(
                    "independent get_routing {page_no}: slot 1 = {}, other 31 slots unchanged",
                    slot(want)
                ),
                first_ok && rest_ok,
                "",
            );
        }
        restore_page(dev, &orig, rep).await?;
    }
    Ok(())
}

async fn mixer_step(adapter: &Galaxy32Adapter, rep: &mut Report) -> Res {
    println!("\n== mixer: MIXER 1 strip 16");
    let dev = adapter.device();
    // Precondition: MIX1 IN 16 (page 13 slot 15) has no source.
    let mix_in = dev.get_routing(13).await?;
    let src = mix_in
        .slots
        .get(usize::from(STRIP.saturating_sub(1)))
        .copied();
    if src.map(|s| s.ty) != Some(SOURCE_NONE) {
        rep.check(
            "precondition: MIX1 IN 16 unpatched",
            false,
            format!("{src:?} — skipping mixer step"),
        );
        return Ok(());
    }
    rep.check("precondition: MIX1 IN 16 unpatched", true, "");
    let orig = dev.get_mixer_strip(MIXER, STRIP).await?;
    println!("  original {orig:?}");
    let level_att = if orig.level < 80 {
        orig.level.saturating_add(10)
    } else {
        orig.level.saturating_sub(10)
    };
    let send_att: u8 = if orig.send == 40 { 50 } else { 40 };
    let pan = if orig.pan == 17 { 0.5 } else { -0.5 };
    let writes = [
        (
            "mixer/1/strip/16/level",
            ParamValue::Level(-f64::from(level_att)),
        ),
        ("mixer/1/strip/16/pan", ParamValue::Pan(pan)),
        ("mixer/1/strip/16/mute", ParamValue::Toggle(orig.mute == 0)),
        (
            "mixer/1/strip/16/send",
            ParamValue::Level(-f64::from(send_att)),
        ),
    ];
    for (path, v) in writes {
        let r = adapter.set_param(path, v.clone()).await;
        rep.result(
            &format!("set_param {path} = {v:?} (get_mixer read-back)"),
            r,
        );
    }
    let now = dev.get_mixer_strip(MIXER, STRIP).await?;
    println!("  now      {now:?}");
    let neighbours_before = dev.get_mixer(MIXER).await?;
    dev.set_mixer_strip(MIXER, STRIP, &orig).await?;
    let back: MixerStrip = settle(|| dev.get_mixer_strip(MIXER, STRIP), &orig).await?;
    rep.check(
        "strip 16 restored to original (read-back)",
        back == orig,
        format!("{back:?}"),
    );
    let neighbours_after = dev.get_mixer(MIXER).await?;
    let others_same = neighbours_before
        .iter()
        .zip(&neighbours_after)
        .enumerate()
        .all(|(i, (a, b))| i == usize::from(STRIP) || a == b);
    rep.check("other MIXER 1 strips untouched", others_same, "");
    Ok(())
}

async fn dim_step(adapter: &Galaxy32Adapter, rep: &mut Report) -> Res {
    println!("\n== monitor dim (output 0)");
    let dev = adapter.device();
    let orig = dev
        .wait_state(Duration::from_secs(2), |_| true)
        .await
        .ok_or("no cyclic state")?
        .monitor
        .dim;
    println!("  original dim = {orig}");
    let r = adapter
        .set_param("monitor/dim", ParamValue::Toggle(!orig))
        .await;
    rep.result(
        &format!("set_param monitor/dim = {} (cyclic confirm)", !orig),
        r,
    );
    let r = adapter
        .set_param("monitor/dim", ParamValue::Toggle(orig))
        .await;
    rep.result(
        &format!("set_param monitor/dim = {orig} (restore, cyclic confirm)"),
        r,
    );
    Ok(())
}

async fn afx_step(
    adapter: &Galaxy32Adapter,
    rep: &mut Report,
    seen: &Arc<Mutex<Vec<Value>>>,
) -> Res {
    println!("\n== AFX 16 (index 15): Opto 2A insert / config / clear");
    let dev = adapter.device();
    let chain = dev.get_afx_strip(AFX_STRIP_INDEX).await?;
    let afx_in = dev.get_routing(12).await?;
    let input = afx_in.slots.get(usize::from(AFX_STRIP_INDEX)).copied();
    if !chain.is_empty() || input.map(|s| s.ty) != Some(SOURCE_NONE) {
        rep.check(
            "precondition: AFX 16 empty and input unpatched",
            false,
            format!("chain {chain:?} input {input:?} — skipping"),
        );
        return Ok(());
    }
    rep.check("precondition: AFX 16 empty and input unpatched", true, "");
    let avail_before = dev
        .get_afx_available()
        .await?
        .into_iter()
        .find(|a| a.type_id == OPTO_2A);

    let r = adapter
        .set_param(
            "afx/strip/16/slot/1/effect",
            ParamValue::Enum(u32::from(OPTO_2A)),
        )
        .await;
    let inserted = rep
        .result(
            "set_param afx/strip/16/slot/1/effect = opto2a (get_afx_strip_order read-back)",
            r,
        )
        .is_some();
    let now = dev.get_afx_strip(AFX_STRIP_INDEX).await?;
    println!("  chain now {now:?}");
    if inserted {
        let inst = now.first().map_or(0, |s| s.inst);
        seen.lock().clear();
        let r = adapter
            .set_afx_conf(16, 1, &[json!(0), json!(1), json!(31), json!(1)])
            .await;
        rep.result(
            &format!("set_afx_conf opto2a #{inst} = [meter 0, limit 1, gain 31, peak 1] sent"),
            r,
        );
        let r = adapter
            .set_param("afx/strip/16/slot/1/gain", ParamValue::Int(40))
            .await;
        rep.result("set_param afx/strip/16/slot/1/gain = 40 sent", r);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let got = seen.lock().clone();
        let want_a = json!(["set_opto2a_conf", [OPTO_2A, inst, 0, 1, 31, 1]]);
        let want_b = json!(["set_opto2a_conf", [OPTO_2A, inst, 0, 1, 40, 1]]);
        let has = |w: &Value| {
            got.iter()
                .any(|g| g.get(0) == w.get(0) && g.get(1) == w.get(1))
        };
        rep.check(
            "observer client received both set_opto2a_conf notifications (server accepted)",
            has(&want_a) && has(&want_b),
            format!("{} opto notifications seen", got.len()),
        );
    }
    // Clear (always).
    dev.set_afx_strip(AFX_STRIP_INDEX, &[]).await?;
    let back: Vec<AfxSlot> = settle(|| dev.get_afx_strip(AFX_STRIP_INDEX), &Vec::new()).await?;
    rep.check(
        "AFX 16 cleared (set_afx_order [15, []], read-back empty)",
        back.is_empty(),
        format!("{back:?}"),
    );
    let avail_after = dev
        .get_afx_available()
        .await?
        .into_iter()
        .find(|a| a.type_id == OPTO_2A);
    rep.check(
        "Opto 2A available-instance count back to original",
        avail_before == avail_after,
        format!("{avail_before:?} → {avail_after:?}"),
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Res {
    let adapter = Galaxy32Adapter::discover_and_connect(None, Duration::from_millis(1500)).await?;
    let addr = adapter.device().client().addr();
    println!("connected: {} at {addr}", DeviceAdapter::info(&adapter).id);

    // A second, independent connection: sees the server's rebroadcasts.
    let observer = Client::connect(addr).await?;
    let obs_notes = Arc::new(AtomicUsize::new(0));
    let own_notes = Arc::new(AtomicUsize::new(0));
    let opto: Arc<Mutex<Vec<Value>>> = Arc::default();
    for (client, counter, collect) in [
        (
            observer.clone(),
            Arc::clone(&obs_notes),
            Some(Arc::clone(&opto)),
        ),
        (
            adapter.device().client().clone(),
            Arc::clone(&own_notes),
            None,
        ),
    ] {
        let mut rx = client.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                let ClientEvent::Frame(f) = ev else { break };
                if let ServerFrame::Notification { contents, .. } = &*f {
                    counter.fetch_add(1, Ordering::Relaxed);
                    if let Some(c) = &collect {
                        if contents.get(0).and_then(Value::as_str) == Some("set_opto2a_conf") {
                            c.lock().push(contents.clone());
                        }
                    }
                }
            }
        });
    }

    let mut rep = Report { rows: Vec::new() };
    routing_step(&adapter, &mut rep).await?;
    mixer_step(&adapter, &mut rep).await?;
    dim_step(&adapter, &mut rep).await?;
    afx_step(&adapter, &mut rep, &opto).await?;

    tokio::time::sleep(Duration::from_millis(300)).await;
    println!(
        "\nnotifications: observer connection {}, writer's own connection {}",
        obs_notes.load(Ordering::Relaxed),
        own_notes.load(Ordering::Relaxed)
    );
    let failed = rep.rows.iter().filter(|r| !r.1).count();
    println!("\n{} checks, {} failed", rep.rows.len(), failed);
    drop(observer);
    Ok(())
}
