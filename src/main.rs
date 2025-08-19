use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use lru::LruCache;
use notify::{event::ModifyKind, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::time::Instant;
use sysinfo::System;
use tendril::{fmt::UTF8, Tendril};

static CACHE_SIZE: Lazy<NonZeroUsize> = Lazy::new(|| NonZeroUsize::new(1000).unwrap());
static HTML_CACHE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static CSS_CACHE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

struct ClassExtractor {
    classes: RefCell<HashSet<String>>,
}

impl TokenSink for ClassExtractor {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<Self::Handle> {
        if let Token::TagToken(tag) = token {
            if tag.kind == TagKind::StartTag {
                for attr in tag.attrs.iter() {
                    if &*attr.name.local == "class" {
                        let class_value = &attr.value;
                        let mut current_class = String::new();
                        let mut paren_level = 0;
                        let mut classes = self.classes.borrow_mut();

                        for c in class_value.chars() {
                            match c {
                                '(' => {
                                    paren_level += 1;
                                    current_class.push(c);
                                }
                                ')' => {
                                    if paren_level > 0 {
                                        paren_level -= 1;
                                    }
                                    current_class.push(c);
                                }
                                ' ' | '\t' | '\n' | '\r' => {
                                    if paren_level == 0 {
                                        if !current_class.is_empty() {
                                            classes.insert(current_class);
                                            current_class = String::new();
                                        }
                                    } else {
                                        current_class.push(c);
                                    }
                                }
                                _ => {
                                    current_class.push(c);
                                }
                            }
                        }
                        if !current_class.is_empty() {
                            classes.insert(current_class);
                        }
                    }
                }
            }
        }
        TokenSinkResult::Continue
    }
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

    let mut class_cache: LruCache<String, ()> = LruCache::new(*CACHE_SIZE);
    read_existing_classes("style.css", &mut class_cache)?;
    rebuild_styles(&mut class_cache, true)?;

    let (tx, rx) = mpsc::channel();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Modify(ModifyKind::Data(_))) {
                    tx.send(()).ok();
                }
            }
        },
        notify::Config::default(),
    )?;

    watcher.watch(Path::new("index.html"), RecursiveMode::NonRecursive)?;
    watcher.watch(Path::new("style.css"), RecursiveMode::NonRecursive)?;

    println!("{}", "Watching index.html and style.css for changes...".cyan());
    while rx.recv().is_ok() {
        rebuild_styles(&mut class_cache, false)?;
    }

    Ok(())
}

fn read_existing_classes(
    css_path: &str,
    cache: &mut LruCache<String, ()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(css_path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.starts_with('.') {
            if let Some(class_part) = trimmed.split('{').next() {
                if let Some(class) = class_part.trim().strip_prefix('.') {
                    if !class.is_empty() {
                        cache.put(class.to_string(), ());
                    }
                }
            }
        }
    }
    Ok(())
}

fn rebuild_styles(
    class_cache: &mut LruCache<String, ()>,
    is_initial_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    let html_content = fs::read_to_string("index.html")?;
    let css_content = fs::read_to_string("style.css")?;

    let mut old_html = HTML_CACHE.lock().unwrap();
    let mut old_css = CSS_CACHE.lock().unwrap();

    let html_changed = *old_html != html_content;
    let css_changed = *old_css != css_content;

    if !is_initial_run && !html_changed && !css_changed {
        return Ok(());
    }

    let mut timer = Instant::now();
    let sink = ClassExtractor {
        classes: RefCell::new(HashSet::new()),
    };
    #[allow(unused_mut)]
    let mut tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let mut buffer = BufferQueue::default();
    let tendril = Tendril::<UTF8>::from_slice(&html_content);
    buffer.push_back(tendril.try_reinterpret().unwrap());
    let _ = tokenizer.feed(&mut buffer);
    tokenizer.end();
    let new_classes = tokenizer.sink.classes.into_inner();
    let parse_extract_duration = timer.elapsed();

    timer = Instant::now();
    let cached_classes: HashSet<_> = class_cache.iter().map(|(k, _)| k.clone()).collect();
    let added: HashSet<_> = new_classes.difference(&cached_classes).cloned().collect();
    let removed: HashSet<_> = cached_classes.difference(&new_classes).cloned().collect();
    let diff_duration = timer.elapsed();

    timer = Instant::now();
    for class in &removed {
        class_cache.pop(class);
    }
    for class in &added {
        class_cache.put(class.clone(), ());
    }
    let cache_update_duration = timer.elapsed();

    let all_classes_to_write: HashSet<_> = class_cache.iter().map(|(k, _v)| k.clone()).collect();
    timer = Instant::now();
    update_css_file(&all_classes_to_write, &css_content)?;
    let write_duration = timer.elapsed();
    let total_duration = total_start.elapsed();

    let timing_details = format!(
        "Total: {} (Parse/Extract: {}, Diff: {}, Cache: {}, Write: {})",
        format_duration(total_duration),
        format_duration(parse_extract_duration),
        format_duration(diff_duration),
        format_duration(cache_update_duration),
        format_duration(write_duration)
    );
    println!(
        "Processed: {} added, {} removed | {}",
        format!("{}", added.len()).green(),
        format!("{}", removed.len()).red(),
        timing_details.bright_black()
    );

    *old_html = html_content;
    *old_css = css_content;

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

fn update_css_file(classes: &HashSet<String>, css_content: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut sorted_classes: Vec<_> = classes.iter().collect();
    sorted_classes.sort();

    let mut utility_css = String::with_capacity(classes.len() * 40);
    for class in sorted_classes {
        let mut escaped_class = String::new();
        serialize_identifier(class, &mut escaped_class).unwrap();
        writeln!(&mut utility_css, ".{} {{ display: flex; }}", escaped_class)?;
    }

    let re = Regex::new(r"(?s)/\* Dx\s*(.*?)\s*\*/")?;
    
    let scss_code = if let Some(captures) = re.captures(css_content) {
        captures.get(1).unwrap().as_str()
    } else {
        ""
    };

    let compiled_scss = if !scss_code.trim().is_empty() {
        grass::from_string(scss_code.to_string(), &grass::Options::default())?
    } else {
        String::new()
    };

    let generated_content = format!("{}\n{}", compiled_scss, utility_css);

    let formatted_generated_css = if !generated_content.trim().is_empty() {
        let stylesheet = StyleSheet::parse(&generated_content, ParserOptions::default())
            .map_err(|e| e.to_string())?;
        let printer_options = PrinterOptions {
            minify: false,
            ..PrinterOptions::default()
        };
        stylesheet.to_css(printer_options)?.code
    } else {
        String::new()
    };

    let final_css_content = format!(
        "{}\n\n/* Dx\n\n*/\n",
        formatted_generated_css.trim()
    );

    fs::write("style.css", final_css_content)?;

    Ok(())
}
