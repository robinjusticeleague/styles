use ahash::{AHashMap, AHashSet};
use memmap2::MmapMut;
use rayon::prelude::*;
use std::fs::OpenOptions;
use std::io::{self, Write, BufWriter};
use std::path::Path;
use std::sync::LazyLock;

// Precompute tables lazily at runtime
static WS: LazyLock<[bool; 256]> = LazyLock::new(make_ws_table);
static QT: LazyLock<[bool; 256]> = LazyLock::new(make_quote_table);

#[inline(always)]
fn make_ws_table() -> [bool; 256] {
    let mut ws = [false; 256];
    ws[b' ' as usize] = true;
    ws[b'\n' as usize] = true;
    ws[b'\r' as usize] = true;
    ws[b'\t' as usize] = true;
    ws
}

#[inline(always)]
fn make_quote_table() -> [bool; 256] {
    let mut qt = [false; 256];
    qt[b'"' as usize] = true;
    qt[b'\'' as usize] = true;
    qt
}

#[inline(always)]
fn is_ws(b: u8) -> bool { WS[b as usize] }

#[inline(always)]
fn is_quote(b: u8) -> bool { QT[b as usize] }

#[inline(always)]
fn eq5(bytes: &[u8], i: usize, pat: &[u8; 5]) -> bool {
    let j = i + 5;
    j <= bytes.len() && &bytes[i..j] == pat
}

#[inline(always)]
fn binary_insert_sorted<'a>(v: &mut Vec<&'a str>, item: &'a str) {
    match v.binary_search(&item) {
        Ok(_) => {}
        Err(pos) => v.insert(pos, item),
    }
}

pub fn collect_classes<'a>(html: &'a [u8]) -> Vec<&'a str> {
    let mut seen: AHashSet<&'a str> = AHashSet::with_capacity((html.len() / 96).max(32));
    let mut out: Vec<&'a str> = Vec::with_capacity((html.len() / 96).max(32));

    let mut i = 0usize;
    let n = html.len();
    const CLASS: &[u8; 5] = b"class";

    while i + 5 <= n {
        if html[i] != b'c' || !eq5(html, i, CLASS) {
            i += 1;
            while i < n && html[i] != b'c' { i += 1; }
            continue;
        }
        i += 5;

        while i < n && is_ws(html[i]) { i += 1; }
        if i >= n || html[i] != b'=' { continue; }
        i += 1;

        while i < n && is_ws(html[i]) { i += 1; }
        if i >= n || !is_quote(html[i]) { continue; }
        let quote = html[i];
        i += 1;

        while i < n && html[i] != quote {
            while i < n && html[i] != quote && is_ws(html[i]) { i += 1; }
            if i >= n || html[i] == quote { break; }
            let start = i;
            while i < n && html[i] != quote && !is_ws(html[i]) { i += 1; }
            let end = i;
            if end > start {
                let cls = unsafe { std::str::from_utf8_unchecked(&html[start..end]) };
                if seen.insert(cls) {
                    binary_insert_sorted(&mut out, cls);
                }
            }
        }
        if i < n && html[i] == quote { i += 1; }
    }

    out
}

pub fn build_css_parallel(keys: &[&str], cache: &AHashMap<String, String>) -> Vec<u8> {
    const CHUNK: usize = 256;

    let lengths: Vec<usize> = keys.par_iter()
        .map(|k| cache.get(*k).map(|s| s.len()).unwrap_or(0))
        .collect();
    let total_len: usize = lengths.iter().sum::<usize>()
        + (if keys.is_empty() { 0 } else { (keys.len() - 1) * 2 });

    let parts: Vec<Vec<u8>> = keys.par_chunks(CHUNK)
        .map(|chunk| {
            let mut buf = Vec::with_capacity(
                chunk.iter()
                     .map(|k| cache.get(*k).map(|s| s.len()).unwrap_or(0))
                     .sum::<usize>()
                + (if chunk.is_empty() { 0 } else { (chunk.len() - 1) * 2 })
            );
            let mut first = true;
            for k in chunk {
                if let Some(body) = cache.get(*k) {
                    if !first { buf.extend_from_slice(b"\n\n"); }
                    first = false;
                    buf.extend_from_slice(body.as_bytes());
                }
            }
            buf
        })
        .collect();

    let mut out = Vec::with_capacity(total_len);
    let mut first = true;
    for mut part in parts {
        if part.is_empty() { continue; }
        if !first { out.extend_from_slice(b"\n\n"); }
        first = false;
        out.append(&mut part);
    }
    out
}

#[inline(always)]
fn write_bufwriter(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file = OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    let mut w = BufWriter::with_capacity(128 * 1024, file);
    w.write_all(bytes)?;
    w.flush()?;
    Ok(())
}

#[inline(always)]
fn write_mmap(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let len = bytes.len() as u64;
    let file = OpenOptions::new().read(true).write(true).create(true).open(path)?;
    file.set_len(len)?;
    let mut mmap: MmapMut = unsafe { MmapMut::map_mut(&file)? };
    mmap[..bytes.len()].copy_from_slice(bytes);
    mmap.flush()?;
    Ok(())
}

pub fn write_css_fast(path: &Path, bytes: &[u8]) -> io::Result<()> {
    const MMAP_CUTOFF: usize = 256 * 1024;
    if bytes.len() >= MMAP_CUTOFF {
        write_mmap(path, bytes)
    } else {
        write_bufwriter(path, bytes)
    }
}

pub fn process_html_and_write(
    html: &[u8],
    cache: &AHashMap<String, String>,
    out_path: &Path,
) -> io::Result<()> {
    let keys = collect_classes(html);
    let css = build_css_parallel(&keys, cache);
    write_css_fast(out_path, &css)
}

// Minimal main so `cargo run` works
fn main() -> io::Result<()> {
    let html = br#"<div class='a b c'></div>"#;
    let mut cache = AHashMap::new();
    cache.insert("a".into(), ".a{color:red;}".into());
    cache.insert("b".into(), ".b{margin:0;}".into());
    cache.insert("c".into(), ".c{padding:4px;}".into());
    process_html_and_write(html, &cache, Path::new("out.css"))
}
