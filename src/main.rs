use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use memmap2::Mmap;
use notify_debouncer_full::new_debouncer;
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

struct AppState {
    html_hash: u64,
    css_hash: u64,
    class_cache: BTreeSet<String>,
    utility_css_cache: HashMap<String, String>,
}

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

    let app_state = Arc::new(Mutex::new(AppState {
        html_hash: 0,
        css_hash: 0,
        class_cache: BTreeSet::new(),
        utility_css_cache: HashMap::new(),
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
    let total_start = Instant::now();

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

    let mut parse_extract_duration = Duration::ZERO;
    let mut diff_duration = Duration::ZERO;
    let mut cache_update_duration = Duration::ZERO;
    let mut added_count = 0;
    let mut removed_count = 0;

    if is_initial_run || html_changed {
        let html_content = std::str::from_utf8(&html_mmap)?;
        let (all_classes, duration) = {
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
        };
        parse_extract_duration = duration;

        let timer = Instant::now();
        let mut state_guard = state.lock().unwrap();
        let added: HashSet<_> = all_classes.difference(&state_guard.class_cache).cloned().collect();
        let removed: HashSet<_> = state_guard.class_cache.difference(&all_classes).cloned().collect();
        diff_duration = timer.elapsed();

        added_count = added.len();
        removed_count = removed.len();

        if !added.is_empty() || !removed.is_empty() {
            let timer = Instant::now();
            state_guard.class_cache = all_classes;

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
    }

    let timer = Instant::now();
    let mut final_css = String::new();
    let state_guard = state.lock().unwrap();
    let mut sorted_classes: Vec<_> = state_guard.utility_css_cache.keys().collect();
    sorted_classes.sort();
    for class_name in sorted_classes {
        if let Some(rule) = state_guard.utility_css_cache.get(class_name) {
            final_css.push_str(rule);
        }
    }
    drop(state_guard);

    let css_content = std::str::from_utf8(&css_mmap)?;
    if css_content.trim() != final_css.trim() {
        let mut file = OpenOptions::new().write(true).truncate(true).open("style.css")?;
        file.write_all(final_css.as_bytes())?;
    }
    let write_duration = timer.elapsed();

    let total_duration = total_start.elapsed();

    if is_initial_run || added_count > 0 || removed_count > 0 {
        let timing_details = format!(
            "Total: {} (HTML Parse: {}, Diff: {}, Cache: {}, Write: {})",
            format_duration(total_duration),
            format_duration(parse_extract_duration),
            format_duration(diff_duration),
            format_duration(cache_update_duration),
            format_duration(write_duration)
        );
        println!(
            "Processed: {} added, {} removed | {}",
            format!("{}", added_count).green(),
            format!("{}", removed_count).red(),
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
