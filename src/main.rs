use colored::Colorize;
use cssparser::serialize_identifier;
use memmap2::Mmap;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use regex::Regex;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write as IoWrite;
use std::path::Path;
use std::time::{Duration, Instant};

static CLASS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"class="([^"]*)""#).unwrap());

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting task...".cyan());

    if !Path::new("style.css").exists() {
        File::create("style.css")?;
    }

    if !Path::new("index.html").exists() {
        File::create("index.html")?;
    }

    let total_start = Instant::now();

    let html_file = File::open("index.html")?;
    let html_mmap = unsafe { Mmap::map(&html_file)? };
    let html_content = std::str::from_utf8(&html_mmap)?;

    let timer = Instant::now();

    let captures: Vec<_> = CLASS_RE.captures_iter(html_content).collect();

    let all_html_classes: Vec<String> = captures
        .par_iter()
        .flat_map_iter(|cap| {
            let group = cap.get(1).unwrap();
            let class_str = group.as_str();
            let mut classes_in_attr = Vec::new();
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
                            classes_in_attr.push(class_str[start..i].to_string());
                        }
                        start = i + c.len_utf8();
                    }
                    _ => {}
                }
            }
            if start < class_str.len() {
                classes_in_attr.push(class_str[start..].to_string());
            }
            classes_in_attr
        })
        .collect();

    let find_and_cache_duration = timer.elapsed();

    let write_timer = Instant::now();
    let mut file = OpenOptions::new().append(true).open("style.css")?;
    let mut css_to_append = String::with_capacity(all_html_classes.len() * 50);
    for class in all_html_classes {
        let mut escaped_class = String::new();
        serialize_identifier(&class, &mut escaped_class).unwrap();
        css_to_append.push_str(".");
        css_to_append.push_str(&escaped_class);
        css_to_append.push_str(" {\n  display: flex;\n}\n\n");
    }
    file.write_all(css_to_append.as_bytes())?;
    let css_write_duration = write_timer.elapsed();

    let total_duration = total_start.elapsed();

    let added_count = all_html_classes.len();

    let timing_details = format!(
        "Total: {} (Parse & Append Prep: {}, CSS Append: {})",
        format_duration(total_duration),
        format_duration(find_and_cache_duration),
        format_duration(css_write_duration)
    );
    println!(
        "Processed: {} appended | {}",
        format!("{}", added_count).green(),
        timing_details.bright_black()
    );

    if total_duration.as_micros() < 200 {
        println!("{}", "Task completed in less than 200µs!".green());
    } else {
        println!("{}", "Task took longer than 200µs.".yellow());
    }

    Ok(())
}

fn format_duration(duration: Duration) -> String {
    let micros = duration.as_micros();
    if micros > 999 {
        format!("{:.2}ms", micros as f64 / 1000.0)
    } else {
        format!("{}µs", micros)
    }
}