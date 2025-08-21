use ahash::{AHashMap, AHashSet, AHasher};
use colored::Colorize;
use cssparser::serialize_identifier;
use memmap2::MmapMut;
use notify_debouncer_full::new_debouncer;
use rayon::prelude::*;
use std::fs::{self, OpenOptions};
use std::hash::Hasher;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, LazyLock};
use std::time::{Duration, Instant};
use sysinfo::System;

// ===== Runtime lookup tables for whitespace and quotes =====
static WS: LazyLock<[bool; 256]> = LazyLock::new(make_ws_table);
static QT: LazyLock<[bool; 256]> = LazyLock::new(make_quote_table);

#[inline(always)]
fn make_ws_table() -> [bool; 256] {
    let mut ws = [false; 256];
    ws[b' ' as usize] = true;
    ws[b'\n' as usize] = true;
    ws[b'\r' as usize] = true;
    ws[b'\t' as usize] = true;
    ws
}
#[inline(always)]
fn make_quote_table() -> [bool; 256] {
    let mut qt = [false; 256];
    qt[b'"' as usize] = true;
    qt[b'\'' as usize] = true;
    qt
}
#[inline(always)]
fn is_ws(b: u8) -> bool { WS[b as usize] }
#[inline(always)]
fn is_quote(b: u8) -> bool { QT[b as usize] }

#[inline(always)]
fn eq5(bytes: &[u8], i: usize, pat: &[u8; 5]) -> bool {
    let j = i + 5;
    j <= bytes.len() && &bytes[i..j] == pat
}
#[inline(always)]
fn binary_insert_sorted<'a>(v: &mut Vec<&'a str>, item: &'a str) {
    match v.binary_search(&item) {
        Ok(_) => {}
        Err(pos) => v.insert(pos, item),
    }
}

/// Ultra‑fast class collector: zero allocations per class, sorted incrementally
pub fn collect_classes<'a>(html: &'a [u8]) -> Vec<&'a str> {
    let mut seen: AHashSet<&'a str> = AHashSet::with_capacity((html.len() / 96).max(32));
    let mut out: Vec<&'a str> = Vec::with_capacity((html.len() / 96).max(32));
    let mut i = 0usize;
    let n = html.len();
    const CLASS: &[u8; 5] = b"class";

    while i + 5 <= n {
        if html[i] != b'c' || !eq5(html, i, CLASS) {
            i += 1;
            while i < n && html[i] != b'c' { i += 1; }
            continue;
        }
        i += 5;
        while i < n && is_ws(html[i]) { i += 1; }
        if i >= n || html[i] != b'=' { continue; }
        i += 1;
        while i < n && is_ws(html[i]) { i += 1; }
        if i >= n || !is_quote(html[i]) { continue; }
        let quote = html[i];
        i += 1;
        while i < n && html[i] != quote {
            while i < n && html[i] != quote && is_ws(html[i]) { i += 1; }
            if i >= n || html[i] == quote { break; }
            let start = i;
            while i < n && html[i] != quote && !is_ws(html[i]) { i += 1; }
            let end = i;
            if end > start {
                let cls = unsafe { std::str::from_utf8_unchecked(&html[start..end]) };
                if seen.insert(cls) {
                    binary_insert_sorted(&mut out, cls);
                }
            }
        }
        if i < n && html[i] == quote { i += 1; }
    }
    out
}

/// Parallel CSS builder from sorted class names
pub fn build_css_parallel(keys: &[&str], cache: &AHashMap<String, String>) -> Vec<u8> {
    const CHUNK: usize = 256;
    let lengths: Vec<usize> = keys.par_iter()
        .map(|k| cache.get(*k).map(|s| s.len()).unwrap_or(0))
        .collect();
    let total_len: usize = lengths.iter().sum::<usize>()
        + (if keys.is_empty() { 0 } else { (keys.len() - 1) * 2 });
    let parts: Vec<Vec<u8>> = keys.par_chunks(CHUNK)
        .map(|chunk| {
            let mut buf = Vec::with_capacity(
                chunk.iter().map(|k| cache.get(*k).map(|s| s.len()).unwrap_or(0)).sum::<usize>()
                + (if chunk.is_empty() { 0 } else { (chunk.len() - 1) * 2 })
            );
            let mut first = true;
            for k in chunk {
                if let Some(body) = cache.get(*k) {
                    if !first { buf.extend_from_slice(b"\n\n"); }
                    first = false;
                    buf.extend_from_slice(body.as_bytes());
                }
            }
            buf
        })
        .collect();
    let mut out = Vec::with_capacity(total_len);
    let mut first = true;
    for mut part in parts {
        if part.is_empty() { continue; }
        if !first { out.extend_from_slice(b"\n\n"); }
        first = false;
        out.append(&mut part);
    }
    out
}

/// BufWriter fast path
#[inline(always)]
fn write_bufwriter(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let file = OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    let mut w = BufWriter::with_capacity(128 * 1024, file);
    w.write_all(bytes)?;
    w.flush()
}
/// mmap fast path
#[inline(always)]
fn write_mmap(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let len = bytes.len() as u64;
    let file = OpenOptions::new().read(true).write(true).create(true).open(path)?;
    file.set_len(len)?;
    let mut mmap: MmapMut = unsafe { MmapMut::map_mut(&file)? };
    mmap[..bytes.len()].copy_from_slice(bytes);
    mmap.flush()
}
pub fn write_css_fast(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    const MMAP_CUTOFF: usize = 256 * 1024;
    if bytes.len() >= MMAP_CUTOFF {
        write_mmap(path, bytes)
    } else {
        write_bufwriter(path, bytes)
    }
}

// ===== Application State =====
struct AppState {
    html_hash: u64,
    class_cache: AHashSet<String>,
    utility_css_cache: AHashMap<String, String>,
    css_hash: u64,
    css_buffer: Vec<u8>,
}

fn print_system_info() {
    let mut sys = System::new_all();
    sys.refresh_memory();
    let total_memory = sys.total_memory() / 1024 / 1024;
    let available_memory = sys.available_memory() / 1024 / 1024;
    let core_count = sys.cpus().len();
    println!(
        "{}",
        format!(
            "System Info: {} Cores, {}MB/{}MB Available Memory",
            core_count, available_memory, total_memory
        )
        .dimmed()
    );
}
fn rebuild_styles(
    state: Arc<Mutex<AppState>>,
    is_initial_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    let html_bytes = fs::read("index.html")?;
    let new_html_hash = {
        let mut hasher = AHasher::default();
        hasher.write(&html_bytes);
        hasher.finish()
    };

    {
        let state_guard = state.lock().unwrap();
        if !is_initial_run && state_guard.html_hash == new_html_hash {
            return Ok(()); // No HTML changes
        }
    }

    let parse_timer = Instant::now();
    let all_classes: Vec<&str> = collect_classes(&html_bytes);
    let parse_extract_duration = parse_timer.elapsed();

    let diff_timer = Instant::now();
    let (added, removed) = {
        let state_guard = state.lock().unwrap();
        let old_set: AHashSet<String> = state_guard.class_cache.clone();
        let new_set: AHashSet<String> = all_classes.iter().map(|s| s.to_string()).collect();
        (
            new_set.difference(&old_set).cloned().collect::<Vec<_>>(),
            old_set.difference(&new_set).cloned().collect::<Vec<_>>(),
        )
    };
    let diff_duration = diff_timer.elapsed();

    if added.is_empty() && removed.is_empty() {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;
        println!(
            "Processed: {} added, {} removed | Wall: {} (Processing: {} [Parse: {}, Diff: {}])",
            format!("{}", 0).green(),
            format!("{}", 0).red(),
            format_duration(total_start.elapsed()),
            format_duration(parse_extract_duration + diff_duration),
            format_duration(parse_extract_duration),
            format_duration(diff_duration),
        );
        return Ok(());
    }

    let cache_update_timer = Instant::now();
    let new_rules: Vec<(String, String)> = added
        .par_iter()
        .map(|class| {
            let mut escaped = String::with_capacity(class.len());
            serialize_identifier(class, &mut escaped).unwrap();
            let rule = format!(".{} {{\n  display: flex;\n}}", escaped);
            (class.clone(), rule)
        })
        .collect();
    let cache_update_duration = cache_update_timer.elapsed();

    let css_write_timer = Instant::now();
    let (css_bytes, css_changed) = {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;
        state_guard.class_cache = all_classes.iter().map(|s| s.to_string()).collect();
        for class in &removed {
            state_guard.utility_css_cache.remove(class);
        }
        if !new_rules.is_empty() {
            state_guard.utility_css_cache.extend(new_rules);
        }
        let mut keys: Vec<&str> = state_guard.utility_css_cache.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        let css_buf = build_css_parallel(&keys, &state_guard.utility_css_cache);
        let new_css_hash = {
            let mut hasher = AHasher::default();
            hasher.write(&css_buf);
            hasher.finish()
        };
        let changed = new_css_hash != state_guard.css_hash;
        if changed {
            state_guard.css_hash = new_css_hash;
            state_guard.css_buffer = css_buf.clone();
        }
        (css_buf, changed)
    };
    let css_write_duration = css_write_timer.elapsed();

    if css_changed {
        let write_start = Instant::now();
        write_css_fast(Path::new("style.css"), &css_bytes)?;
        let write_duration = write_start.elapsed();
        let wall_time = total_start.elapsed();
        let processing_time = parse_extract_duration
            + diff_duration
            + cache_update_duration
            + css_write_duration
            + write_duration;

        println!(
            "Processed: {} added, {} removed | Wall: {} (Processing: {} [Parse: {}, Diff: {}, Cache: {}, CSS Build: {}, CSS Write: {}])",
            format!("{}", added.len()).green(),
            format!("{}", removed.len()).red(),
            format_duration(wall_time),
            format_duration(processing_time),
            format_duration(parse_extract_duration),
            format_duration(diff_duration),
            format_duration(cache_update_duration),
            format_duration(css_write_duration),
            format_duration(write_duration)
        );
    } else {
        println!("{}", "No CSS changes, skipping write.".yellow());
    }

    Ok(())
}

fn format_duration(d: Duration) -> String {
    format!("{}µs", d.as_micros())
}


fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX Style Engine...".cyan());
    print_system_info();
    if !Path::new("style.css").exists() {
        OpenOptions::new().create(true).write(true).open("style.css")?;
    }
    if !Path::new("index.html").exists() {
        OpenOptions::new().create(true).write(true).open("index.html")?;
    }

    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        class_cache: AHashSet::default(),
        utility_css_cache: AHashMap::default(),
        css_hash: 0,
        css_buffer: Vec::with_capacity(1024),
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
