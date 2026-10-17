//! mdeo model → PCM XMI: turns the model the platform sends ([`WireModel`]) into the PCM files
//! the simulator loads, one file per root instance.
//!
//! The files are written the way EMF saves them: contained objects nest, references within a
//! file are IDREF attributes, references across files and into Palladio's library models are
//! `href` elements with an `xsi:type`. An instance's name is its XMI id; objects of classes
//! without an id attribute are referred to by their fragment path (`//@measuringPoints.0`).
//! Every list is in the order the instances are declared.

use crate::protocol::{WireModel, WireValue};
use crate::schema::{AttrType, PropKind, Schema};
use simoxide_model::meta::prelude::*;
use simoxide_model::meta::{ClassId, FeatureId, PackageId};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

/// The PCM files of a model: `(file name, XMI text)`, the usage model first.
#[derive(Debug, Clone)]
pub struct PcmFiles {
    pub files: Vec<(String, String)>,
}

struct Inst<'a> {
    name: &'a str,
    ci: usize,
    eclass: ClassId,
    attrs: HashMap<usize, &'a [WireValue]>,
    refs: HashMap<usize, Vec<usize>>,
}

struct Placement {
    parent: Option<(usize, FeatureId, Option<usize>)>,
    file: usize,
    fragment: String,
}

pub fn to_xmi(s: &Schema, model: &WireModel) -> Result<PcmFiles, String> {
    // Instances with their properties resolved against the schema.
    let index: HashMap<&str, usize> =
        model.instances.iter().enumerate().map(|(i, x)| (x.name.as_str(), i)).collect();
    if index.len() != model.instances.len() {
        return Err("two instances share a name".into());
    }
    let mut insts: Vec<Inst> = Vec::with_capacity(model.instances.len());
    for x in &model.instances {
        if x.name.chars().any(char::is_whitespace) {
            return Err(format!("instance name '{}' contains whitespace, which a PCM id cannot", x.name));
        }
        let ci = s
            .class(&x.class_name)
            .ok_or_else(|| format!("{}: class {} is not part of the Palladio metamodel", x.name, x.class_name))?;
        let mut attrs = HashMap::new();
        for (name, vals) in &x.attributes {
            let pi = s.prop(ci, name).ok_or_else(|| format!("{}: {} has no property {name}", x.name, x.class_name))?;
            if !vals.is_empty() {
                attrs.insert(pi, vals.as_slice());
            }
        }
        let mut refs = HashMap::new();
        for (name, targets) in &x.references {
            let pi = s.prop(ci, name).ok_or_else(|| format!("{}: {} has no property {name}", x.name, x.class_name))?;
            let mut ts = targets
                .iter()
                .map(|t| index.get(t.as_str()).copied().ok_or_else(|| format!("{}.{name}: no instance {t}", x.name)))
                .collect::<Result<Vec<_>, _>>()?;
            // Association ends are unordered in mdeo (the platform sends them sorted by name),
            // while PCM lists are ordered and their order can matter to the simulation (branch
            // transitions, scenarios, resource specifications). The order of the instances is
            // kept, so lists follow it: a model imported from PCM lists its objects in PCM
            // order and gets back exactly the original lists.
            ts.sort_unstable();
            ts.dedup();
            refs.insert(pi, ts);
        }
        insts.push(Inst { name: &x.name, ci, eclass: s.classes[ci].eclass, attrs, refs });
    }

    // The containment tree, from the containment ends (both ends of a composition are sent).
    let mut parent: Vec<Option<(usize, FeatureId, Option<usize>)>> = vec![None; insts.len()];
    for (i, x) in insts.iter().enumerate() {
        let mut props: Vec<(&usize, &Vec<usize>)> = x.refs.iter().collect();
        props.sort_by_key(|(pi, _)| **pi);
        for (&pi, children) in props {
            let p = &s.props[pi];
            let PropKind::Ref { containment: true, .. } = p.kind else { continue };
            let fid = p.feature.expect("a containment has a feature");
            for (k, &ch) in children.iter().enumerate() {
                if let Some((q, _, _)) = parent[ch]
                    && q != i
                {
                    return Err(format!("{} is contained in both {} and {}", insts[ch].name, insts[q].name, x.name));
                }
                parent[ch] = Some((i, fid, p.many.then_some(k)));
            }
        }
    }

    // Files: one per root; every other instance must be contained in one.
    let mut files: Vec<(String, usize)> = Vec::new(); // (file name, root)
    let mut used_names: HashMap<String, usize> = HashMap::new();
    for (i, x) in insts.iter().enumerate() {
        if parent[i].is_some() {
            continue;
        }
        let Some(ext) = s.root_extension(x.ci) else {
            return Err(format!(
                "{} ({}) is not contained in anything; only {} can stand alone",
                x.name,
                s.classes[x.ci].name,
                "Repository, System, ResourceEnvironment, Allocation, UsageModel, MonitorRepository and MeasuringPointRepository"
            ));
        };
        let base: String =
            x.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let n = used_names.entry(format!("{base}.{ext}")).or_default();
        *n += 1;
        let name = if *n == 1 { format!("{base}.{ext}") } else { format!("{base}_{n}.{ext}") };
        files.push((name, i));
    }

    // Where every instance lives, and how it is referred to.
    let mut placement: Vec<Option<Placement>> = (0..insts.len()).map(|_| None).collect();
    fn place(
        i: usize,
        insts: &[Inst],
        parent: &[Option<(usize, FeatureId, Option<usize>)>],
        placement: &mut Vec<Option<Placement>>,
        file_of_root: &HashMap<usize, usize>,
        depth: usize,
    ) -> Result<(), String> {
        if placement[i].is_some() {
            return Ok(());
        }
        if depth > insts.len() {
            return Err(format!("{} contains itself", insts[i].name));
        }
        let (file, path) = match parent[i] {
            None => (file_of_root[&i], "/".to_string()),
            Some((p, fid, k)) => {
                place(p, insts, parent, placement, file_of_root, depth + 1)?;
                let pp = placement[p].as_ref().unwrap();
                let base = if pp.fragment.starts_with('/') { pp.fragment.clone() } else { path_of(p, insts, parent) };
                let seg = match k {
                    Some(k) => format!("@{}.{}", fid.name(), k),
                    None => format!("@{}", fid.name()),
                };
                (pp.file, if base == "/" { format!("//{seg}") } else { format!("{base}/{seg}") })
            }
        };
        let fragment = if insts[i].eclass.def().id_attribute.is_some() { insts[i].name.to_string() } else { path };
        placement[i] = Some(Placement { parent: parent[i], file, fragment });
        Ok(())
    }
    fn path_of(i: usize, insts: &[Inst], parent: &[Option<(usize, FeatureId, Option<usize>)>]) -> String {
        let mut segs = Vec::new();
        let mut cur = i;
        while let Some((p, fid, k)) = parent[cur] {
            segs.push(match k {
                Some(k) => format!("@{}.{}", fid.name(), k),
                None => format!("@{}", fid.name()),
            });
            cur = p;
        }
        let _ = insts;
        let mut out = String::from("/");
        for seg in segs.iter().rev() {
            out.push('/');
            out.push_str(seg);
        }
        out
    }
    let file_of_root: HashMap<usize, usize> = files.iter().enumerate().map(|(fi, (_, r))| (*r, fi)).collect();
    for i in 0..insts.len() {
        place(i, &insts, &parent, &mut placement, &file_of_root, 0)?;
    }
    let placement: Vec<Placement> = placement.into_iter().map(Option::unwrap).collect();

    let w = Writer { s, insts: &insts, placement: &placement, files: &files };
    let mut out: Vec<(String, String)> = Vec::new();
    for (fi, (name, root)) in files.iter().enumerate() {
        out.push((name.clone(), w.file(fi, *root)?));
    }
    // The usage model first, so a caller can find it.
    out.sort_by_key(|(n, _)| !n.ends_with(".usagemodel"));
    Ok(PcmFiles { files: out })
}

struct Writer<'a> {
    s: &'a Schema,
    insts: &'a [Inst<'a>],
    placement: &'a [Placement],
    files: &'a [(String, usize)],
}

/// Namespace prefixes of one file.
#[derive(Default)]
struct Namespaces {
    by_package: BTreeMap<u32, String>,
}

impl Namespaces {
    fn prefix(&mut self, p: PackageId) -> String {
        if let Some(x) = self.by_package.get(&p.0) {
            return x.clone();
        }
        let base = p.def().prefix.to_string();
        let mut prefix = base.clone();
        let mut n = 1;
        while self.by_package.values().any(|x| *x == prefix) {
            n += 1;
            prefix = format!("{base}{n}");
        }
        self.by_package.insert(p.0, prefix.clone());
        prefix
    }
    fn qname(&mut self, cl: ClassId) -> String {
        format!("{}:{}", self.prefix(cl.def().package), cl.name())
    }
}

impl Writer<'_> {
    fn file(&self, fi: usize, root: usize) -> Result<String, String> {
        let mut ns = Namespaces::default();
        let mut body = String::new();
        let root_tag = ns.qname(self.insts[root].eclass);
        self.element(root, &root_tag, None, 0, fi, &mut ns, &mut body)?;
        // The namespace declarations go into the root's start tag.
        let mut decl = String::from(" xmi:version=\"2.0\" xmlns:xmi=\"http://www.omg.org/XMI\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"");
        for (p, prefix) in &ns.by_package {
            let _ = write!(decl, " xmlns:{prefix}=\"{}\"", PackageId(*p).def().ns_uri);
        }
        let insert_at = 1 + root_tag.len();
        body.insert_str(insert_at, &decl);
        Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{body}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn element(
        &self,
        i: usize,
        tag: &str,
        declared: Option<ClassId>,
        depth: usize,
        fi: usize,
        ns: &mut Namespaces,
        out: &mut String,
    ) -> Result<(), String> {
        let s = self.s;
        let x = &self.insts[i];
        let cl = x.eclass;
        let indent = "  ".repeat(depth);
        let _ = write!(out, "{indent}<{tag}");
        if declared.is_some_and(|d| d != cl) {
            let _ = write!(out, " xsi:type=\"{}\"", ns.qname(cl));
        }
        if let Some(idf) = cl.def().id_attribute {
            let _ = write!(out, " {}=\"{}\"", idf.name(), esc(x.name));
        }
        let mut children = String::new();
        let inner = "  ".repeat(depth + 1);
        for fid in cl.features() {
            let Some(pi) = s.prop_by_feature(x.ci, fid) else { continue };
            let p = &s.props[pi];
            match &p.kind {
                PropKind::Attr { ty } => {
                    let Some(vals) = x.attrs.get(&pi) else { continue };
                    let texts = vals
                        .iter()
                        .map(|v| attr_text(s, ty, v).ok_or_else(|| format!("{}.{}: {v:?} is not a {:?}", x.name, p.name, ty)))
                        .collect::<Result<Vec<_>, _>>()?;
                    if p.many {
                        for t in texts {
                            let _ = writeln!(children, "{inner}<{}>{}</{}>", fid.name(), esc(&t), fid.name());
                        }
                    } else {
                        let _ = write!(out, " {}=\"{}\"", fid.name(), esc(&texts[0]));
                    }
                }
                PropKind::RandomVar => {
                    let Some(vals) = x.attrs.get(&pi) else { continue };
                    let Some(WireValue::String(spec)) = vals.first() else {
                        return Err(format!("{}.{}: a stochastic expression is a string", x.name, p.name));
                    };
                    let _ = writeln!(children, "{inner}<{} specification=\"{}\"/>", fid.name(), esc(spec));
                }
                PropKind::NamedRef => {
                    let Some(vals) = x.attrs.get(&pi) else { continue };
                    let Some(WireValue::String(r)) = vals.first() else {
                        return Err(format!("{}.{}: a variable reference is a string", x.name, p.name));
                    };
                    named_reference(&mut children, fid.name(), r, depth + 1, ns);
                }
                PropKind::Library { lib_enum } => {
                    let Some(vals) = x.attrs.get(&pi) else { continue };
                    let e = &s.enums[*lib_enum];
                    for v in vals.iter() {
                        let WireValue::String(entry) = v else { continue };
                        let n = e
                            .entries
                            .iter()
                            .find(|n| &n.name == entry)
                            .ok_or_else(|| format!("{}.{}: {} has no entry {entry}", x.name, p.name, e.name))?;
                        let _ = writeln!(
                            children,
                            "{inner}<{} xsi:type=\"{}\" href=\"{}\"/>",
                            fid.name(),
                            ns.qname(n.class.expect("library entry")),
                            esc(&n.value)
                        );
                    }
                }
                PropKind::Ref { containment: true, .. } => {
                    let Some(ts) = x.refs.get(&pi) else { continue };
                    let declared = fid.target();
                    for &t in ts {
                        self.element(t, fid.name(), declared, depth + 1, fi, ns, &mut children)?;
                    }
                }
                PropKind::Ref { containment: false, .. } => {
                    let Some(ts) = x.refs.get(&pi) else { continue };
                    let mut same = Vec::new();
                    for &t in ts {
                        let tp = &self.placement[t];
                        if tp.file == fi {
                            same.push(tp.fragment.clone());
                        } else {
                            let _ = writeln!(
                                children,
                                "{inner}<{} xsi:type=\"{}\" href=\"{}#{}\"/>",
                                fid.name(),
                                ns.qname(self.insts[t].eclass),
                                esc(&self.files[tp.file].0),
                                esc(&tp.fragment)
                            );
                        }
                    }
                    if !same.is_empty() {
                        let _ = write!(out, " {}=\"{}\"", fid.name(), esc(&same.join(" ")));
                    }
                }
                PropKind::Container { .. } => {}
            }
        }
        let _ = self.placement[i].parent;
        if children.is_empty() {
            out.push_str("/>\n");
        } else {
            let _ = write!(out, ">\n{children}{indent}</{}>\n", tag);
        }
        Ok(())
    }
}

/// `a.b.c` → `NamespaceReference(a, NamespaceReference(b, VariableReference(c)))`.
fn named_reference(out: &mut String, tag: &str, r: &str, depth: usize, ns: &mut Namespaces) {
    let parts: Vec<&str> = r.split('.').collect();
    let namespace = ns.qname(simoxide_model::meta::class::NamespaceReference);
    let variable = ns.qname(simoxide_model::meta::class::VariableReference);
    let mut close = Vec::new();
    for (k, part) in parts.iter().enumerate() {
        let indent = "  ".repeat(depth + k);
        let t = if k == 0 { tag } else { "innerReference_NamespaceReference" };
        if k + 1 == parts.len() {
            let _ = writeln!(out, "{indent}<{t} xsi:type=\"{variable}\" referenceName=\"{}\"/>", esc(part));
        } else {
            let _ = writeln!(out, "{indent}<{t} xsi:type=\"{namespace}\" referenceName=\"{}\">", esc(part));
            close.push(format!("{indent}</{t}>"));
        }
    }
    for c in close.iter().rev() {
        out.push_str(c);
        out.push('\n');
    }
}

fn attr_text(s: &Schema, ty: &AttrType, v: &WireValue) -> Option<String> {
    Some(match (ty, v) {
        (AttrType::Int | AttrType::Long, WireValue::Int(i)) => i.to_string(),
        (AttrType::Int | AttrType::Long, WireValue::Long(i)) => i.to_string(),
        // The model language has one number syntax; a whole double may arrive as an integer.
        (AttrType::Int | AttrType::Long, WireValue::Double(d)) if d.fract() == 0.0 => (*d as i64).to_string(),
        (AttrType::Double, WireValue::Double(d)) => format!("{d}"),
        (AttrType::Double, WireValue::Float(d)) => format!("{d}"),
        (AttrType::Double, WireValue::Int(i)) => i.to_string(),
        (AttrType::Double, WireValue::Long(i)) => i.to_string(),
        (AttrType::Bool, WireValue::Bool(b)) => b.to_string(),
        (AttrType::String, WireValue::String(x)) => x.clone(),
        (AttrType::Enum(e), WireValue::String(entry)) | (AttrType::Enum(e), WireValue::Enum { entry, .. }) => {
            s.enums[*e].entries.iter().find(|n| &n.name == entry)?.value.clone()
        }
        _ => return None,
    })
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            '\t' => out.push_str("&#x9;"),
            c => out.push(c),
        }
    }
    out
}
