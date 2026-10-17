//! On battery? From `/sys/class/power_supply`, polled by the daemon every
//! few seconds (a handful of tiny sysfs reads).

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Supply {
    pub kind: String,
    pub online: Option<bool>,
    /// `Device` for a mouse's or headset's battery.
    pub scope: String,
}

/// On battery: the machine has a (system) battery and no external supply
/// is online. Desktops (no battery) never are.
pub fn on_battery(supplies: &[Supply]) -> bool {
    let has_battery = supplies
        .iter()
        .any(|s| s.kind == "Battery" && s.scope != "Device");
    let external_online = supplies
        .iter()
        .any(|s| s.kind != "Battery" && s.scope != "Device" && s.online == Some(true));
    has_battery && !external_online
}

pub fn read(root: &Path) -> Vec<Supply> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let get = |p: &Path, f: &str| {
        std::fs::read_to_string(p.join(f))
            .ok()
            .map(|s| s.trim().to_string())
    };
    rd.flatten()
        .map(|e| e.path())
        .map(|p| Supply {
            kind: get(&p, "type").unwrap_or_default(),
            online: get(&p, "online").map(|s| s == "1"),
            scope: get(&p, "scope").unwrap_or_default(),
        })
        .collect()
}

pub fn on_battery_now() -> bool {
    on_battery(&read(Path::new("/sys/class/power_supply")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(kind: &str, online: Option<bool>, scope: &str) -> Supply {
        Supply {
            kind: kind.into(),
            online,
            scope: scope.into(),
        }
    }

    #[test]
    fn laptop_desktop_and_peripherals() {
        let bat = s("Battery", None, "");
        assert!(on_battery(&[bat.clone(), s("Mains", Some(false), "")]));
        assert!(!on_battery(&[bat.clone(), s("Mains", Some(true), "")]));
        assert!(
            !on_battery(&[
                bat.clone(),
                s("Mains", Some(false), ""),
                s("USB", Some(true), "")
            ]),
            "USB-C charger"
        );
        assert!(!on_battery(&[]), "desktop");
        assert!(
            !on_battery(&[s("Battery", None, "Device")]),
            "a mouse's battery"
        );
    }

    #[test]
    fn reads_sysfs_layout() {
        let root = std::env::temp_dir().join(format!("ukiyo-power-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, kind, online) in [("BAT0", "Battery", None), ("AC", "Mains", Some("0"))] {
            let d = root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("type"), format!("{kind}\n")).unwrap();
            if let Some(o) = online {
                std::fs::write(d.join("online"), format!("{o}\n")).unwrap();
            }
        }
        assert!(on_battery(&read(&root)));
        std::fs::write(root.join("AC/online"), "1\n").unwrap();
        assert!(!on_battery(&read(&root)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
