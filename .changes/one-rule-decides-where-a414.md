---
"patchbay" = "minor"
---

Changed how the Patchbay engine starts: the engine config decides where it listens, not whichever app starts it; no app runs a second engine while a Patchbay holds the port (it waits, then refuses); PATCHBAY_PIPEWIRE=off is logged as a warning and shows in status and health. (studio)
