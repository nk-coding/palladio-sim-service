//! Writes a [`WireModel`] of the Palladio metamodel as an mdeo model file (`.m`).

use crate::mm::ident;
use crate::protocol::{WireModel, WireValue};
use crate::schema::{AttrType, PropKind, Schema};
use std::collections::HashSet;
use std::fmt::Write;

/// The `.m` text of `model`, importing the metamodel from `metamodel_import` (e.g.
/// `"../palladio.mm"`).
pub fn write(s: &Schema, model: &WireModel, metamodel_import: &str) -> Result<String, String> {
    // Of two opposite references, links are written from the first end only.
    let second_ends: HashSet<usize> =
        s.assocs.iter().filter(|a| a.operator == "<-->").filter_map(|a| a.target_prop).collect();

    let mut out = String::new();
    let _ = writeln!(out, "using {}\n", string(metamodel_import));
    let mut links = String::new();
    for inst in &model.instances {
        let ci = s.class(&inst.class_name).ok_or_else(|| format!("unknown class {}", inst.class_name))?;
        let _ = writeln!(out, "{} : {} {{", ident(&inst.name), ident(&inst.class_name));
        for (name, values) in &inst.attributes {
            let pi = s.prop(ci, name).ok_or_else(|| format!("{}.{name} is not a property", inst.class_name))?;
            let p = &s.props[pi];
            let enum_name = match &p.kind {
                PropKind::Attr { ty: AttrType::Enum(e) } | PropKind::Library { lib_enum: e } => Some(&s.enums[*e].name),
                _ => None,
            };
            let lit = |v: &WireValue| -> Result<String, String> {
                Ok(match (v, enum_name) {
                    (WireValue::String(entry), Some(e)) => format!("{}.{}", ident(e), ident(entry)),
                    (WireValue::String(x), None) => string(x),
                    (WireValue::Int(i), _) => i.to_string(),
                    (WireValue::Long(i), _) => i.to_string(),
                    (WireValue::Double(d), _) => number(*d)?,
                    (WireValue::Float(d), _) => number(*d as f64)?,
                    (WireValue::Bool(b), _) => b.to_string(),
                    (other, _) => return Err(format!("{}.{name}: cannot write {other:?}", inst.name)),
                })
            };
            if values.is_empty() && !p.many {
                continue;
            }
            let text = if p.many {
                let items: Vec<String> = values.iter().map(lit).collect::<Result<_, _>>()?;
                format!("[{}]", items.join(", "))
            } else {
                lit(&values[0])?
            };
            let _ = writeln!(out, "    {} = {text}", ident(name));
        }
        out.push_str("}\n\n");

        let mut any = false;
        for (name, targets) in &inst.references {
            let pi = s.prop(ci, name).ok_or_else(|| format!("{}.{name} is not a property", inst.class_name))?;
            if matches!(s.props[pi].kind, PropKind::Container { .. }) || second_ends.contains(&pi) {
                continue;
            }
            for t in targets {
                let _ = writeln!(links, "{}.{} -- {}", ident(&inst.name), ident(name), ident(t));
                any = true;
            }
        }
        if any {
            links.push('\n');
        }
    }
    out.push_str(&links);
    while out.ends_with("\n\n") {
        out.pop();
    }
    Ok(out)
}

/// A string literal of the mdeo languages.
fn string(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A double as the model language reads it: decimal, never with an exponent.
fn number(d: f64) -> Result<String, String> {
    if !d.is_finite() {
        return Err(format!("{d} cannot be written in a model"));
    }
    let mut s = format!("{d}");
    if !s.contains('.') {
        s.push_str(".0");
    }
    Ok(s)
}
