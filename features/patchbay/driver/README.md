# patchbay-driver

The `Patchbay.driver` AudioServerPlugIn. It publishes **loopback** virtual
audio devices — whatever an app plays into a device comes back out of that
device's input — and lets Patchbay.app create, rename and remove them **at
runtime**, without restarting coreaudiod.

- **Control:** Patchbay.app writes a JSON `DesiredState` (`{devices: [{uid,
  name, channels, hidden}]}`) to the plug-in object's custom property
  `'pbds'`; the plug-in diffs it against what it publishes and asks the
  host for a device-configuration change, applying it when the host says
  it's safe (`PerformDeviceConfigurationChange`).
- **Persistence:** the applied state is kept in coreaudiod's per-plug-in
  storage (`WriteToStorage`), so devices come back after a coreaudiod
  restart or reboot even when Patchbay.app isn't running. First load
  (nothing stored) publishes **Patchbay** (16 ch) and **Broadcast** (2 ch).
- **Audio:** per device, a sample-time-indexed ring (BlackHole's scheme):
  `WriteMix` writes at the output time, `ReadInput` reads at the input
  time, and silence is returned (and the ring cleared once) when nothing
  has been written recently.
- **Clock:** a host-clock anchored zero timestamp (from MARS).

Forked from [MARS](https://github.com/JacobLinCool/mars) `mars-hal`
(MIT, see `LICENSE.MARS`, commit in `UPSTREAM`): the COM vtable, property
handling, runtime device registry and configuration-change flow. The
shared-memory transport was replaced by the in-driver loopback, and the
devices are single loopback devices with an input and an output stream.
