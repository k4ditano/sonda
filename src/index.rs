//! Reading the folders: what is there (a walk, and a watcher for what
//! changes after), what is new or changed since last time, and each such
//! file read into pieces with their vectors. Code is read last.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::embed::Embedder;
use crate::extract::{self, Draft, Kind, Limits, Media};
use crate::search::Vectors;
use crate::store::{Piece, Store};

#[derive(Serialize, Clone, Debug, Default)]
pub struct Status {
    /// starting, walking, reading, idle, paused, error
    pub phase: String,
    pub current: String,
    pub done: i64,
    pub pending: i64,
    pub errors: i64,
    pub skipped: i64,
    pub vectors: usize,
    pub gpu: bool,
    pub message: String,
    /// Files read in the last minute.
    pub per_minute: f64,
}

pub struct Shared {
    pub status: Mutex<Status>,
    pub vectors: RwLock<Vectors>,
    pub config: Mutex<Config>,
    /// Paths the watcher saw change.
    pub changed: Mutex<HashSet<PathBuf>>,
    pub resync: AtomicBool,
    pub stop: AtomicBool,
}

const SKIP_FOLDERS: &[&str] = &["node_modules", "bower_components", "venv", ".venv", "site-packages", "__pycache__", "target", "build", "dist", "vendor", "Vendor", "third_party", "deps", "DerivedData", "Pods", "obj", "out", "coverage", ".git"];
const PROJECT_MARKERS: &[&str] = &[".git", "package.json", "pyproject.toml", "setup.py", "Cargo.toml", "go.mod", "pom.xml", "build.gradle", "CMakeLists.txt", "Gemfile", "composer.json", "flake.nix"];

fn mtime_of(m: &std::fs::Metadata) -> f64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn is_project(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Every file to read in the folders read by their contents. Inside a code
/// project (a folder with .git, Cargo.toml…) only pictures: its text is code.
pub fn walk_main(config: &Config, mut found: impl FnMut(&Path, Kind, &std::fs::Metadata)) {
    for root in &config.folders {
        let root_path = PathBuf::from(root);
        if !root_path.is_dir() {
            continue;
        }
        let mut stack = vec![(root_path.clone(), false)];
        while let Some((dir, in_project)) = stack.pop() {
            let project = in_project || (dir != root_path && is_project(&dir));
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                let p = path.to_string_lossy();
                if config.is_excluded(&p) {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                if ft.is_dir() {
                    if !SKIP_FOLDERS.contains(&name.as_str()) && !name.ends_with(".noindex") {
                        stack.push((path, project));
                    }
                } else if ft.is_file() {
                    let Some(kind) = extract::classify(&path) else { continue };
                    if project && !matches!(kind, Kind::Screenshot | Kind::Image | Kind::Video | Kind::Audio | Kind::Pdf) {
                        continue;
                    }
                    if let Ok(m) = e.metadata() {
                        if m.len() > 0 {
                            found(&path, kind, &m);
                        }
                    }
                }
            }
        }
    }
}

/// Every source file in the folders read as code, as git sees them (what it
/// ignores is left out), without vendored or built code.
pub fn walk_code(config: &Config, mut found: impl FnMut(&Path, &std::fs::Metadata)) {
    for root in &config.code_folders {
        let walker = ignore::WalkBuilder::new(root)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .parents(false)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !(e.file_type().is_some_and(|t| t.is_dir()) && SKIP_FOLDERS.contains(&name.as_ref()))
            })
            .build();
        for e in walker.flatten() {
            if !e.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = e.path();
            if config.is_excluded(&path.to_string_lossy()) || extract::classify_code(path).is_none() {
                continue;
            }
            if let Ok(m) = e.metadata() {
                if m.len() > 0 && m.len() < (1 << 20) {
                    found(path, &m);
                }
            }
        }
    }
}

/// Brings the index in line with the folders: new and changed files waiting,
/// gone ones forgotten.
pub fn sync(store: &Store, shared: &Shared) -> Result<()> {
    let config = shared.config.lock().unwrap().clone();
    let mut seen: HashSet<String> = HashSet::new();
    store.db.execute_batch("BEGIN")?;
    let mut n = 0;
    walk_main(&config, |path, kind, m| {
        let p = path.to_string_lossy().into_owned();
        let _ = store.upsert(&p, kind.name(), false, mtime_of(m), m.len() as i64);
        seen.insert(p);
        n += 1;
    });
    walk_code(&config, |path, m| {
        let p = path.to_string_lossy().into_owned();
        if seen.contains(&p) {
            return;
        }
        let _ = store.upsert(&p, "code", true, mtime_of(m), m.len() as i64);
        seen.insert(p);
        n += 1;
    });
    let mut gone = Vec::new();
    for (p, _) in store.all_paths()? {
        if !seen.contains(&p) {
            gone.push(p);
        }
    }
    for p in &gone {
        if let Some(id) = store.forget(p)? {
            shared.vectors.write().unwrap().forget_file(id);
        }
    }
    store.db.execute_batch("COMMIT")?;
    log(&format!("walked: {n} files, {} gone", gone.len()));
    Ok(())
}

pub fn log(line: &str) {
    use std::io::Write;
    let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let text = format!("{stamp} {line}\n");
    eprint!("sonda · {line}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::paths::log()) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// The code project a file is in (the nearest folder above it with .git), and
/// its path there: what a stretch of code is titled.
fn code_title(path: &Path) -> String {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d.join(".git").exists() {
            let base = d.parent().unwrap_or(d);
            return path.strip_prefix(base).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        }
        dir = d.parent();
    }
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn root_of(config: &Config, path: &str) -> PathBuf {
    config.folders.iter().chain(config.code_folders.iter()).find(|r| path.starts_with(&format!("{r}/"))).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Reads one file: its pieces, each with its vector, into the index.
fn read_file(store: &mut Store, shared: &Shared, embedder: &Arc<Mutex<Option<Embedder>>>, row: &crate::store::FileRow) -> Result<()> {
    let path = PathBuf::from(&row.path);
    let config = shared.config.lock().unwrap().clone();
    let limits = Limits { max_pages: config.max_pages, max_seconds: config.max_minutes * 60.0 };
    let kind = Kind::from_name(&row.kind).unwrap_or(Kind::Doc);
    let mut pieces: Vec<Piece> = Vec::new();
    let root = root_of(&config, &row.path);
    let mut sink = |d: Draft| -> Result<()> {
        if shared.stop.load(Ordering::Relaxed) {
            anyhow::bail!("stopping");
        }
        let Draft { mut piece, media } = d;
        let vec = match media {
            Media::None => None,
            Media::Text(t) => Some(with_model(embedder, |e| e.text(&t))?),
            Media::Picture(img, budget) => Some(with_model(embedder, |e| e.picture(&img, budget))?),
            Media::Sound(s) => Some(with_model(embedder, |e| e.sound(&s))?),
        };
        piece.vec = vec;
        pieces.push(piece);
        Ok(())
    };
    let info = if kind == Kind::Code {
        extract::code(&path, &code_title(&path), &mut sink)?
    } else {
        sink(extract::name_piece(&path, &root))?;
        extract::read(kind, &path, &limits, &mut sink)?
    };
    let ids = store.finish(row.id, &pieces, &info.to_string())?;
    let mut v = shared.vectors.write().unwrap();
    v.forget_file(row.id);
    for (p, id) in pieces.iter().zip(ids) {
        if let Some(vec) = &p.vec {
            v.add(id, row.id, &p.modality, kind == Kind::Code, vec);
        }
    }
    Ok(())
}

/// The model, opened the first time it is needed and kept while there is
/// work; searches use their own (words only), so they never wait for it.
fn with_model<T>(embedder: &Arc<Mutex<Option<Embedder>>>, f: impl FnOnce(&mut Embedder) -> Result<T>) -> Result<T> {
    let mut guard = embedder.lock().unwrap();
    if guard.is_none() {
        let gpu = crate::config::Config::load().gpu;
        let started = Instant::now();
        *guard = Some(Embedder::open(true, gpu)?);
        log(&format!("the model is open ({:.1} s, {})", started.elapsed().as_secs_f64(), if gpu && Embedder::has_gpu() { "on the graphics card" } else { "on the processor" }));
    }
    f(guard.as_mut().unwrap())
}

pub fn refresh_status(store: &Store, shared: &Shared) {
    if let Ok((done, pending, errors, skipped)) = store.counts() {
        let vectors = shared.vectors.read().unwrap().len();
        let mut s = shared.status.lock().unwrap();
        s.done = done;
        s.pending = pending;
        s.errors = errors;
        s.skipped = skipped;
        s.vectors = vectors;
    }
}

fn set_phase(shared: &Shared, phase: &str, current: &str) {
    let mut s = shared.status.lock().unwrap();
    s.phase = phase.to_owned();
    s.current = current.to_owned();
}

/// Paths the watcher reported: each one known again as it is now.
fn apply_changes(store: &Store, shared: &Shared) -> Result<()> {
    let changed: Vec<PathBuf> = shared.changed.lock().unwrap().drain().collect();
    if changed.is_empty() {
        return Ok(());
    }
    let config = shared.config.lock().unwrap().clone();
    for p in changed {
        let ps = p.to_string_lossy().into_owned();
        if ps.split('/').any(|part| part.starts_with('.') && part.len() > 1) || config.is_excluded(&ps) {
            continue;
        }
        if p.is_dir() {
            shared.resync.store(true, Ordering::Relaxed);
            continue;
        }
        match std::fs::metadata(&p) {
            Ok(m) if m.is_file() => {
                let in_code = config.code_folders.iter().any(|r| ps.starts_with(&format!("{r}/")));
                let in_main = config.folders.iter().any(|r| ps.starts_with(&format!("{r}/")));
                if in_main {
                    if let Some(k) = extract::classify(&p) {
                        store.upsert(&ps, k.name(), false, mtime_of(&m), m.len() as i64)?;
                        continue;
                    }
                }
                if in_code && extract::classify_code(&p).is_some() && m.len() < (1 << 20) {
                    store.upsert(&ps, "code", true, mtime_of(&m), m.len() as i64)?;
                }
            }
            _ => {
                if let Some(id) = store.forget(&ps)? {
                    shared.vectors.write().unwrap().forget_file(id);
                }
                // A folder gone takes what was inside it.
                let inside: Vec<String> = store.all_paths()?.into_iter().map(|(p, _)| p).filter(|x| x.starts_with(&format!("{ps}/"))).collect();
                for x in inside {
                    if let Some(id) = store.forget(&x)? {
                        shared.vectors.write().unwrap().forget_file(id);
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn watch(shared: Arc<Shared>) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let s2 = shared.clone();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            let mut c = s2.changed.lock().unwrap();
            for p in ev.paths {
                c.insert(p);
            }
        }
    })
    .ok()?;
    // Only the folders read by their contents: code folders hold build
    // trees of thousands of folders, and are walked again every half hour.
    let config = shared.config.lock().unwrap().clone();
    for root in config.folders.iter() {
        if let Err(e) = w.watch(Path::new(root), RecursiveMode::Recursive) {
            log(&format!("cannot watch {root}: {e}"));
        }
    }
    Some(w)
}

/// The reading loop: walk, read what waits, and then wait for changes.
pub fn run(shared: Arc<Shared>) {
    let mut store = match Store::open(&crate::paths::index_db()) {
        Ok(s) => s,
        Err(e) => {
            set_phase(&shared, "error", "");
            shared.status.lock().unwrap().message = format!("the index cannot be opened: {e}");
            return;
        }
    };
    // Whatever was left halfway when it stopped is read again.
    let _ = store.db.execute("UPDATE files SET status = 'pending' WHERE status = 'reading'", []);
    let embedder: Arc<Mutex<Option<Embedder>>> = Arc::new(Mutex::new(None));
    set_phase(&shared, "walking", "");
    if let Err(e) = sync(&store, &shared) {
        log(&format!("walk failed: {e}"));
    }
    let mut _watcher = watch(shared.clone());
    let mut last_walk = Instant::now();
    let mut idle_since: Option<Instant> = None;
    let mut times: Vec<Instant> = Vec::new();
    let mut media = Watching::default();
    refresh_status(&store, &shared);
    while !shared.stop.load(Ordering::Relaxed) {
        if shared.resync.swap(false, Ordering::Relaxed) || last_walk.elapsed() > Duration::from_secs(1800) {
            set_phase(&shared, "walking", "");
            if let Err(e) = sync(&store, &shared) {
                log(&format!("walk failed: {e}"));
            }
            _watcher = watch(shared.clone());
            last_walk = Instant::now();
        }
        if let Err(e) = apply_changes(&store, &shared) {
            log(&format!("changes: {e}"));
        }
        if shared.config.lock().unwrap().paused {
            set_phase(&shared, "paused", "");
            refresh_status(&store, &shared);
            std::thread::sleep(Duration::from_millis(700));
            continue;
        }
        let batch = store.next_pending(20).unwrap_or_default();
        // A video playing (a browser, mpv…): reading waits. Opening the model
        // puts about 1 GB on the card in a second or two, and a video read is
        // its frames decoded and looked at for many seconds: the video being
        // watched stuttered then.
        if !batch.is_empty() && media.playing() {
            set_phase(&shared, "waiting", "");
            refresh_status(&store, &shared);
            let mut g = embedder.lock().unwrap();
            if g.is_some() {
                *g = None;
                log("a video is playing: the model is closed until it stops");
            }
            drop(g);
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        if batch.is_empty() {
            set_phase(&shared, "idle", "");
            refresh_status(&store, &shared);
            // The model goes after a while with nothing to read (about 1 GB of the card).
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(90) {
                let mut g = embedder.lock().unwrap();
                if g.is_some() {
                    *g = None;
                    log("nothing to read: the model is closed");
                }
            }
            std::thread::sleep(Duration::from_millis(1500));
            continue;
        }
        idle_since = None;
        for row in batch {
            if shared.stop.load(Ordering::Relaxed) || shared.config.lock().unwrap().paused || shared.resync.load(Ordering::Relaxed) || media.playing() {
                break;
            }
            if !Path::new(&row.path).exists() {
                if let Ok(Some(id)) = store.forget(&row.path) {
                    shared.vectors.write().unwrap().forget_file(id);
                }
                continue;
            }
            set_phase(&shared, "reading", &row.path);
            let started = Instant::now();
            let _ = store.db.execute("UPDATE files SET status = 'reading' WHERE id = ?", [row.id]);
            match read_file(&mut store, &shared, &embedder, &row) {
                Ok(()) => {
                    let secs = started.elapsed().as_secs_f64();
                    if secs > 5.0 {
                        log(&format!("read {} in {secs:.1} s", row.path));
                    }
                }
                Err(e) => {
                    let why = e.to_string();
                    if why == "stopping" {
                        let _ = store.db.execute("UPDATE files SET status = 'pending' WHERE id = ?", [row.id]);
                        break;
                    }
                    let soft = why.contains("too small") || why.contains("no text") || why.contains("no sound") || why.contains("too big") || why.contains("private key") || why.contains("too thin");
                    if soft {
                        let _ = store.skipped(row.id, &why);
                    } else {
                        log(&format!("{}: {why}", row.path));
                        let _ = store.failed(row.id, &why);
                    }
                }
            }
            times.push(Instant::now());
            times.retain(|t| t.elapsed() < Duration::from_secs(60));
            shared.status.lock().unwrap().per_minute = times.len() as f64;
            refresh_status(&store, &shared);
        }
    }
    set_phase(&shared, "stopped", "");
}

/// Whether a player that shows video is playing, asked over MPRIS on the
/// session bus at most every three seconds. Music players don't count: music
/// played all day would keep everything from being read.
#[derive(Default)]
struct Watching {
    bus: Option<zbus::blocking::Connection>,
    asked: Option<Instant>,
    playing: bool,
}

impl Watching {
    fn playing(&mut self) -> bool {
        if self.asked.is_some_and(|a| a.elapsed() < Duration::from_secs(3)) {
            return self.playing;
        }
        self.asked = Some(Instant::now());
        if self.bus.is_none() {
            self.bus = zbus::blocking::Connection::session().ok();
        }
        self.playing = match &self.bus {
            Some(bus) => Self::ask(bus).unwrap_or(false),
            None => false,
        };
        self.playing
    }

    fn ask(bus: &zbus::blocking::Connection) -> zbus::Result<bool> {
        const VIDEO: [&str; 11] = ["firefox", "zen", "chromium", "chrome", "brave", "vivaldi", "edge", "mpv", "vlc", "celluloid", "totem"];
        let names = zbus::blocking::fdo::DBusProxy::new(bus)?.list_names()?;
        for name in names {
            let Some(player) = name.as_str().strip_prefix("org.mpris.MediaPlayer2.") else { continue };
            let player = player.to_lowercase();
            if !VIDEO.iter().any(|v| player.starts_with(v)) {
                continue;
            }
            let proxy = zbus::blocking::Proxy::new(bus, name.as_str(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player")?;
            if proxy.get_property::<String>("PlaybackStatus").is_ok_and(|s| s == "Playing") {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
