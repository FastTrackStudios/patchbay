//! Device snapshots: capture (live → saved) and restore planning
//! (saved vs live → the minimal set of writes).
//!
//! Pure like every planner here: the live side is a [`DeviceView`] the
//! caller just read from the device, the saved side a
//! [`DeviceSettingSnapshot`] from config. Execution (the actual
//! `set_param` / `set_route` calls) lives in `devices::hub`.
//!
//! Idempotent: re-planning against the device state a plan's writes
//! produced yields no `Planned` items.

use std::collections::HashMap;

use patchbay_proto::{
    DeviceChannel, DeviceParamSetting, DeviceParamValue, DeviceRestoreItem, DeviceRestoreStatus,
    DeviceRouteSetting, DeviceSettingSnapshot, DeviceView, ParamView, parse_route_path,
    path_selected, route_path, source_label,
};

/// One write a restore performs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DeviceOp {
    Param {
        path: String,
        value: DeviceParamValue,
    },
    Route {
        output: DeviceChannel,
        source: Option<DeviceChannel>,
    },
}

/// A differing snapshot item: the report line plus, when `Planned`,
/// the write that would fix it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedItem {
    pub item: DeviceRestoreItem,
    pub op: Option<DeviceOp>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DevicePlan {
    /// Selected snapshot items already equal to live.
    pub unchanged: u32,
    pub items: Vec<PlannedItem>,
}

/// Capture the writable params + every crosspoint of `live` selected by
/// `include` / `exclude` (path prefixes, whole segments).
pub(crate) fn capture(
    live: &DeviceView,
    include: &[String],
    exclude: &[String],
) -> (Vec<DeviceParamSetting>, Vec<DeviceRouteSetting>) {
    let params = live
        .params
        .iter()
        .filter(|p| p.writable && path_selected(&p.path, include, exclude))
        .map(|p| DeviceParamSetting::new(p.path.clone(), &p.value))
        .collect();
    let routes = live
        .routes
        .iter()
        .map(|c| (route_path(&c.output), c))
        .filter(|(path, _)| path_selected(path, include, exclude))
        .map(|(path, c)| DeviceRouteSetting {
            path,
            source: c
                .source
                .as_ref()
                .map(DeviceChannel::label)
                .unwrap_or_default(),
        })
        .collect();
    (params, routes)
}

fn item(
    path: &str,
    current: String,
    target: String,
    status: DeviceRestoreStatus,
) -> DeviceRestoreItem {
    DeviceRestoreItem {
        path: path.to_owned(),
        current,
        target,
        status,
        error: String::new(),
    }
}

/// Parse a saved route source (`GROUP:N` 1-based, empty = none).
/// `Err` carries the unparseable text.
fn saved_source(s: &str) -> Result<Option<DeviceChannel>, String> {
    let t = s.trim();
    if t.is_empty() || t == "-" {
        return Ok(None);
    }
    DeviceChannel::parse_label(t)
        .map(Some)
        .ok_or_else(|| t.to_owned())
}

/// The crosspoint half of [`plan`].
fn plan_routes(
    live: &DeviceView,
    snap: &DeviceSettingSnapshot,
    only: &[String],
    out: &mut DevicePlan,
) {
    let routes: HashMap<&DeviceChannel, Option<&DeviceChannel>> = live
        .routes
        .iter()
        .map(|c| (&c.output, c.source.as_ref()))
        .collect();
    let selected = |path: &str| path_selected(path, only, &[]);
    let has_input = |g: &str| live.inputs.iter().any(|i| i.id == g);
    for r in snap.routes.iter().filter(|r| selected(&r.path)) {
        let output = parse_route_path(&r.path);
        let current = output.as_ref().and_then(|o| routes.get(o).copied());
        let (Some(output), Some(current)) = (output, current) else {
            out.items.push(PlannedItem {
                item: item(
                    &r.path,
                    "(missing)".into(),
                    r.source.clone(),
                    DeviceRestoreStatus::SkippedMissing,
                ),
                op: None,
            });
            continue;
        };
        let target = match saved_source(&r.source) {
            Ok(t) if t.as_ref().is_none_or(|c| has_input(&c.group)) => t,
            Ok(_) | Err(_) => {
                out.items.push(PlannedItem {
                    item: item(
                        &r.path,
                        source_label(current),
                        r.source.clone(),
                        DeviceRestoreStatus::SkippedMissing,
                    ),
                    op: None,
                });
                continue;
            }
        };
        if current == target.as_ref() {
            out.unchanged = out.unchanged.saturating_add(1);
            continue;
        }
        out.items.push(PlannedItem {
            item: item(
                &r.path,
                source_label(current),
                source_label(target.as_ref()),
                DeviceRestoreStatus::Planned,
            ),
            op: Some(DeviceOp::Route {
                output,
                source: target,
            }),
        });
    }
}

/// Plan a restore of `snap` onto `live`.
///
/// - `only` narrows to path prefixes (empty = the whole snapshot).
/// - Params: skipped when missing live, read-only live, or disruptive
///   without `allow_disruptive`; planned when the value differs
///   (float-tolerant, see [`DeviceParamValue::same_as`]).
/// - Routes: skipped when the output (or source group) doesn't exist
///   live; planned when the source differs.
pub(crate) fn plan(
    live: &DeviceView,
    snap: &DeviceSettingSnapshot,
    only: &[String],
    allow_disruptive: bool,
) -> DevicePlan {
    let params: HashMap<&str, &ParamView> =
        live.params.iter().map(|p| (p.path.as_str(), p)).collect();
    let mut out = DevicePlan::default();
    let selected = |path: &str| path_selected(path, only, &[]);

    for s in snap.params.iter().filter(|s| selected(&s.path)) {
        let (Some(p), Some(value)) = (params.get(s.path.as_str()), s.value()) else {
            out.items.push(PlannedItem {
                item: item(
                    &s.path,
                    params
                        .get(s.path.as_str())
                        .map_or_else(|| "(missing)".into(), |p| p.value.display(Some(&p.kind))),
                    s.value()
                        .map_or_else(|| "(no value)".into(), |v| v.display(None)),
                    DeviceRestoreStatus::SkippedMissing,
                ),
                op: None,
            });
            continue;
        };
        if p.value.same_as(&value) {
            out.unchanged = out.unchanged.saturating_add(1);
            continue;
        }
        let current = p.value.display(Some(&p.kind));
        let target = value.display(Some(&p.kind));
        let status = if !p.writable {
            DeviceRestoreStatus::SkippedReadOnly
        } else if p.disruptive && !allow_disruptive {
            DeviceRestoreStatus::SkippedDisruptive
        } else {
            DeviceRestoreStatus::Planned
        };
        let op = (status == DeviceRestoreStatus::Planned).then(|| DeviceOp::Param {
            path: s.path.clone(),
            value,
        });
        out.items.push(PlannedItem {
            item: item(&s.path, current, target, status),
            op,
        });
    }

    plan_routes(live, snap, only, &mut out);
    // Disruptive writes (clock, sample rate, scene recall) replace state
    // wholesale, so they go FIRST and the fine params land on top. The
    // sort is stable: everything else keeps snapshot order.
    let disruptive = |i: &PlannedItem| match &i.op {
        Some(DeviceOp::Param { path, .. }) => {
            params.get(path.as_str()).is_some_and(|p| p.disruptive)
        }
        _ => false,
    };
    out.items.sort_by_key(|i| !disruptive(i));
    out
}

#[cfg(test)]
mod tests {
    use patchbay_proto::{
        DeviceCrosspoint, DeviceLinkState, DeviceParamKind, DevicePortGroup, DeviceSummary,
    };

    use super::*;

    fn pv(path: &str, kind: DeviceParamKind, value: DeviceParamValue) -> ParamView {
        ParamView {
            path: path.into(),
            label: path.into(),
            kind,
            value,
            writable: true,
            disruptive: false,
        }
    }

    fn level() -> DeviceParamKind {
        DeviceParamKind::Level {
            min_db: -96.0,
            max_db: 0.0,
        }
    }

    fn live() -> DeviceView {
        let mut rate = pv(
            "clock/sample_rate",
            DeviceParamKind::Enum {
                options: vec!["44100".into(), "48000".into()],
            },
            DeviceParamValue::Enum(1),
        );
        rate.disruptive = true;
        let mut locked = pv(
            "clock/locked",
            DeviceParamKind::Toggle,
            DeviceParamValue::Toggle(true),
        );
        locked.writable = false;
        DeviceView {
            summary: DeviceSummary {
                id: "antelope:galaxy32:1".into(),
                name: "galaxy32".into(),
                kind: "antelope-galaxy32".into(),
                vendor: String::new(),
                model: String::new(),
                serial: String::new(),
                firmware: String::new(),
                transport: String::new(),
                state: DeviceLinkState::Online,
                error: String::new(),
            },
            inputs: vec![DevicePortGroup {
                id: "COM_PLAY1".into(),
                name: "DAW OUT 33-64".into(),
                channels: 32,
            }],
            outputs: vec![DevicePortGroup {
                id: "DIGI_OUT0".into(),
                name: "HDX OUT 1-32".into(),
                channels: 32,
            }],
            routes: vec![
                DeviceCrosspoint {
                    output: DeviceChannel::new("DIGI_OUT0", 0),
                    source: Some(DeviceChannel::new("COM_PLAY1", 0)),
                },
                DeviceCrosspoint {
                    output: DeviceChannel::new("DIGI_OUT0", 1),
                    source: None,
                },
            ],
            params: vec![
                pv(
                    "mixer/1/strip/1/level",
                    level(),
                    DeviceParamValue::Level(-8.0),
                ),
                pv(
                    "mixer/1/strip/16/level",
                    level(),
                    DeviceParamValue::Level(0.0),
                ),
                pv(
                    "mixer/1/strip/16/mute",
                    DeviceParamKind::Toggle,
                    DeviceParamValue::Toggle(true),
                ),
                pv(
                    "monitor/dim",
                    DeviceParamKind::Toggle,
                    DeviceParamValue::Toggle(false),
                ),
                rate,
                locked,
            ],
        }
    }

    fn snapshot_of(v: &DeviceView, include: &[String]) -> DeviceSettingSnapshot {
        let (params, routes) = capture(v, include, &[]);
        DeviceSettingSnapshot {
            name: "s".into(),
            device: v.summary.id.clone(),
            created: 0,
            include: include.to_vec(),
            exclude: Vec::new(),
            params,
            routes,
        }
    }

    fn set_param(v: &mut DeviceView, path: &str, value: DeviceParamValue) {
        v.params.iter_mut().find(|p| p.path == path).unwrap().value = value;
    }

    fn planned(p: &DevicePlan) -> Vec<&str> {
        p.items
            .iter()
            .filter(|i| i.item.status == DeviceRestoreStatus::Planned)
            .map(|i| i.item.path.as_str())
            .collect()
    }

    #[test]
    fn capture_skips_read_only_and_respects_segment_prefixes() {
        let v = live();
        let s = snapshot_of(&v, &[]);
        assert!(s.params.iter().all(|p| p.path != "clock/locked"));
        assert!(s.params.iter().any(|p| p.path == "clock/sample_rate"));
        assert_eq!(s.routes.len(), 2);
        assert_eq!(s.routes[0].source, "COM_PLAY1:1");
        assert_eq!(s.routes[1].source, "");

        let s = snapshot_of(&v, &["mixer/1/strip/1".into()]);
        let paths: Vec<_> = s.params.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            ["mixer/1/strip/1/level"],
            "strip/1 must not pull strip/16"
        );
        assert!(s.routes.is_empty());
    }

    #[test]
    fn unchanged_device_plans_nothing() {
        let v = live();
        let p = plan(&v, &snapshot_of(&v, &[]), &[], false);
        assert!(p.items.is_empty(), "{:?}", p.items);
        assert_eq!(p.unchanged, 7);
    }

    #[test]
    fn plans_only_the_differences() {
        let v = live();
        let snap = snapshot_of(&v, &[]);
        let mut now = v;
        set_param(
            &mut now,
            "mixer/1/strip/16/level",
            DeviceParamValue::Level(-12.0),
        );
        set_param(&mut now, "monitor/dim", DeviceParamValue::Toggle(true));
        now.routes[1].source = Some(DeviceChannel::new("COM_PLAY1", 4));

        let p = plan(&now, &snap, &[], false);
        assert_eq!(
            planned(&p),
            ["mixer/1/strip/16/level", "monitor/dim", "route/DIGI_OUT0/2"]
        );
        let lvl = &p.items[0];
        assert_eq!(lvl.item.current, "-12.0 dB");
        assert_eq!(lvl.item.target, "0.0 dB");
        assert_eq!(
            lvl.op,
            Some(DeviceOp::Param {
                path: "mixer/1/strip/16/level".into(),
                value: DeviceParamValue::Level(0.0)
            })
        );
        let route = &p.items[2];
        assert_eq!(route.item.current, "COM_PLAY1:5");
        assert_eq!(route.item.target, "-");
        assert_eq!(
            route.op,
            Some(DeviceOp::Route {
                output: DeviceChannel::new("DIGI_OUT0", 1),
                source: None
            })
        );

        // Applying the plan's writes makes the next plan empty.
        let mut after = now.clone();
        for i in &p.items {
            match &i.op {
                Some(DeviceOp::Param { path, value }) => set_param(&mut after, path, value.clone()),
                Some(DeviceOp::Route { output, source }) => {
                    after
                        .routes
                        .iter_mut()
                        .find(|c| &c.output == output)
                        .unwrap()
                        .source = source.clone();
                }
                None => {}
            }
        }
        assert!(plan(&after, &snap, &[], false).items.is_empty());
    }

    #[test]
    fn only_filter_narrows_the_plan() {
        let v = live();
        let snap = snapshot_of(&v, &[]);
        let mut now = v;
        set_param(
            &mut now,
            "mixer/1/strip/16/level",
            DeviceParamValue::Level(-12.0),
        );
        set_param(
            &mut now,
            "mixer/1/strip/1/level",
            DeviceParamValue::Level(-3.0),
        );
        let p = plan(&now, &snap, &["mixer/1/strip/16".into()], false);
        assert_eq!(planned(&p), ["mixer/1/strip/16/level"]);
    }

    #[test]
    fn disruptive_needs_opt_in_and_quantization_is_tolerated() {
        let v = live();
        let snap = snapshot_of(&v, &[]);
        let mut now = v;
        set_param(&mut now, "clock/sample_rate", DeviceParamValue::Enum(0));
        set_param(
            &mut now,
            "mixer/1/strip/1/level",
            DeviceParamValue::Level(-8.004),
        );

        let p = plan(&now, &snap, &[], false);
        assert!(planned(&p).is_empty());
        assert_eq!(p.items.len(), 1);
        assert_eq!(
            p.items[0].item.status,
            DeviceRestoreStatus::SkippedDisruptive
        );
        assert_eq!(p.items[0].item.target, "48000 (#1)");
        assert!(p.items[0].op.is_none());

        let p = plan(&now, &snap, &[], true);
        assert_eq!(planned(&p), ["clock/sample_rate"]);
    }

    #[test]
    fn disruptive_writes_are_ordered_first() {
        let v = live();
        let snap = snapshot_of(&v, &[]);
        let mut now = v;
        set_param(
            &mut now,
            "mixer/1/strip/1/level",
            DeviceParamValue::Level(-3.0),
        );
        set_param(&mut now, "clock/sample_rate", DeviceParamValue::Enum(0));
        let p = plan(&now, &snap, &[], true);
        assert_eq!(planned(&p), ["clock/sample_rate", "mixer/1/strip/1/level"]);
    }

    #[test]
    fn missing_and_read_only_targets_are_skipped() {
        let v = live();
        let mut snap = snapshot_of(&v, &[]);
        snap.params.push(DeviceParamSetting::new(
            "clock/locked",
            &DeviceParamValue::Toggle(false),
        ));
        snap.params.push(DeviceParamSetting::new(
            "mixer/9/strip/1/level",
            &DeviceParamValue::Level(0.0),
        ));
        snap.params.push(DeviceParamSetting {
            path: "monitor/dim".into(),
            ..DeviceParamSetting::default()
        });
        snap.routes.push(DeviceRouteSetting {
            path: "route/NOPE0/1".into(),
            source: String::new(),
        });
        snap.routes.push(DeviceRouteSetting {
            path: "route/DIGI_OUT0/2".into(),
            source: "GHOST_IN0:1".into(),
        });
        // The original DIGI_OUT0/2 entry is unchanged; the ghost one differs.
        let p = plan(&v, &snap, &[], true);
        let st: Vec<_> = p
            .items
            .iter()
            .map(|i| (i.item.path.as_str(), i.item.status))
            .collect();
        assert_eq!(
            st,
            [
                ("clock/locked", DeviceRestoreStatus::SkippedReadOnly),
                ("mixer/9/strip/1/level", DeviceRestoreStatus::SkippedMissing),
                ("monitor/dim", DeviceRestoreStatus::SkippedMissing),
                ("route/NOPE0/1", DeviceRestoreStatus::SkippedMissing),
                ("route/DIGI_OUT0/2", DeviceRestoreStatus::SkippedMissing),
            ]
        );
        assert!(p.items.iter().all(|i| i.op.is_none()));
    }
}
