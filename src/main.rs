use colored::Colorize;
use cssparser::serialize_identifier;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use lru::LruCache;
use notify::{event::ModifyKind, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use rayon::prelude::*;
use similar::{ChangeTag, TextDiff};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sysinfo::System;
use tendril::{fmt::UTF8, Tendril};
use tokio::sync::mpsc;
use tokio::task;

static CACHE_SIZE: Lazy<NonZeroUsize> = Lazy::new(|| NonZeroUsize::new(1000).unwrap());
static FILE_CACHE: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX HTML Class Parser...".cyan());
    print_system_info();

    if !Path::new("style.css").exists() {
        File::create("style.css")?;
    }

    let class_cache = Arc::new(Mutex::new(LruCache::new(*CACHE_SIZE)));
    read_existing_classes("style.css", Arc::clone(&class_cache))?;
    process_html_file("index.html", Arc::clone(&class_cache), true).await?;

    let (tx, mut rx) = mpsc::channel(1);

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Modify(ModifyKind::Data(_))) {
                    tx.blocking_send(()).ok();
                }
            }
        },
        notify::Config::default(),
    )?;

    watcher.watch(Path::new("index.html"), RecursiveMode::NonRecursive)?;

    println!("{}", "Watching index.html for changes...".cyan());
    while rx.recv().await.is_some() {
        process_html_file("index.html", Arc::clone(&class_cache), false).await?;
    }

    Ok(())
}

fn read_existing_classes(
    css_path: &str,
    cache: Arc<Mutex<LruCache<String, ()>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(css_path)?;
    let reader = BufReader::new(file);
    let mut cache = cache.lock().unwrap();

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

async fn process_html_file(
    html_path: &str,
    class_cache: Arc<Mutex<LruCache<String, ()>>>,
    is_initial_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    let new_content = tokio::fs::read_to_string(html_path).await?;

    let (should_process, css_to_write, timings) = task::spawn_blocking(move || {
        let mut timings = Vec::new();
        let mut timer = Instant::now();

        let mut old_content_guard = FILE_CACHE.lock().unwrap();

        if !is_initial_run {
            let diff = TextDiff::from_lines(&*old_content_guard, &new_content);
            let change_count = diff
                .iter_all_changes()
                .filter(|c| c.tag() != ChangeTag::Equal)
                .count();
            if change_count == 0 {
                return (false, None, timings);
            }
        }
        *old_content_guard = new_content.clone();
        drop(old_content_guard);
        timings.push(timer.elapsed()); // Diff time

        timer = Instant::now();
        let sink = ClassExtractor {
            classes: RefCell::new(HashSet::new()),
        };
        #[allow(unused_mut)]
        let mut tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
        let mut buffer = BufferQueue::default();
        let tendril = Tendril::<UTF8>::from_slice(&new_content);
        buffer.push_back(tendril.try_reinterpret().unwrap());
        let _ = tokenizer.feed(&mut buffer);
        tokenizer.end();
        let new_classes = tokenizer.sink.classes.into_inner();
        timings.push(timer.elapsed()); // Parse/Extract time

        timer = Instant::now();
        let mut cache = class_cache.lock().unwrap();
        let cached_classes: HashSet<_> = cache.iter().map(|(k, _)| k.clone()).collect();

        let added: HashSet<_> = new_classes.difference(&cached_classes).cloned().collect();
        let removed: HashSet<_> = cached_classes.difference(&new_classes).cloned().collect();
        timings.push(timer.elapsed()); // Set Difference time

        if !added.is_empty() || !removed.is_empty() {
            timer = Instant::now();
            for class in &removed {
                cache.pop(class);
            }
            for class in &added {
                cache.put(class.clone(), ());
            }
            timings.push(timer.elapsed()); // Cache update time

            let css_rules: Vec<String> = new_classes
                .par_iter()
                .map(|class| {
                    let mut escaped = String::new();
                    serialize_identifier(class, &mut escaped).unwrap();
                    format!(".{} {{ display: flex; }}\n", escaped)
                })
                .collect();

            let mut css_content = String::with_capacity(css_rules.len() * 40);
            for rule in css_rules {
                css_content.push_str(&rule);
            }
            (true, Some(css_content), timings)
        } else {
            (true, None, timings)
        }
    })
    .await?;

    if should_process {
        let write_timer = Instant::now();
        if let Some(css) = css_to_write {
            tokio::fs::write("style.css", css).await?;
        }
        let write_duration = write_timer.elapsed();
        let total_duration = total_start.elapsed();

        if !timings.is_empty() {
            let timing_details = format!(
                "Total: {} (Diff: {}, Parse/Extract: {}, Set-Diff: {}, Cache: {}, Write: {})",
                format_duration(total_duration),
                format_duration(timings[0]),
                format_duration(timings[1]),
                format_duration(timings[2]),
                format_duration(timings[3]),
                format_duration(write_duration)
            );
             println!(
                "{} | {}",
                "Processed".yellow(),
                timing_details.bright_black()
            );
        } else if is_initial_run {
            println!("{}", "No initial changes to classnames detected.".blue());
        }
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
