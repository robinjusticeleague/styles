use colored::Colorize;
use cssparser::serialize_identifier;
use notify_debouncer_full::new_debouncer;
use once_cell::sync::Lazy;
use seahash::SeaHasher;
use std::collections::HashSet;
use std::fs::{File, OpenOptions, read_to_string};
use std::hash::Hasher;
use std::io::Write as IoWrite;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tl::ParserOptions;

static CSS_CLASS_RE: Lazy<tl::ParserOptions> = Lazy::new(|| ParserOptions::default());

struct AppState {
    html_hash: u64,
    css_hash: u64,
    class_cache: HashSet<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", "Starting DX Style Engine...".cyan());

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

    // Initialize class cache with existing classes from style.css
    {
        let css_content = read_to_string("style.css")?;
        let mut state_guard = app_state.lock().unwrap();
        let dom = tl::parse(&css_content, *CSS_CLASS_RE)?;
        for node in dom.query_selector(".") {
            if let Some(tag) = node.as_tag() {
                if let Some(class) = tag.attributes().get("class") {
                    for cls in class.as_utf8_str().split_whitespace() {
                        state_guard.class_cache.insert(cls.to_string());
                    }
                }
            }
        }
    }

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
    let html_content = read_to_string("index.html")?;
    let css_content = read_to_string("style.css")?;

    let mut html_hasher = SeaHasher::new();
    html_hasher.write(html_content.as_bytes());
    let new_html_hash = html_hasher.finish();

    let mut css_hasher = SeaHasher::new();
    css_hasher.write(css_content.as_bytes());
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

    if is_initial_run || html_changed || css_changed {
        let timer = Instant::now();
        let mut css_to_append = String::with_capacity(256);
        let mut new_classes = Vec::with_capacity(16);

        // Update class cache if style.css changed
        if css_changed {
            let mut state_guard = state.lock().unwrap();
            state_guard.class_cache.clear();
            let dom = tl::parse(&css_content, *CSS_CLASS_RE)?;
            for node in dom.query_selector(".") {
                if let Some(tag) = node.as_tag() {
                    if let Some(class) = tag.attributes().get("class") {
                        for cls in class.as_utf8_str().split_whitespace() {
                            state_guard.class_cache.insert(cls.to_string());
                        }
                    }
                }
            }
        }

        // Collect classes from index.html using tl
        let dom = tl::parse(&html_content, ParserOptions::default())?;
        for node in dom.query_selector("*[class]") {
            if let Some(tag) = node.as_tag() {
                if let Some(class) = tag.attributes().get("class") {
                    for cls in class.as_utf8_str().split_whitespace() {
                        new_classes.push(cls.to_string());
                    }
                }
            }
        }

        // Append only new classes
        {
            let mut state_guard = state.lock().unwrap();
            for class in new_classes {
                if state_guard.class_cache.insert(class.clone()) {
                    let mut escaped_class = String::with_capacity(class.len());
                    serialize_identifier(&class, &mut escaped_class).unwrap();
                    css_to_append.push('.');
                    css_to_append.push_str(&escaped_class);
                    css_to_append.push_str(" {\n  display: flex;\n}\n\n");
                    added_count += 1;
                }
            }
        }

        find_and_cache_duration = timer.elapsed();

        if added_count > 0 {
            let write_timer = Instant::now();
            let mut file = OpenOptions::new().append(true).open("style.css")?;
            file.write_all(css_to_append.as_bytes())?;
            css_write_duration = write_timer.elapsed();
        }
    }

    let total_duration = total_start.elapsed();

    if is_initial_run || added_count > 0 {
        let timing_details = format!(
            "Processed: {} appended | Total: {} (Parse & Append Prep: {}, CSS Append: {})",
            format!("{}", added_count).green(),
            format_duration(total_duration),
            format_duration(find_and_cache_duration),
            format_duration(css_write_duration)
        );
        println!("{}", timing_details);
    }

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