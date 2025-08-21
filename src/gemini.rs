use ahash::{AHashMap, AHashSet, AHasher};
use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use memmap2::Mmap;
use notify_debouncer_full::{new_debouncer};
use rayon::prelude::*;
use std::cell::RefCell;
use std::fs::File;
use std::hash::Hasher;
use std::io::{BufWriter, Write as IoWrite};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;
use tendril::{fmt::UTF8, Tendril};

struct AppState {
    html_hash: u64,
    class_cache: AHashSet<String>,
    utility_css_cache: AHashMap<String, String>,
}

struct ClassExtractor {
    classes: RefCell<AHashSet<String>>,
}

impl TokenSink for ClassExtractor {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<Self::Handle> {
        if let Token::TagToken(tag) = token {
            if tag.kind == TagKind::StartTag {
                for attr in tag.attrs.iter() {
                    if &*attr.name.local == "class" {
                        let mut classes = self.classes.borrow_mut();
                        for class in attr.value.split_whitespace() {
                            classes.insert(class.to_string());
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
        class_cache: AHashSet::default(),
        utility_css_cache: AHashMap::default(),
    }));

    rebuild_styles(app_state.clone(), true)?;

    let (tx, rx) = mpsc::channel();
    
    let mut debouncer = new_debouncer(Duration::from_millis(50), None, tx)?;

    debouncer
        .watch(Path::new("index.html"), notify::RecursiveMode::NonRecursive)?;

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
        let mut hasher = AHasher::default();
        hasher.write(&html_mmap);
        hasher.finish()
    };

    let (old_hash, old_class_cache) = {
        let state_guard = state.lock().unwrap();
        (state_guard.html_hash, state_guard.class_cache.clone())
    };

    if !is_initial_run && old_hash == new_html_hash {
        return Ok(());
    }

    let html_content = std::str::from_utf8(&html_mmap)?;

    let timer = Instant::now();
    let sink = ClassExtractor {
        classes: RefCell::new(AHashSet::with_capacity(256)),
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
    let added: AHashSet<&String> = all_classes.difference(&old_class_cache).collect();
    let removed: AHashSet<&String> = old_class_cache.difference(&all_classes).collect();
    let diff_duration = timer.elapsed();

    if !added.is_empty() || !removed.is_empty() {
        let added_count = added.len();
        let removed_count = removed.len();

        let timer = Instant::now();
        let new_rules: Vec<(String, String)> = added
            .par_iter()
            .map(|&class| {
                let mut escaped_class = String::with_capacity(class.len());
                serialize_identifier(class, &mut escaped_class).unwrap();
                let rule = format!(".{} {{\n  display: flex;\n}}", escaped_class);
                (class.clone(), rule)
            })
            .collect();
        let cache_update_duration = timer.elapsed();

        // FIX: Clone only the strings to be removed to decouple the lifetime from `all_classes`.
        let classes_to_remove: Vec<String> = removed.iter().map(|s| (*s).clone()).collect();

        let css_write_timer = Instant::now();
        let utility_css_cache = {
            let mut state_guard = state.lock().unwrap();
            state_guard.html_hash = new_html_hash;
            
            // Now that the borrows are no longer needed, we can safely move `all_classes`.
            state_guard.class_cache = all_classes;

            // Perform updates with the owned data.
            for class in &classes_to_remove {
                state_guard.utility_css_cache.remove(class);
            }
            state_guard.utility_css_cache.extend(new_rules);
            state_guard.utility_css_cache.clone()
        };

        let file = File::create("style.css")?;
        let mut writer = BufWriter::new(file);
        let mut is_first = true;

        for rule in utility_css_cache.values() {
            if !is_first {
                writer.write_all(b"\n\n")?;
            }
            writer.write_all(rule.as_bytes())?;
            is_first = false;
        }

        if !utility_css_cache.is_empty() {
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        let css_write_duration = css_write_timer.elapsed();

        let wall_time = total_start.elapsed();
        let processing_time =
            parse_extract_duration + diff_duration + cache_update_duration + css_write_duration;

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
