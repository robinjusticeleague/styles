// PART 1/2

use ahash::{AHashSet, AHasher};
use colored::Colorize;
use cssparser::serialize_identifier;
use memchr::{memchr, memmem::Finder};
use notify_debouncer_full::new_debouncer;
use std::fs::{File, OpenOptions};
use std::hash::Hasher;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

struct AppState {
    html_hash: u64,
    class_cache: AHashSet<String>,
    css_file: BufWriter<File>, // persistent handle for fast appends
}

fn format_duration(duration: std::time::Duration) -> String {
    let micros = duration.as_micros();
    if micros > 999 {
        format!("{:.2}ms", micros as f64 / 1000.0)
    } else {
        format!("{}µs", micros)
    }
}

fn extract_classes_fast(html_bytes: &[u8], capacity_hint: usize) -> AHashSet<String> {
    let mut set = AHashSet::with_capacity(capacity_hint.max(64));
    let finder = Finder::new(b"class");
    let mut pos = 0usize;
    let n = html_bytes.len();

    while let Some(idx) = finder.find(&html_bytes[pos..]) {
        let start = pos + idx + 5;
        let mut i = start;
        while i < n && matches!(html_bytes[i], b' ' | b'\n' | b'\r' | b'\t') {
            i += 1;
        }
        if i >= n || html_bytes[i] != b'=' {
            pos = start;
            continue;
        }
        i += 1;
        while i < n && matches!(html_bytes[i], b' ' | b'\n' | b'\r' | b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let quote = html_bytes[i];
        if quote != b'"' && quote != b'\'' {
            pos = i;
            continue;
        }
        i += 1;
        let value_start = i;
        let rel_end = memchr(quote, &html_bytes[value_start..]);
        let value_end = match rel_end {
            Some(off) => value_start + off,
            None => break,
        };
        if let Ok(value_str) = std::str::from_utf8(&html_bytes[value_start..value_end]) {
            for cls in value_str.split_whitespace() {
                if !cls.is_empty() {
                    set.insert(cls.to_owned());
                }
            }
        }
        pos = value_end + 1;
    }

    set
}

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
        .append(true)
        .create(true)
        .open("style.css")?;
    let css_writer = BufWriter::with_capacity(65536, css_file);

    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        class_cache: AHashSet::default(),
        css_file: css_writer,
    }));

    rebuild_styles(app_state.clone(), true)?;

    let (tx, rx) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(1), None, tx)?;
    debouncer.watch(Path::new("index.html"), notify::RecursiveMode::NonRecursive)?;
    println!("{}", "Watching index.html for changes...".cyan());

    for res in rx {
        match res {
            Ok(_) => {
                if let Err(e) = rebuild_styles(app_state.clone(), false) {
                    eprintln!("{} {}", "Error rebuilding styles:".red(), e);
                }
            }
            Err(e) => eprintln!("{} {:?}", "Watch error:".red(), e),
        }
    }

    Ok(())
}

// rebuild_styles in PART 2
// PART 2/2

fn rebuild_styles(
    app_state: Arc<Mutex<AppState>>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let start_time = Instant::now();

    let html_bytes = std::fs::read("index.html")?;
    let mut hasher = AHasher::default();
    hasher.write(&html_bytes);
    let new_hash = hasher.finish();

    let mut state = app_state.lock().unwrap();
    if !force && new_hash == state.html_hash {
        return Ok(()); // no change
    }
    state.html_hash = new_hash;

    let new_classes = extract_classes_fast(&html_bytes, state.class_cache.len());
    let added: Vec<_> = new_classes.difference(&state.class_cache).cloned().collect();
    let removed: Vec<_> = state.class_cache.difference(&new_classes).cloned().collect();

    if !added.is_empty() {
        for cls in &added {
            let mut buf = String::with_capacity(cls.len() + 6);
            buf.push('.');
            serialize_identifier(cls, &mut buf).unwrap();
            buf.push_str(" {}\n");
            state.css_file.write_all(buf.as_bytes())?;
        }
        state.css_file.flush()?;
    }

    if !removed.is_empty() {
        // Instead of truncating the live file, write a new temp file and swap
        let tmp_path = "style.css.tmp";
        {
            let mut tmp = BufWriter::with_capacity(
                65536,
                File::create(tmp_path)?
            );
            for cls in &new_classes {
                let mut buf = String::with_capacity(cls.len() + 6);
                buf.push('.');
                serialize_identifier(cls, &mut buf).unwrap();
                buf.push_str(" {}\n");
                tmp.write_all(buf.as_bytes())?;
            }
            tmp.flush()?;
        }
        std::fs::rename(tmp_path, "style.css")?;

        // Reopen css_file handle for future appends
        let css_file = OpenOptions::new()
            .write(true)
            .append(true)
            .open("style.css")?;
        state.css_file = BufWriter::with_capacity(65536, css_file);
    }

    state.class_cache = new_classes;

    let elapsed = start_time.elapsed();
    println!(
        "{} {} ({} added, {} removed)",
        "Styles rebuilt in".green(),
        format_duration(elapsed),
        added.len(),
        removed.len()
    );

    Ok(())
}
