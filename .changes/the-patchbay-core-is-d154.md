---
"patchbay" = "minor"
---

The Patchbay core is a linked Architect extension (patchbay::ext::Patchbay): the patchbay view and the PATCHBAY_STATUS, _HEALTH, _NOW, _DEVICE_LIST and _DEVICE_SHOW actions, over one engine per machine (patchbay::home: joins a running engine or starts one, and takes it over when its app quits; PATCHBAY_PIPEWIRE=off keeps an engine off PipeWire, whose client is now named "patchbay"). (studio)
