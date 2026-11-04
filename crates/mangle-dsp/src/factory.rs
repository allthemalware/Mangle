//! Factory bank file formats: `sources.tsv`, `.mpreset` and `factory:<id>`
//! slot paths (see `FACTORY.md`). Pure text parsing; no IO, never called from
//! the audio thread.

use std::fmt::Write;

pub const CATEGORIES: [&str; 8] = [
    "Pads", "Textures", "Drones", "Plucks", "Keys", "Rhythmic", "Basses", "FX",
];

const SOURCES_HEADER: &str = "id\tname\tcategory\tkind\tfile\troot\tlicense\torigin";
const PRESET_MAGIC: &str = "mangle-preset 1";
const FACTORY_PREFIX: &str = "factory:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Generated,
    Recorded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub id: u16,
    pub name: String,
    pub category: String,
    pub kind: SourceKind,
    pub file: String,
    pub root: i16,
    pub license: String,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetFile {
    pub name: String,
    /// Index into [`CATEGORIES`].
    pub category: u8,
    pub author: String,
    pub slots: [Option<u16>; 6],
    /// `(host param id, normalised value 0..1)` in file order.
    pub params: Vec<(u32, f32)>,
}

fn parse_id(s: &str) -> Option<u16> {
    if s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

fn bad_file_path(f: &str) -> bool {
    f.is_empty()
        || f.starts_with('/')
        || f.contains('\\')
        || f.contains("..")
        || f.as_bytes().get(1) == Some(&b':')
}

pub fn parse_sources(text: &str) -> Result<Vec<SourceEntry>, String> {
    let mut lines = text.lines().enumerate();
    match lines.next() {
        Some((_, h)) if h.trim_end() == SOURCES_HEADER => {}
        _ => return Err("line 1: missing or malformed header".into()),
    }
    let mut out: Vec<SourceEntry> = Vec::new();
    for (i, raw) in lines {
        let n = i + 1;
        let line = raw.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        if cols.len() < 8 {
            return Err(format!(
                "line {n}: expected 8 columns, found {}",
                cols.len()
            ));
        }
        let id = parse_id(cols[0]).ok_or_else(|| format!("line {n}: bad id '{}'", cols[0]))?;
        if out.iter().any(|e| e.id == id) {
            return Err(format!("line {n}: duplicate id {id:04}"));
        }
        let kind = match cols[3] {
            "gen" => SourceKind::Generated,
            "rec" => SourceKind::Recorded,
            k => return Err(format!("line {n}: bad kind '{k}'")),
        };
        if bad_file_path(cols[4]) {
            return Err(format!("line {n}: bad file path '{}'", cols[4]));
        }
        let root: i16 = cols[5]
            .parse()
            .map_err(|_| format!("line {n}: bad root '{}'", cols[5]))?;
        if !(-1..=127).contains(&root) {
            return Err(format!("line {n}: root {root} out of range"));
        }
        out.push(SourceEntry {
            id,
            name: cols[1].to_string(),
            category: cols[2].to_string(),
            kind,
            file: cols[4].to_string(),
            root,
            license: cols[6].to_string(),
            origin: cols[7].to_string(),
        });
    }
    Ok(out)
}

fn check_text(n: usize, what: &str, s: &str) -> Result<(), String> {
    if s.chars().any(char::is_control) {
        Err(format!("line {n}: control character in {what}"))
    } else {
        Ok(())
    }
}

pub fn parse_preset(text: &str) -> Result<PresetFile, String> {
    let mut seen_magic = false;
    let mut name: Option<String> = None;
    let mut category: Option<u8> = None;
    let mut author = String::new();
    let mut slots = [None; 6];
    let mut params = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !seen_magic {
            if line != PRESET_MAGIC {
                return Err(format!("line {n}: expected '{PRESET_MAGIC}'"));
            }
            seen_magic = true;
            continue;
        }
        let (key, rest) = match line.split_once(char::is_whitespace) {
            Some((k, r)) => (k, r.trim()),
            None => (line, ""),
        };
        match key {
            "name" => {
                check_text(n, "name", rest)?;
                if rest.is_empty() {
                    return Err(format!("line {n}: empty name"));
                }
                name = Some(rest.to_string());
            }
            "category" => {
                let idx = CATEGORIES
                    .iter()
                    .position(|c| *c == rest)
                    .ok_or_else(|| format!("line {n}: unknown category '{rest}'"))?;
                category = Some(idx as u8);
            }
            "author" => {
                check_text(n, "author", rest)?;
                author = rest.to_string();
            }
            "slot" => {
                let mut it = rest.split_whitespace();
                let (Some(s), Some(id), None) = (it.next(), it.next(), it.next()) else {
                    return Err(format!("line {n}: expected 'slot <0-5> <id>'"));
                };
                let s: usize = s
                    .parse()
                    .ok()
                    .filter(|s| *s < 6)
                    .ok_or_else(|| format!("line {n}: bad slot '{s}'"))?;
                let id = parse_id(id).ok_or_else(|| format!("line {n}: bad source id '{id}'"))?;
                if slots[s].is_some() {
                    return Err(format!("line {n}: slot {s} given twice"));
                }
                slots[s] = Some(id);
            }
            "p" => {
                let mut it = rest.split_whitespace();
                let (Some(id), Some(v), None) = (it.next(), it.next(), it.next()) else {
                    return Err(format!("line {n}: expected 'p <hex id> <value>'"));
                };
                if id.len() != 6 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("line {n}: bad param id '{id}'"));
                }
                let id = u32::from_str_radix(id, 16).map_err(|_| format!("line {n}: bad id"))?;
                let v: f32 = v
                    .parse()
                    .map_err(|_| format!("line {n}: bad value '{v}'"))?;
                if !v.is_finite() {
                    return Err(format!("line {n}: non-finite value"));
                }
                params.push((id, v.clamp(0.0, 1.0)));
            }
            k => return Err(format!("line {n}: unknown key '{k}'")),
        }
    }
    if !seen_magic {
        return Err("line 1: missing preset header".into());
    }
    let last = text.lines().count().max(1);
    Ok(PresetFile {
        name: name.ok_or_else(|| format!("line {last}: missing name"))?,
        category: category.ok_or_else(|| format!("line {last}: missing category"))?,
        author,
        slots,
        params,
    })
}

pub fn write_preset(p: &PresetFile) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{PRESET_MAGIC}");
    let _ = writeln!(s, "name {}", p.name);
    let _ = writeln!(
        s,
        "category {}",
        CATEGORIES[(p.category as usize).min(CATEGORIES.len() - 1)]
    );
    if !p.author.is_empty() {
        let _ = writeln!(s, "author {}", p.author);
    }
    for (i, slot) in p.slots.iter().enumerate() {
        if let Some(id) = slot {
            let _ = writeln!(s, "slot {i} {id:04}");
        }
    }
    for (id, v) in &p.params {
        let _ = writeln!(s, "p {id:06x} {}", v.clamp(0.0, 1.0));
    }
    s
}

pub fn factory_path(id: u16) -> String {
    format!("{FACTORY_PREFIX}{id:04}")
}

pub fn parse_factory_path(s: &str) -> Option<u16> {
    parse_id(s.strip_prefix(FACTORY_PREFIX)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_crlf_and_spaces() {
        let t = format!("{SOURCES_HEADER}\r\n 0007 \t A \tc\t gen \tf.wav\t60\tg\to\r\n");
        let v = parse_sources(&t).unwrap();
        assert_eq!(v[0].id, 7);
        assert_eq!(v[0].name, "A");
        assert_eq!(v[0].origin, "o");
    }

    #[test]
    fn sources_error_has_line() {
        let t = format!("{SOURCES_HEADER}\n\n0001\tA\tc\tgen\t/abs\t60\tg\to\n");
        assert!(parse_sources(&t).unwrap_err().starts_with("line 3"));
        let t = format!("{SOURCES_HEADER}\n0001\tA\tc\tgen\ta\\b\t60\tg\to\n");
        assert!(parse_sources(&t).is_err());
    }

    #[test]
    fn preset_crlf_and_spaces() {
        let p = parse_preset(
            "mangle-preset 1\r\nname   Big  Sky \r\ncategory  FX\r\np 000001   0.5\r\n",
        )
        .unwrap();
        assert_eq!(p.name, "Big  Sky");
        assert_eq!(p.author, "");
        assert_eq!(p.params, vec![(1, 0.5)]);
    }

    #[test]
    fn preset_rejects_tab_in_name() {
        assert!(parse_preset("mangle-preset 1\nname a\tb\ncategory FX\n").is_err());
    }

    #[test]
    fn preset_rejects_infinity_and_error_has_line() {
        let e = parse_preset("mangle-preset 1\nname x\ncategory FX\np 000001 inf\n").unwrap_err();
        assert!(e.starts_with("line 4"));
    }

    #[test]
    fn factory_path_rejects_junk() {
        assert_eq!(parse_factory_path("factory:42"), None);
        assert_eq!(parse_factory_path("factory:00420"), None);
    }
}
