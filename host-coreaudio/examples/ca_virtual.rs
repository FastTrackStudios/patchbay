//! Manage Patchbay's runtime virtual devices through `Patchbay.driver`.
//!
//! ```bash
//! cargo run -p patchbay-host-coreaudio --example ca_virtual -- list
//! cargo run -p patchbay-host-coreaudio --example ca_virtual -- create "Discord Mix" 2
//! cargo run -p patchbay-host-coreaudio --example ca_virtual -- rename Patchbay-Discord-Mix_UID "Stream Mix"
//! cargo run -p patchbay-host-coreaudio --example ca_virtual -- remove Patchbay-Discord-Mix_UID
//! ```

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use patchbay_host_coreaudio::virtual_devices as vd;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or_default();
    match arg(0) {
        "create" => println!("{:?}", vd::create(arg(1), arg(2).parse().unwrap_or(2))?),
        "rename" => vd::rename(arg(1), arg(2))?,
        "remove" => vd::remove(arg(1))?,
        "stats" => println!("{}", vd::runtime_stats()?),
        _ => {}
    }
    if !vd::driver_loaded() {
        println!("Patchbay.driver is not loaded");
        return Ok(());
    }
    for d in vd::list()? {
        println!("{:<24} {:>3} ch  {}", d.name, d.channels, d.uid);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
