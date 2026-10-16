//! `sonda bridge`: what the window's logic talks to, a JSON line each way
//! (the logic `spawn`s it with its input open). It does the file manager's
//! work here —folders, small pictures, copies, the trash— and asks the
//! daemon for searches and how the reading goes.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::Result;
use serde_json::{Value, json};

use crate::{files, jobs};

struct Out(Mutex<std::io::Stdout>);

impl Out {
    fn send(&self, v: Value) {
        let mut o = self.0.lock().unwrap();
        let _ = o.write_all(v.to_string().as_bytes());
        let _ = o.write_all(b"\n");
        let _ = o.flush();
    }
}

/// Small pictures to make, newest asks first; a new folder or search drops
/// what the last one asked for.
struct Thumbs {
    queue: Mutex<VecDeque<(u64, String, String)>>,
    ready: Condvar,
    generation: AtomicU64,
}

fn thumb_worker(thumbs: Arc<Thumbs>, out: Arc<Out>) {
    loop {
        let (generation_id, path, kind) = {
            let mut q = thumbs.queue.lock().unwrap();
            while q.is_empty() {
                q = thumbs.ready.wait(q).unwrap();
            }
            q.pop_front().unwrap()
        };
        if generation_id != thumbs.generation.load(Ordering::Relaxed) {
            continue;
        }
        if let Some(t) = files::thumb(Path::new(&path), &kind) {
            out.send(json!({ "t": "thumb", "gen": generation_id, "path": path, "thumb": t.to_string_lossy() }));
        }
    }
}

fn want_thumbs(thumbs: &Thumbs, generation_id: u64, items: &[(String, String)]) {
    let mut q = thumbs.queue.lock().unwrap();
    q.retain(|x| x.0 == generation_id);
    for (path, kind) in items {
        if matches!(kind.as_str(), "image" | "screenshot" | "video" | "pdf") {
            q.push_back((generation_id, path.clone(), kind.clone()));
        }
    }
    thumbs.ready.notify_all();
}

pub fn places() -> Value {
    let es = files::spanish();
    let home = crate::paths::home();
    let mut items = vec![json!({ "name": if es { "Inicio" } else { "Home" }, "path": home.to_string_lossy(), "icon": "home" })];
    for (key, en, es_name, icon) in [
        ("DESKTOP", "Desktop", "Escritorio", "desktop"),
        ("DOCUMENTS", "Documents", "Documentos", "documents"),
        ("DOWNLOAD", "Downloads", "Descargas", "downloads"),
        ("PICTURES", "Pictures", "Imágenes", "pictures"),
        ("MUSIC", "Music", "Música", "music"),
        ("VIDEOS", "Videos", "Vídeos", "videos"),
        ("PROJECTS", "Projects", "Proyectos", "projects"),
    ] {
        if let Some(d) = crate::paths::user_dir(key) {
            items.push(json!({ "name": if es { es_name } else { en }, "path": d.to_string_lossy(), "icon": icon }));
        }
    }
    items.push(json!({ "name": if es { "Papelera" } else { "Trash" }, "path": files::trash_dir().to_string_lossy(), "icon": "trash" }));
    items.push(json!({ "name": if es { "Equipo" } else { "Computer" }, "path": "/", "icon": "computer" }));
    json!({ "t": "places", "items": items, "home": home.to_string_lossy(), "trash": files::trash_dir().to_string_lossy() })
}

fn crumbs(path: &Path) -> Vec<Value> {
    let home = crate::paths::home();
    let es = files::spanish();
    let mut out = Vec::new();
    let mut p = Some(path);
    while let Some(d) = p {
        if d == home {
            out.push(json!({ "name": if es { "Inicio" } else { "Home" }, "path": d.to_string_lossy() }));
            break;
        }
        if d == files::trash_dir() {
            out.push(json!({ "name": if es { "Papelera" } else { "Trash" }, "path": d.to_string_lossy() }));
            break;
        }
        let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        out.push(json!({ "name": name, "path": d.to_string_lossy() }));
        p = d.parent();
    }
    out.reverse();
    out
}

/// A place typed in the location bar: `~` is home, a relative one is from
/// the folder shown, `..` climbs. A folder is gone to; a file, its folder
/// with it chosen. With `complete` (Tab), the name being typed is finished
/// as far as the folders that start like it agree.
fn where_to(typed: &str, base: &str, complete: bool) -> Value {
    let typed = typed.trim();
    let home = crate::paths::home();
    let raw = if typed == "~" {
        home.clone()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        home.join(rest)
    } else if typed.is_empty() {
        PathBuf::from(base)
    } else {
        Path::new(base).join(typed)
    };
    let mut path = PathBuf::from("/");
    for c in raw.components() {
        match c {
            std::path::Component::ParentDir => {
                path.pop();
            }
            std::path::Component::Normal(n) => path.push(n),
            _ => {}
        }
    }
    if complete {
        // What is typed after the last slash is the start of a name.
        let (dir, start) = if typed.ends_with('/') || typed.is_empty() { (path.clone(), String::new()) } else { (path.parent().map(Path::to_path_buf).unwrap_or_else(|| "/".into()), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) };
        let low = start.to_lowercase();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.retain(|n| n.to_lowercase().starts_with(&low) && (start.starts_with('.') || !n.starts_with('.')));
        names.sort();
        let Some(first) = names.first() else {
            return json!({ "t": "where", "complete": true, "matches": 0 });
        };
        // As far as all of them agree (ignoring case, as it was matched).
        let mut common: Vec<char> = first.chars().collect();
        for n in &names[1..] {
            let same = common.iter().zip(n.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
            common.truncate(same);
        }
        let mut done = dir.join(common.iter().collect::<String>()).to_string_lossy().into_owned();
        if names.len() == 1 {
            done.push('/');
        }
        return json!({ "t": "where", "complete": true, "matches": names.len(), "text": done, "names": names.iter().take(8).collect::<Vec<_>>() });
    }
    if path.is_dir() {
        json!({ "t": "where", "dir": path.to_string_lossy() })
    } else if path.exists() {
        json!({ "t": "where", "dir": path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into()), "select": path.to_string_lossy() })
    } else {
        json!({ "t": "where", "missing": true, "path": path.to_string_lossy() })
    }
}

fn tell_daemon(paths: &[String]) {
    let mut all: Vec<String> = paths.to_vec();
    // A folder's parent changes too (something left it).
    for p in paths {
        if let Some(parent) = Path::new(p).parent() {
            all.push(parent.to_string_lossy().into_owned());
        }
    }
    std::thread::spawn(move || {
        let _ = crate::daemon::ask(&json!({ "op": "changed", "paths": all }));
    });
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// The words of a document or of code around where it was found.
fn text_preview(path: &str, kind: &str, loc: Option<f64>, segment: &str) -> Option<String> {
    let p = Path::new(path);
    if kind == "code" || (kind == "doc" && segment != "chunk") {
        let text = crate::extract::read_text_file(p).ok()?;
        let lines: Vec<&str> = text.lines().collect();
        let start = loc.map(|l| (l as usize).saturating_sub(1)).unwrap_or(0).min(lines.len().saturating_sub(1));
        return Some(lines[start..(start + 60).min(lines.len())].join("\n"));
    }
    if kind == "doc" {
        if let Ok(v) = crate::daemon::ask(&json!({ "op": "text", "path": path, "loc": loc })) {
            if let Some(t) = v["text"].as_str() {
                return Some(t.to_owned());
            }
        }
        let text = crate::extract::doc_text(p).ok()?;
        return Some(text.chars().take(4000).collect());
    }
    if kind == "pdf" {
        if let Ok(v) = crate::daemon::ask(&json!({ "op": "text", "path": path, "loc": loc })) {
            if let Some(t) = v["text"].as_str() {
                return Some(t.to_owned());
            }
        }
    }
    None
}

fn handle(req: &Value, out: &Arc<Out>, thumbs: &Arc<Thumbs>, jobs: &Arc<jobs::Jobs>) -> Result<()> {
    let op = req["op"].as_str().unwrap_or("");
    match op {
        "places" => out.send(places()),
        "where" => out.send(where_to(req["text"].as_str().unwrap_or(""), req["base"].as_str().unwrap_or("/"), req["complete"].as_bool().unwrap_or(false))),
        "list" => {
            let path = PathBuf::from(req["path"].as_str().unwrap_or("/"));
            let hidden = req["hidden"].as_bool().unwrap_or(false);
            let sort = req["sort"].as_str().unwrap_or("name");
            let generation_id = thumbs.generation.fetch_add(1, Ordering::Relaxed) + 1;
            match files::list(&path, hidden, sort) {
                Ok(mut items) => {
                    // Picking a file for a program: only what its filter lets through.
                    let globs = strs(&req["filter"]["globs"]);
                    let mimes = strs(&req["filter"]["mimes"]);
                    if !globs.is_empty() || !mimes.is_empty() {
                        items.retain(|e| e.dir || crate::apps::passes(Path::new(&e.path), &globs, &mimes));
                    }
                    if req["dirs_only"].as_bool().unwrap_or(false) {
                        items.retain(|e| e.dir);
                    }
                    let in_trash = path == files::trash_dir();
                    let wanted: Vec<(String, String)> = items.iter().map(|e| (e.path.clone(), e.kind.clone())).collect();
                    if in_trash {
                        for e in items.iter_mut() {
                            if let Some(o) = files::trash_origin(&e.name) {
                                e.date_text = o;
                            }
                        }
                    }
                    let parent = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                    let free = files::disk_space(&path).map(|(f, _)| files::size_text(f)).unwrap_or_default();
                    let writable = std::fs::metadata(&path).map(|m| !m.permissions().readonly()).unwrap_or(false) && unsafe { libc::access(std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default().as_ptr(), libc::W_OK) } == 0;
                    out.send(json!({ "t": "list", "gen": generation_id, "path": path.to_string_lossy(), "parent": parent, "crumbs": crumbs(&path), "items": items, "trash": in_trash, "free": free, "writable": writable, "keep": req["keep"].as_bool().unwrap_or(false) }));
                    want_thumbs(thumbs, generation_id, &wanted);
                }
                Err(e) => out.send(json!({ "t": "error", "op": "list", "path": path.to_string_lossy(), "error": e.to_string() })),
            }
        }
        "search" => {
            let generation_id = thumbs.generation.fetch_add(1, Ordering::Relaxed) + 1;
            let mut q = req.clone();
            q["op"] = json!("search");
            let out = out.clone();
            let thumbs = thumbs.clone();
            std::thread::spawn(move || {
                crate::daemon::ensure();
                match crate::daemon::ask(&q) {
                    Ok(mut v) => {
                        let mut wanted = Vec::new();
                        let globs = strs(&q["filter"]["globs"]);
                        let mimes = strs(&q["filter"]["mimes"]);
                        if let Some(hits) = v["hits"].as_array_mut() {
                            if !globs.is_empty() || !mimes.is_empty() {
                                hits.retain(|h| crate::apps::passes(Path::new(h["path"].as_str().unwrap_or("")), &globs, &mimes));
                            }
                            for h in hits.iter_mut() {
                                let path = h["path"].as_str().unwrap_or("").to_owned();
                                let kind = h["kind"].as_str().unwrap_or("").to_owned();
                                let (_, icon) = files::kind_of(Path::new(&path), false);
                                h["icon"] = json!(icon);
                                h["date_text"] = json!(files::date_text(h["mtime"].as_f64().unwrap_or(0.0)));
                                h["size_text"] = json!(files::size_text(h["size"].as_u64().unwrap_or(0)));
                                h["folder"] = json!(Path::new(&path).parent().map(|p| p.to_string_lossy().replace(&*crate::paths::home().to_string_lossy(), "~")).unwrap_or_default());
                                wanted.push((path, kind));
                            }
                        }
                        v["t"] = json!("results");
                        v["gen"] = json!(generation_id);
                        out.send(v);
                        want_thumbs(&thumbs, generation_id, &wanted);
                    }
                    Err(e) => out.send(json!({ "t": "error", "op": "search", "error": e.to_string() })),
                }
            });
        }
        "preview" => {
            let path = req["path"].as_str().unwrap_or("").to_owned();
            let kind = req["kind"].as_str().unwrap_or("").to_owned();
            let segment = req["segment"].as_str().unwrap_or("").to_owned();
            let loc = req["loc"].as_f64();
            let token = req["token"].clone();
            let out = out.clone();
            std::thread::spawn(move || {
                let image = files::preview(Path::new(&path), &kind, loc).map(|p| p.to_string_lossy().into_owned());
                let text = text_preview(&path, &kind, loc, &segment);
                let size = if Path::new(&path).is_dir() { Some(files::size_text(files::folder_size(Path::new(&path)))) } else { None };
                out.send(json!({ "t": "preview", "token": token, "path": path, "image": image, "text": text, "folder_size": size }));
            });
        }
        "open" => {
            let path = req["path"].as_str().unwrap_or("");
            let r = match req["loc"].as_f64() {
                Some(l) => files::open_at(path, req["kind"].as_str().unwrap_or(""), Some(l)),
                None => files::open(path),
            };
            out.send(json!({ "t": "done", "op": "open", "ok": r.is_ok(), "error": r.err().map(|e| e.to_string()) }));
        }
        "copy" | "move" | "drop" => {
            let dest = PathBuf::from(req["dest"].as_str().unwrap_or("/nonexistent"));
            let (op, sources) = if op == "drop" {
                // What another program dropped: a list of file:// addresses.
                let data = req["data"].as_str().unwrap_or("");
                let dropped: Vec<PathBuf> = data.lines().filter_map(|l| files::from_uri(l.trim()).or_else(|| l.trim().starts_with('/').then(|| l.trim().to_owned()))).map(PathBuf::from).filter(|p| p.exists()).collect();
                (if req["move"].as_bool().unwrap_or(false) { jobs::Op::Move } else { jobs::Op::Copy }, dropped)
            } else {
                (if op == "move" { jobs::Op::Move } else { jobs::Op::Copy }, strs(&req["paths"]).into_iter().map(PathBuf::from).collect())
            };
            let policy = req["policy"].as_str().and_then(jobs::Policy::parse);
            if let Err(e) = jobs.transfer(op, sources, dest, policy) {
                out.send(json!({ "t": "done", "op": if op == jobs::Op::Move { "move" } else { "copy" }, "ok": false, "error": e.to_string() }));
            }
        }
        "resolve" => jobs.resolve(req["job"].as_u64().unwrap_or(0), req["how"].as_str().unwrap_or("cancel")),
        "cancel" => jobs.stop(req["job"].as_u64().unwrap_or(0)),
        "undo" => match jobs.undo() {
            Ok((what, touched)) => {
                tell_daemon(&touched);
                out.send(json!({ "t": "done", "op": "undo", "ok": true, "what": what, "made": touched }));
            }
            Err(e) => out.send(json!({ "t": "done", "op": "undo", "ok": false, "error": e.to_string() })),
        },
        "compress" => {
            let paths: Vec<PathBuf> = strs(&req["paths"]).into_iter().map(PathBuf::from).collect();
            let name = req["name"].as_str().unwrap_or("").trim().to_owned();
            let r = (|| {
                let parent = paths.first().and_then(|p| p.parent()).ok_or_else(|| anyhow::anyhow!("nothing to pack"))?;
                if name.is_empty() || name.contains('/') {
                    anyhow::bail!("that name cannot be used");
                }
                // No format said: a zip.
                let lower = name.to_lowercase();
                let name = if [".zip", ".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.zst", ".tar.bz2", ".7z", ".cpio", ".iso"].iter().any(|e| lower.ends_with(e)) { name.clone() } else { format!("{name}.zip") };
                jobs.compress(paths.clone(), files::free_name(parent, &name))
            })();
            if let Err(e) = r {
                out.send(json!({ "t": "done", "op": "compress", "ok": false, "error": e.to_string() }));
            }
        }
        "extract" => {
            let path = PathBuf::from(req["path"].as_str().unwrap_or(""));
            let dest = req["dest"].as_str().map(PathBuf::from).or_else(|| path.parent().map(Path::to_path_buf)).unwrap_or_default();
            let jobs = jobs.clone();
            let out = out.clone();
            std::thread::spawn(move || {
                if let Err(e) = jobs.extract(path, dest) {
                    out.send(json!({ "t": "done", "op": "extract", "ok": false, "error": e.to_string() }));
                }
            });
        }
        "trash" | "restore" | "empty_trash" | "rename" | "mkdir" | "clip" => {
            let paths = strs(&req["paths"]);
            let r: Result<Vec<String>> = match op {
                "trash" => files::trash_tracked(&paths).map(|names| {
                    jobs.trashed(names);
                    paths.clone()
                }),
                "restore" => files::restore(&strs(&req["names"])).map(|_| Vec::new()),
                "empty_trash" => files::empty_trash().map(|_| Vec::new()),
                "rename" => {
                    let from = req["path"].as_str().unwrap_or("");
                    files::rename(from, req["name"].as_str().unwrap_or("")).map(|p| {
                        jobs.renamed(Path::new(from), Path::new(&p));
                        vec![p]
                    })
                }
                "mkdir" => files::mkdir(Path::new(req["dir"].as_str().unwrap_or("/nonexistent")), req["name"].as_str().unwrap_or("Nueva carpeta")).map(|p| {
                    jobs.made(Path::new(&p));
                    vec![p]
                }),
                "clip" => files::clip(&paths, req["cut"].as_bool().unwrap_or(false)).map(|_| Vec::new()),
                _ => unreachable!(),
            };
            let mut touched = paths.clone();
            if let Ok(made) = &r {
                touched.extend(made.iter().cloned());
            }
            if let Some(p) = req["path"].as_str() {
                touched.push(p.to_owned());
            }
            if !touched.is_empty() && op != "clip" {
                tell_daemon(&touched);
            }
            match r {
                Ok(made) => out.send(json!({ "t": "done", "op": op, "ok": true, "made": made })),
                Err(e) => out.send(json!({ "t": "done", "op": op, "ok": false, "error": e.to_string() })),
            }
        }
        "apps" => {
            let path = req["path"].as_str().unwrap_or("").to_owned();
            let out = out.clone();
            std::thread::spawn(move || {
                let mime = crate::apps::mime_of(Path::new(&path));
                let items = crate::apps::for_mime(&mime);
                out.send(json!({ "t": "apps", "path": path, "mime": mime, "items": items }));
            });
        }
        "open_with" => {
            let r = crate::apps::open_with(req["app"].as_str().unwrap_or(""), &strs(&req["paths"]), req["always"].as_bool().unwrap_or(false));
            out.send(json!({ "t": "done", "op": "open_with", "ok": r.is_ok(), "error": r.err().map(|e| e.to_string()) }));
        }
        "props" => {
            let path = PathBuf::from(req["path"].as_str().unwrap_or(""));
            match files::properties(&path) {
                Ok(mut v) => {
                    v["t"] = json!("props");
                    let dir = v["dir"].as_bool().unwrap_or(false);
                    out.send(v);
                    if dir {
                        let out = out.clone();
                        std::thread::spawn(move || {
                            let (bytes, nfiles, ndirs) = files::folder_count(&path);
                            out.send(json!({ "t": "props_size", "path": path.to_string_lossy(), "size_text": files::size_text(bytes), "files": nfiles, "dirs": ndirs }));
                        });
                    }
                }
                Err(e) => out.send(json!({ "t": "done", "op": "props", "ok": false, "error": e.to_string() })),
            }
        }
        "chmod" => {
            let path = req["path"].as_str().unwrap_or("");
            let r = files::chmod(Path::new(path), req["mode"].as_u64().unwrap_or(0o644) as u32);
            out.send(json!({ "t": "done", "op": "chmod", "ok": r.is_ok(), "error": r.err().map(|e| e.to_string()) }));
        }
        "terminal" => {
            let r = files::terminal_at(Path::new(req["path"].as_str().unwrap_or("/")));
            out.send(json!({ "t": "done", "op": "terminal", "ok": r.is_ok(), "error": r.err().map(|e| e.to_string()) }));
        }
        "drives" => out.send(json!({ "t": "drives", "items": crate::drives::list() })),
        "mount" | "eject" | "unmount" => {
            let dev = req["dev"].as_str().unwrap_or("").to_owned();
            let op = op.to_owned();
            let out = out.clone();
            // udisks may wait for a password (polkit): not in the way of the rest.
            std::thread::spawn(move || {
                let r = match op.as_str() {
                    "mount" => crate::drives::mount(&dev),
                    "eject" => crate::drives::eject(&dev).map(|_| String::new()),
                    _ => crate::drives::unmount(&dev).map(|_| String::new()),
                };
                match r {
                    Ok(at) => out.send(json!({ "t": "done", "op": op, "ok": true, "dev": dev, "path": at })),
                    Err(e) => out.send(json!({ "t": "done", "op": op, "ok": false, "dev": dev, "error": e.to_string() })),
                }
                out.send(json!({ "t": "drives", "items": crate::drives::list() }));
            });
        }
        "exists" => {
            let p = req["path"].as_str().unwrap_or("");
            out.send(json!({ "t": "exists", "path": p, "exists": Path::new(p).exists(), "token": req["token"] }));
        }
        "pick" => {
            // The window was picking for a program: the answer goes where it waits, and the window closes.
            if let Some(file) = std::env::var_os("SONDA_PICK_OUT").filter(|f| !f.is_empty()) {
                if !req["cancel"].as_bool().unwrap_or(false) {
                    let _ = std::fs::write(&file, json!({ "paths": strs(&req["paths"]), "filter": req["filter"] }).to_string());
                }
            }
            unsafe {
                libc::kill(libc::getppid(), libc::SIGTERM);
            }
        }
        "status" | "pause" | "resume" | "folders" | "resync" => {
            let req = req.clone();
            let out = out.clone();
            std::thread::spawn(move || {
                crate::daemon::ensure();
                match crate::daemon::ask(&req) {
                    Ok(mut v) => {
                        v["t"] = json!("status");
                        out.send(v);
                    }
                    Err(e) => out.send(json!({ "t": "status", "ok": false, "error": e.to_string() })),
                }
            });
        }
        "entry" => {
            let path = req["path"].as_str().unwrap_or("");
            out.send(json!({ "t": "entry", "entry": files::entry(Path::new(path)) }));
        }
        _ => out.send(json!({ "t": "error", "error": format!("no such thing to do: {op}") })),
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let out = Arc::new(Out(Mutex::new(std::io::stdout())));
    let thumbs = Arc::new(Thumbs { queue: Mutex::new(VecDeque::new()), ready: Condvar::new(), generation: AtomicU64::new(0) });
    for _ in 0..2 {
        let t = thumbs.clone();
        let o = out.clone();
        std::thread::spawn(move || thumb_worker(t, o));
    }
    std::thread::spawn(|| {
        crate::daemon::ensure();
        let _ = crate::daemon::ask(&json!({ "op": "warm" }));
    });
    // The long jobs say how far they are; when one ends, the daemon hears what changed.
    let jobs = {
        let out = out.clone();
        Arc::new(jobs::Jobs::new(Arc::new(move |v: Value| {
            if v["t"] == "done" {
                let touched = strs(&v["touched"]);
                if !touched.is_empty() {
                    tell_daemon(&touched);
                }
            }
            out.send(v);
        })))
    };
    // Drives: a stick put in or pulled out shows up by itself.
    {
        let out = out.clone();
        std::thread::spawn(move || {
            let mut last = Vec::new();
            loop {
                let now = crate::drives::list();
                if now != last {
                    out.send(json!({ "t": "drives", "items": now }));
                    last = now;
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
        });
    }
    out.send(json!({ "t": "hello", "version": env!("CARGO_PKG_VERSION"), "spanish": files::spanish() }));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                if let Err(e) = handle(&req, &out, &thumbs, &jobs) {
                    out.send(json!({ "t": "error", "error": e.to_string() }));
                }
            }
            Err(e) => out.send(json!({ "t": "error", "error": format!("not JSON: {e}") })),
        }
    }
    Ok(())
}
