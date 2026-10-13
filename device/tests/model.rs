#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use patchbay_device::{
    ChannelRef, Crosspoint, DeviceId, DeviceInfo, DeviceSnapshot, Param, ParamKind, ParamValue,
    PortGroup, Transport,
};

fn sample() -> DeviceSnapshot {
    DeviceSnapshot {
        info: DeviceInfo {
            id: DeviceId::from_parts("antelope", "galaxy32", "4202524000109"),
            vendor: "Antelope Audio".into(),
            model: "Galaxy32".into(),
            serial: Some("4202524000109".into()),
            firmware: Some("8.24".into()),
            transport: Transport::Tcp {
                addr: "127.0.0.1:2023".into(),
                via: Some("Antelope Manager Server 1.8.19".into()),
            },
            online: true,
        },
        inputs: vec![PortGroup {
            id: "LINE_IN0".into(),
            name: "LINE IN 1-32".into(),
            channels: 32,
        }],
        outputs: vec![PortGroup {
            id: "LINE_OUT0".into(),
            name: "LINE OUT 1-32".into(),
            channels: 32,
        }],
        routes: vec![
            Crosspoint {
                output: ChannelRef::new("LINE_OUT0", 0),
                source: Some(ChannelRef::new("LINE_IN0", 3)),
            },
            Crosspoint {
                output: ChannelRef::new("LINE_OUT0", 1),
                source: None,
            },
        ],
        params: vec![Param {
            path: "mixer/1/strip/16/level".into(),
            label: "MIX1 16 level".into(),
            kind: ParamKind::Level {
                min_db: -96.0,
                max_db: 0.0,
            },
            value: ParamValue::Level(-26.0),
            writable: true,
            disruptive: false,
        }],
    }
}

#[test]
fn id_format() {
    let id = DeviceId::from_parts("antelope", "galaxy32", "4202524000109");
    assert_eq!(id.as_str(), "antelope:galaxy32:4202524000109");
    assert_eq!(
        serde_json::to_string(&id).unwrap(),
        "\"antelope:galaxy32:4202524000109\""
    );
}

#[test]
fn snapshot_lookups_and_serde_roundtrip() {
    let s = sample();
    assert_eq!(
        s.source_of(&ChannelRef::new("LINE_OUT0", 0)),
        Some(Some(&ChannelRef::new("LINE_IN0", 3)))
    );
    assert_eq!(s.source_of(&ChannelRef::new("LINE_OUT0", 1)), Some(None));
    assert_eq!(s.source_of(&ChannelRef::new("LINE_OUT0", 9)), None);
    assert!(s.param("mixer/1/strip/16/level").is_some());
    let json = serde_json::to_string(&s).unwrap();
    let back: DeviceSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);
}

#[test]
fn value_kind_matching() {
    assert!(ParamValue::Toggle(true).matches(&ParamKind::Toggle));
    assert!(!ParamValue::Toggle(true).matches(&ParamKind::Pan));
    assert!(ParamValue::Enum(2).matches(&ParamKind::Enum { options: vec![] }));
    assert_eq!(ChannelRef::new("LINE_IN0", 0).to_string(), "LINE_IN0:1");
}

#[test]
fn mirror_applies_events_and_diffs_back() {
    use patchbay_device::{DeviceEvent, MirrorUpdate, apply_event, diff_events};
    let old = sample();
    let mut m = old.clone();
    let level = DeviceEvent::ParamChanged {
        path: "mixer/1/strip/16/level".into(),
        value: ParamValue::Level(-12.0),
    };
    let route = DeviceEvent::RouteChanged(Crosspoint {
        output: ChannelRef::new("LINE_OUT0", 1),
        source: Some(ChannelRef::new("LINE_IN0", 7)),
    });
    assert_eq!(apply_event(&mut m, &level), MirrorUpdate::Applied);
    assert_eq!(apply_event(&mut m, &route), MirrorUpdate::Applied);
    assert_eq!(
        m.param("mixer/1/strip/16/level").unwrap().value,
        ParamValue::Level(-12.0)
    );
    assert_eq!(
        m.source_of(&ChannelRef::new("LINE_OUT0", 1)),
        Some(Some(&ChannelRef::new("LINE_IN0", 7)))
    );
    let unknown = DeviceEvent::ParamChanged {
        path: "nope".into(),
        value: ParamValue::Toggle(true),
    };
    assert_eq!(apply_event(&mut m, &unknown), MirrorUpdate::Unknown);
    assert_eq!(
        apply_event(&mut m, &DeviceEvent::SnapshotReplaced),
        MirrorUpdate::Stale
    );
    assert_eq!(
        apply_event(&mut m, &DeviceEvent::Offline),
        MirrorUpdate::Ignored
    );

    // Diffing old → mirror yields exactly the two applied events.
    let evs = diff_events(&old, &m);
    assert_eq!(evs, vec![level, route]);
    assert!(diff_events(&m, &m).is_empty());
}
