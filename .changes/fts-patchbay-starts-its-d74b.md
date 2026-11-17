---
"fts-patchbay" = "minor"
---

fts-patchbay joins the Patchbay engine already running on its port (an Architect app with the Patchbay extension, patchbay serve) instead of running a second one, and takes it over if that app quits; status and health come from patchbay::report. (studio)
