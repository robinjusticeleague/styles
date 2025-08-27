use crate::{generator, parser::extract_classes_fast, telemetry::format_duration};
use ahash::{AHashSet, AHasher};
use colored::Colorize;
use std::fs::File;
use std::hash::Hasher;
use std::io::BufWriter;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct AppState {
    pub html_hash: u64,
    pub class_cache: AHashSet<String>,
    pub css_file: BufWriter<File>,
}

pub fn rebuild_styles(
    state: Arc<Mutex<AppState>>,
    is_initial_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_start = Instant::now();

    let read_timer = Instant::now();
    let html_bytes = std::fs::read("playgrounds/html/index.html")?;
    let read_duration = read_timer.elapsed();

    let hash_timer = Instant::now();
    let new_html_hash = {
        let mut hasher = AHasher::default();
        hasher.write(&html_bytes);
        hasher.finish()
    };
    let hash_duration = hash_timer.elapsed();

    {
        let state_guard = state.lock().unwrap();
        if !is_initial_run && state_guard.html_hash == new_html_hash {
            return Ok(());
        }
    }

    let parse_timer = Instant::now();
    let prev_len_hint = { state.lock().unwrap().class_cache.len() };
    let all_classes = extract_classes_fast(&html_bytes, prev_len_hint.next_power_of_two());
    let parse_extract_duration = parse_timer.elapsed();

    {
        let state_guard = state.lock().unwrap();
        if all_classes.is_empty() && !state_guard.class_cache.is_empty() {
            return Ok(());
        }
    }

    let diff_timer = Instant::now();
    let (added, removed, old_hash_just_for_info) = {
        let state_guard = state.lock().unwrap();
        let old = &state_guard.class_cache;
        let added: Vec<String> = all_classes.difference(old).cloned().collect();
        let removed: Vec<String> = old.difference(&all_classes).cloned().collect();
        (added, removed, state_guard.html_hash)
    };
    let diff_duration = diff_timer.elapsed();

    if added.is_empty() && removed.is_empty() {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;
        return Ok(());
    }

    let cache_update_timer = Instant::now();
    {
        let mut state_guard = state.lock().unwrap();
        state_guard.html_hash = new_html_hash;
        state_guard.class_cache = all_classes.clone();
    }
    let cache_update_duration = cache_update_timer.elapsed();

    let css_write_timer = Instant::now();
    {
        let mut state_guard = state.lock().unwrap();

        if !removed.is_empty() {
            let classes_to_write: Vec<String> = state_guard.class_cache.iter().cloned().collect();
            generator::write_css(&mut state_guard.css_file, classes_to_write, false)?;
        } else {
            generator::write_css(&mut state_guard.css_file, added.clone(), true)?;
        }
    }
    let css_write_duration = css_write_timer.elapsed();

    println!(
        "Processed: {} added, {} removed (prev hash: {:x}) | (Total: {} -> Read: {}, Hash: {}, Parse: {}, Diff: {}, Cache: {}, Write: {})",
        format!("{}", added.len()).green(),
        format!("{}", removed.len()).red(),
        old_hash_just_for_info,
        format_duration(total_start.elapsed()),
        format_duration(read_duration),
        format_duration(hash_duration),
        format_duration(parse_extract_duration),
        format_duration(diff_duration),
        format_duration(cache_update_duration),
        format_duration(css_write_duration)
    );

    Ok(())
}
