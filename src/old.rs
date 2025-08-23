use ahash::{AHashSet, AHasher};
use colored::Colorize;
use cssparser::serialize_identifier;
use memchr::{memchr, memmem::Finder};
use notify_debouncer_full::new_debouncer;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::hash::Hasher;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

struct AppState {
    html_hash: u64,
    class_cache: AHashSet<String>,
    utility_css_cache: BTreeMap<String, String>,
    css_hash: u64,
}

fn write_css_optimized(path: &str, data: &[u8]) -> std::io::Result<()> {
    fs::write(path, data)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX Style Engine...".cyan());

    if !Path::new("style.css").exists() {
        File::create("style.css")?;
    }
    if !Path::new("index.html").exists() {
        File::create("index.html")?;
    }

    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        class_cache: AHashSet::default(),
        utility_css_cache: BTreeMap::default(),
        css_hash: 0,
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
                    set.insert(cls.to_string());
                }
            }
        }
        pos = value_end + 1;
    }

    set
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
            return Ok(());
        }
    }

    let parse_timer = Instant::now();
    let prev_len_hint = { state.lock().unwrap().class_cache.len() };
    let all_classes = extract_classes_fast(&html_bytes, prev_len_hint.next_power_of_two());
    let parse_extract_duration = parse_timer.elapsed();

    let diff_timer = Instant::now();
    let (added, removed, old_hash_just_for_info) = {
        let state_guard = state.lock().unwrap();

        if !is_initial_run && state_guard.html_hash == new_html_hash {
            return Ok(());
        }

        let old = &state_guard.class_cache;
        let added: Vec<String> = all_classes.difference(old).cloned().collect();
        let removed: Vec<String> = old.difference(&all_classes).cloned().collect();

        (added, removed, state_guard.html_hash)
    };
    let diff_duration = diff_timer.elapsed();

    if added.is_empty() && removed.is_empty() {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;

        let wall_time = total_start.elapsed();
        let processing_time = parse_extract_duration + diff_duration;
        println!(
            "Processed: {} added, {} removed | Wall: {} (Processing: {} [Parse: {}, Diff: {}])",
            format!("{}", 0).green(),
            format!("{}", 0).red(),
            format_duration(wall_time),
            format_duration(processing_time),
            format_duration(parse_extract_duration),
            format_duration(diff_duration),
        );
        return Ok(());
    }

    const PAR_THRESHOLD: usize = 512;
    let cache_update_timer = Instant::now();
    let new_rules: Vec<(String, String)> = if added.len() >= PAR_THRESHOLD {
        added
            .par_iter()
            .map(|class| {
                let mut escaped = String::with_capacity(class.len());
                serialize_identifier(class, &mut escaped).unwrap();
                let rule = format!(".{} {{\n  display: flex;\n}}", escaped);
                (class.clone(), rule)
            })
            .collect()
    } else {
        let mut escaped = String::with_capacity(128);
        added
            .iter()
            .map(|class| {
                escaped.clear();
                serialize_identifier(class, &mut escaped).unwrap();
                let rule = format!(".{} {{\n  display: flex;\n}}", &escaped);
                (class.clone(), rule)
            })
            .collect()
    };
    let cache_update_duration = cache_update_timer.elapsed();

    let css_write_timer = Instant::now();
    let css_bytes_opt = {
        let mut state_guard = state.lock().unwrap();

        state_guard.html_hash = new_html_hash;
        state_guard.class_cache = all_classes;

        for class in removed.iter() {
            state_guard.utility_css_cache.remove(class);
        }
        if !new_rules.is_empty() {
            for (k, v) in new_rules {
                state_guard.utility_css_cache.insert(k, v);
            }
        }

        let n = state_guard.utility_css_cache.len();
        let total_len: usize = if n >= PAR_THRESHOLD {
            state_guard.utility_css_cache.par_iter().map(|(_, v)| v.len()).sum()
        } else {
            state_guard.utility_css_cache.values().map(|v| v.len()).sum()
        };
        let extra = if n > 0 { (n - 1) * 2 + 1 } else { 0 };

        let mut css_buffer = Vec::with_capacity(total_len + extra);

        let mut iter = state_guard.utility_css_cache.values();
        if let Some(first) = iter.next() {
            css_buffer.extend_from_slice(first.as_bytes());
            for v in iter {
                css_buffer.extend_from_slice(b"\n");
                css_buffer.extend_from_slice(v.as_bytes());
            }
            css_buffer.push(b'\n');
        }

        let mut hasher = AHasher::default();
        hasher.write(&css_buffer);
        let new_css_hash = hasher.finish();
        let css_changed_flag = is_initial_run || new_css_hash != state_guard.css_hash;
        if css_changed_flag {
            state_guard.css_hash = new_css_hash;
            Some(css_buffer)
        } else {
            None
        }
    };
    if let Some(css_bytes) = css_bytes_opt {
        write_css_optimized("style.css", &css_bytes)?;
    }
    let css_write_duration = css_write_timer.elapsed();

    let wall_time = total_start.elapsed();
    let processing_time =
        parse_extract_duration + diff_duration + cache_update_duration + css_write_duration;

    println!(
        "Processed: {} added, {} removed (prev hash: {:x}) | Wall: {} (Processing: {} [Parse: {}, Diff: {}, Cache: {}, CSS Write: {}])",
        format!("{}", added.len()).green(),
        format!("{}", removed.len()).red(),
        old_hash_just_for_info,
        format_duration(wall_time),
        format_duration(processing_time),
        format_duration(parse_extract_duration),
        format_duration(diff_duration),
        format_duration(cache_update_duration),
        format_duration(css_write_duration)
    );

    Ok(())
}

fn format_duration(duration: std::time::Duration) -> String {
    let micros = duration.as_micros();
    if micros > 999 {
        format!("{:.2}ms", micros as f64 / 1000.0)
    } else {
        format!("{}µs", micros)
    }
}
