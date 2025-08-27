use gix::Repository;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::fs;
use std::env;
use diff;

fn get_timestamp() -> String {
    let now = SystemTime::now();
    let since_epoch = now.duration_since(UNIX_EPOCH).expect("Time went backwards");
    let secs = since_epoch.as_secs();
    let micros = since_epoch.subsec_micros();
    format!("{}.{:06}", secs, micros)
}

fn log_changed_lines(repo: &Repository, path: &Path, workdir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let start_time = SystemTime::now();
    
    let rel_path = path.strip_prefix(workdir)?;
    
    let current_content = fs::read_to_string(path).unwrap_or_default();
    
    let head_commit = repo.head_commit()?;
    let tree = head_commit.tree()?;
    
    let head_content = match tree.lookup_entry_by_path(rel_path) {
        Ok(Some(entry)) => {
            let blob = entry.object()?.into_blob();
            String::from_utf8_lossy(&blob.data).to_string()
        },
        _ => String::new(),
    };

    if current_content == head_content {
        return Ok(());
    }

    let duration = start_time.elapsed()?;

    println!("\n[{}] Change detected in {:?}", get_timestamp(), rel_path);
    
    let millis = duration.as_millis();
    if millis > 0 {
        println!("Time to detect change: {} ms", millis);
    } else {
        let micros = duration.as_micros();
        println!("Time to detect change: {} qs", micros);
    }

    for diff in diff::lines(&head_content, &current_content) {
        match diff {
            diff::Result::Left(l)    => println!("- {}", l),
            diff::Result::Right(r)   => println!("+ {}", r),
            diff::Result::Both(_, _) => (),
        }
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workdir = env::current_dir()?.canonicalize()?;
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
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in event.paths {
                            if path.is_file() {
                                if let Err(e) = log_changed_lines(&repo, &path, &workdir) {
                                    eprintln!("Error processing file '{}': {}", path.display(), e);
                                }
                            }
                        }
                    },
                    _ => {}
                }
            }
            Err(e) => eprintln!("Channel error: {:?}", e),
        }
    }
}
