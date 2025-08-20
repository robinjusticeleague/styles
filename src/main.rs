use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use memmap2::Mmap;
use notify_debouncer_full::new_debouncer;
use once_cell::sync::Lazy;
use regex::Regex;
use seahash::SeaHasher;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::hash::Hasher;
use std::io::Write as IoWrite;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;
use tendril::{fmt::UTF8, Tendril};

// A static regex to find the custom SCSS block in the CSS file.
static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)/\* Dx\s*(.*?)\s*\*/").unwrap());

// A struct to hold the application's state, protected by a Mutex for thread-safe access.
struct AppState {
    html_hash: u64,
    css_hash: u64,
    class_cache: BTreeSet<String>,
    utility_css_cache: HashMap<String, String>,
    formatted_custom_css: String,
    old_scss: String,
}

// The ClassExtractor is a sink for the HTML5 tokenizer. It collects all class attributes.
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
                        // Custom parsing logic to handle complex class attributes, including those with parentheses.
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

// Prints system information like CPU cores and available memory.
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

// Main function: sets up files, state, and the file watcher loop.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX Style Engine...".cyan());
    print_system_info();

    // Ensure index.html and style.css exist before we start.
    if !Path::new("style.css").exists() {
        File::create("style.css")?;
    }
    if !Path::new("index.html").exists() {
        File::create("index.html")?;
    }

    // Initialize the application state within an Arc<Mutex> for shared, thread-safe access.
    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        css_hash: 0,
        class_cache: BTreeSet::new(),
        utility_css_cache: HashMap::new(),
        formatted_custom_css: String::new(),
        old_scss: String::new(),
    }));

    // Perform the initial style generation.
    rebuild_styles(app_state.clone(), true)?;

    // Set up a debounced file watcher to handle file change events efficiently.
    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(50), None, tx)?;

    debouncer
        .watch(Path::new("index.html"), notify::RecursiveMode::NonRecursive)?;
    debouncer
        .watch(Path::new("style.css"), notify::RecursiveMode::NonRecursive)?;

    println!("{}", "Watching index.html and style.css for changes...".cyan());

    // Main event loop: waits for file change events and triggers style rebuilds.
    for res in rx {
        match res {
            Ok(_) => {
                // We got a debounced event, so we can rebuild styles.
                if let Err(e) = rebuild_styles(app_state.clone(), false) {
                    eprintln!("{} {}", "Error rebuilding styles:".red(), e);
                }
            }
            Err(e) => eprintln!("{} {:?}", "Watch error:".red(), e),
        }
    }

    Ok(())
}

// Core function to rebuild styles when a file changes.
fn rebuild_styles(
    state: Arc<Mutex<AppState>>,
    is_initial_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    // Memory map the files to avoid blocking I/O. This is much faster than reading.
    let html_file = File::open("index.html")?;
    let css_file = File::open("style.css")?;
    let html_mmap = unsafe { Mmap::map(&html_file)? };
    let css_mmap = unsafe { Mmap::map(&css_file)? };

    // Calculate hashes of the file contents to quickly check for changes.
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

        // Update hashes in the state.
        state_guard.html_hash = new_html_hash;
        state_guard.css_hash = new_css_hash;
        (html_changed, css_changed)
    };

    // If nothing changed and it's not the first run, we're done.
    if !is_initial_run && !html_changed && !css_changed {
        return Ok(());
    }

    // Convert memory-mapped bytes to string slices.
    let html_content = std::str::from_utf8(&html_mmap)?;
    let css_content = std::str::from_utf8(&css_mmap)?;

    // Use Rayon to parallelize HTML parsing and SCSS processing.
    let (new_classes, scss_results) = rayon::join(
        || {
            // Task 1: Parse HTML to extract classes.
            let timer = Instant::now();
            let sink = ClassExtractor {
                classes: RefCell::new(BTreeSet::new()),
            };
            let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
            let mut buffer = BufferQueue::default();
            let tendril = Tendril::<UTF8>::from_slice(html_content);
            buffer.push_back(tendril.try_reinterpret().unwrap());
            let _ = tokenizer.feed(&mut buffer);
            tokenizer.end();
            let classes = tokenizer.sink.classes.into_inner();
            (classes, timer.elapsed())
        },
        || {
            // Task 2: Process the SCSS part of the stylesheet.
            let timer = Instant::now();
            let scss_code = RE
                .captures(css_content)
                .and_then(|cap| cap.get(1))
                .map_or("", |m| m.as_str());

            let mut state_guard = state.lock().unwrap();
            let scss_changed = state_guard.old_scss != scss_code;

            if scss_changed || is_initial_run {
                state_guard.old_scss = scss_code.to_string();
                let compiled_scss = if !scss_code.trim().is_empty() {
                    grass::from_string(scss_code.to_string(), &grass::Options::default())
                        .unwrap_or_else(|e| {
                            eprintln!("{} {}", "SCSS Error:".red(), e);
                            String::new()
                        })
                } else {
                    String::new()
                };

                let stylesheet = StyleSheet::parse(&compiled_scss, ParserOptions::default())
                    .map_err(|e| e.to_string());

                if let Ok(ss) = stylesheet {
                    let formatted_custom = ss
                        .to_css(PrinterOptions {
                            minify: false,
                            ..PrinterOptions::default()
                        })
                        .map(|out| out.code)
                        .unwrap_or_default();
                    state_guard.formatted_custom_css = formatted_custom;
                }
            }
            timer.elapsed()
        },
    );

    let (all_classes, parse_extract_duration) = new_classes;
    let scss_duration = scss_results;

    // Calculate the difference between old and new classes for incremental updates.
    let timer = Instant::now();
    let mut state_guard = state.lock().unwrap();
    let added: HashSet<_> = all_classes.difference(&state_guard.class_cache).cloned().collect();
    let removed: HashSet<_> = state_guard.class_cache.difference(&all_classes).cloned().collect();
    let diff_duration = timer.elapsed();

    let mut cache_update_duration = std::time::Duration::ZERO;
    if !added.is_empty() || !removed.is_empty() {
        let timer = Instant::now();
        // Update the main class cache.
        state_guard.class_cache = all_classes;

        // Incrementally update the utility CSS cache.
        for class in removed.iter() {
            state_guard.utility_css_cache.remove(class);
        }
        for class in added.iter() {
            let mut escaped_class = String::new();
            serialize_identifier(class.as_str(), &mut escaped_class).unwrap();
            let rule = format!(".{} {{\n  display: flex;\n}}\n", escaped_class);
            state_guard.utility_css_cache.insert(class.clone(), rule);
        }
        cache_update_duration = timer.elapsed();
    }

    // Reconstruct the final CSS and write it to the file.
    let timer = Instant::now();
    let mut final_css = String::new();
    if !state_guard.formatted_custom_css.is_empty() {
        final_css.push_str(&state_guard.formatted_custom_css);
        if !final_css.ends_with('\n') {
            final_css.push('\n');
        }
    }
    // Append utility classes in a consistent order.
    let mut sorted_classes: Vec<_> = state_guard.utility_css_cache.keys().collect();
    sorted_classes.sort();
    for class_name in sorted_classes {
        if let Some(rule) = state_guard.utility_css_cache.get(class_name) {
            final_css.push_str(rule);
        }
    }

    let scss_block = format!("\n/* Dx\n{}\n*/", state_guard.old_scss);
    final_css.push_str(&scss_block);

    // Drop the lock before writing to the file to avoid holding it during I/O.
    drop(state_guard);

    // Only write to the file if the content has actually changed.
    if css_content.trim() != final_css.trim() {
        let mut file = OpenOptions::new().write(true).truncate(true).open("style.css")?;
        file.write_all(final_css.as_bytes())?;
    }
    let write_duration = timer.elapsed();

    let total_duration = total_start.elapsed();

    // Log detailed performance metrics for the update.
    let timing_details = format!(
        "Total: {} (HTML Parse: {}, SCSS: {}, Diff: {}, Cache: {}, Write: {})",
        format_duration(total_duration),
        format_duration(parse_extract_duration),
        format_duration(scss_duration),
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

    Ok(())
}

// Helper function to format a Duration into a human-readable string (ms or µs).
fn format_duration(duration: std::time::Duration) -> String {
    let micros = duration.as_micros();
    if micros > 999 {
        format!("{:.2}ms", micros as f64 / 1000.0)
    } else {
        format!("{}µs", micros)
    }
}
