use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use memmap2::Mmap;
use notify_debouncer_full::new_debouncer;
use rayon::prelude::*;
use seahash::SeaHasher;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::Hasher;
use std::io::{BufWriter, Write as IoWrite};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;
use tendril::{fmt::UTF8, Tendril};

struct AppState {
    html_hash: u64,
    class_cache: HashSet<String>,
    utility_css_cache: HashMap<String, String>,
}

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
                                ' ' | '\t' | '\n' | '\r' if paren_level == 0 => {
                                    if !current_class.is_empty() {
                                        classes.insert(std::mem::take(&mut current_class));
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
        class_cache: HashSet::new(),
        utility_css_cache: HashMap::new(),
    }));

    rebuild_styles(app_state.clone(), true)?;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(50), None, tx)?;

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

    let html_file = File::open("index.html")?;
    let html_mmap = unsafe { Mmap::map(&html_file)? };

    let new_html_hash = {
        let mut hasher = SeaHasher::new();
        hasher.write(&html_mmap);
        hasher.finish()
    };

    let html_changed = {
        let mut state_guard = state.lock().unwrap();
        if state_guard.html_hash != new_html_hash {
            state_guard.html_hash = new_html_hash;
            true
        } else {
            false
        }
    };

    if !is_initial_run && !html_changed {
        return Ok(());
    }

    let html_content = std::str::from_utf8(&html_mmap)?;

    let timer = Instant::now();
    let sink = ClassExtractor {
        classes: RefCell::new(HashSet::with_capacity(256)),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let mut buffer = BufferQueue::default();
    let tendril = Tendril::<UTF8>::from_slice(html_content);
    buffer.push_back(tendril.try_reinterpret().unwrap());
    let _ = tokenizer.feed(&mut buffer);
    tokenizer.end();
    let all_classes = tokenizer.sink.classes.into_inner();
    let parse_extract_duration = timer.elapsed();

    let timer = Instant::now();
    let mut state_guard = state.lock().unwrap();
    let added: Vec<_> = all_classes.difference(&state_guard.class_cache).cloned().collect();
    let removed: Vec<_> = state_guard.class_cache.difference(&all_classes).cloned().collect();
    let diff_duration = timer.elapsed();

    let added_count = added.len();
    let removed_count = removed.len();

    if !added.is_empty() || !removed.is_empty() {
        let timer = Instant::now();
        state_guard.class_cache = all_classes;

        for class in removed {
            state_guard.utility_css_cache.remove(&class);
        }

        let new_rules: HashMap<String, String> = added
            .par_iter()
            .map(|class| {
                let mut escaped_class = String::new();
                serialize_identifier(class.as_str(), &mut escaped_class).unwrap();
                let rule = format!(".{} {{\n  display: flex;\n}}", escaped_class);
                (class.clone(), rule)
            })
            .collect();
        
        state_guard.utility_css_cache.extend(new_rules);
        let cache_update_duration = timer.elapsed();

        let css_write_timer = Instant::now();
        let mut sorted_classes: Vec<_> = state_guard.utility_css_cache.keys().cloned().collect();
        sorted_classes.par_sort_unstable();

        let file = File::create("style.css")?;
        let mut writer = BufWriter::with_capacity(state_guard.utility_css_cache.len() * 50, file);

        let mut first = true;
        for class_name in sorted_classes {
            if let Some(rule) = state_guard.utility_css_cache.get(&class_name) {
                if !first {
                    writer.write_all(b"\n\n")?;
                }
                writer.write_all(rule.as_bytes())?;
                first = false;
            }
        }
        if !first {
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        let css_write_duration = css_write_timer.elapsed();

        let wall_time = total_start.elapsed();
        let processing_time = parse_extract_duration + diff_duration + cache_update_duration + css_write_duration;

        let timing_details = format!(
            "Wall: {} (Processing: {} [Parse: {}, Diff: {}, Cache: {}, CSS Write: {}])",
            format_duration(wall_time),
            format_duration(processing_time),
            format_duration(parse_extract_duration),
            format_duration(diff_duration),
            format_duration(cache_update_duration),
            format_duration(css_write_duration)
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
