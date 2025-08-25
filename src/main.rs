use ahash::AHashSet;
use colored::Colorize;
use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::Path;
use std::sync::{Arc, Mutex};

mod engine;
mod generator;
mod parser;
mod telemetry;
mod watcher;

use engine::{rebuild_styles, AppState};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX Style Engine...".cyan());

    if !Path::new("style.css").exists() {
        File::create("style.css")?;
    }
    if !Path::new("index.html").exists() {
        File::create("index.html")?;
    }

    let css_file = OpenOptions::new()
        .write(true)
        .truncate(false)
        .create(true)
        .open("style.css")?;
    let css_writer = BufWriter::with_capacity(65536, css_file);

    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        class_cache: AHashSet::default(),
        css_file: css_writer,
    }));

    rebuild_styles(app_state.clone(), true)?;

    watcher::start(app_state)?;

    Ok(())
}
