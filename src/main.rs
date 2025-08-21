use gix::{
    Repository, diff::{self, DiffLine},
    object::Kind,
    objs::{Blob, Commit},
    status::{self, Status},
};
use imara_diff::{diff, Algorithm, UnifiedDiffBuilder};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::Path,
    sync::mpsc::channel,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize Git repository
    let repo = Repository::open(".")?;
    
    // Set up notify watcher
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default().with_poll_interval(Duration::from_secs(1)))?;
    watcher.watch(Path::new("."), RecursiveMode::Recursive)?;
    
    println!("Watching for changes in Git repository...");
    
    // Process file change events
    for event in rx {
        let event = event?;
        let start = Instant::now();
        
        // Get status of changes (worktree vs index)
        let mut status = repo.status()?;
        let mut changes_found = false;
        
        // Collect diffs using gix
        for item in status.iter() {
            match item {
                Status::IndexWorktree { path, status, .. } => {
                    if status.is_changed() {
                        changes_found = true;
                        let path = path.to_string_lossy();
                        println!("\nChange detected in: {}", path);
                        
                        // Get old (index) and new (worktree) content
                        let old_content = get_index_content(&repo, &path)?;
                        let new_content = std::fs::read_to_string(&path)?;
                        
                        // Compute diff with imara-diff
                        let diff_output = diff(
                            &old_content.unwrap_or_default(),
                            &new_content,
                            Algorithm::Histogram,
                            UnifiedDiffBuilder::new(&path),
                        );
                        print!("{}", diff_output);
                    }
                }
                _ => continue,
            }
        }
        
        if !changes_found {
            println!("No relevant changes detected.");
        }
        
        let duration = start.elapsed();
        println!("Diff computation took: {} microseconds", duration.as_micros());
    }
    
    Ok(())
}

// Helper to get content from index for a given path
fn get_index_content(repo: &Repository, path: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let index = repo.index()?;
    let entry = index.entry_by_path(path);
    
    match entry {
        Some(entry) => {
            let blob_id = entry.id;
            let blob = repo.find_object(blob_id)?.try_into_blob()?;
            Ok(Some(String::from_utf8_lossy(&blob.data).to_string()))
        }
        None => Ok(None),
    }
}