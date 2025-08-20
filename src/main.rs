use colored::Colorize;
use cssparser::serialize_identifier;
use memmap2::Mmap;
use notify_debouncer_full::new_debouncer;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use regex::Regex;
use seahash::SeaHasher;
use std::collections::HashSet;
use std::fs::{self, File};
use std::hash::Hasher;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sysinfo::System;

static CLASS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"class="([^"]*)""#).unwrap());
static LAST_HTML_HASH: Mutex<u64> = Mutex::new(0);

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
        fs::write("style.css", "")?;
    }
    if !Path::new("index.html").exists() {
        fs::write("index.html", "")?;
    }

    rebuild_styles(true)?;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(50), None, tx)?;

    debouncer.watch(Path::new("index.html"), notify::RecursiveMode::NonRecursive)?;

    println!("{}", "Watching index.html for changes...".cyan());

    for res in rx {
        match res {
            Ok(_) => {
                if let Err(e) = rebuild_styles(false) {
                    eprintln!("{} {}", "Error rebuilding styles:".red(), e);
                }
            }
            Err(e) => eprintln!("{} {:?}", "Watch error:".red(), e),
        }
    }

    Ok(())
}

fn rebuild_styles(is_initial_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    let html_file = File::open("index.html")?;
    let html_mmap = unsafe { Mmap::map(&html_file)? };

    let new_html_hash = {
        let mut hasher = SeaHasher::new();
        hasher.write(&html_mmap);
        hasher.finish()
    };

    let html_changed = {
        let mut last_hash = LAST_HTML_HASH.lock().unwrap();
        if *last_hash != new_html_hash {
            *last_hash = new_html_hash;
            true
        } else {
            false
        }
    };

    if !is_initial_run && !html_changed {
        return Ok(());
    }

    let total_start = Instant::now();

    let html_content = std::str::from_utf8(&html_mmap)?;
    let timer = Instant::now();

    let captures: Vec<_> = CLASS_RE.captures_iter(html_content).collect();

    let all_html_classes: HashSet<String> = captures
        .par_iter()
        .fold(
            || HashSet::with_capacity(256),
            |mut acc, cap| {
                let group = cap.get(1).unwrap();
                let class_str = group.as_str();
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
                                acc.insert(class_str[start..i].to_string());
                            }
                            start = i + c.len_utf8();
                        }
                        _ => {}
                    }
                }
                if start < class_str.len() {
                    acc.insert(class_str[start..].to_string());
                }
                acc
            },
        )
        .reduce(
            || HashSet::with_capacity(1024),
            |mut a, b| {
                a.extend(b);
                a
            },
        );

    let parse_duration = timer.elapsed();
    let write_timer = Instant::now();

    let mut sorted_classes: Vec<_> = all_html_classes.into_iter().collect();
    sorted_classes.sort_unstable();

    let file = File::create("style.css")?;
    let mut writer = BufWriter::with_capacity(sorted_classes.len() * 50, file);

    for class in &sorted_classes {
        let mut escaped_class = String::new();
        serialize_identifier(class, &mut escaped_class).unwrap();
        write!(
            writer,
            ".{} {{\n  display: flex;\n}}\n\n",
            escaped_class
        )?;
    }
    writer.flush()?;
    let write_duration = write_timer.elapsed();

    let total_duration = total_start.elapsed();
    let class_count = sorted_classes.len();

    let timing_details = format!(
        "Total: {} (Parse: {}, Write: {})",
        format_duration(total_duration),
        format_duration(parse_duration),
        format_duration(write_duration)
    );
    println!(
        "Generated: {} classes | {}",
        format!("{}", class_count).green(),
        timing_details.bright_black()
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
