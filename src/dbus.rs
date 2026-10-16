//! `sonda dbus`: Sonda as the desktop's file manager, on the session bus.
//!
//! - `org.freedesktop.FileManager1`: what a browser or a chat calls for
//!   «Show in folder» (`ShowItems`), and to open folders or a file's
//!   properties.
//! - `org.freedesktop.impl.portal.FileChooser`: the «Open» and «Save» dialog
//!   of every program that asks xdg-desktop-portal for one —Chrome, Firefox
//!   with the portal on, GTK and Qt programs with it—. The dialog is Sonda's
//!   window in picking mode, search included: «the electricity bill from
//!   March», and attach it.
//!
//! The bus starts it when one of them is asked for (its .service files).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::json;
use zbus::object_server::ObjectServer;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::interface;

fn window(args: &[String], env: &[(&str, String)]) -> std::io::Result<std::process::Child> {
    // Updated while it runs, its own file is «… (deleted)»: the new one is where it was.
    let exe = std::env::current_exe()?;
    let exe = PathBuf::from(exe.to_string_lossy().trim_end_matches(" (deleted)"));
    let mut c = Command::new(exe);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.spawn()
}

fn paths_of(uris: &[String]) -> Vec<String> {
    uris.iter().filter_map(|u| if u.starts_with('/') { Some(u.clone()) } else { crate::files::from_uri(u) }).collect()
}

/// Windows opened for the bus, waited for so they leave no zombies.
fn detach(child: std::io::Result<std::process::Child>) {
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

struct FileManager;

#[interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_folders(&self, uris: Vec<String>, _startup_id: String) {
        for p in paths_of(&uris) {
            detach(window(&[p], &[]));
        }
    }

    /// «Show in folder»: its folder, with it chosen.
    fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        for p in paths_of(&uris) {
            detach(window(&["--select".into(), p], &[]));
        }
    }

    fn show_item_properties(&self, uris: Vec<String>, _startup_id: String) {
        for p in paths_of(&uris) {
            detach(window(&["--properties".into(), p], &[]));
        }
    }
}

/// A dialog open for a program: the portal may close it (`Request.Close`).
struct Request {
    pid: Arc<Mutex<Option<u32>>>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {
        if let Some(pid) = *self.pid.lock().unwrap() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
}

fn opt_bool(o: &HashMap<String, Value<'_>>, k: &str) -> bool {
    o.get(k).and_then(|v| v.downcast_ref::<bool>().ok()).unwrap_or(false)
}

fn opt_str(o: &HashMap<String, Value<'_>>, k: &str) -> String {
    o.get(k).and_then(|v| v.downcast_ref::<&str>().ok().map(String::from)).unwrap_or_default()
}

/// A path the portal gives as bytes ending in a NUL.
fn bytes_of(v: &Value<'_>) -> Option<String> {
    let Value::Array(a) = v else { return None };
    let bytes: Vec<u8> = a.iter().filter_map(|x| if let Value::U8(b) = x { Some(*b) } else { None }).collect();
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

fn opt_path(o: &HashMap<String, Value<'_>>, k: &str) -> String {
    o.get(k).and_then(|v| bytes_of(v)).unwrap_or_default()
}

type Filter = (String, Vec<(u32, String)>);

fn opt_filters(o: &HashMap<String, Value<'_>>) -> Vec<Filter> {
    let Some(v) = o.get("filters").and_then(|v| v.try_clone().ok()) else { return Vec::new() };
    Vec::<Filter>::try_from(v).unwrap_or_default()
}

fn opt_filter(o: &HashMap<String, Value<'_>>) -> Option<Filter> {
    let v = o.get("current_filter")?.try_clone().ok()?;
    Filter::try_from(v).ok()
}

fn filter_json(f: &Filter) -> serde_json::Value {
    let globs: Vec<&str> = f.1.iter().filter(|(k, _)| *k == 0).map(|(_, g)| g.as_str()).collect();
    let mimes: Vec<&str> = f.1.iter().filter(|(k, _)| *k == 1).map(|(_, m)| m.as_str()).collect();
    json!({ "name": f.0, "globs": globs, "mimes": mimes })
}

fn owned(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("no file descriptors here")
}

/// What the window answers: the files chosen (paths), and which filter.
struct Picked {
    paths: Vec<String>,
    filter: Option<usize>,
}

/// Opens Sonda's window in picking mode and waits for it to close.
fn pick(spec: serde_json::Value, pid: Arc<Mutex<Option<u32>>>) -> Option<Picked> {
    let dir = std::env::var("XDG_RUNTIME_DIR").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/tmp".into());
    let out = PathBuf::from(dir).join(format!("sonda-pick-{}-{}.json", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    let start = spec["folder"].as_str().filter(|f| !f.is_empty() && Path::new(f).is_dir()).map(String::from);
    let mut args = Vec::new();
    if let Some(s) = start {
        args.push(s);
    }
    let mut child = window(&args, &[("SONDA_PICK", spec.to_string()), ("SONDA_PICK_OUT", out.to_string_lossy().into_owned())]).ok()?;
    *pid.lock().unwrap() = Some(child.id());
    let _ = child.wait();
    *pid.lock().unwrap() = None;
    let text = std::fs::read_to_string(&out).ok();
    let _ = std::fs::remove_file(&out);
    let v: serde_json::Value = serde_json::from_str(&text?).ok()?;
    let paths: Vec<String> = v["paths"].as_array()?.iter().filter_map(|p| p.as_str().map(String::from)).collect();
    if paths.is_empty() {
        return None;
    }
    Some(Picked { paths, filter: v["filter"].as_u64().map(|f| f as usize) })
}

struct Chooser;

impl Chooser {
    async fn ask(&self, server: &ObjectServer, handle: OwnedObjectPath, spec: serde_json::Value, filters: Vec<Filter>, save_names: Option<Vec<String>>) -> (u32, HashMap<String, OwnedValue>) {
        let pid = Arc::new(Mutex::new(None));
        let _ = server.at(&handle, Request { pid: pid.clone() }).await;
        let p = pid.clone();
        let picked = blocking::unblock(move || pick(spec, p)).await;
        let _ = server.remove::<Request, _>(&handle).await;
        let mut results = HashMap::new();
        let Some(picked) = picked else { return (1, results) };
        let uris: Vec<String> = match save_names {
            // SaveFiles: a folder was chosen; the files go in it with their names.
            Some(names) => {
                let dir = Path::new(&picked.paths[0]);
                names.iter().map(|n| crate::files::uri(&dir.join(n).to_string_lossy())).collect()
            }
            None => picked.paths.iter().map(|p| crate::files::uri(p)).collect(),
        };
        results.insert("uris".to_owned(), owned(Value::from(uris)));
        if let Some(f) = picked.filter.and_then(|i| filters.get(i)) {
            results.insert("current_filter".to_owned(), owned(Value::from(f.clone())));
        }
        results.insert("writable".to_owned(), owned(Value::from(true)));
        (0, results)
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl Chooser {
    async fn open_file(&self, #[zbus(object_server)] server: &ObjectServer, handle: OwnedObjectPath, app_id: String, _parent_window: String, title: String, options: HashMap<String, Value<'_>>) -> (u32, HashMap<String, OwnedValue>) {
        let filters = opt_filters(&options);
        let current = opt_filter(&options).and_then(|c| filters.iter().position(|f| *f == c)).unwrap_or(0);
        let folder = if opt_bool(&options, "directory") { "folder" } else { "open" };
        eprintln!("sonda · {app_id} asks for a file: «{title}»");
        let spec = json!({
            "mode": folder, "multiple": opt_bool(&options, "multiple"), "title": title, "app": app_id,
            "accept": opt_str(&options, "accept_label"), "folder": opt_path(&options, "current_folder"),
            "filters": filters.iter().map(filter_json).collect::<Vec<_>>(), "filter": current,
        });
        self.ask(server, handle, spec, filters, None).await
    }

    async fn save_file(&self, #[zbus(object_server)] server: &ObjectServer, handle: OwnedObjectPath, app_id: String, _parent_window: String, title: String, options: HashMap<String, Value<'_>>) -> (u32, HashMap<String, OwnedValue>) {
        let filters = opt_filters(&options);
        let current = opt_filter(&options).and_then(|c| filters.iter().position(|f| *f == c)).unwrap_or(0);
        let file = opt_path(&options, "current_file");
        let mut folder = opt_path(&options, "current_folder");
        let mut name = opt_str(&options, "current_name");
        if !file.is_empty() {
            let f = Path::new(&file);
            if folder.is_empty() {
                folder = f.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            }
            if name.is_empty() {
                name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            }
        }
        let spec = json!({
            "mode": "save", "title": title, "app": app_id, "accept": opt_str(&options, "accept_label"),
            "folder": folder, "name": name, "filters": filters.iter().map(filter_json).collect::<Vec<_>>(), "filter": current,
        });
        self.ask(server, handle, spec, filters, None).await
    }

    async fn save_files(&self, #[zbus(object_server)] server: &ObjectServer, handle: OwnedObjectPath, app_id: String, _parent_window: String, title: String, options: HashMap<String, Value<'_>>) -> (u32, HashMap<String, OwnedValue>) {
        let names: Vec<String> = options
            .get("files")
            .and_then(|v| if let Value::Array(a) = v { Some(a.iter().filter_map(bytes_of).collect()) } else { None })
            .unwrap_or_default();
        let spec = json!({ "mode": "folder", "title": title, "app": app_id, "accept": opt_str(&options, "accept_label"), "folder": opt_path(&options, "current_folder"), "filters": [], "filter": 0 });
        self.ask(server, handle, spec, Vec::new(), Some(names)).await
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}

pub fn run() -> Result<()> {
    use zbus::fdo::RequestNameFlags;
    let conn = zbus::blocking::connection::Builder::session()?
        .serve_at("/org/freedesktop/FileManager1", FileManager)?
        .serve_at("/org/freedesktop/portal/desktop", Chooser)?
        .build()?;
    // Each name on its own: another file manager running keeps working if
    // one of them cannot be had.
    for name in ["org.freedesktop.impl.portal.desktop.sonda", "org.freedesktop.FileManager1"] {
        match conn.request_name_with_flags(name, RequestNameFlags::ReplaceExisting | RequestNameFlags::DoNotQueue) {
            Ok(_) => eprintln!("sonda · on the bus as {name}"),
            Err(e) => eprintln!("sonda · {name}: {e}"),
        }
    }
    // Started by the bus, it lives as long as the bus does.
    let bus = zbus::blocking::fdo::DBusProxy::new(&conn)?;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        if bus.get_id().is_err() {
            return Ok(());
        }
    }
}
