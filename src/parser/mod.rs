use ahash::AHashSet;
use memchr::{memchr, memmem::Finder};

pub fn extract_classes_fast(html_bytes: &[u8], capacity_hint: usize) -> AHashSet<String> {
    let mut set = AHashSet::with_capacity(capacity_hint.max(64));
    let finder = Finder::new(b"class");
    let mut pos = 0usize;
    let n = html_bytes.len();

    while let Some(idx) = finder.find(&html_bytes[pos..]) {
        let start = pos + idx + 5;
        let mut i = start;
        while i < n && matches!(html_bytes[i], b' ' | b'\n' | b'\r' | b'\t') {
            i += 1;
        }
        if i >= n || html_bytes[i] != b'=' {
            pos = start;
            continue;
        }
        i += 1;
        while i < n && matches!(html_bytes[i], b' ' | b'\n' | b'\r' | b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let quote = html_bytes[i];
        if quote != b'"' && quote != b'\'' {
            pos = i;
            continue;
        }
        i += 1;
        let value_start = i;
        let rel_end = memchr(quote, &html_bytes[value_start..]);
        let value_end = match rel_end {
            Some(off) => value_start + off,
            None => break,
        };
        if let Ok(value_str) = std::str::from_utf8(&html_bytes[value_start..value_end]) {
            for cls in value_str.split_whitespace() {
                if !cls.is_empty() {
                    set.insert(cls.to_owned());
                }
            }
        }
        pos = value_end + 1;
    }

    set
}
