use gix::Repository;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::fs;
use std::env;

fn get_timestamp() -> String {
    let now = SystemTime::now();
    let since_epoch = now.duration_since(UNIX_EPOCH).expect("Time went backwards");
    let secs = since_epoch.as_secs();
    let micros = since_epoch.subsec_micros();
    format!("{}.{:06}", secs, micros)
}

fn log_changed_lines(repo: &Repository, path: &Path, workdir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let start_time = SystemTime::now();
    
    // The path from `notify` is absolute, and our workdir is also canonicalized.
    let rel_path = path.strip_prefix(workdir)?;
    
    let current_content = fs::read_to_string(path).unwrap_or_default();
    let current_lines: Vec<&str> = current_content.lines().collect();
    
    let head_commit = repo.head_commit()?;
    let tree = head_commit.tree()?;
    
    let head_content = match tree.lookup_entry_by_path(rel_path) {
        Ok(Some(entry)) => {
            let blob = entry.object()?.into_blob();
            String::from_utf8_lossy(&blob.data).to_string()
        },
        _ => String::new(), // File is new or not in repo
    };
    let head_lines: Vec<&str> = head_content.lines().collect();

    if current_lines == head_lines {
        // No actual content change, so we can skip logging.
        return Ok(());
    }

    let mut changes_found = false;
    let max_len = std::cmp::max(head_lines.len(), current_lines.len());

    for i in 0..max_len {
        let old_line = head_lines.get(i);
        let new_line = current_lines.get(i);

        if old_line != new_line {
            if !changes_found {
                println!("\n[{}] Change detected in {:?}", get_timestamp(), rel_path);
                changes_found = true;
            }
            if let Some(line) = old_line {
                println!("- Line {}: {}", i + 1, line);
            }
            if let Some(line) = new_line {
                println!("+ Line {}: {}", i + 1, line);
            }
        }
    }


    if changes_found {
        let duration = start_time.elapsed()?;
        let millis = duration.as_millis();
        if millis > 0 {
            println!("Time to log change: {} ms", millis);
        } else {
            let micros = duration.as_micros();
            println!("Time to log change: {} qs", micros);
        }
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the canonical path of the current directory. This is the most reliable way.
    let workdir = env::current_dir()?.canonicalize()?;

    // Open the Git repository located in the workdir.
    let repo = gix::open(&workdir)?;

    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                tx.send(event).unwrap();
            }
        },
        Config::default().with_poll_interval(Duration::from_millis(200)),
    )?;

    // Construct the watch path from our canonical workdir.
    let watch_path = workdir.join("test");
    if !watch_path.exists() {
        eprintln!("Error: The 'test' directory does not exist in '{}'. Please create it.", workdir.display());
        return Ok(());
    }
    watcher.watch(&watch_path, RecursiveMode::Recursive)?;

    println!("Monitoring '{}' for changes. Press Ctrl+C to stop.", watch_path.display());

    loop {
        match rx.recv() {
            Ok(event) => {
                match event.kind {
                    // Only act on events that indicate a file's content has changed.
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in event.paths {
                            if path.is_file() {
                                 // Pass the canonical workdir to the logging function.
                                if let Err(e) = log_changed_lines(&repo, &path, &workdir) {
                                    eprintln!("Error processing file '{}': {}", path.display(), e);
                                }
                            }
                        }
                    },
                    _ => {
                        // Ignore all other events (Access, Remove, etc.) silently.
                    }
                }
            }
            Err(e) => eprintln!("Channel error: {:?}", e),
        }
    }
}
