---
"fts-patchbay" = "minor"
"patchbay" = "minor"
---

Added device families: the device hub connects each configured device through the family for its kind (patchbay::family), a family registered from outside the crate (patchbay::family::register) joins the built-ins, a family that panics fails only its own devices, and patchbay families / PATCHBAY_FAMILIES list them with where they run. (studio)
