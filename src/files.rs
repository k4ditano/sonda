//! The file manager's hands: what a folder holds, small pictures of it, and
//! what can be done with a file —open it (at the page, the moment, the line),
//! copy, move, rename, put it in the trash—. And setting Sonda up, and
//! opening its window.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::extract::{self, Kind};

#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    /// folder, image, screenshot, pdf, doc, audio, video, code, archive, other
    pub kind: String,
    pub size: u64,
    pub mtime: f64,
    pub size_text: String,
    pub date_text: String,
    /// An icon name, for what has no picture of its own.
    pub icon: String,
    pub hidden: bool,
}

pub fn spanish() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty())).is_some_and(|l| l.starts_with("es"))
}

pub fn size_text(n: u64) -> String {
    let es = spanish();
    let f = |x: f64, unit: &str| {
        let s = if x < 10.0 { format!("{x:.1}") } else { format!("{x:.0}") };
        format!("{} {unit}", if es { s.replace('.', ",") } else { s })
    };
    match n {
        0..1000 => format!("{n} B"),
        1000..1_000_000 => f(n as f64 / 1e3, "kB"),
        1_000_000..1_000_000_000 => f(n as f64 / 1e6, "MB"),
        _ => f(n as f64 / 1e9, "GB"),
    }
}

/// «today 14:32», «yesterday», «12 Oct», «12 Oct 2024».
pub fn date_text(t: f64) -> String {
    let es = spanish();
    let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let tz = local_offset();
    let day = |x: f64| ((x + tz) / 86400.0).floor() as i64;
    let (y, mo, d, h, mi) = civil(t + tz);
    let months_es = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
    let months_en = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = if es { months_es[mo - 1] } else { months_en[mo - 1] };
    let (ny, ..) = civil(now + tz);
    if day(t) == day(now) {
        format!("{} {h:02}:{mi:02}", if es { "hoy" } else { "today" })
    } else if day(t) == day(now) - 1 {
        format!("{} {h:02}:{mi:02}", if es { "ayer" } else { "yesterday" })
    } else if y == ny {
        format!("{d} {month}")
    } else {
        format!("{d} {month} {y}")
    }
}

fn local_offset() -> f64 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm.tm_gmtoff as f64
    }
}

fn civil(t: f64) -> (i64, usize, u32, u32, u32) {
    let secs = t.floor() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32)
}

pub fn kind_of(path: &Path, dir: bool) -> (&'static str, &'static str) {
    if dir {
        return ("folder", "folder");
    }
    if let Some(k) = extract::classify(path) {
        return match k {
            Kind::Screenshot => ("screenshot", "image-x-generic"),
            Kind::Image => ("image", "image-x-generic"),
            Kind::Pdf => ("pdf", "application-pdf"),
            Kind::Doc => ("doc", "text-x-generic"),
            Kind::Audio => ("audio", "audio-x-generic"),
            Kind::Video => ("video", "video-x-generic"),
            Kind::Code => ("code", "text-x-script"),
        };
    }
    let e = extract::ext(path);
    if extract::classify_code(path).is_some() || ["json", "jsonl", "csv", "tsv", "log", "xml", "ini", "conf", "cfg"].contains(&e.as_str()) {
        return ("code", "text-x-script");
    }
    if crate::jobs::is_archive(path) || ["gz", "xz", "zst", "bz2"].contains(&e.as_str()) {
        return ("archive", "package-x-generic");
    }
    if ["xlsx", "xls", "ods"].contains(&e.as_str()) {
        return ("other", "x-office-spreadsheet");
    }
    if ["svg"].contains(&e.as_str()) {
        return ("image", "image-x-generic");
    }
    ("other", "text-x-generic")
}

pub fn entry(path: &Path) -> Option<Entry> {
    let m = std::fs::metadata(path).ok()?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let dir = m.is_dir();
    let (kind, icon) = kind_of(path, dir);
    let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let size = if dir { std::fs::read_dir(path).map(|r| r.count() as u64).unwrap_or(0) } else { m.len() };
    let size_text = if dir {
        let es = spanish();
        match size {
            1 => (if es { "1 elemento" } else { "1 item" }).to_owned(),
            n => format!("{n} {}", if es { "elementos" } else { "items" }),
        }
    } else {
        size_text(size)
    };
    Some(Entry { hidden: name.starts_with('.'), name, path: path.to_string_lossy().into_owned(), dir, kind: kind.into(), size, mtime, size_text, date_text: date_text(mtime), icon: icon.into() })
}

/// What a folder holds: folders first, then by `sort` (name, date, size, kind).
pub fn list(dir: &Path, hidden: bool, sort: &str) -> Result<Vec<Entry>> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir)?.flatten().filter_map(|e| entry(&e.path())).filter(|e| hidden || !e.hidden).collect();
    out.sort_by(|a, b| {
        b.dir.cmp(&a.dir).then_with(|| match sort {
            "date" => b.mtime.partial_cmp(&a.mtime).unwrap_or(std::cmp::Ordering::Equal),
            "size" => b.size.cmp(&a.size),
            "kind" => a.kind.cmp(&b.kind).then_with(|| natural(&a.name, &b.name)),
            _ => natural(&a.name, &b.name),
        })
    });
    Ok(out)
}

/// «foto 2» before «foto 10», and case not counting.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(c) = y.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n2.push(c);
                    y.next();
                }
                let o = n1.trim_start_matches('0').len().cmp(&n2.trim_start_matches('0').len()).then(n1.trim_start_matches('0').cmp(n2.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
}

// ── small pictures ──

fn key(path: &str, mtime: f64, what: &str) -> String {
    sha1_smol::Sha1::from(format!("{path}\u{0}{mtime}\u{0}{what}")).digest().to_string()
}

/// A small picture of a file (256 px), made once and kept: a picture, a
/// video's frame, a PDF's first page.
pub fn thumb(path: &Path, kind: &str) -> Option<PathBuf> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    let out = crate::paths::cache_sub("thumbs").join(format!("{}.png", key(&path.to_string_lossy(), mtime, "t256")));
    if out.exists() {
        return Some(out);
    }
    let made = match kind {
        "image" | "screenshot" => {
            if extract::ext(path) == "svg" {
                return None;
            }
            let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
            img.thumbnail(256, 256).save(&out).ok()
        }
        "video" => video_frame(path, 1.0, 256, &out),
        "pdf" => {
            let prefix = out.with_extension("");
            extract::render_page(path, 1, 256, &prefix).ok().map(|_| ())
        }
        _ => None,
    };
    made.and_then(|_| out.exists().then_some(out))
}

/// A bigger picture for the preview: the page, or the moment, that was found.
pub fn preview(path: &Path, kind: &str, loc: Option<f64>) -> Option<PathBuf> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    let what = format!("p1000-{}", loc.unwrap_or(-1.0));
    let out = crate::paths::cache_sub("previews").join(format!("{}.png", key(&path.to_string_lossy(), mtime, &what)));
    if out.exists() {
        return Some(out);
    }
    let made = match kind {
        "image" | "screenshot" => {
            let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
            img.thumbnail(1000, 1000).save(&out).ok()
        }
        "video" => video_frame(path, loc.unwrap_or(1.0), 1000, &out),
        "pdf" => {
            let prefix = out.with_extension("");
            extract::render_page(path, loc.unwrap_or(1.0).max(1.0) as usize, 1000, &prefix).ok().map(|_| ())
        }
        _ => None,
    };
    made.and_then(|_| out.exists().then_some(out))
}

fn video_frame(path: &Path, at: f64, side: u32, out: &Path) -> Option<()> {
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-ss", &format!("{at}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale='min({side},iw)':-2")])
        .arg(out)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?
        .success();
    if !ok || !out.exists() {
        // A short clip: its first frame.
        Command::new("ffmpeg").args(["-v", "error", "-nostdin", "-y", "-i"]).arg(path).args(["-frames:v", "1", "-vf", &format!("scale='min({side},iw)':-2")]).arg(out).stderr(Stdio::null()).status().ok()?;
    }
    out.exists().then_some(())
}

// ── doing things ──

fn detach(cmd: &str, args: &[String]) -> Result<()> {
    Command::new("setsid")
        .arg("-f")
        .arg(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("{cmd}: {e}"))?;
    Ok(())
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

pub fn open(path: &str) -> Result<()> {
    detach("xdg-open", &[path.to_owned()])
}

/// Opened where it was found: a PDF at its page, sound and video at their
/// moment, code at its line. What cannot be told a place opens from the start.
pub fn open_at(path: &str, kind: &str, loc: Option<f64>) -> Result<()> {
    let Some(loc) = loc else { return open(path) };
    match kind {
        "pdf" => {
            let url = format!("file://{}#page={}", path.replace(' ', "%20"), loc.max(1.0) as u64);
            for b in ["zathura", "okular", "evince", "papers"] {
                if which(b) {
                    let flag = if b == "zathura" { format!("--page={}", loc as u64) } else { format!("-p{}", loc as u64) };
                    return detach(b, &[flag, path.to_owned()]);
                }
            }
            // The PDF viewer chosen for the desktop, when it is a browser (they all
            // take #page=N); then the browsers there are.
            let chosen = Command::new("xdg-mime").args(["query", "default", "application/pdf"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
            for (id, b) in [("chrome.canary", "google-chrome-canary"), ("chromium", "chromium"), ("google-chrome", "google-chrome-stable"), ("zen", "zen-browser"), ("firefox", "firefox")] {
                if chosen.contains(id) && which(b) {
                    return detach(b, &[url]);
                }
            }
            for b in ["zen-browser", "zen", "firefox", "chromium", "google-chrome-stable", "google-chrome-canary"] {
                if which(b) {
                    return detach(b, &[url]);
                }
            }
            open(path)
        }
        "audio" | "video" => {
            if which("mpv") {
                return detach("mpv", &[format!("--start={loc}"), path.to_owned()]);
            }
            if which("vlc") {
                return detach("vlc", &[format!("--start-time={}", loc as u64), path.to_owned()]);
            }
            open(path)
        }
        "code" | "doc" => {
            let line = loc.max(1.0) as u64;
            for (b, args) in [("zed", vec![format!("{path}:{line}")]), ("code", vec!["-g".into(), format!("{path}:{line}")]), ("kate", vec!["--line".into(), line.to_string(), path.into()])] {
                if which(b) {
                    return detach(b, &args);
                }
            }
            if which("kitty") {
                let editor = std::env::var("EDITOR").ok().filter(|e| !e.is_empty()).unwrap_or_else(|| "vim".into());
                return detach("kitty", &[editor, format!("+{line}"), path.to_owned()]);
            }
            open(path)
        }
        _ => open(path),
    }
}

/// Into the trash (the desktop's, which file managers share).
pub fn trash(paths: &[String]) -> Result<()> {
    for p in paths {
        let ok = Command::new("gio").arg("trash").arg("--").arg(p).stderr(Stdio::null()).status()?.success();
        if !ok {
            bail!("could not put {p} in the trash");
        }
    }
    Ok(())
}

/// Into the trash, saying under which names each one went (so that it can
/// come back: Ctrl+Z).
pub fn trash_tracked(paths: &[String]) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for p in paths {
        trash(std::slice::from_ref(p))?;
        // The newest note in the trash that says it came from here.
        let info = trash_root().join("info");
        let mut best: Option<(String, String)> = None;
        for e in std::fs::read_dir(&info).into_iter().flatten().flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            let Some(name) = fname.strip_suffix(".trashinfo") else { continue };
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            let origin = text.lines().find_map(|l| l.strip_prefix("Path=")).map(percent_decode);
            if origin.as_deref() != Some(p.as_str()) {
                continue;
            }
            let when = text.lines().find_map(|l| l.strip_prefix("DeletionDate=")).unwrap_or("").to_owned();
            if best.as_ref().is_none_or(|(w, _)| when > *w) {
                best = Some((when, name.to_owned()));
            }
        }
        if let Some((_, n)) = best {
            names.push(n);
        }
    }
    Ok(names)
}

/// A path as an address: `file:///home/a/My%20file.txt`.
pub fn uri(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn from_uri(u: &str) -> Option<String> {
    let rest = u.strip_prefix("file://")?;
    // «file://host/path»: only this machine's.
    let path = if rest.starts_with('/') { rest } else { &rest[rest.find('/')?..] };
    Some(percent_decode(path))
}

/// Free and total bytes on the disk a path is on.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some((s.f_bavail as u64 * s.f_frsize as u64, s.f_blocks as u64 * s.f_frsize as u64))
}

fn user_name(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() { uid.to_string() } else { std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned() }
    }
}

fn group_name(gid: u32) -> String {
    unsafe {
        let gr = libc::getgrgid(gid);
        if gr.is_null() { gid.to_string() } else { std::ffi::CStr::from_ptr((*gr).gr_name).to_string_lossy().into_owned() }
    }
}

/// «16 oct 2026, 14:32»
pub fn full_date(t: f64) -> String {
    let es = spanish();
    let (y, mo, d, h, mi) = civil(t + local_offset());
    let months_es = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"];
    let months_en = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {} {y}, {h:02}:{mi:02}", if es { months_es[mo - 1] } else { months_en[mo - 1] })
}

/// Everything about one thing: what it is, how big, when, whose, who may
/// read and write it, and how much room is left where it is.
pub fn properties(path: &Path) -> Result<serde_json::Value> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path)?;
    let link = m.file_type().is_symlink();
    let m = if link { std::fs::metadata(path).unwrap_or(m) } else { m };
    let dir = m.is_dir();
    let secs = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| full_date(d.as_secs_f64())).unwrap_or_default();
    let mode = m.mode();
    let (free, total) = disk_space(if dir { path } else { path.parent().unwrap_or(path) }).unwrap_or((0, 0));
    let (kind, icon) = kind_of(path, dir);
    Ok(serde_json::json!({
        "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into()),
        "path": path.to_string_lossy(),
        "folder": path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "kind": kind,
        "icon": icon,
        "mime": crate::apps::mime_of(path),
        "dir": dir,
        "link": if link { std::fs::read_link(path).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() },
        "size": if dir { 0 } else { m.len() },
        "size_text": if dir { String::new() } else { format!("{} ({} bytes)", size_text(m.len()), m.len()) },
        "modified": secs(m.modified()),
        "accessed": secs(m.accessed()),
        "created": secs(m.created()),
        "owner": user_name(m.uid()),
        "group": group_name(m.gid()),
        "mine": m.uid() == unsafe { libc::getuid() },
        "mode": mode & 0o7777,
        "free_text": size_text(free),
        "total_text": size_text(total),
        "used": if total > 0 { 1.0 - free as f64 / total as f64 } else { 0.0 },
    }))
}

/// What a folder holds, counted: bytes, files and folders.
pub fn folder_count(path: &Path) -> (u64, u64, u64) {
    let (mut bytes, mut files, mut dirs) = (0, 0, 0);
    let mut stack = vec![path.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                dirs += 1;
                stack.push(e.path());
            } else {
                files += 1;
                if let Ok(m) = e.metadata() {
                    bytes += m.len();
                }
            }
            if files + dirs > 2_000_000 {
                return (bytes, files, dirs);
            }
        }
    }
    (bytes, files, dirs)
}

pub fn chmod(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))?;
    Ok(())
}

/// A terminal in that folder.
pub fn terminal_at(dir: &Path) -> Result<()> {
    let d = dir.to_string_lossy().into_owned();
    for (t, args) in [("kitty", vec!["--directory".to_owned(), d.clone()]), ("foot", vec!["-D".into(), d.clone()]), ("alacritty", vec!["--working-directory".into(), d.clone()]), ("wezterm", vec!["start".into(), "--cwd".into(), d.clone()]), ("konsole", vec!["--workdir".into(), d.clone()]), ("gnome-terminal", vec![format!("--working-directory={d}")])] {
        if which(t) {
            return detach(t, &args);
        }
    }
    bail!("no terminal found")
}

/// The desktop's trash, where gio and the file managers put things.
fn trash_root() -> PathBuf {
    match std::env::var("XDG_DATA_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d).join("Trash"),
        _ => crate::paths::home().join(".local/share/Trash"),
    }
}

pub fn trash_dir() -> PathBuf {
    trash_root().join("files")
}

/// What is in the trash came from.
pub fn trash_origin(name: &str) -> Option<String> {
    let info = trash_root().join("info").join(format!("{name}.trashinfo"));
    let text = std::fs::read_to_string(info).ok()?;
    let line = text.lines().find_map(|l| l.strip_prefix("Path="))?;
    Some(percent_decode(line))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Back from the trash to where it was.
pub fn restore(names: &[String]) -> Result<()> {
    for n in names {
        let Some(origin) = trash_origin(n) else { bail!("{n}: where it came from is not known") };
        let target = PathBuf::from(&origin);
        if target.exists() {
            bail!("{origin} is there already");
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(trash_dir().join(n), &target).or_else(|_| {
            let ok = Command::new("mv").arg("--").arg(trash_dir().join(n)).arg(&target).status()?.success();
            if ok { Ok(()) } else { Err(std::io::Error::other("mv failed")) }
        })?;
        let _ = std::fs::remove_file(trash_root().join("info").join(format!("{n}.trashinfo")));
    }
    Ok(())
}

pub fn empty_trash() -> Result<()> {
    Command::new("gio").args(["trash", "--empty"]).status()?;
    Ok(())
}

/// A name not taken in `dir`: «foto.png», then «foto (2).png»…
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_owned());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 2.. {
        let c = dir.join(format!("{stem} ({n}){ext}"));
        if !c.exists() {
            return c;
        }
    }
    unreachable!()
}

pub fn rename(path: &str, name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        bail!("that name cannot be used");
    }
    let src = Path::new(path);
    let target = src.parent().ok_or_else(|| anyhow!("no folder"))?.join(name);
    if target.exists() {
        bail!("there is already something called {name}");
    }
    std::fs::rename(src, &target)?;
    Ok(target.to_string_lossy().into_owned())
}

pub fn mkdir(dir: &Path, name: &str) -> Result<String> {
    let target = free_name(dir, name.trim());
    std::fs::create_dir(&target)?;
    Ok(target.to_string_lossy().into_owned())
}

/// The files as the clipboard carries them: other programs paste them as files.
pub fn clip(paths: &[String], cut: bool) -> Result<()> {
    let uris: Vec<String> = paths.iter().map(|p| format!("file://{}", p.replace('%', "%25").replace(' ', "%20"))).collect();
    let text = if cut { format!("cut\n{}", uris.join("\n")) } else { uris.join("\r\n") };
    let ty = if cut { "x-special/gnome-copied-files" } else { "text/uri-list" };
    let mut child = Command::new("wl-copy").args(["--type", ty]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    use std::io::Write;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    Ok(())
}

pub fn folder_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 200_000 {
                return total;
            }
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    stack.push(e.path());
                } else if let Ok(m) = e.metadata() {
                    total += m.len();
                }
            }
        }
    }
    total
}

// ── setting up, and the window ──

pub fn ui_dir() -> PathBuf {
    let d = crate::paths::data().join("ui");
    let _ = std::fs::create_dir_all(&d);
    d
}

const UI: &[(&str, &str)] = &[
    ("sonda.plm", include_str!("../ui/sonda.plm")),
    ("sonda.luau", include_str!("../ui/sonda.luau")),
    ("shaders/deep.wgsl", include_str!("../ui/shaders/deep.wgsl")),
    ("shaders/ping.wgsl", include_str!("../ui/shaders/ping.wgsl")),
    ("shaders/fill.wgsl", include_str!("../ui/shaders/fill.wgsl")),
    ("art/anchor.svg", include_str!("../ui/art/anchor.svg")),
];

fn write_ui() -> Result<PathBuf> {
    // Working on the window: its files as they are in that folder.
    if let Some(d) = std::env::var_os("SONDA_UI_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(d).join("sonda.plm"));
    }
    let dir = ui_dir();
    for (name, text) in UI {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*text) {
            std::fs::write(&path, text)?;
        }
    }
    Ok(dir.join("sonda.plm"))
}

pub fn setup() -> Result<()> {
    let models = crate::paths::models();
    std::fs::create_dir_all(&models)?;
    for m in [crate::paths::TEXT_MODEL, crate::paths::MEDIA_MODEL] {
        let target = models.join(m);
        if target.exists() {
            continue;
        }
        let url = format!("https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/{m}");
        println!("downloading {m}…");
        let part = target.with_extension("part");
        let ok = Command::new("curl").args(["-fL", "--progress-bar", "-o"]).arg(&part).arg(&url).status()?.success();
        if !ok {
            bail!("could not download {url}");
        }
        std::fs::rename(&part, &target)?;
    }
    write_ui()?;
    let exe = std::env::current_exe()?;
    let share = std::env::var("XDG_DATA_HOME").ok().filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| crate::paths::home().join(".local/share"));
    let apps = share.join("applications");
    std::fs::create_dir_all(&apps)?;
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName=Sonda\nGenericName=Files\nGenericName[es]=Archivos\nComment=Find your files by what is in them\nComment[es]=Encuentra tus archivos por lo que contienen\nExec={exe} %U\nIcon=system-file-manager\nTerminal=false\nCategories=Utility;FileManager;System;\nMimeType=inode/directory;\nKeywords=files;search;folder;archivos;buscar;carpeta;\n",
        exe = exe.display()
    );
    std::fs::write(apps.join("sonda.desktop"), desktop)?;
    // On the session bus when someone asks: «Show in folder», and the Open/Save dialog.
    let services = share.join("dbus-1/services");
    std::fs::create_dir_all(&services)?;
    for name in ["org.freedesktop.FileManager1", "org.freedesktop.impl.portal.desktop.sonda"] {
        std::fs::write(services.join(format!("{name}.service")), format!("[D-BUS Service]\nName={name}\nExec={} dbus\n", exe.display()))?;
    }
    println!("Sonda is set up: models in {}, the window in {}, on the bus in {}", models.display(), ui_dir().display(), services.display());
    Ok(())
}

/// The window: pleamar-wm runs it (so it can hold a player inside), or
/// pleamar where there is no pleamar-wm.
pub fn open_window(args: &[String]) -> Result<()> {
    crate::daemon::ensure();
    let mut scene = write_ui()?;
    // Picking for another program (the portal), the window says so in its
    // title, «Sonda · Elegir»: the compositor can float it over that program
    // as it does a dialog (pleamar-wm: `window title="Sonda · Elegir" float`).
    // A copy of the window in `pick/`, still called sonda.plm: pleamar names
    // a scene, and finds its logic, by its file's name.
    if std::env::var_os("SONDA_PICK").is_some_and(|v| !v.is_empty()) {
        let from = scene.parent().map(Path::to_path_buf).unwrap_or_default();
        let dir = from.join("pick");
        for (name, _) in UI {
            let mut text = std::fs::read_to_string(from.join(name))?;
            if *name == "sonda.plm" {
                text = text.replacen("title: \"Sonda\";", "title: \"Sonda · Elegir\";", 1);
            }
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::read_to_string(&path).ok() != Some(text.clone()) {
                std::fs::write(&path, text)?;
            }
        }
        scene = dir.join("sonda.plm");
    }
    let mut start = String::new();
    let mut search = false;
    let mut select = String::new();
    let mut props = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--search" | "-s" => search = true,
            "--select" | "--properties" => {
                props = a == "--properties";
                let Some(f) = it.next() else { continue };
                let f = from_uri(f).unwrap_or_else(|| f.clone());
                let f = std::fs::canonicalize(&f).unwrap_or_else(|_| PathBuf::from(&f));
                start = f.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                select = f.to_string_lossy().into_owned();
            }
            a if !a.starts_with('-') => {
                let p = from_uri(a).unwrap_or_else(|| a.to_owned());
                let p = Path::new(&p);
                // A file: its folder, with it chosen.
                let p = if p.is_dir() {
                    p.to_path_buf()
                } else {
                    select = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned();
                    p.parent().map(Path::to_path_buf).unwrap_or_default()
                };
                start = std::fs::canonicalize(&p).unwrap_or(p).to_string_lossy().into_owned();
            }
            _ => {}
        }
    }
    let runner = ["pleamar-wm", "pleamar"].into_iter().find(|b| which(b)).or_else(|| {
        let local = crate::paths::home().join("Proyectos/pleamar-wm/target/release/pleamar-wm");
        local.exists().then_some("")
    });
    let runner = match runner {
        Some("") => crate::paths::home().join("Proyectos/pleamar-wm/target/release/pleamar-wm").to_string_lossy().into_owned(),
        Some(b) => b.to_owned(),
        None => bail!("pleamar is not installed: Sonda's window is a pleamar scene"),
    };
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    // The window starts `sonda bridge` by name: wherever it was started from
    // —a portal started by the bus has only /usr/bin in its PATH—, that is
    // this very program.
    let path = match (Path::new(&exe).parent(), std::env::var("PATH")) {
        (Some(d), Ok(p)) if !p.is_empty() => format!("{}:{p}", d.display()),
        (Some(d), _) => format!("{}:/usr/local/bin:/usr/bin", d.display()),
        (None, p) => p.unwrap_or_default(),
    };
    let err = exec::exec(&runner, &["--scene", &scene.to_string_lossy(), "--no-hud"], &[("SONDA_START", &start), ("SONDA_SEARCH", if search { "1" } else { "" }), ("SONDA_SELECT", &select), ("SONDA_PROPS", if props { "1" } else { "" }), ("SONDA_BIN", &exe), ("PATH", &path)]);
    Err(anyhow!("cannot start {runner}: {err}"))
}

mod exec {
    use std::os::unix::process::CommandExt;
    pub fn exec(cmd: &str, args: &[&str], env: &[(&str, &str)]) -> std::io::Error {
        let mut c = std::process::Command::new(cmd);
        c.args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.exec()
    }
}
