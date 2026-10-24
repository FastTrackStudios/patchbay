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

use crate::proto::{
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
