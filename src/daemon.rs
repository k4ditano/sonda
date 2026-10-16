//! `sonda daemon`: one per session. It keeps the index read (index.rs) and
//! answers on a socket, a JSON line for each question: searches, how the
//! reading goes, pause and resume, which folders.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::Config;
use crate::embed::{self, Embedder};
use crate::index::{self, Shared, Status};
use crate::search::{self, Query, Vectors};
use crate::store::Store;

struct Asker {
    model: Option<Embedder>,
    used: Instant,
}

pub fn running() -> bool {
    UnixStream::connect(crate::paths::socket()).is_ok()
}

pub fn run() -> Result<()> {
    let sock = crate::paths::socket();
    if running() {
        eprintln!("sonda · already running ({})", sock.display());
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);
    // Whatever goes wrong in a thread is written down, not lost with stderr.
    std::panic::set_hook(Box::new(|info| index::log(&format!("panic: {info}"))));
    // Gentle with the machine: it reads in the background.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 15);
        // Its disk reads only when nobody else wants the disk (IOPRIO_CLASS_IDLE).
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
    let store = Store::open(&crate::paths::index_db())?;
    let vectors = Vectors::load(&store.db)?;
    index::log(&format!("starting: {} vectors", vectors.len()));
    drop(store);
    let shared = Arc::new(Shared {
        status: Mutex::new(Status { phase: "starting".into(), gpu: Embedder::has_gpu(), ..Default::default() }),
        vectors: RwLock::new(vectors),
        config: Mutex::new(Config::load()),
        changed: Mutex::new(Default::default()),
        resync: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });
    {
        let s = shared.clone();
        std::thread::Builder::new().name("reader".into()).spawn(move || index::run(s))?;
    }
    let asker = Arc::new(Mutex::new(Asker { model: None, used: Instant::now() }));
    {
        // The words-only model goes after two quiet minutes: it holds about
        // 580 MB of the card, and a search after that waits for it to load again.
        let a = asker.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                let mut g = a.lock().unwrap();
                if g.model.is_some() && g.used.elapsed() > Duration::from_secs(120) {
                    g.model = None;
                }
            }
        });
    }
    let listener = UnixListener::bind(&sock)?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let s2 = shared.clone();
        let asker = asker.clone();
        std::thread::spawn(move || {
            let _ = serve(stream, &s2, &asker);
        });
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
    }
    Ok(())
}

fn serve(stream: UnixStream, shared: &Arc<Shared>, asker: &Arc<Mutex<Asker>>) -> Result<()> {
    let mut out = stream.try_clone()?;
    let reader = BufReader::new(stream);
    let db = Store::open(&crate::paths::index_db())?;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(req) => answer(&req, shared, asker, &db).unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() })),
            Err(e) => json!({ "ok": false, "error": format!("not JSON: {e}") }),
        };
        out.write_all(answer.to_string().as_bytes())?;
        out.write_all(b"\n")?;
        if shared.stop.load(Ordering::Relaxed) {
            std::process::exit(0);
        }
    }
    Ok(())
}

fn answer(req: &Value, shared: &Arc<Shared>, asker: &Arc<Mutex<Asker>>, db: &Store) -> Result<Value> {
    let op = req["op"].as_str().unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    match op {
        "search" => {
            let started = Instant::now();
            let raw = req["q"].as_str().unwrap_or("").trim().to_owned();
            let (code, text) = match raw.strip_prefix("code:").or_else(|| raw.strip_prefix("código:")) {
                Some(rest) => (true, rest.trim().to_owned()),
                None => (false, raw.clone()),
            };
            if text.is_empty() {
                return Ok(json!({ "ok": true, "id": id, "hits": [], "q": raw }));
            }
            let vector = {
                let mut a = asker.lock().unwrap();
                if a.model.is_none() {
                    let gpu = shared.config.lock().unwrap().gpu;
                    a.model = Some(Embedder::open(false, gpu)?);
                }
                a.used = Instant::now();
                let prompt = if code { embed::code_query(&text) } else { embed::query(&text) };
                a.model.as_mut().unwrap().text(&prompt).ok()
            };
            let kinds = req["kinds"].as_array().map(|k| k.iter().filter_map(|x| x.as_str().map(String::from)).collect());
            let under = req["under"].as_str().filter(|s| !s.is_empty()).map(String::from);
            let limit = req["limit"].as_u64().unwrap_or(60) as usize;
            let v = shared.vectors.read().unwrap();
            let hits = search::search(&db.db, &v, &Query { text: &text, vector: vector.as_deref(), code, kinds, limit, under })?;
            Ok(json!({ "ok": true, "id": id, "q": raw, "code": code, "hits": hits, "ms": started.elapsed().as_millis() as u64 }))
        }
        "warm" => {
            // A window opened: the words-only model, ready before the first key.
            let mut a = asker.lock().unwrap();
            if a.model.is_none() {
                let gpu = shared.config.lock().unwrap().gpu;
                a.model = Some(Embedder::open(false, gpu)?);
                let _ = a.model.as_mut().unwrap().text(&embed::query("warm up"));
            }
            a.used = Instant::now();
            Ok(json!({ "ok": true, "id": id }))
        }
        "status" => {
            let s = shared.status.lock().unwrap().clone();
            let c = shared.config.lock().unwrap().clone();
            Ok(json!({ "ok": true, "id": id, "status": s, "config": c }))
        }
        "pause" | "resume" => {
            let mut c = shared.config.lock().unwrap();
            c.paused = op == "pause";
            c.save();
            Ok(json!({ "ok": true, "id": id, "paused": c.paused }))
        }
        "folders" => {
            let mut c = shared.config.lock().unwrap();
            if let Some(f) = req["folders"].as_array() {
                c.folders = f.iter().filter_map(|x| x.as_str().map(String::from)).collect();
            }
            if let Some(f) = req["code_folders"].as_array() {
                c.code_folders = f.iter().filter_map(|x| x.as_str().map(String::from)).collect();
            }
            if let Some(f) = req["excluded"].as_array() {
                c.excluded = f.iter().filter_map(|x| x.as_str().map(String::from)).collect();
            }
            c.save();
            shared.resync.store(true, Ordering::Relaxed);
            Ok(json!({ "ok": true, "id": id, "config": *c }))
        }
        "resync" => {
            shared.resync.store(true, Ordering::Relaxed);
            Ok(json!({ "ok": true, "id": id }))
        }
        "changed" => {
            // The file manager moved, renamed or removed something: read it now.
            if let Some(ps) = req["paths"].as_array() {
                let mut c = shared.changed.lock().unwrap();
                for p in ps.iter().filter_map(|x| x.as_str()) {
                    c.insert(p.into());
                }
            }
            Ok(json!({ "ok": true, "id": id }))
        }
        "text" => {
            // The words of a piece (a page, a stretch), for the preview.
            let path = req["path"].as_str().unwrap_or("");
            let loc = req["loc"].as_f64();
            let body: Option<String> = db
                .db
                .query_row(
                    "SELECT fts.body FROM segments s JOIN files f ON f.id = s.file JOIN fts ON fts.rowid = s.id WHERE f.path = ?1 AND (?2 IS NULL OR s.loc = ?2) AND s.kind != 'name' ORDER BY s.loc LIMIT 1",
                    rusqlite::params![path, loc],
                    |r| r.get(0),
                )
                .ok();
            Ok(json!({ "ok": true, "id": id, "text": body }))
        }
        "quit" => {
            shared.stop.store(true, Ordering::Relaxed);
            Ok(json!({ "ok": true, "id": id }))
        }
        _ => Ok(json!({ "ok": false, "id": id, "error": format!("no such question: {op}") })),
    }
}

/// One question to the daemon, and its answer.
pub fn ask(req: &Value) -> Result<Value> {
    match ask_once(req) {
        Ok(v) => Ok(v),
        // The daemon went away halfway (restarted, updated): once more.
        Err(_) => {
            ensure();
            ask_once(req)
        }
    }
}

fn ask_once(req: &Value) -> Result<Value> {
    let mut s = UnixStream::connect(crate::paths::socket())?;
    s.set_read_timeout(Some(Duration::from_secs(120)))?;
    s.write_all(req.to_string().as_bytes())?;
    s.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

/// Starts the daemon if it is not running, loose from whoever asked.
pub fn ensure() {
    if running() {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "sonda".into());
    let _ = std::process::Command::new("setsid")
        .arg("-f")
        .arg(exe)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    for _ in 0..50 {
        if running() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
