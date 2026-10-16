//! Sonda: search your computer by what is in your files —photos, PDFs,
//! documents, recordings, videos and code— and walk it, in one window made
//! with pleamar. EmbeddingGemma 2 on llama.cpp, all of it on this machine.

mod apps;
mod bridge;
mod config;
mod daemon;
mod dbus;
mod drives;
mod embed;
mod extract;
mod files;
mod index;
mod jobs;
mod paths;
mod search;
mod store;

use anyhow::Result;
use serde_json::json;

const HELP: &str = "sonda — search your files by what is in them, and walk them

  sonda [FOLDER]          the window: a file manager that finds by meaning
  sonda --search          the window, with the search ready to type
  sonda --select FILE     the window at FILE's folder, with FILE chosen
  sonda --properties FILE the window at FILE's folder, showing its properties
  sonda search WORDS…     search from here (code: WORDS… for code)
  sonda status            how the reading goes
  sonda pause | resume    stop reading for a while, and go on
  sonda daemon            the reader and searcher (started by itself)
  sonda setup             puts the model and the window where they go
  sonda dbus              the file manager and the Open/Save dialog on the bus
  sonda quit              stops the daemon
";

fn main() {
    // `sonda search … | head` ends quietly when head has had enough.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("daemon") => daemon::run(),
        Some("bridge") => bridge::run(),
        Some("dbus") => dbus::run(),
        Some("search") => cli_search(&args[1..].join(" ")),
        Some("status") => cli_status(),
        Some("pause") | Some("resume") => {
            daemon::ensure();
            daemon::ask(&json!({ "op": args[0] })).map(|v| println!("{v}"))
        }
        Some("quit") => daemon::ask(&json!({ "op": "quit" })).map(|_| ()),
        Some("setup") => files::setup(),
        Some("embed-test") => embed_test(&args[1..]),
        Some("-h") | Some("--help") | Some("help") => {
            print!("{HELP}");
            Ok(())
        }
        _ => files::open_window(&args),
    };
    if let Err(e) = result {
        eprintln!("sonda · {e}");
        std::process::exit(1);
    }
}

fn cli_search(q: &str) -> Result<()> {
    daemon::ensure();
    let v = daemon::ask(&json!({ "op": "search", "q": q, "limit": 15 }))?;
    if let Some(e) = v["error"].as_str() {
        anyhow::bail!("{e}");
    }
    for h in v["hits"].as_array().into_iter().flatten() {
        let place = match (h["segment"].as_str(), h["loc"].as_f64()) {
            (Some("page") | Some("page_image"), Some(p)) => format!(" · page {p}"),
            (Some("audio") | Some("frame"), Some(t)) => format!(" · {}:{:02}", t as u64 / 60, t as u64 % 60),
            (Some("lines"), Some(l)) => format!(" · line {l}"),
            _ => String::new(),
        };
        println!("{:5.2}  {}{place}", h["score"].as_f64().unwrap_or(0.0), h["path"].as_str().unwrap_or(""));
        if let Some(x) = h["excerpt"].as_str() {
            let one: String = x.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(140).collect();
            println!("       {one}");
        }
    }
    println!("({} ms)", v["ms"]);
    Ok(())
}

fn cli_status() -> Result<()> {
    daemon::ensure();
    let v = daemon::ask(&json!({ "op": "status" }))?;
    println!("{}", serde_json::to_string_pretty(&v["status"])?);
    Ok(())
}

/// The model against itself: words near the pictures they describe.
fn embed_test(args: &[String]) -> Result<()> {
    let started = std::time::Instant::now();
    let mut e = embed::Embedder::open(true, true)?;
    println!("open in {:.2} s, gpu {}", started.elapsed().as_secs_f64(), embed::Embedder::has_gpu());
    let words = ["a dog on the beach", "un perro en la playa", "a spreadsheet of invoices", "source code in Rust"];
    let t = std::time::Instant::now();
    let tv: Vec<Vec<f32>> = e.texts(&words.iter().map(|w| embed::query(w)).collect::<Vec<_>>())?;
    println!("4 texts in {} ms", t.elapsed().as_millis());
    println!("dog·perro {:.3}  dog·invoices {:.3}", embed::cosine(&tv[0], &tv[1]), embed::cosine(&tv[0], &tv[2]));
    for round in 0..3 {
        let t = std::time::Instant::now();
        let _ = e.texts(&words.iter().map(|w| embed::document("x", w)).collect::<Vec<_>>())?;
        let one = std::time::Instant::now();
        let _ = e.text(&embed::query("a cat sleeping on a sofa"))?;
        println!("round {round}: 4 texts {} ms, 1 text {} ms", t.elapsed().as_millis() - one.elapsed().as_millis(), one.elapsed().as_millis());
    }
    for p in args.iter().chain(args.iter()) {
        let t = std::time::Instant::now();
        let img = extract::load_rgb(std::path::Path::new(p))?;
        let v = e.picture(&img, 140)?;
        print!("{p} ({} ms):", t.elapsed().as_millis());
        for (w, q) in words.iter().zip(&tv) {
            print!("  {w}: {:.3}", embed::cosine(&v, q));
        }
        println!();
    }
    Ok(())
}
