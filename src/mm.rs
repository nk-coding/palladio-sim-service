//! Writes the [`Schema`] as an mdeo metamodel (`.mm`).

use crate::schema::{AttrType, PropKind, Schema};
use simoxide_model::meta::prelude::*;
use std::fmt::Write;

/// Words the metamodel, model or script languages reserve; a name that is one is written quoted.
const KEYWORDS: &[&str] = &[
    "abstract", "as", "break", "class", "continue", "else", "enum", "extends", "false", "for", "fun", "if",
    "import", "in", "is", "null", "record", "return", "true", "using", "val", "var", "view", "void", "while",
];

/// A name as the mdeo languages read it: quoted with backticks unless it is a plain identifier.
pub fn ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_alphabetic())
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&name);
    if plain { name.to_string() } else { format!("`{name}`") }
}

/// The files of the metamodel in import order (a file may import only files before it): path
/// relative to the aggregate, and contents.
///
/// The base part holds what the rest of PCM builds on and imports nothing; everything else lives
/// in the aggregate `palladio.mm`, which models and scripts use. Views of the aggregate show the
/// metamodel by package group (see [`SECTIONS`]), whichever file a class lives in.
pub const FILES: &[(&str, &str)] = &[
    ("pcm/repository.mm", "component types, interfaces, signatures, roles and data types"),
    (AGGREGATE, "everything else, and a view of every package group"),
];

/// The file models and scripts use.
pub const AGGREGATE: &str = "palladio.mm";

/// The package groups: the view showing them, and their heading in the file that lists them.
const SECTIONS: &[(&str, &str, &[&str])] = &[
    ("Core", "Core: named elements and roles", &["pcm.core", "pcm.core.entity"]),
    ("Repository", "Repository: components, interfaces, signatures, data types", &["pcm.repository", "pcm.subsystem"]),
    (
        "Behaviour",
        "Behaviour (SEFFs): actions, resource demands, calls",
        &["pcm.seff", "pcm.seff.seff_performance", "pcm.seff.seff_reliability"],
    ),
    ("Parameters", "Parameters: variable usages and characterisations", &["pcm.parameter"]),
    ("Composition", "Composition and system: assembly contexts and connectors", &["pcm.core.composition", "pcm.system"]),
    (
        "ResourceEnvironment",
        "Resource environment: containers, processing and linking resources",
        &["pcm.resourceenvironment"],
    ),
    ("Allocation", "Allocation", &["pcm.allocation"]),
    ("UsageModel", "Usage model: scenarios, workloads, user behaviour", &["pcm.usagemodel"]),
    (
        "Monitoring",
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
        _ => 1,
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

/// The metamodel as files: the aggregate `palladio.mm` first, then the parts it imports, each
/// with its path relative to the aggregate.
pub fn write(s: &Schema) -> Vec<(String, String)> {
    let l = layout(s);
    (0..FILES.len()).rev().map(|fi| (FILES[fi].0.to_string(), write_file(s, &l, fi))).collect()
}

/// The path under which one file of [`FILES`] imports another.
fn import_path(from: usize, to: usize) -> String {
    let dir = |f: usize| FILES[f].0.rsplit_once('/').map_or("", |(d, _)| d);
    let name = FILES[to].0.rsplit('/').next().unwrap_or(FILES[to].0);
    match (dir(from), dir(to)) {
        (a, b) if a == b => format!("./{name}"),
        ("", b) => format!("./{b}/{name}"),
        (_, "") => format!("../{name}"),
        _ => format!("../{}", FILES[to].0),
    }
}

fn write_file(s: &Schema, l: &Layout, file: usize) -> String {
    let mut out = String::new();
    if FILES[file].0 == AGGREGATE {
        out.push_str(
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
             //\n",
        );
        let _ = writeln!(
            out,
            "// The {} live in\n\
             // {}, which this file imports; everything else lives here.\n\
             //\n\
             // BasicComponent, CompositeComponent, ImplementationComponentType and PassiveResource are\n\
             // repository classes, but live here: the metamodel language declares an association end with\n\
             // its class, and parameter usages (VariableUsage) are contained by component implementations,\n\
             // SEFF actions, assembly contexts and usage model calls alike. The views at the end show the\n\
             // metamodel by package group, wherever a class lives.",
            FILES[0].1,
            import_path(file, 0)
        );
    } else {
        let _ = writeln!(out, "// The Palladio metamodel's {}.", FILES[file].1);
        let _ = writeln!(
            out,
            "//\n// The base part of the metamodel, which {} imports.",
            import_path(file, FILES.len() - 1)
        );
    }
    let imports = references(s, l, file);
    if !imports.is_empty() {
        out.push('\n');
        for f in imports {
            let _ = writeln!(out, "import \"{}\"", import_path(file, f));
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

    for (_, title, packages) in SECTIONS {
        let mut members: Vec<usize> = (0..s.classes.len())
            .filter(|&ci| l.class_file[ci] == file && packages.contains(&package_of(s, ci)))
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
    if FILES[file].0 == AGGREGATE {
        write_views(s, &mut out);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The PCM package a class belongs to.
fn package_of(s: &Schema, ci: usize) -> &'static str {
    s.classes[ci].eclass.def().package.def().name
}

/// The classes a view shows: those of its package group, wherever they live.
fn view_members(s: &Schema, packages: &[&str]) -> Vec<usize> {
    let mut members: Vec<usize> = (0..s.classes.len()).filter(|&ci| packages.contains(&package_of(s, ci))).collect();
    members.sort_by(|&a, &b| s.classes[a].name.cmp(&s.classes[b].name));
    members
}

/// Writes one view per package group. A view lists the group's classes; the associations and
/// inheritance between them come with them.
fn write_views(s: &Schema, out: &mut String) {
    out.push_str("// ---------------------------------------------------------------------------------------\n");
    out.push_str("// Views: the metamodel by package group\n\n");
    for (view, _, packages) in SECTIONS {
        let members = view_members(s, packages);
        if members.is_empty() {
            continue;
        }
        let _ = writeln!(out, "view {} {{", ident(view));
        for ci in members {
            let _ = writeln!(out, "    {}", ident(&s.classes[ci].name));
        }
        out.push_str("}\n\n");
    }
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
        assert!(references(s, &l, 0).is_empty(), "the base part imports another file");
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
                "BasicComponent -> palladio.mm",
                "CompositeComponent -> palladio.mm",
                "ImplementationComponentType -> palladio.mm",
                "PassiveResource -> palladio.mm",
            ]
        );
    }

    /// Every class is written, and shown by exactly one view.
    #[test]
    fn views_cover_every_class_once() {
        let s = schema();
        let mut shown = vec![0; s.classes.len()];
        for (_, _, packages) in SECTIONS {
            for ci in view_members(s, packages) {
                shown[ci] += 1;
            }
        }
        let wrong: Vec<&str> = (0..s.classes.len()).filter(|&ci| shown[ci] != 1).map(|ci| s.classes[ci].name.as_str()).collect();
        assert!(wrong.is_empty(), "not shown by exactly one view: {wrong:?}");
    }

    /// The aggregate imports the base part by its path, and the base part imports nothing.
    #[test]
    fn files_and_imports() {
        let files = write(schema());
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["palladio.mm", "pcm/repository.mm"]);
        assert!(files[0].1.contains("\nimport \"./pcm/repository.mm\"\n"));
        assert!(!files[1].1.contains("\nimport "));
        assert!(files[0].1.contains("\nview Repository {\n"));
        assert!(!files[1].1.contains("\nview "));
    }
}
