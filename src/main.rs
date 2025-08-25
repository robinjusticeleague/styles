use ahash::{AHashSet, AHasher};
use colored::Colorize;
use cssparser::serialize_identifier;
use memchr::{memchr, memmem::Finder};
use notify_debouncer_full::new_debouncer;
use std::fs::{File, OpenOptions};
use std::hash::Hasher;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

struct AppState {
    html_hash: u64,
    class_cache: AHashSet<String>,
    css_file: BufWriter<File>,
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

fn rebuild_styles(
    state: Arc<Mutex<AppState>>,
    is_initial_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    let read_timer = Instant::now();
    let html_bytes = std::fs::read("index.html")?;
    let read_duration = read_timer.elapsed();

    let hash_timer = Instant::now();
    let new_html_hash = {
        let mut hasher = AHasher::default();
        hasher.write(&html_bytes);
        hasher.finish()
    };
    let hash_duration = hash_timer.elapsed();

    {
        let state_guard = state.lock().unwrap();
        if !is_initial_run && state_guard.html_hash == new_html_hash {
            return Ok(());
        }
    }

    let parse_timer = Instant::now();
    let prev_len_hint = { state.lock().unwrap().class_cache.len() };
    let all_classes = extract_classes_fast(&html_bytes, prev_len_hint.next_power_of_two());
    let parse_extract_duration = parse_timer.elapsed();

    {
        let state_guard = state.lock().unwrap();
        if all_classes.is_empty() && !state_guard.class_cache.is_empty() {
            return Ok(());
        }
    }

    let diff_timer = Instant::now();
    let (added, removed, old_hash_just_for_info) = {
        let state_guard = state.lock().unwrap();
        let old = &state_guard.class_cache;
        let added: Vec<String> = all_classes.difference(old).cloned().collect();
        let removed: Vec<String> = old.difference(&all_classes).cloned().collect();
        (added, removed, state_guard.html_hash)
    };
    let diff_duration = diff_timer.elapsed();

    if added.is_empty() && removed.is_empty() {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;
        return Ok(());
    }

    let cache_update_timer = Instant::now();
    {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;
        state_guard.class_cache = all_classes.clone();
    }
    let cache_update_duration = cache_update_timer.elapsed();

    let css_write_timer = Instant::now();
    {
        let mut state_guard = state.lock().unwrap();

        if !removed.is_empty() {
            let classes: Vec<String> = state_guard.class_cache.iter().cloned().collect();
            state_guard.css_file.get_mut().set_len(0)?;
            state_guard.css_file.seek(SeekFrom::Start(0))?;
            let mut escaped = String::with_capacity(64);
            for class in classes {
                state_guard.css_file.write_all(b".")?;
                escaped.clear();
                serialize_identifier(&class, &mut escaped).unwrap();
                state_guard.css_file.write_all(escaped.as_bytes())?;
                state_guard.css_file.write_all(b" {\n  display: flex;\n}\n")?;
            }
        } else {
            let added_classes: Vec<String> = added.clone();
            state_guard.css_file.seek(SeekFrom::End(0))?;
            let mut escaped = String::with_capacity(64);
            for class in added_classes {
                state_guard.css_file.write_all(b".")?;
                escaped.clear();
                serialize_identifier(&class, &mut escaped).unwrap();
                state_guard.css_file.write_all(escaped.as_bytes())?;
                state_guard.css_file.write_all(b" {\n  display: flex;\n}\n")?;
            }
        }
        state_guard.css_file.flush()?;
    }
    let css_write_duration = css_write_timer.elapsed();

    println!(
        "Processed: {} added, {} removed (prev hash: {:x}) | (Total: {} -> Read: {}, Hash: {}, Parse: {}, Diff: {}, Cache: {}, Write: {})",
        format!("{}", added.len()).green(),
        format!("{}", removed.len()).red(),
        old_hash_just_for_info,
        format_duration(total_start.elapsed()),
        format_duration(read_duration),
        format_duration(hash_duration),
        format_duration(parse_extract_duration),
        format_duration(diff_duration),
        format_duration(cache_update_duration),
        format_duration(css_write_duration)
    );

    Ok(())
}
