//! Known overlays and the sim, which must not run during a measurement.

use serde::{Deserialize, Serialize};

use crate::collect::ProcessEntry;
use crate::collect::tree::same_exe;

/// Executable names of overlays and capture tools that draw over games or hook into them. Names change between
/// versions; a plan can add more with `detect`.
pub const KNOWN: &[(&str, &str)] = &[
    (
        "RTSS.exe",
        "RivaTuner Statistics Server (Afterburner overlay)",
    ),
    ("MSIAfterburner.exe", "MSI Afterburner"),
    ("Discord.exe", "Discord (in-game overlay)"),
    ("NVIDIA Overlay.exe", "NVIDIA overlay"),
    ("RadeonSoftware.exe", "AMD Software overlay"),
    ("steam.exe", "Steam (overlay in games it starts)"),
    ("GameOverlayUI.exe", "Steam overlay"),
    ("GameBar.exe", "Xbox Game Bar"),
    ("CapFrameX.exe", "CapFrameX"),
    ("SimHubWPF.exe", "SimHub"),
    // The overlay and the Pitot window since 0.1.4; the overlay alone before.
    ("pitot.exe", "Pitot"),
    ("pitot-overlay.exe", "Pitot overlay"),
];

/// The sim itself. Tier A runs without it.
pub const SIM: &[(&str, &str)] = &[
    ("iRacingSim64DX11.exe", "iRacing sim"),
    ("iRacingUI.exe", "iRacing UI"),
];

/// A running process that should not run during a measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub pid: u32,
    pub name: String,
    pub what: String,
}

/// Known overlays, the sim, and `extra` names among `entries`, in process-list order.
pub fn find(entries: &[ProcessEntry], extra: &[String]) -> Vec<Found> {
    let mut found = Vec::new();
    for entry in entries {
        let known = KNOWN
            .iter()
            .chain(SIM)
            .find(|(name, _)| same_exe(name, &entry.name))
            .map(|(_, what)| (*what).to_owned())
            .or_else(|| {
                extra
                    .iter()
                    .any(|name| same_exe(name, &entry.name))
                    .then(|| "listed in the plan".to_owned())
            });
        if let Some(what) = known {
            found.push(Found {
                pid: entry.pid,
                name: entry.name.clone(),
                what,
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, name: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent: 1,
            name: name.to_owned(),
        }
    }

    #[test]
    fn finds_known_and_listed_names() {
        let entries = [
            entry(10, "explorer.exe"),
            entry(11, "rtss.exe"),
            entry(12, "iRacingSim64DX11.exe"),
            entry(13, "MyOverlay.exe"),
            entry(14, "Discord.exe"),
            entry(15, "pitot.exe"),
            entry(16, "Pitot-Overlay.exe"),
        ];
        let found = find(&entries, &["myoverlay".to_owned()]);
        let pids: Vec<u32> = found.iter().map(|f| f.pid).collect();
        assert_eq!(pids, [11, 12, 13, 14, 15, 16]);
        assert_eq!(found[2].what, "listed in the plan");
        assert_eq!(found[4].what, "Pitot");
        assert_eq!(found[5].what, "Pitot overlay");
        assert!(find(&entries[..1], &[]).is_empty());
    }
}
