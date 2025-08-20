use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use regex::Regex;
use std::collections::HashSet;
use std::error::Error;
use std::fs::{read_to_string, write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let file_path = "index.html";
    let output_path = "dummy.css";

    let (tx, rx): (Sender<DebounceEventResult>, Receiver<DebounceEventResult>) = channel();

    let mut debouncer = new_debouncer(Duration::from_millis(200), None, tx)?;

    debouncer.watch(Path::new(file_path), RecursiveMode::NonRecursive)?;

    let mut previous_classes: HashSet<String> = HashSet::new();

    // Compile regex for class names in HTML (class attributes or CSS selectors in <style>)
    let re = Regex::new(r#"(?:class\s*=\s*["'](.*?)["']|\.([_a-zA-Z0-9-]+))"#)?;

    loop {
        match rx.recv() {
            Ok(Ok(events)) => {
                for event in events {
                    // Check if the event is a modify event and involves index.html
                    if matches!(event.kind, EventKind::Modify(_))
                        && event.paths.iter().any(|p| p.to_str() == Some(file_path))
                    {
                        let start = Instant::now();

                        let content = read_to_string(file_path)?;

                        let mut new_classes: HashSet<String> = HashSet::new();

                        // Extract classes from class attributes and CSS selectors
                        for cap in re.captures_iter(&content) {
                            // Check class attribute (group 1)
                            if let Some(cls_match) = cap.get(1) {
                                // Split multiple classes in class="..."
                                for cls in cls_match.as_str().split_whitespace() {
                                    new_classes.insert(cls.to_string());
                                }
                            }
                            // Check CSS selectors (group 2)
                            if let Some(cls_match) = cap.get(2) {
                                new_classes.insert(cls_match.as_str().to_string());
                            }
                        }

                        let added = &new_classes - &previous_classes;
                        let removed = &previous_classes - &new_classes;

                        println!("Added classes: {:?}", added);
                        println!("Removed classes: {:?}", removed);

                        let mut dummy_css = String::new();
                        for cls in &new_classes {
                            dummy_css.push_str(&format!(".{} {{ display: flex; }}\n", cls));
                        }

                        write(output_path, dummy_css)?;

                        previous_classes = new_classes;

                        let elapsed = start.elapsed().as_micros();
                        println!("Processing time: {} μs", elapsed);
                    }
                }
            }
            Ok(Err(errors)) => {
                for err in errors {
                    println!("Watcher error: {:?}", err);
                }
            }
            Err(e) => {
                println!("Channel error: {:?}", e);
                break Ok(());
            }
        }
    }
}