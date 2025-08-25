use crate::core::{rebuild_styles, AppState};
use colored::Colorize;
use notify::{RecursiveMode};
use notify_debouncer_full::new_debouncer;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub fn start(state: Arc<Mutex<AppState>>) -> Result<(), Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel();
    let mut debouncer = new_debouncer(Duration::from_millis(1), None, tx)?;

    debouncer
        .watch(Path::new("index.html"), RecursiveMode::NonRecursive)?;
        
    println!("{}", "Watching index.html for changes...".cyan());

    for res in rx {
        match res {
            Ok(_) => {
                if let Err(e) = rebuild_styles(state.clone(), false) {
                    eprintln!("{} {}", "Error rebuilding styles:".red(), e);
                }
            }
            Err(e) => eprintln!("{} {:?}", "Watch error:".red(), e),
        }
    }

    Ok(())
}
