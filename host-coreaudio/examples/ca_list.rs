//! Print the Core Audio host graph as JSON; optionally watch for changes.
//!
//! ```bash
//! cargo run -p patchbay-host-coreaudio --example ca_list
//! cargo run -p patchbay-host-coreaudio --example ca_list -- --watch 20   # + events for 20 s
//! ```
//!
//! Read-only: enumerates devices and audio-client processes, registers
//! property listeners, changes nothing.

#[cfg(target_os = "macos")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use patchbay_host::HostBackend;
    use patchbay_host_coreaudio::CoreAudioBackend;
    use tokio::sync::broadcast::error::RecvError;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let watch_secs = match args.iter().position(|a| a == "--watch") {
        Some(i) => args
            .get(i.saturating_add(1))
            .map_or(Ok(10), |s| s.parse::<u64>())?,
        None => 0,
    };

    let backend = CoreAudioBackend::new()?;
    let mut events = backend.subscribe();
    let snapshot = backend.snapshot().await?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    eprintln!(
        "{} nodes; default output = {:?}, default input = {:?}",
        snapshot.nodes.len(),
        backend.default_device(true),
        backend.default_device(false)
    );

    if watch_secs > 0 {
        eprintln!("watching for {watch_secs} s (events as JSON lines on stdout)…");
        let deadline = tokio::time::sleep(Duration::from_secs(watch_secs));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                () = &mut deadline => break,
                event = events.recv() => match event {
                    Ok(event) => println!("{}", serde_json::to_string(&event)?),
                    Err(RecvError::Lagged(n)) => eprintln!("lagged {n} events"),
                    Err(RecvError::Closed) => break,
                },
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ca_list: Core Audio is macOS-only");
}
