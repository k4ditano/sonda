//! Drives: USB sticks, discs and the other disks of the machine that are not
//! the system's own, mounted and unmounted through udisks (as any desktop
//! does: a stick needs nobody's password, an internal disk asks polkit).

use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Drive {
    pub name: String,
    pub dev: String,
    /// Where it is mounted, or "".
    pub mount: String,
    pub size_text: String,
    /// A stick or a card: it can be ejected.
    pub removable: bool,
    pub icon: String,
}

/// Mounted where the system lives: not a drive to show.
fn system_mount(m: &str) -> bool {
    !(m.starts_with("/run/media/") || m.starts_with("/media/") || m.starts_with("/mnt/"))
}

fn walk(v: &Value, parent_removable: bool, parent_model: &str, out: &mut Vec<Drive>) {
    let removable = parent_removable || v["rm"].as_bool().unwrap_or(false) || v["hotplug"].as_bool().unwrap_or(false);
    let model = v["model"].as_str().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| parent_model.to_owned());
    let kind = v["type"].as_str().unwrap_or("");
    let fstype = v["fstype"].as_str().unwrap_or("");
    let mounts: Vec<String> = v["mountpoints"].as_array().map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect()).unwrap_or_default();
    let usable = matches!(kind, "part" | "disk" | "crypt" | "rom") && !fstype.is_empty() && !["swap", "crypto_LUKS", "LVM2_member", "linux_raid_member", "zfs_member"].contains(&fstype);
    let name_dev = v["name"].as_str().unwrap_or("");
    if usable && !name_dev.starts_with("zram") && !name_dev.starts_with("loop") && !mounts.iter().any(|m| system_mount(m) || m == "[SWAP]") {
        let size = v["size"].as_u64().unwrap_or(0);
        let label = v["label"].as_str().filter(|s| !s.is_empty()).map(String::from);
        let es = crate::files::spanish();
        let name = label.unwrap_or_else(|| if model.is_empty() { format!("{} {}", if es { "Volumen de" } else { "Volume," }, crate::files::size_text(size)) } else { model.clone() });
        out.push(Drive {
            name,
            dev: v["path"].as_str().unwrap_or("").to_owned(),
            mount: mounts.first().cloned().unwrap_or_default(),
            size_text: crate::files::size_text(size),
            removable,
            icon: if kind == "rom" { "media-optical" } else if removable { "drive-removable-media" } else { "drive-harddisk" }.into(),
        });
    }
    if let Some(children) = v["children"].as_array() {
        for c in children {
            walk(c, removable, &model, out);
        }
    }
}

pub fn list() -> Vec<Drive> {
    let Ok(o) = Command::new("lsblk").args(["-J", "-b", "-o", "NAME,PATH,LABEL,SIZE,FSTYPE,MOUNTPOINTS,HOTPLUG,RM,TYPE,MODEL"]).stderr(Stdio::null()).output() else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&o.stdout) else { return Vec::new() };
    let mut out = Vec::new();
    for d in v["blockdevices"].as_array().into_iter().flatten() {
        walk(d, false, "", &mut out);
    }
    out
}

fn udisks(args: &[&str]) -> Result<String> {
    let o = Command::new("udisksctl").args(args).stdin(Stdio::null()).output()?;
    if !o.status.success() {
        let e = String::from_utf8_lossy(&o.stderr);
        bail!("{}", e.trim().lines().last().unwrap_or("udisksctl failed").trim_start_matches("Error ").trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Mounts it, and says where.
pub fn mount(dev: &str) -> Result<String> {
    let said = udisks(&["mount", "-b", dev])?;
    // «Mounted /dev/sdb1 at /run/media/abel/STICK»
    if let Some(at) = said.split(" at ").nth(1) {
        return Ok(at.trim().trim_end_matches('.').to_owned());
    }
    list().into_iter().find(|d| d.dev == dev && !d.mount.is_empty()).map(|d| d.mount).ok_or_else(|| anyhow::anyhow!("mounted, but where is not known"))
}

pub fn unmount(dev: &str) -> Result<()> {
    udisks(&["unmount", "-b", dev]).map(|_| ())
}

/// Unmounted, and the stick switched off so it can be pulled out.
pub fn eject(dev: &str) -> Result<()> {
    if list().iter().any(|d| d.dev == dev && !d.mount.is_empty()) {
        unmount(dev)?;
    }
    // The whole disk the partition is on.
    let disk = Command::new("lsblk").args(["-no", "PKNAME", dev]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).filter(|s| !s.is_empty()).map(|p| format!("/dev/{p}")).unwrap_or_else(|| dev.to_owned());
    let _ = udisks(&["power-off", "-b", &disk]);
    Ok(())
}
