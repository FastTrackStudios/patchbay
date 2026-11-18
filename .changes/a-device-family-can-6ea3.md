---
"patchbay" = "minor"
---

A device family can add its own RPC section to the engine (DeviceFamily::routes); every client reaches an engine over one connection with a lane per service (patchbay::lanes::Lanes; ui::kit::use_family_lane (by kind; an engine without the family reads as not here) for a family view; Engine::lanes; server::dial returns the lanes). The core service and its stream are now lanes on one websocket instead of two (studio)
