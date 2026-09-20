//! The stylesheet, assembled at compile time.
//!
//! Layered so later rules win by order, not by specificity hacks:
//! tokens define the palette and scales, base resets and lays out the
//! shell, ui styles the shared widgets, views carries what each view
//! still owns.

/// Every rule the app ships, in cascade order.
pub const CSS: &str = concat!(
    include_str!("tokens.css"),
    include_str!("base.css"),
    include_str!("ui.css"),
    include_str!("now.css"),
    include_str!("mixes.css"),
    include_str!("devices.css"),
    include_str!("settings.css"),
    include_str!("views.css"),
);
