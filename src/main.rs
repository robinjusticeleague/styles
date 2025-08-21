use gix::Repository;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::fs;

// Function to get current time with microseconds
fn get_timestamp() -> String {
    let now = SystemTime::now();
    let since_epoch = now.duration_since(UNIX_EPOCH).expect("Time went backwards");
    let secs = since_epoch.as_secs();
    let micros = since_epoch.subsec_micros();
    format!("{}.{:06}", secs, micros)
}

// Function to compute and log changed lines for a file
fn log_changed_lines(repo: &Repository, path: &Path) -> Result<(), gix::object::find::existing::Error> {
    let start_time = SystemTime::now();
    
    let workdir = repo.workdir().expect("Repository has no workdir");
    let rel_path = match path.strip_prefix(workdir) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => {
            eprintln!("[{}] Skipping file not in workdir: {:?}", get_timestamp(), path);
            return Ok(());
        }
    };
    
    // Get the current file content
    let current_content = fs::read_to_string(path).unwrap_or_default();
    let current_lines: Vec<&str> = current_content.lines().collect();
    
    // Get the content from the last commit (HEAD)
    let head_commit = repo.head_commit().expect("Failed to get HEAD commit");
    let tree = head_commit.tree().expect("Failed to get tree");
    let blob = match tree.lookup_entry_by_path(&rel_path)? {
        Some(entry) => match entry.object() {
            Ok(obj) => Some(obj.into_blob()),
            Err(e) => return Err(e),
        },
        None => None,
    };
    
    let head_content = blob
        .map(|b| String::from_utf8_lossy(&b.data).to_string())
        .unwrap_or_default();
    let head_lines: Vec<&str> = head_content.lines().collect();

    // Find changed lines
    let mut changes_found = false;
    for (i, (old_line, new_line)) in head_lines.iter().zip(current_lines.iter()).enumerate() {
        if old_line != new_line {
            if !changes_found {
                println!("[{}] Change detected in {:?}", get_timestamp(), rel_path);
                changes_found = true;
            }
            println!("Line {}: -{}", i + 1, old_line);
            println!("Line {}: +{}", i + 1, new_line);
        }
    }

    // Handle lines added or removed (if lengths differ)
    if head_lines.len() < current_lines.len() {
        if !changes_found {
            println!("[{}] Change detected in {:?}", get_timestamp(), rel_path);
            changes_found = true;
        }
        for (i, line) in current_lines[head_lines.len()..].iter().enumerate() {
            println!("Line {}: +{}", head_lines.len() + i + 1, line);
        }
    } else if head_lines.len() > current_lines.len() {
        if !changes_found {
            println!("[{}] Change detected in {:?}", get_timestamp(), rel_path);
            changes_found = true;
        }
        for (i, line) in head_lines[current_lines.len()..].iter().enumerate() {
            println!("Line {}: -{}", current_lines.len() + i + 1, line);
        }
    }

    // Calculate and log time taken
    if changes_found {
        let duration = start_time.elapsed().expect("Time went backwards");
        let micros = duration.as_secs() * 1_000_000 + duration.subsec_micros() as u64;
        println!("Time to log change: {} microseconds", micros);
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open the Git repository in the current directory
    let repo = gix::open(".")?;

    // Set up the notify watcher
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            let _ = tx.send(res);
        },
        Config::default().with_poll_interval(Duration::from_millis(100)),
    )?;

    // Watch the current directory recursively
    watcher.watch(Path::new("."), RecursiveMode::Recursive)?;

    println!("Monitoring Git repository for changes. Press Ctrl+C to stop.");

    // Main loop to process file system events
    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                for path in event.paths {
                    // Only process files that exist and are not git internal files
                    if path.exists() && !path.to_string_lossy().contains(".git/") {
                        if let Err(e) = log_changed_lines(&repo, &path) {
                            eprintln!("Error processing file {:?}: {:?}", path, e);
                        }
                    }
                }
            }
            Ok(Err(e)) => eprintln!("Watch error: {:?}", e),
            Err(e) => eprintln!("Channel error: {:?}", e),
        }
    }
}