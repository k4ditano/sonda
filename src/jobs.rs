//! The long things the file manager does —copying, moving, packing and
//! unpacking— in a thread of their own, saying how far they are, stoppable
//! halfway, asking first when a name is taken, and each one written down so
//! that Ctrl+Z can undo it.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::files;

pub type Send = Arc<dyn Fn(Value) + std::marker::Send + Sync>;

/// What a name already taken in the folder gets.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Policy {
    /// The one there goes to the trash (so undoing brings it back).
    Replace,
    Skip,
    /// The new one takes another name: «foto (2).png».
    Both,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "replace" => Some(Policy::Replace),
            "skip" => Some(Policy::Skip),
            "both" => Some(Policy::Both),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Copy,
    Move,
}

/// How a thing done is undone.
#[derive(Debug)]
enum Undo {
    /// Made here (copied, unpacked, packed, a new folder): it goes to the
    /// trash, and what it replaced comes back from it.
    Made { made: Vec<PathBuf>, replaced: Vec<String>, what: &'static str },
    /// Moved: back where each one was.
    Moved { pairs: Vec<(PathBuf, PathBuf)>, replaced: Vec<String> },
    Renamed { from: PathBuf, to: PathBuf },
    /// Put in the trash: these names in it come back.
    Trashed { names: Vec<String> },
}

struct Waiting {
    op: Op,
    sources: Vec<PathBuf>,
    dest: PathBuf,
}

pub struct Jobs {
    next: AtomicU64,
    stops: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    waiting: Mutex<HashMap<u64, Waiting>>,
    undo: Mutex<Vec<Undo>>,
    send: Send,
}

/// How far a job is, said a few times a second.
struct Progress<'a> {
    id: u64,
    op: &'static str,
    count: usize,
    dest: String,
    done: u64,
    total: u64,
    file: String,
    said: Instant,
    stop: &'a AtomicBool,
    send: &'a Send,
}

impl Progress<'_> {
    fn add(&mut self, n: u64) -> std::io::Result<()> {
        self.done += n;
        if self.said.elapsed() > Duration::from_millis(120) {
            self.say();
        }
        if self.stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped"));
        }
        Ok(())
    }

    fn at(&mut self, file: &Path) {
        self.file = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }

    fn say(&mut self) {
        self.said = Instant::now();
        (self.send)(json!({ "t": "job", "id": self.id, "op": self.op, "count": self.count, "dest": self.dest, "done": self.done, "total": self.total, "file": self.file, "done_text": files::size_text(self.done), "total_text": files::size_text(self.total) }));
    }
}

fn name_of(p: &Path) -> Result<String> {
    Ok(p.file_name().ok_or_else(|| anyhow!("{} has no name", p.display()))?.to_string_lossy().into_owned())
}

/// Bytes under a path, without following links.
fn bytes(p: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else { return 0 };
    if m.is_dir() {
        fs::read_dir(p).map(|rd| rd.flatten().map(|e| bytes(&e.path())).sum()).unwrap_or(0)
    } else if m.is_file() {
        m.len()
    } else {
        0
    }
}

fn keep_times(dst: &Path, m: &fs::Metadata) {
    if let (Ok(f), Ok(modified)) = (File::open(dst), m.modified()) {
        let mut t = fs::FileTimes::new().set_modified(modified);
        if let Ok(a) = m.accessed() {
            t = t.set_accessed(a);
        }
        let _ = f.set_times(t);
    }
}

/// One file: a reflink when the disk can share it (btrfs, xfs: instant),
/// otherwise a megabyte at a time.
fn copy_file(src: &Path, dst: &Path, m: &fs::Metadata, prog: &mut Progress) -> std::io::Result<()> {
    prog.at(src);
    let mut from = File::open(src)?;
    let mut to = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    const FICLONE: libc::c_ulong = 0x4004_9409;
    // (`SONDA_NO_REFLINK`: always the slow way, to see a copy's progress in a test.)
    let cloned = std::env::var_os("SONDA_NO_REFLINK").is_none() && unsafe { libc::ioctl(to.as_raw_fd(), FICLONE as _, from.as_raw_fd()) } == 0;
    let result = if cloned {
        prog.add(m.len())
    } else {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            };
            if let Err(e) = to.write_all(&buf[..n]).and_then(|_| prog.add(n as u64)) {
                break Err(e);
            }
        }
    };
    drop(to);
    if result.is_err() {
        // Half a file is worse than none.
        let _ = fs::remove_file(dst);
        return result;
    }
    let _ = fs::set_permissions(dst, m.permissions());
    keep_times(dst, m);
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path, prog: &mut Progress) -> std::io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    let ft = m.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
    } else if ft.is_dir() {
        fs::create_dir(dst)?;
        let mut entries: Vec<_> = fs::read_dir(src)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            copy_tree(&e.path(), &dst.join(e.file_name()), prog)?;
        }
        let _ = fs::set_permissions(dst, m.permissions());
        keep_times(dst, &m);
    } else if ft.is_file() {
        copy_file(src, dst, &m, prog)?;
    }
    // Sockets, pipes and devices are not copied.
    Ok(())
}

fn remove_tree(p: &Path) -> std::io::Result<()> {
    let m = fs::symlink_metadata(p)?;
    if m.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) }
}

/// «foto.tar.gz» → «foto».
pub fn archive_stem(name: &str) -> String {
    let lower = name.to_lowercase();
    for ext in [".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar.lz", ".tar.lzma", ".tgz", ".tbz2", ".txz", ".tar", ".zip", ".7z", ".rar", ".gz", ".xz", ".zst", ".bz2", ".iso", ".cpio", ".jar", ".apk", ".deb", ".rpm", ".cab"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return name[..name.len() - ext.len()].to_owned();
        }
    }
    Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_owned())
}

pub fn is_archive(p: &Path) -> bool {
    let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    [".zip", ".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.zst", ".tar.bz2", ".tbz2", ".7z", ".rar", ".iso", ".cpio", ".jar", ".apk", ".deb", ".rpm", ".cab", ".tar.lz", ".tar.lzma"].iter().any(|e| n.ends_with(e))
}

impl Jobs {
    pub fn new(send: Send) -> Jobs {
        Jobs { next: AtomicU64::new(1), stops: Mutex::new(HashMap::new()), waiting: Mutex::new(HashMap::new()), undo: Mutex::new(Vec::new()), send }
    }

    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn remember(&self, u: Undo) {
        let mut v = self.undo.lock().unwrap();
        v.push(u);
        if v.len() > 50 {
            v.remove(0);
        }
    }

    pub fn stop(&self, id: u64) {
        if let Some(s) = self.stops.lock().unwrap().get(&id) {
            s.store(true, Ordering::Relaxed);
        }
        // A question nobody will answer.
        self.waiting.lock().unwrap().remove(&id);
    }

    // ── the small things, written down to be undone ──

    pub fn renamed(&self, from: &Path, to: &Path) {
        self.remember(Undo::Renamed { from: from.into(), to: to.into() });
    }

    pub fn made(&self, p: &Path) {
        self.remember(Undo::Made { made: vec![p.into()], replaced: Vec::new(), what: "mkdir" });
    }

    pub fn trashed(&self, names: Vec<String>) {
        if !names.is_empty() {
            self.remember(Undo::Trashed { names });
        }
    }

    /// Ctrl+Z: the last thing done, undone. Says what it was.
    pub fn undo(&self) -> Result<(&'static str, Vec<String>)> {
        let Some(u) = self.undo.lock().unwrap().pop() else { bail!("nothing to undo") };
        let mut touched = Vec::new();
        let what = match u {
            Undo::Made { made, replaced, what } => {
                for p in &made {
                    touched.push(p.to_string_lossy().into_owned());
                    // An empty folder just made simply goes; anything else to the trash.
                    if fs::remove_dir(p).is_err() && p.exists() {
                        files::trash(&[p.to_string_lossy().into_owned()])?;
                    }
                }
                files::restore(&replaced)?;
                what
            }
            Undo::Moved { pairs, replaced } => {
                for (from, to) in pairs.iter().rev() {
                    if from.exists() {
                        bail!("{} is there again", from.display());
                    }
                    if fs::rename(to, from).is_err() {
                        let ok = Command::new("mv").arg("--").arg(to).arg(from).stderr(Stdio::null()).status()?.success();
                        if !ok {
                            bail!("could not move {} back", to.display());
                        }
                    }
                    touched.push(from.to_string_lossy().into_owned());
                    touched.push(to.to_string_lossy().into_owned());
                }
                files::restore(&replaced)?;
                "move"
            }
            Undo::Renamed { from, to } => {
                if from.exists() {
                    bail!("{} is there again", from.display());
                }
                fs::rename(&to, &from)?;
                touched.push(from.to_string_lossy().into_owned());
                "rename"
            }
            Undo::Trashed { names } => {
                let origins: Vec<String> = names.iter().filter_map(|n| files::trash_origin(n)).collect();
                files::restore(&names)?;
                touched.extend(origins);
                "trash"
            }
        };
        Ok((what, touched))
    }

    // ── copying and moving ──

    /// Copies or moves `sources` into `dest`. When names are taken there and
    /// no policy was given, it asks (a `conflict` line) and waits for
    /// `resolve`; otherwise it starts, and says `job` lines, then `done`.
    pub fn transfer(self: &Arc<Self>, op: Op, sources: Vec<PathBuf>, dest: PathBuf, policy: Option<Policy>) -> Result<()> {
        if !dest.is_dir() {
            bail!("{} is not a folder", dest.display());
        }
        let mut list = Vec::new();
        for s in sources {
            if !s.exists() && fs::symlink_metadata(&s).is_err() {
                continue;
            }
            if dest.starts_with(&s) {
                bail!("a folder cannot go inside itself");
            }
            // Moved to where it is: nothing to do.
            if op == Op::Move && s.parent() == Some(dest.as_path()) {
                continue;
            }
            list.push(s);
        }
        if list.is_empty() {
            (self.send)(json!({ "t": "done", "op": if op == Op::Copy { "copy" } else { "move" }, "ok": true, "made": [] }));
            return Ok(());
        }
        let id = self.id();
        // Copied onto itself («Paste» in the same folder) is a copy with another name.
        let taken: Vec<String> = list.iter().filter_map(|s| name_of(s).ok()).filter(|n| dest.join(n).exists()).collect();
        let onto_itself = list.iter().all(|s| s.parent() == Some(dest.as_path()));
        let policy = match policy {
            Some(p) => p,
            None if taken.is_empty() => Policy::Both,
            None if op == Op::Copy && onto_itself => Policy::Both,
            None => {
                let dest_name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dest.to_string_lossy().into_owned());
                (self.send)(json!({ "t": "conflict", "job": id, "names": taken.iter().take(4).collect::<Vec<_>>(), "count": taken.len(), "of": list.len(), "dest": dest_name, "folders": taken.iter().any(|n| dest.join(n).is_dir()) }));
                self.waiting.lock().unwrap().insert(id, Waiting { op, sources: list, dest });
                return Ok(());
            }
        };
        self.start(id, op, list, dest, policy);
        Ok(())
    }

    /// The answer to a `conflict`: replace, skip, both, or cancel.
    pub fn resolve(self: &Arc<Self>, id: u64, how: &str) {
        let Some(w) = self.waiting.lock().unwrap().remove(&id) else { return };
        match Policy::parse(how) {
            Some(p) => self.start(id, w.op, w.sources, w.dest, p),
            None => (self.send)(json!({ "t": "done", "op": if w.op == Op::Copy { "copy" } else { "move" }, "ok": false, "cancelled": true, "job": id })),
        }
    }

    fn start(self: &Arc<Self>, id: u64, op: Op, list: Vec<PathBuf>, dest: PathBuf, policy: Policy) {
        let stop = Arc::new(AtomicBool::new(false));
        self.stops.lock().unwrap().insert(id, stop.clone());
        let me = self.clone();
        std::thread::spawn(move || {
            let opname = if op == Op::Copy { "copy" } else { "move" };
            let r = me.run_transfer(id, op, &list, &dest, policy, &stop);
            me.stops.lock().unwrap().remove(&id);
            let mut touched: Vec<String> = list.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            touched.push(dest.to_string_lossy().into_owned());
            match r {
                Ok(made) => {
                    touched.extend(made.iter().cloned());
                    (me.send)(json!({ "t": "done", "op": opname, "ok": true, "made": made, "job": id, "touched": touched }));
                }
                Err(e) => {
                    let cancelled = stop.load(Ordering::Relaxed);
                    (me.send)(json!({ "t": "done", "op": opname, "ok": false, "cancelled": cancelled, "error": e.to_string(), "job": id, "touched": touched }));
                }
            }
        });
    }

    fn run_transfer(&self, id: u64, op: Op, list: &[PathBuf], dest: &Path, policy: Policy, stop: &AtomicBool) -> Result<Vec<String>> {
        let dest_name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        let mut prog = Progress { id, op: if op == Op::Copy { "copy" } else { "move" }, count: list.len(), dest: dest_name, done: 0, total: 0, file: String::new(), said: Instant::now() - Duration::from_secs(1), stop, send: &self.send };
        // Where each one goes, and what is in the way.
        let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut in_the_way: Vec<PathBuf> = Vec::new();
        for s in list {
            let name = name_of(s)?;
            let mut target = dest.join(&name);
            if target.exists() || fs::symlink_metadata(&target).is_ok() {
                if &target == s {
                    target = files::free_name(dest, &name);
                } else {
                    match policy {
                        Policy::Skip => continue,
                        Policy::Both => target = files::free_name(dest, &name),
                        Policy::Replace => in_the_way.push(target.clone()),
                    }
                }
            }
            plan.push((s.clone(), target));
        }
        // What is replaced goes to the trash first: undoing brings it back.
        let replaced = if in_the_way.is_empty() { Vec::new() } else { files::trash_tracked(&in_the_way.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>())? };
        let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
        let result: Result<()> = (|| {
            if op == Op::Move {
                // On the same disk a move is a rename, instant; across disks, a copy and then away.
                let mut across = Vec::new();
                for (s, t) in &plan {
                    match fs::rename(s, t) {
                        Ok(()) => done.push((s.clone(), t.clone())),
                        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => across.push((s.clone(), t.clone())),
                        Err(e) => bail!("{}: {e}", s.display()),
                    }
                }
                prog.total = across.iter().map(|(s, _)| bytes(s)).sum();
                prog.say();
                for (s, t) in across {
                    if let Err(e) = copy_tree(&s, &t, &mut prog) {
                        let _ = remove_tree(&t);
                        bail!("{}: {e}", s.display());
                    }
                    remove_tree(&s).map_err(|e| anyhow!("{}: {e}", s.display()))?;
                    done.push((s, t));
                }
            } else {
                prog.total = plan.iter().map(|(s, _)| bytes(s)).sum();
                prog.say();
                for (s, t) in &plan {
                    if let Err(e) = copy_tree(s, t, &mut prog) {
                        let _ = remove_tree(t);
                        bail!("{}: {e}", s.display());
                    }
                    done.push((s.clone(), t.clone()));
                }
            }
            Ok(())
        })();
        prog.say();
        // Even when stopped halfway, what was done can be undone.
        if !done.is_empty() || !replaced.is_empty() {
            if op == Op::Move {
                self.remember(Undo::Moved { pairs: done.clone(), replaced });
            } else {
                self.remember(Undo::Made { made: done.iter().map(|(_, t)| t.clone()).collect(), replaced, what: "copy" });
            }
        }
        result?;
        Ok(done.into_iter().map(|(_, t)| t.to_string_lossy().into_owned()).collect())
    }

    // ── packing and unpacking (bsdtar: zip, tar.*, 7z, rar, iso…) ──

    /// Packs `sources` (all in one folder) into `archive`; the format comes from its name.
    pub fn compress(self: &Arc<Self>, sources: Vec<PathBuf>, archive: PathBuf) -> Result<()> {
        let parent = sources.first().and_then(|p| p.parent()).ok_or_else(|| anyhow!("nothing to pack"))?.to_path_buf();
        if archive.exists() {
            bail!("{} is there already", archive.display());
        }
        let names: Vec<String> = sources.iter().map(|p| name_of(p)).collect::<Result<_>>()?;
        let total: u64 = sources.iter().map(|p| count_files(p)).sum();
        let id = self.id();
        let stop = Arc::new(AtomicBool::new(false));
        self.stops.lock().unwrap().insert(id, stop.clone());
        let me = self.clone();
        std::thread::spawn(move || {
            let mut cmd = Command::new("bsdtar");
            cmd.arg("-a").arg("-c").arg("-v").arg("-f").arg(&archive).arg("-C").arg(&parent).arg("--").args(&names);
            let r = me.run_tar(id, "compress", cmd, total, archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), &stop);
            me.stops.lock().unwrap().remove(&id);
            let a = archive.to_string_lossy().into_owned();
            match r {
                Ok(()) => {
                    me.remember(Undo::Made { made: vec![archive.clone()], replaced: Vec::new(), what: "compress" });
                    (me.send)(json!({ "t": "done", "op": "compress", "ok": true, "made": [a.clone()], "job": id, "touched": [a] }));
                }
                Err(e) => {
                    let _ = fs::remove_file(&archive);
                    (me.send)(json!({ "t": "done", "op": "compress", "ok": false, "cancelled": stop.load(Ordering::Relaxed), "error": e.to_string(), "job": id }));
                }
            }
        });
        Ok(())
    }

    /// Unpacks `archive` in `dest`: when everything in it hangs from one
    /// folder, that folder; otherwise into a new folder named like it.
    pub fn extract(self: &Arc<Self>, archive: PathBuf, dest: PathBuf) -> Result<()> {
        let listing = Command::new("bsdtar").arg("-t").arg("-f").arg(&archive).stderr(Stdio::piped()).output()?;
        if !listing.status.success() {
            bail!("{}", String::from_utf8_lossy(&listing.stderr).trim());
        }
        let entries: Vec<String> = String::from_utf8_lossy(&listing.stdout).lines().map(|l| l.trim_start_matches("./").to_owned()).filter(|l| !l.is_empty() && l != ".").collect();
        let mut tops: Vec<String> = entries.iter().map(|e| e.split('/').next().unwrap_or("").to_owned()).filter(|t| !t.is_empty()).collect();
        tops.sort();
        tops.dedup();
        let name = name_of(&archive)?;
        let (into, made) = if tops.len() == 1 && !dest.join(&tops[0]).exists() {
            (dest.clone(), dest.join(&tops[0]))
        } else {
            let folder = files::free_name(&dest, &archive_stem(&name));
            fs::create_dir(&folder)?;
            (folder.clone(), folder)
        };
        let id = self.id();
        let stop = Arc::new(AtomicBool::new(false));
        self.stops.lock().unwrap().insert(id, stop.clone());
        let total = entries.len() as u64;
        let me = self.clone();
        std::thread::spawn(move || {
            let mut cmd = Command::new("bsdtar");
            cmd.arg("-x").arg("-v").arg("-f").arg(&archive).arg("-C").arg(&into);
            let r = me.run_tar(id, "extract", cmd, total, made.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), &stop);
            me.stops.lock().unwrap().remove(&id);
            let m = made.to_string_lossy().into_owned();
            match r {
                Ok(()) => {
                    me.remember(Undo::Made { made: vec![made.clone()], replaced: Vec::new(), what: "extract" });
                    (me.send)(json!({ "t": "done", "op": "extract", "ok": true, "made": [m.clone()], "job": id, "touched": [m] }));
                }
                Err(e) => {
                    if made.exists() {
                        let _ = remove_tree(&made);
                    }
                    (me.send)(json!({ "t": "done", "op": "extract", "ok": false, "cancelled": stop.load(Ordering::Relaxed), "error": e.to_string(), "job": id }));
                }
            }
        });
        Ok(())
    }

    /// bsdtar with -v says a line for each file: that is how far it is.
    fn run_tar(&self, id: u64, op: &'static str, mut cmd: Command, total: u64, dest: String, stop: &AtomicBool) -> Result<()> {
        let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
        let pid = child.id() as i32;
        let err = child.stderr.take().unwrap();
        let mut prog = Progress { id, op, count: 1, dest, done: 0, total: total.max(1), file: String::new(), said: Instant::now() - Duration::from_secs(1), stop, send: &self.send };
        prog.say();
        let mut complaints = String::new();
        for line in BufReader::new(err).lines() {
            let Ok(line) = line else { break };
            if let Some(f) = line.strip_prefix("a ").or_else(|| line.strip_prefix("x ")) {
                prog.file = f.rsplit('/').find(|s| !s.is_empty()).unwrap_or(f).to_owned();
                if prog.add(1).is_err() {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                    let _ = child.wait();
                    bail!("stopped");
                }
            } else {
                complaints.push_str(&line);
                complaints.push('\n');
            }
        }
        let status = child.wait()?;
        prog.done = prog.total;
        prog.say();
        if !status.success() {
            bail!("{}", complaints.trim().lines().last().unwrap_or("bsdtar failed"));
        }
        Ok(())
    }
}

fn count_files(p: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else { return 0 };
    if m.is_dir() { 1 + fs::read_dir(p).map(|rd| rd.flatten().map(|e| count_files(&e.path())).sum()).unwrap_or(0) } else { 1 }
}
