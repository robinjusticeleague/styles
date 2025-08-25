use cssparser::serialize_identifier;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};

pub fn write_css(
    css_file: &mut BufWriter<File>,
    classes_to_write: Vec<String>,
    append: bool,
) -> Result<(), std::io::Error> {
    if !append {
        css_file.get_mut().set_len(0)?;
        css_file.seek(SeekFrom::Start(0))?;
    } else {
        css_file.seek(SeekFrom::End(0))?;
    }

    let mut escaped = String::with_capacity(64);
    for class in classes_to_write {
        css_file.write_all(b".")?;
        escaped.clear();
        serialize_identifier(&class, &mut escaped).unwrap();
        css_file.write_all(escaped.as_bytes())?;
        css_file.write_all(b" {\n  display: flex;\n}\n")?;
    }
    css_file.flush()?;
    Ok(())
}
