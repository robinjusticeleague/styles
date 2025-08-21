use gix::{Repository, progress::Discard};
use gix::status::index_worktree::Item;
use imara_diff::{Diff, Algorithm, intern::InternedInput};
use imara_diff::unified::{BasicLineDiffPrinter, UnifiedDiffConfig};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::Path,
    sync::mpsc::channel,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize Git repository
    let repo = gix::open(".")?;
    
    // Set up notify watcher
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, Config::default().with_poll_interval(Duration::from_secs(1)))?;
    watcher.watch(Path::new("./index.html"), RecursiveMode::Recursive)?;
    
    println!("Watching for changes in Git repository...");
    
    // Process file change events
    for event in rx {
        let event = event?;
        let start = Instant::now();
        
        // Get status of changes (worktree vs index)
        let platform = repo.status(Discard)?;
        let mut iter = platform.into_index_worktree_iter(None)?;
        let mut changes_found = false;
        
        // Collect diffs using gix
        for item_result in &mut iter {
            let item = item_result?;
            if item.status.is_changed() {
                changes_found = true;
                let path = item.rela_path.to_string_lossy();
                println!("\nChange detected in: {}", path);
                
                // Get old (index) and new (worktree) content
                let old_content = get_index_content(&repo, &path)?;
                let new_content = std::fs::read_to_string(&path)?;
                
                // Compute diff with imara-diff
                let old_str = old_content.unwrap_or_default();
                let new_str = new_content.as_str();
                let input = InternedInput::new(old_str.as_str(), new_str);
                let mut diff = Diff::compute(Algorithm::Histogram, &input);
                diff.postprocess_lines(&input);
                let diff_output = diff.unified_diff(
                    &BasicLineDiffPrinter(&input.interner),
                    UnifiedDiffConfig::default(),
                    &input,
                ).to_string();
                // Manually add file headers to mimic unified diff with path
                print!("--- a/{}\n+++ b/{}\n{}", path, path, diff_output);
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
    let entry = index.entry_by_path(path.into());
    
    match entry {
        Some(entry) => {
            let blob_id = entry.id;
            let blob = repo.find_object(blob_id)?.try_into_blob()?;
            Ok(Some(String::from_utf8_lossy(&blob.data).to_string()))
        }
        None => Ok(None),
    }
}