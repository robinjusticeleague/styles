use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use regex::Regex;
use std::collections::HashSet;
use std::error::Error;
use std::fs::{read_to_string, File};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use log::{info, error, debug};

fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logger with timestamps
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_micros()
        .init();

    info!("Starting file watcher for style.css");

    let file_path = "style.css";
    let output_path = "dummy.css";

    // Create style.css if it doesn't exist
    if !Path::new(file_path).exists() {
        info!("style.css does not exist, creating empty file");
        File::create(file_path)?;
        info!("style.css created successfully");
    }

    info!("Setting up debouncer for file: {}", file_path);
    let (tx, rx): (Sender<DebounceEventResult>, Receiver<DebounceEventResult>) = channel();

    let mut debouncer = new_debouncer(Duration::from_millis(200), None, tx)?;
    debouncer.watch(Path::new(file_path), RecursiveMode::NonRecursive)?;
    info!("Debouncer initialized, watching {}", file_path);

    let mut previous_classes: HashSet<String> = HashSet::new();

    // Compile regex for CSS class names
    info!("Compiling regex for class extraction");
    let re = Regex::new(r"\.([_a-zA-Z0-9-]+)")?;
    info!("Regex compiled successfully");

    loop {
        match rx.recv() {
            Ok(Ok(events)) => {
                for event in events {
                    if matches!(event.kind, EventKind::Modify(_))
                        && event.paths.iter().any(|p| p.to_str() == Some(file_path))
                    {
                        info!("Detected modification in {}", file_path);
                        let total_start = Instant::now();

                        // Read file
                        let read_start = Instant::now();
                        let content = match read_to_string(file_path) {
                            Ok(content) => content,
                            Err(e) => {
                                error!("Failed to read {}: {}", file_path, e);
                                continue;
                            }
                        };
                        let read_time = read_start.elapsed().as_micros();
                        debug!("File read completed in {} μs", read_time);

                        // Extract classes
                        let extract_start = Instant::now();
                        let mut new_classes: HashSet<String> = HashSet::new();
                        for cap in re.captures_iter(&content) {
                            if let Some(cls_match) = cap.get(1) {
                                new_classes.insert(cls_match.as_str().to_string());
                            }
                        }
                        let extract_time = extract_start.elapsed().as_micros();
                        debug!("Class extraction completed in {} μs ({} classes found)", extract_time, new_classes.len());

                        // Compute differences
                        let diff_start = Instant::now();
                        let added = &new_classes - &previous_classes;
                        let removed = &previous_classes - &new_classes;
                        let diff_time = diff_start.elapsed().as_micros();
                        debug!("Class diffing completed in {} μs ({} added, {} removed)", diff_time, added.len(), removed.len());

                        if !added.is_empty() {
                            info!("Added classes: {:?}", added);
                        }
                        if !removed.is_empty() {
                            info!("Removed classes: {:?}", removed);
                        }

                        // Generate dummy CSS
                        let generate_start = Instant::now();
                        let mut dummy_css = String::new();
                        for cls in &new_classes {
                            dummy_css.push_str(&format!(".{} {{ display: flex; }}\n", cls));
                        }
                        let generate_time = generate_start.elapsed().as_micros();
                        debug!("CSS generation completed in {} μs", generate_time);

                        // Write output
                        let write_start = Instant::now();
                        if let Err(e) = std::fs::write(output_path, &dummy_css) {
                            error!("Failed to write {}: {}", output_path, e);
                            continue;
                        }
                        let write_time = write_start.elapsed().as_micros();
                        debug!("File write completed in {} μs", write_time);

                        previous_classes = new_classes;

                        let total_time = total_start.elapsed().as_micros();
                        info!("Total processing time: {} μs (read: {}, extract: {}, diff: {}, generate: {}, write: {})",
                              total_time, read_time, extract_time, diff_time, generate_time, write_time);
                    }
                }
            }
            Ok(Err(errors)) => {
                for err in errors {
                    error!("Watcher error: {:?}", err);
                }
            }
            Err(e) => {
                error!("Channel error: {:?}", e);
                break Ok(());
            }
        }
    }
}