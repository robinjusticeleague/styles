use colored::Colorize;
use cssparser::serialize_identifier;
use memmap2::Mmap;
use notify_debouncer_full::new_debouncer;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use regex::Regex;
use seahash::SeaHasher;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::hash::Hasher;
use std::io::Write as IoWrite;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;

static CLASS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"class="([^"]*)""#).unwrap());

struct AppState {
    html_hash: u64,
    css_hash: u64,
    class_cache: HashSet<String>,
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX Style Engine...".cyan());
    print_system_info();

    if !Path::new("style.css").exists() {
        File::create("style.css")?;
    }
    if !Path::new("index.html").exists() {
        File::create("index.html")?;
    }

    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        css_hash: 0,
        class_cache: HashSet::with_capacity(2048),
    }));

    rebuild_styles(app_state.clone(), true)?;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(50), None, tx)?;

    debouncer.watch(Path::new("index.html"), notify::RecursiveMode::NonRecursive)?;
    debouncer.watch(Path::new("style.css"), notify::RecursiveMode::NonRecursive)?;

    println!("{}", "Watching index.html and style.css for changes...".cyan());

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
    let html_file = File::open("index.html")?;
    let css_file = File::open("style.css")?;
    let html_mmap = unsafe { Mmap::map(&html_file)? };
    let css_mmap = unsafe { Mmap::map(&css_file)? };

    let mut html_hasher = SeaHasher::new();
    html_hasher.write(&html_mmap);
    let new_html_hash = html_hasher.finish();

    let mut css_hasher = SeaHasher::new();
    css_hasher.write(&css_mmap);
    let new_css_hash = css_hasher.finish();

    let (html_changed, css_changed) = {
        let mut state_guard = state.lock().unwrap();
        let html_changed = state_guard.html_hash != new_html_hash;
        let css_changed = state_guard.css_hash != new_css_hash;

        if html_changed {
            state_guard.html_hash = new_html_hash;
        }
        if css_changed {
            state_guard.css_hash = new_css_hash;
        }

        (html_changed, css_changed)
    };

    if !is_initial_run && !html_changed && !css_changed {
        return Ok(());
    }

    let total_start = Instant::now();
    let mut find_and_cache_duration = Duration::ZERO;
    let mut css_write_duration = Duration::ZERO;
    let mut added_count = 0;

    if is_initial_run || html_changed {
        let html_content = std::str::from_utf8(&html_mmap)?;
        let timer = Instant::now();

        let captures: Vec<_> = CLASS_RE.captures_iter(html_content).collect();

        let all_html_classes: HashSet<String> = captures
            .par_iter()
            .map(|cap| {
                let group = cap.get(1).unwrap();
                let class_str = group.as_str();
                let mut classes_in_attr = HashSet::with_capacity(8);
                let mut start = 0;
                let mut paren_level = 0;

                for (i, c) in class_str.char_indices() {
                    match c {
                        '(' => paren_level += 1,
                        ')' => {
                            if paren_level > 0 {
                                paren_level -= 1;
                            }
                        }
                        ' ' | '\t' | '\n' | '\r' if paren_level == 0 => {
                            if i > start {
                                classes_in_attr.insert(class_str[start..i].to_string());
                            }
                            start = i + c.len_utf8();
                        }
                        _ => {}
                    }
                }
                if start < class_str.len() {
                    classes_in_attr.insert(class_str[start..].to_string());
                }
                classes_in_attr
            })
            .reduce(HashSet::new, |mut a, b| {
                a.extend(b);
                a
            });

        let mut new_classes_to_add = Vec::with_capacity(all_html_classes.len());
        {
            let mut state_guard = state.lock().unwrap();
            for class in all_html_classes {
                if state_guard.class_cache.insert(class.clone()) {
                    new_classes_to_add.push(class);
                }
            }
        }

        added_count = new_classes_to_add.len();
        find_and_cache_duration = timer.elapsed();

        if !new_classes_to_add.is_empty() {
            let write_timer = Instant::now();
            let mut file = OpenOptions::new().append(true).open("style.css")?;
            let mut css_to_append = String::with_capacity(new_classes_to_add.len() * 50);
            for class in new_classes_to_add {
                let mut escaped_class = String::new();
                serialize_identifier(&class, &mut escaped_class).unwrap();
                css_to_append.push_str(".");
                css_to_append.push_str(&escaped_class);
                css_to_append.push_str(" {\n  display: flex;\n}\n\n");
            }
            file.write_all(css_to_append.as_bytes())?;
            css_write_duration = write_timer.elapsed();
        }
    }

    let total_duration = total_start.elapsed();

    if is_initial_run || added_count > 0 {
        let timing_details = format!(
            "Total: {} (Parse & Cache: {}, CSS Append: {})",
            format_duration(total_duration),
            format_duration(find_and_cache_duration),
            format_duration(css_write_duration)
        );
        println!(
            "Processed: {} added | {}",
            format!("{}", added_count).green(),
            timing_details.bright_black()
        );
    }

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
