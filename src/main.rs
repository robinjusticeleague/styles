use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use notify::{event::ModifyKind, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::fmt::Write;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::time::Instant;
use sysinfo::System;
use tendril::{fmt::UTF8, Tendril};

static HTML_CACHE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static CSS_CACHE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static OLD_SCSS: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static FORMATTED_CUSTOM_CSS: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)/\* Dx\s*(.*?)\s*\*/").unwrap());

struct ClassExtractor {
    classes: RefCell<BTreeSet<String>>,
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

    let mut class_cache: BTreeSet<String> = BTreeSet::new();
    read_existing_classes("style.css", &mut class_cache)?;
    rebuild_styles(&mut class_cache, true)?;

    let (tx, mut rx) = mpsc::channel();

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
    loop {
        if rx.recv().is_err() {
            break;
        }

        drop(watcher);

        rebuild_styles(&mut class_cache, false)?;

        let (new_tx, new_rx) = mpsc::channel();
        rx = new_rx;

        let mut new_watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    if matches!(event.kind, EventKind::Modify(ModifyKind::Data(_))) {
                        new_tx.send(()).ok();
                    }
                }
            },
            notify::Config::default(),
        )?;
        new_watcher.watch(Path::new("index.html"), RecursiveMode::NonRecursive)?;
        new_watcher.watch(Path::new("style.css"), RecursiveMode::NonRecursive)?;
        watcher = new_watcher;
    }

    Ok(())
}

fn read_existing_classes(
    css_path: &str,
    cache: &mut BTreeSet<String>,
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
                        cache.insert(class.to_string());
                    }
                }
            }
        }
    }
    Ok(())
}

fn rebuild_styles(
    class_cache: &mut BTreeSet<String>,
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

    let mut parse_extract_duration = std::time::Duration::ZERO;
    let new_classes = if html_changed || is_initial_run {
        let timer = Instant::now();
        let sink = ClassExtractor {
            classes: RefCell::new(BTreeSet::new()),
        };
        let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
        let mut buffer = BufferQueue::default();
        let tendril = Tendril::<UTF8>::from_slice(&html_content);
        buffer.push_back(tendril.try_reinterpret().unwrap());
        let _ = tokenizer.feed(&mut buffer);
        tokenizer.end();
        let classes = tokenizer.sink.classes.into_inner();
        parse_extract_duration = timer.elapsed();
        classes
    } else {
        class_cache.clone()
    };

    let timer = Instant::now();
    let cached_classes = class_cache.clone();
    let added: HashSet<_> = new_classes.difference(&cached_classes).cloned().collect();
    let removed: HashSet<_> = cached_classes.difference(&new_classes).cloned().collect();
    let diff_duration = timer.elapsed();

    let mut cache_update_duration = std::time::Duration::ZERO;

    if !added.is_empty() || !removed.is_empty() {
        let timer = Instant::now();
        *class_cache = new_classes;
        cache_update_duration = timer.elapsed();
    }

    let scss_code = if let Some(captures) = RE.captures(&css_content) {
        captures.get(1).unwrap().as_str().to_string()
    } else {
        String::new()
    };

    let old_scss_guard = OLD_SCSS.lock().unwrap();
    let scss_changed = *old_scss_guard != scss_code;
    drop(old_scss_guard);

    let full_regen = scss_changed || css_changed;
    let should_update_css = full_regen || !added.is_empty() || !removed.is_empty();
    let mut current_css = css_content.clone();
    let mut write_duration = std::time::Duration::ZERO;
    if should_update_css {
        let timer = Instant::now();
        current_css = update_css_file(class_cache, &scss_code, full_regen, &css_content)?;
        write_duration = timer.elapsed();
    }

    let total_duration = total_start.elapsed();

    if should_update_css {
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
    }

    *old_html = html_content;
    *old_css = current_css;

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

fn update_css_file(
    classes: &BTreeSet<String>,
    scss_code: &str,
    full_regen: bool,
    css_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let final_css_content = if full_regen {
        let compiled_scss = if !scss_code.trim().is_empty() {
            grass::from_string(scss_code.to_string(), &grass::Options::default())?
        } else {
            String::new()
        };

        let stylesheet = StyleSheet::parse(&compiled_scss, ParserOptions::default())
            .map_err(|e| e.to_string())?;

        let printer_options = PrinterOptions {
            minify: false,
            ..PrinterOptions::default()
        };
        let formatted_custom = stylesheet.to_css(printer_options)?.code;

        let mut formatted_custom_guard = FORMATTED_CUSTOM_CSS.lock().unwrap();
        *formatted_custom_guard = formatted_custom.clone();
        drop(formatted_custom_guard);

        let mut old_scss_guard = OLD_SCSS.lock().unwrap();
        *old_scss_guard = scss_code.to_string();
        drop(old_scss_guard);

        let mut utility_css = String::with_capacity(classes.len() * 60);
        for class in classes {
            let mut escaped_class = String::new();
            serialize_identifier(class.as_str(), &mut escaped_class).unwrap();
            utility_css.write_fmt(format_args!(".{} {{\n  display: flex;\n}}\n\n", escaped_class))?;
        }

        let mut generated = String::new();
        if !formatted_custom.is_empty() {
            generated.push_str(&formatted_custom);
            if !generated.ends_with('\n') {
                generated.push('\n');
            }
        }
        generated.push_str(&utility_css);

        format!(
            "{}\n\n/* Dx\n{}\n*/\n",
            generated.trim(),
            scss_code
        )
    } else {
        let formatted_custom = FORMATTED_CUSTOM_CSS.lock().unwrap().clone();

        let mut utility_css = String::with_capacity(classes.len() * 60);
        for class in classes {
            let mut escaped_class = String::new();
            serialize_identifier(class.as_str(), &mut escaped_class).unwrap();
            utility_css.write_fmt(format_args!(".{} {{\n  display: flex;\n}}\n\n", escaped_class))?;
        }

        let mut generated = String::new();
        if !formatted_custom.is_empty() {
            generated.push_str(&formatted_custom);
            if !generated.ends_with('\n') {
                generated.push('\n');
            }
        }
        generated.push_str(&utility_css);

        format!(
            "{}\n\n/* Dx\n{}\n*/\n",
            generated.trim(),
            scss_code
        )
    };

    if final_css_content != css_content {
        fs::write("style.css", &final_css_content)?;
    }

    Ok(final_css_content)
}
