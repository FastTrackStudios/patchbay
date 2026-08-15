//! ChanMap parse/merge roundtrip (no PipeWire needed).

use std::collections::BTreeMap;

#[test]
fn roundtrip_preserves_foreign_lines() {
    let dir = std::env::temp_dir().join(format!("patchbay-chanmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.ReaperChanMap");
    std::fs::write(
        &path,
        "[reaper_chanmap]\nch0=0\nch1=1\nname0=1 - Kick In\nname5=6 - Old Name\n",
    )
    .unwrap();
    let path_str = path.to_str().unwrap();

    let names = patchbay::chanmap::read_names(path_str).unwrap();
    assert_eq!(names.get(&1).unwrap(), "1 - Kick In");
    assert_eq!(names.get(&6).unwrap(), "6 - Old Name");

    // Overwrite channel 6, add channel 23; keep 1 and the ch lines.
    let mut update = BTreeMap::new();
    update.insert(6, "Bass".to_string());
    update.insert(23, "Guitar".to_string());
    patchbay::chanmap::write_names(path_str, &update).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[reaper_chanmap]"));
    assert!(text.contains("ch1=1"));
    assert!(text.contains("name0=1 - Kick In"));
    assert!(text.contains("name5=Bass"));
    assert!(text.contains("name22=Guitar"));
    assert!(!text.contains("Old Name"));

    let names = patchbay::chanmap::read_names(path_str).unwrap();
    assert_eq!(names.get(&23).unwrap(), "Guitar");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn creates_identity_map_when_missing() {
    let dir = std::env::temp_dir().join(format!("patchbay-chanmap-new-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fresh.ReaperChanMap");
    let mut names = BTreeMap::new();
    names.insert(23, "Guitar".to_string());
    patchbay::chanmap::write_names(path.to_str().unwrap(), &names).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("[reaper_chanmap]"));
    assert!(text.contains("ch127=127"));
    assert!(text.contains("name22=Guitar"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn channel_of_port() {
    assert_eq!(patchbay::chanmap::channel_of_port("playback_97"), Some(97));
    assert_eq!(patchbay::chanmap::channel_of_port("capture_1"), Some(1));
    assert_eq!(patchbay::chanmap::channel_of_port("monitor_12"), Some(12));
    assert_eq!(patchbay::chanmap::channel_of_port("midi_out"), None);
    assert_eq!(patchbay::chanmap::channel_of_port("42"), None);
}
