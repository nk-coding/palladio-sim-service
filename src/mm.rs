//! Writes the [`Schema`] as an mdeo metamodel (`.mm`).

use crate::schema::{AttrType, PropKind, Schema};
use std::fmt::Write;

/// Words the metamodel, model or script languages reserve; a name that is one is written quoted.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "break", "class", "continue", "else", "enum", "extends", "false", "for", "fun", "if",
    "import", "in", "is", "null", "record", "return", "true", "using", "val", "var", "void", "while",
];

/// A name as the mdeo languages read it: quoted with backticks unless it is a plain identifier.
pub fn ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_alphabetic())
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&name);
    if plain { name.to_string() } else { format!("`{name}`") }
}

/// The files of the metamodel beside the aggregate `palladio.mm`, in import order (a file may
/// import only files before it): name, heading and contents.
pub const FILES: &[(&str, &str, &str)] = &[
    ("repository.mm", "Repository", "component types, interfaces, signatures, roles and data types"),
    ("architecture.mm", "Architecture", "components, their behaviour (SEFFs), the system and the usage model"),
    ("deployment.mm", "Deployment", "the resource environment and the allocation of the system onto it"),
    ("measurement.mm", "Measurement", "monitors, measurement specifications and measuring points"),
];

/// The directory of [`FILES`], relative to the aggregate.
pub const DIR: &str = "pcm";

/// The package groups of a file, in the order the file lists them.
const SECTIONS: &[(&str, &[&str])] = &[
    ("Core: named elements and roles", &["pcm.core", "pcm.core.entity"]),
    ("Repository: components, interfaces, signatures, data types", &["pcm.repository", "pcm.subsystem"]),
    ("Behaviour (SEFFs): actions, resource demands, calls", &["pcm.seff", "pcm.seff.seff_performance", "pcm.seff.seff_reliability"]),
    ("Parameters: variable usages and characterisations", &["pcm.parameter"]),
    ("Composition and system: assembly contexts and connectors", &["pcm.core.composition", "pcm.system"]),
    ("Resource environment: containers, processing and linking resources", &["pcm.resourceenvironment"]),
    ("Allocation", &["pcm.allocation"]),
    ("Usage model: scenarios, workloads, user behaviour", &["pcm.usagemodel"]),
    (
        "Monitoring: monitors, measurement specifications and measuring points",
        &["monitorrepository", "models.measuringpoint", "pcmmeasuringpoint", "simulizarmeasuringpoint"],
    ),
];

/// Which of [`FILES`] declares every class, enum and association.
pub struct Layout {
    pub class_file: Vec<usize>,
    pub enum_file: Vec<usize>,
    pub assoc_file: Vec<usize>,
}

/// The file a class of a PCM package starts in.
fn package_file(package: &str) -> usize {
    match package {
        "pcm.core" | "pcm.core.entity" | "pcm.repository" => 0,
        "pcm.seff" | "pcm.seff.seff_performance" | "pcm.seff.seff_reliability" | "pcm.parameter"
        | "pcm.core.composition" | "pcm.system" | "pcm.subsystem" | "pcm.usagemodel" => 1,
        "pcm.resourceenvironment" | "pcm.allocation" => 2,
        _ => 3,
    }
}

/// Distributes the metamodel over [`FILES`], as the metamodel language requires:
/// - a file sees the classes and enums of the files it imports, and imports only earlier files;
/// - a file may name an association end only on a class of its own. An association named at both
///   ends (every containment, every pair of opposites) therefore lives in the file of both of its
///   classes, and a one-way reference in the file of the referring class.
///
/// Every class starts in the file of its PCM package and moves to a later file where a superclass,
/// a referenced class or the other end of an association requires it.
pub fn layout(s: &Schema) -> Layout {
    let mut class_file: Vec<usize> =
        s.classes.iter().map(|c| package_file(c.eclass.def().package.def().name)).collect();
    loop {
        let mut changed = false;
        let mut raise = |ci: usize, to: usize, class_file: &mut Vec<usize>| {
            if class_file[ci] < to {
                class_file[ci] = to;
                changed = true;
            }
        };
        for ci in 0..s.classes.len() {
            for &sup in &s.classes[ci].supers {
                raise(ci, class_file[sup], &mut class_file);
            }
        }
        for a in &s.assocs {
            if a.target_prop.is_some() {
                let f = class_file[a.source].max(class_file[a.target]);
                raise(a.source, f, &mut class_file);
                raise(a.target, f, &mut class_file);
            } else {
                raise(a.source, class_file[a.target], &mut class_file);
            }
        }
        if !changed {
            break;
        }
    }
    let assoc_file = s.assocs.iter().map(|a| class_file[a.source]).collect();
    let enum_file = (0..s.enums.len())
        .map(|ei| {
            (0..s.classes.len())
                .filter(|&ci| s.classes[ci].own.iter().any(|&pi| enum_of(s, pi) == Some(ei)))
                .map(|ci| class_file[ci])
                .min()
                .unwrap_or(0)
        })
        .collect();
    Layout { class_file, enum_file, assoc_file }
}

/// The enum a property holds values of.
fn enum_of(s: &Schema, pi: usize) -> Option<usize> {
    match &s.props[pi].kind {
        PropKind::Attr { ty: AttrType::Enum(e) } => Some(*e),
        PropKind::Library { lib_enum } => Some(*lib_enum),
        _ => None,
    }
}

/// The files a file refers to: those declaring a superclass, an associated class or an enum of
/// one of its own.
fn references(s: &Schema, l: &Layout, file: usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for ci in (0..s.classes.len()).filter(|&ci| l.class_file[ci] == file) {
        out.extend(s.classes[ci].supers.iter().map(|&sup| l.class_file[sup]));
        out.extend(s.classes[ci].own.iter().filter_map(|&pi| enum_of(s, pi)).map(|e| l.enum_file[e]));
    }
    for (_, a) in s.assocs.iter().enumerate().filter(|(ai, _)| l.assoc_file[*ai] == file) {
        out.push(l.class_file[a.source]);
        out.push(l.class_file[a.target]);
    }
    out.sort_unstable();
    out.dedup();
    out.retain(|&f| f != file);
    out
}

/// The metamodel as files: the aggregate `palladio.mm` first, then [`FILES`] under [`DIR`], each
/// with its path relative to the aggregate.
pub fn write(s: &Schema) -> Vec<(String, String)> {
    let l = layout(s);
    let mut files = vec![("palladio.mm".to_string(), write_aggregate())];
    for (fi, (name, heading, contents)) in FILES.iter().enumerate() {
        files.push((format!("{DIR}/{name}"), write_file(s, &l, fi, heading, contents)));
    }
    files
}

fn write_aggregate() -> String {
    let mut out = String::from(
        "// The Palladio Component Model (PCM 5.2), as simulated by SimuLizar 5.2.2.\n\
         //\n\
         // Generated from the PCM Ecore metamodel by `palladio-sim-service gen-metamodel`: the\n\
         // subset of classes and features the simulator reads. Mapping rules:\n\
         // - classes keep their PCM names; features drop their EMF suffix\n\
         //   (`components__Repository` -> `components`);\n\
         // - an instance's name is its PCM id;\n\
         // - stochastic expressions (PCMRandomVariable) are strings holding the StoEx, e.g. \"Exp(2.0)\";\n\
         // - variable references are dot-separated names, e.g. \"items.NUMBER_OF_ELEMENTS\";\n\
         // - references into Palladio's library models (resource types, scheduling policies,\n\
         //   resource interfaces, metrics) are enums.\n\
         //\n\
         // Every root (Repository, System, ResourceEnvironment, Allocation, UsageModel,\n\
         // MonitorRepository, MeasuringPointRepository) becomes one PCM file for the simulator.\n\
         //\n\
         // This file only gathers the parts, which models and scripts use through it. Each part\n\
         // imports the parts before it:\n",
    );
    for (name, _, contents) in FILES {
        let _ = writeln!(out, "// - {DIR}/{name}: {contents}");
    }
    out.push('\n');
    for (name, _, _) in FILES {
        let _ = writeln!(out, "import \"./{DIR}/{name}\"");
    }
    out
}

fn write_file(s: &Schema, l: &Layout, file: usize, heading: &str, contents: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "// {heading}: {contents}.");
    out.push_str("//\n// Part of the Palladio metamodel, which ../palladio.mm gathers.\n");
    if file == 1 {
        out.push_str(
            "//\n\
             // These areas share one file because the metamodel language declares an association\n\
             // end with its class, and parameter usages (VariableUsage) are contained by component\n\
             // implementations, SEFF actions, assembly contexts and usage model calls alike.\n",
        );
    }
    let imports = references(s, l, file);
    if !imports.is_empty() {
        out.push('\n');
        for f in imports {
            let _ = writeln!(out, "import \"./{}\"", FILES[f].0);
        }
    }

    let enums: Vec<usize> = (0..s.enums.len()).filter(|&e| l.enum_file[e] == file).collect();
    if !enums.is_empty() {
        out.push_str("\n// ---------------------------------------------------------------------------------------\n");
        out.push_str("// Enums\n\n");
        for e in enums {
            write_enum(s, e, &mut out);
        }
    } else {
        out.push('\n');
    }

    for (title, packages) in SECTIONS {
        let mut members: Vec<usize> = (0..s.classes.len())
            .filter(|&ci| l.class_file[ci] == file && packages.contains(&s.classes[ci].eclass.def().package.def().name))
            .collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by(|&a, &b| s.classes[a].name.cmp(&s.classes[b].name));
        out.push_str("// ---------------------------------------------------------------------------------------\n");
        let _ = writeln!(out, "// {title}\n");
        for &ci in &members {
            write_class(s, ci, &mut out);
        }
        for (ai, a) in s.assocs.iter().enumerate() {
            if l.assoc_file[ai] != file || !members.contains(&a.source) {
                continue;
            }
            let end = |ci: usize, p: Option<usize>| match p {
                Some(pi) => {
                    let pr = &s.props[pi];
                    let m = if pr.many { "[*]" } else { "[?]" };
                    format!("{}.{}{m}", ident(&s.classes[ci].name), ident(&pr.name))
                }
                None => ident(&s.classes[ci].name),
            };
            let _ = writeln!(out, "{} {} {}", end(a.source, a.source_prop), a.operator, end(a.target, a.target_prop));
        }
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn write_enum(s: &Schema, e: usize, out: &mut String) {
    let e = &s.enums[e];
    let what = match e.source {
        crate::schema::EnumSource::Ecore(_) => "",
        crate::schema::EnumSource::Library(_) => " // elements of Palladio's library models",
    };
    let _ = writeln!(out, "enum {} {{{what}", ident(&e.name));
    for n in &e.entries {
        let _ = writeln!(out, "    {}", ident(&n.name));
    }
    out.push_str("}\n\n");
}

fn write_class(s: &Schema, ci: usize, out: &mut String) {
    let cl = &s.classes[ci];
    let attrs: Vec<String> = cl
        .own
        .iter()
        .filter_map(|&pi| {
            let p = &s.props[pi];
            let ty = match &p.kind {
                PropKind::Attr { ty } => ty.mm_name(s).to_string(),
                PropKind::RandomVar | PropKind::NamedRef => AttrType::String.mm_name(s).to_string(),
                PropKind::Library { lib_enum } => s.enums[*lib_enum].name.clone(),
                _ => return None,
            };
            let m = if p.many { "[*]" } else { "[?]" };
            Some(format!("    {}: {}{m}", ident(&p.name), ident(&ty)))
        })
        .collect();
    let mut head = String::new();
    if cl.is_abstract {
        head.push_str("abstract ");
    }
    let _ = write!(head, "class {}", ident(&cl.name));
    if !cl.supers.is_empty() {
        let supers: Vec<String> = cl.supers.iter().map(|&x| ident(&s.classes[x].name)).collect();
        let _ = write!(head, " extends {}", supers.join(", "));
    }
    if attrs.is_empty() {
        let _ = writeln!(out, "{head} {{}}\n");
    } else {
        let _ = writeln!(out, "{head} {{\n{}\n}}\n", attrs.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::schema;

    /// The layout keeps every rule of the metamodel language, and so the files import each other
    /// without cycles.
    #[test]
    fn layout_is_valid() {
        let s = schema();
        let l = layout(s);
        for ci in 0..s.classes.len() {
            for &sup in &s.classes[ci].supers {
                assert!(l.class_file[sup] <= l.class_file[ci], "{} extends a later file's class", s.classes[ci].name);
            }
        }
        for (ai, a) in s.assocs.iter().enumerate() {
            let f = l.assoc_file[ai];
            assert_eq!(l.class_file[a.source], f, "association of {} is not with its source", s.classes[a.source].name);
            if a.target_prop.is_some() {
                assert_eq!(l.class_file[a.target], f, "named end of {} is in another file", s.classes[a.target].name);
            } else {
                assert!(l.class_file[a.target] <= f);
            }
        }
        for f in 0..FILES.len() {
            assert!(references(s, &l, f).iter().all(|&g| g < f), "{} imports a later file", FILES[f].0);
        }
        for ci in 0..s.classes.len() {
            for &pi in &s.classes[ci].own {
                if let Some(e) = enum_of(s, pi) {
                    assert!(l.enum_file[e] <= l.class_file[ci]);
                }
            }
        }
    }

    /// Only the classes the language's rules force out of their package's file move.
    #[test]
    fn moved_classes() {
        let s = schema();
        let l = layout(s);
        let mut moved: Vec<String> = (0..s.classes.len())
            .filter(|&ci| l.class_file[ci] != package_file(s.classes[ci].eclass.def().package.def().name))
            .map(|ci| format!("{} -> {}", s.classes[ci].name, FILES[l.class_file[ci]].0))
            .collect();
        moved.sort();
        assert_eq!(
            moved,
            [
                "BasicComponent -> architecture.mm",
                "CompositeComponent -> architecture.mm",
                "ImplementationComponentType -> architecture.mm",
                "PassiveResource -> architecture.mm",
            ]
        );
    }
}
