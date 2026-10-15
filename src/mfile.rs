//! Reads an mdeo model file (`.m`) into the model data the platform would send ([`WireModel`]):
//! `name : Class { attr = value }` objects and `a.feature -- b` links.
//!
//! Values: strings, numbers (a literal with a `.` or exponent is a double, else an int), `true`
//! and `false`, enum entries `Enum.ENTRY` (sent as the entry's name) and lists `[a, b]`. The
//! opposite ends of Palladio references are completed, as the platform sends them.

use crate::graph::Graph;
use palladio_sim_service::protocol::{WireInstance, WireModel, WireValue};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Punct(char),
    Arrow,
    Eof,
}

fn lex(text: &str) -> Result<Vec<(Tok, usize)>, String> {
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut line = 1;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                if b[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            let mut s = String::new();
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' && i + 1 < b.len() {
                    i += 1;
                    match b[i] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        'u' => {
                            let hex: String = b[i + 1..(i + 5).min(b.len())].iter().collect();
                            let code = u32::from_str_radix(&hex, 16).map_err(|_| format!("line {line}: bad \\u escape"))?;
                            s.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            i += 4;
                        }
                        other => s.push(other),
                    }
                } else {
                    s.push(b[i]);
                }
                i += 1;
            }
            i += 1;
            out.push((Tok::Str(s), line));
        } else if c == '`' {
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != '`' {
                i += 1;
            }
            out.push((Tok::Ident(b[start..i].iter().collect()), line));
            i += 1;
        } else if c.is_ascii_digit() || (c == '-' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_digit()
                    || b[i] == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                    || matches!(b[i], 'e' | 'E')
                    || (matches!(b[i], '+' | '-') && matches!(b[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            out.push((Tok::Num(b[start..i].iter().collect()), line));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push((Tok::Ident(b[start..i].iter().collect()), line));
        } else if c == '-' && b.get(i + 1) == Some(&'-') {
            out.push((Tok::Arrow, line));
            i += 2;
        } else if "{}[]:.,=".contains(c) {
            out.push((Tok::Punct(c), line));
            i += 1;
        } else {
            return Err(format!("line {line}: unexpected character {c:?}"));
        }
    }
    out.push((Tok::Eof, line));
    Ok(out)
}

/// Parses `.m` text. The result has both ends of every Palladio link.
pub fn parse(text: &str) -> Result<WireModel, String> {
    let toks = lex(text)?;
    let mut i = 0;
    let mut using = String::new();
    let mut insts: Vec<WireInstance> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut links: Vec<(String, String, String, usize)> = Vec::new();
    let err = |t: &(Tok, usize), what: &str| format!("line {}: expected {what}, found {:?}", t.1, t.0);
    let ident = |i: &mut usize| -> Result<String, String> {
        match &toks[*i] {
            (Tok::Ident(s), _) => {
                *i += 1;
                Ok(s.clone())
            }
            t => Err(err(t, "a name")),
        }
    };
    fn value(toks: &[(Tok, usize)], i: &mut usize) -> Result<Vec<WireValue>, String> {
        let t = &toks[*i];
        *i += 1;
        Ok(match &t.0 {
            Tok::Str(s) => vec![WireValue::String(s.clone())],
            Tok::Num(n) => {
                if n.contains(['.', 'e', 'E']) {
                    vec![WireValue::Double(n.parse().map_err(|_| format!("line {}: bad number {n}", t.1))?)]
                } else {
                    match n.parse::<i32>() {
                        Ok(v) => vec![WireValue::Int(v)],
                        Err(_) => vec![WireValue::Long(n.parse().map_err(|_| format!("line {}: bad number {n}", t.1))?)],
                    }
                }
            }
            Tok::Ident(s) if s == "true" => vec![WireValue::Bool(true)],
            Tok::Ident(s) if s == "false" => vec![WireValue::Bool(false)],
            Tok::Ident(_) => {
                // Enum.ENTRY
                if toks[*i].0 == Tok::Punct('.') {
                    *i += 1;
                    match &toks[*i].0 {
                        Tok::Ident(e) => {
                            *i += 1;
                            vec![WireValue::String(e.clone())]
                        }
                        _ => return Err(format!("line {}: expected an enum entry", t.1)),
                    }
                } else {
                    return Err(format!("line {}: unexpected name in a value", t.1));
                }
            }
            Tok::Punct('[') => {
                let mut items = Vec::new();
                while toks[*i].0 != Tok::Punct(']') {
                    items.extend(value(toks, i)?);
                    if toks[*i].0 == Tok::Punct(',') {
                        *i += 1;
                    }
                }
                *i += 1;
                items
            }
            other => return Err(format!("line {}: unexpected {other:?} in a value", t.1)),
        })
    }
    loop {
        match &toks[i].0 {
            Tok::Eof => break,
            Tok::Ident(s) if s == "using" => {
                i += 1;
                match &toks[i].0 {
                    Tok::Str(p) => using = p.clone(),
                    _ => return Err(err(&toks[i], "a metamodel path")),
                }
                i += 1;
            }
            Tok::Ident(_) => {
                let line = toks[i].1;
                let name = ident(&mut i)?;
                match toks[i].0 {
                    Tok::Punct(':') => {
                        i += 1;
                        let class = ident(&mut i)?;
                        if toks[i].0 != Tok::Punct('{') {
                            return Err(err(&toks[i], "{"));
                        }
                        i += 1;
                        let mut attributes = Vec::new();
                        while toks[i].0 != Tok::Punct('}') {
                            let key = ident(&mut i)?;
                            if toks[i].0 != Tok::Punct('=') {
                                return Err(err(&toks[i], "="));
                            }
                            i += 1;
                            attributes.push((key, value(&toks, &mut i)?));
                        }
                        i += 1;
                        if index.insert(name.clone(), insts.len()).is_some() {
                            return Err(format!("line {line}: {name} is declared twice"));
                        }
                        insts.push(WireInstance { name, class_name: class, attributes, references: Vec::new() });
                    }
                    Tok::Punct('.') => {
                        i += 1;
                        let feature = ident(&mut i)?;
                        if toks[i].0 != Tok::Arrow {
                            return Err(err(&toks[i], "--"));
                        }
                        i += 1;
                        let target = ident(&mut i)?;
                        links.push((name, feature, target, line));
                    }
                    _ => return Err(err(&toks[i], "':' or '.'")),
                }
            }
            _ => return Err(err(&toks[i], "an object or a link")),
        }
    }
    for (a, f, b, line) in links {
        let (Some(&ia), true) = (index.get(&a), index.contains_key(&b)) else {
            return Err(format!("line {line}: link {a}.{f} -- {b} names an unknown object"));
        };
        let x = &mut insts[ia];
        match x.references.iter_mut().find(|(k, _)| *k == f) {
            Some((_, v)) => v.push(b),
            None => x.references.push((f, vec![b])),
        }
    }
    let mut g = Graph::new(&WireModel { metamodel_path: using, instances: insts });
    g.complete_opposites();
    let mut model = g.to_wire(|_| true);
    // As the platform sends them: every reference list sorted by name.
    for x in &mut model.instances {
        for (_, v) in &mut x.references {
            v.sort();
            v.dedup();
        }
    }
    Ok(model)
}

pub fn read(path: &std::path::Path) -> Result<WireModel, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}
