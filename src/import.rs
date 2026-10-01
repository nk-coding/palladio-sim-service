//! PCM XMI → mdeo model: loads a directory of PCM files with SimOxide's EMF-faithful loader and
//! expresses it in the [`Schema`]'s terms, as the model the platform would send ([`WireModel`]).
//! [`crate::mtext`] writes it as a `.m` file.

use crate::protocol::{WireInstance, WireModel, WireValue};
use crate::schema::{AttrType, PRIMITIVE_TYPES, PropKind, Schema, schema};
use simoxide_model::meta::{FeatureKind, class as c, feat as f};
use simoxide_model::raw::{Graph, ObjId, ResId, Slot, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// The entry files SimuLizar would load for `dir` (see `simoxide_sim::RunSpec::load_model`).
pub fn entry_files(dir: &Path, spec: &simoxide_sim::RunSpec) -> Result<Vec<std::path::PathBuf>, String> {
    let list = |ext: &str| -> Result<Vec<std::path::PathBuf>, String> {
        let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == ext))
            .collect();
        v.sort();
        Ok(v)
    };
    let mut files = Vec::new();
    match &spec.usagemodel {
        Some(u) => files.push(dir.join(u)),
        None => files.extend(list("usagemodel")?),
    }
    if spec.allocation.is_empty() {
        files.extend(list("allocation")?);
    } else {
        files.extend(spec.allocation.iter().map(|a| dir.join(a)));
    }
    match &spec.monitorrepository {
        Some(m) => files.push(dir.join(m)),
        None => files.extend(list("monitorrepository")?),
    }
    Ok(files)
}

/// The run parameters of a corpus model directory (`run.json`), or the defaults.
pub fn run_spec(dir: &Path) -> Result<simoxide_sim::RunSpec, String> {
    let p = dir.join("run.json");
    if p.exists() { simoxide_sim::RunSpec::load(&p) } else { Ok(simoxide_sim::RunSpec::default()) }
}

/// The converted model, and what could not be expressed.
pub struct Imported {
    pub model: WireModel,
    /// `(file name, instance name)` of every root object, for callers that address files.
    pub roots: Vec<(String, String)>,
    /// What could not be expressed in the Palladio metamodel and was left out.
    pub warnings: Vec<String>,
}

/// How imported instances are named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// By their PCM id, so measuring point keys and results match the original model's.
    Ids,
    /// By their entity name (`Server1`, `Assembly_BookingSystem`), made unique.
    Readable,
}

/// Loads every PCM file of `dir` and converts it. `metamodel_path` is what the model's
/// `metamodelPath` names.
pub fn import_dir(dir: &Path, metamodel_path: &str, naming: Naming) -> Result<Imported, String> {
    let mut l = simoxide_model::Loader::default();
    l.load_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    import_loaded(l, metamodel_path, naming)
}

/// Loads the given PCM files (e.g. usage model, allocation, monitor repository) and what they
/// refer to, and converts them.
pub fn import_files(files: &[std::path::PathBuf], metamodel_path: &str, naming: Naming) -> Result<Imported, String> {
    let mut l = simoxide_model::Loader::default();
    for f in files {
        l.load_file(f);
    }
    import_loaded(l, metamodel_path, naming)
}

fn import_loaded(mut l: simoxide_model::Loader, metamodel_path: &str, naming: Naming) -> Result<Imported, String> {
    l.resolve_all();
    let (g, diags) = l.finish();
    let errors: Vec<String> = g.resources.iter().flat_map(|r| r.errors.iter().cloned()).collect();
    if !errors.is_empty() {
        return Err(format!("the model does not load: {}", errors.join("; ")));
    }
    let _ = diags;
    Importer::new(&g, schema(), naming).run(metamodel_path)
}

struct Importer<'a> {
    g: &'a Graph,
    s: &'a Schema,
    names: HashMap<ObjId, String>,
    taken: HashSet<String>,
    dropped: BTreeMap<String, usize>,
    /// Library primitive types referenced, materialised as repository data types.
    primitives: BTreeMap<String, String>,
    naming: Naming,
}

fn is_model_resource(g: &Graph, r: ResId) -> bool {
    let res = &g.resources[r.0 as usize];
    !res.uri.starts_with("pathmap:") && !res.failed
}

impl<'a> Importer<'a> {
    fn new(g: &'a Graph, s: &'a Schema, naming: Naming) -> Self {
        Importer {
            g,
            s,
            names: HashMap::new(),
            taken: HashSet::new(),
            dropped: BTreeMap::new(),
            primitives: BTreeMap::new(),
            naming,
        }
    }

    fn run(mut self, metamodel_path: &str) -> Result<Imported, String> {
        let g = self.g;
        // Every object of the model files, in tree order.
        let mut objects: Vec<ObjId> = Vec::new();
        for ri in 0..g.resources.len() {
            let r = ResId(ri as u32);
            if is_model_resource(g, r) {
                objects.extend(g.all_contents(r));
            }
        }
        // Names: the PCM id (or entity name) where there is one, else the class name and a counter.
        let mut counters: HashMap<String, usize> = HashMap::new();
        for &o in &objects {
            if self.flattened(o) {
                continue;
            }
            let entity = (self.naming == Naming::Readable)
                .then(|| g[o].class.feature("entityName").and_then(|f| g.str_attr(o, f)))
                .flatten()
                .map(|n| readable(&n))
                .filter(|n| !n.is_empty());
            let base = match g.id(o) {
                _ if entity.is_some() => entity.unwrap(),
                Some(id) if !g.has_generated_id(o) && self.naming == Naming::Ids => id.to_string(),
                // A root without an id is named after its file.
                _ if g[o].container.is_none() => {
                    let res = &g.resources[g[o].resource.0 as usize];
                    let stem = Path::new(&res.name).file_stem().and_then(|s| s.to_str()).unwrap_or("model");
                    let taken = |n: &str| self.taken.contains(n);
                    let ext = Path::new(&res.name).extension().and_then(|s| s.to_str()).unwrap_or("root");
                    if taken(stem) { format!("{stem}_{ext}") } else { stem.to_string() }
                }
                _ => {
                    let cn = o_class_name(g, o);
                    let n = counters.entry(cn.clone()).or_default();
                    *n += 1;
                    format!("{}_{}", lower_first(&cn), n)
                }
            };
            let mut name = base.clone();
            let mut i = 1;
            while !self.taken.insert(name.clone()) {
                i += 1;
                name = format!("{base}_{i}");
            }
            self.names.insert(o, name);
        }

        let mut instances: Vec<WireInstance> = Vec::new();
        let mut first_repository: Option<usize> = None;
        for &o in &objects {
            if self.flattened(o) {
                continue;
            }
            let cl = g[o].class;
            let Some(ci) = self.s.class_of(cl) else {
                *self.dropped.entry(format!("objects of class {}", cl.qualified_name())).or_default() += 1;
                continue;
            };
            if first_repository.is_none() && cl.is_a(c::repository_Repository) {
                first_repository = Some(instances.len());
            }
            instances.push(self.instance(o, ci)?);
        }
        // Library primitive types become data types of the first repository.
        if !self.primitives.is_empty() {
            let Some(ri) = first_repository else {
                return Err("the model refers to primitive data types but has no repository".into());
            };
            let names: Vec<String> = self.primitives.values().cloned().collect();
            let refs = &mut instances[ri].references;
            match refs.iter_mut().find(|(k, _)| k == "dataTypes") {
                Some((_, v)) => v.extend(names.iter().cloned()),
                None => refs.push(("dataTypes".into(), names.clone())),
            }
            for (ty, name) in &self.primitives {
                instances.push(WireInstance {
                    name: name.clone(),
                    class_name: "PrimitiveDataType".into(),
                    attributes: vec![("type".into(), vec![WireValue::String(ty.clone())])],
                    references: vec![],
                });
            }
        }
        // References to objects that were dropped cannot be kept either.
        let known: HashSet<String> = instances.iter().map(|i| i.name.clone()).collect();
        for inst in &mut instances {
            for (_, targets) in &mut inst.references {
                targets.retain(|t| known.contains(t));
            }
            inst.references.retain(|(_, t)| !t.is_empty());
        }
        let warnings = self.dropped.iter().map(|(what, n)| format!("dropped {n}x {what}")).collect();
        let roots = objects
            .iter()
            .filter(|&&o| g[o].container.is_none() && self.names.contains_key(&o))
            .map(|&o| {
                let res = &g.resources[g[o].resource.0 as usize];
                let file = Path::new(&res.name).file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
                (file, self.names[&o].clone())
            })
            .collect();
        Ok(Imported { model: WireModel { metamodel_path: metamodel_path.to_string(), instances }, roots, warnings })
    }

    /// Objects that are not instances of their own: random variables and variable references,
    /// which are properties of the object containing them.
    fn flattened(&self, o: ObjId) -> bool {
        let cl = self.g[o].class;
        cl.is_a(c::RandomVariable) || cl.is_a(c::AbstractNamedReference)
    }

    fn instance(&mut self, o: ObjId, ci: usize) -> Result<WireInstance, String> {
        let g = self.g;
        let s = self.s;
        let mut attributes = Vec::new();
        let mut references = Vec::new();
        for (fid, slot) in &g[o].slots {
            let fid = *fid;
            if Some(fid) == g[o].class.def().id_attribute {
                continue;
            }
            let Some(pi) = s.prop_by_feature(ci, fid) else {
                if !fid.def().derived && !fid.def().transient && !fid.is_container() {
                    let set = match slot {
                        Slot::Many(v) => !v.is_empty(),
                        Slot::Refs(v) => !v.is_empty(),
                        Slot::Ref(r) => r.is_some(),
                        Slot::One(_) => true,
                    };
                    if set {
                        *self
                            .dropped
                            .entry(format!("{}.{}", g[o].class.qualified_name(), fid.name()))
                            .or_default() += 1;
                    }
                }
                continue;
            };
            let p = &s.props[pi];
            match &p.kind {
                PropKind::Attr { ty } => {
                    let vals: Vec<Value> = match slot {
                        Slot::One(v) => vec![v.clone()],
                        Slot::Many(v) => v.clone(),
                        _ => continue,
                    };
                    let vals: Vec<WireValue> =
                        vals.iter().filter_map(|v| attr_value(s, ty, fid, v)).collect();
                    attributes.push((p.name.clone(), vals));
                }
                PropKind::RandomVar => {
                    if let Some(rv) = g.get_ref(o, fid)
                        && let Some(spec) = g.str_attr(rv, f::RandomVariable_specification)
                    {
                        attributes.push((p.name.clone(), vec![WireValue::String(spec.to_string())]));
                    }
                }
                PropKind::NamedRef => {
                    if let Some(r) = g.get_ref(o, fid) {
                        attributes.push((p.name.clone(), vec![WireValue::String(named_reference(g, r))]));
                    }
                }
                PropKind::Library { lib_enum } => {
                    let mut vals = Vec::new();
                    for &t in g.get_refs(o, fid) {
                        let frag = g.fragment(t);
                        let e = &s.enums[*lib_enum];
                        let entry = e
                            .entries
                            .iter()
                            .find(|n| n.value.rsplit_once('#').is_some_and(|(_, fr)| fr == frag))
                            .ok_or_else(|| {
                                format!("{}: {} refers to {}, which is not in Palladio's library", g.describe(o), fid.name(), g.describe(t))
                            })?;
                        vals.push(WireValue::String(entry.name.clone()));
                    }
                    attributes.push((p.name.clone(), vals));
                }
                PropKind::Ref { .. } => {
                    let mut targets = Vec::new();
                    for &t in g.get_refs(o, fid) {
                        let res = &g.resources[g[t].resource.0 as usize];
                        if g[t].proxy.is_none() && res.uri == PRIMITIVE_TYPES && g[t].class.is_a(c::PrimitiveDataType) {
                            let ty = match g.attr(t, f::PrimitiveDataType_type) {
                                Value::Enum(e) => {
                                    simoxide_model::meta::enums::PrimitiveTypeEnum.def().literals[e as usize].name.to_string()
                                }
                                _ => continue,
                            };
                            let name = self.primitive(&ty);
                            targets.push(name);
                            continue;
                        }
                        match self.names.get(&t) {
                            Some(n) => targets.push(n.clone()),
                            None => {
                                *self.dropped.entry(format!("references to {}", g.describe(t))).or_default() += 1;
                            }
                        }
                    }
                    references.push((p.name.clone(), targets));
                }
                PropKind::Container { .. } => {}
            }
        }
        Ok(WireInstance {
            name: self.names[&o].clone(),
            class_name: s.classes[ci].name.clone(),
            attributes,
            references,
        })
    }

    fn primitive(&mut self, ty: &str) -> String {
        if let Some(n) = self.primitives.get(ty) {
            return n.clone();
        }
        let mut name = format!("primitive_{ty}");
        while !self.taken.insert(name.clone()) {
            name.push('_');
        }
        self.primitives.insert(ty.to_string(), name.clone());
        name
    }
}

fn o_class_name(g: &Graph, o: ObjId) -> String {
    let cl = g[o].class;
    schema().class_of(cl).map_or_else(|| cl.name().to_string(), |ci| schema().classes[ci].name.clone())
}

/// An entity name as an identifier: `Allocation_Assembly_C1 <C1>` → `Allocation_Assembly_C1_C1`.
fn readable(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("n{out}") } else { out }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `NamespaceReference(a, NamespaceReference(b, VariableReference(c)))` → `a.b.c`.
fn named_reference(g: &Graph, mut r: ObjId) -> String {
    let mut parts = Vec::new();
    loop {
        parts.push(g.str_attr(r, f::AbstractNamedReference_referenceName).unwrap_or_default().to_string());
        match g.get_ref(r, f::NamespaceReference_innerReference_NamespaceReference) {
            Some(inner) if g[r].class.is_a(c::NamespaceReference) => r = inner,
            _ => break,
        }
    }
    parts.join(".")
}

fn attr_value(s: &Schema, ty: &AttrType, fid: simoxide_model::meta::FeatureId, v: &Value) -> Option<WireValue> {
    Some(match (ty, v) {
        (_, Value::Null) => return None,
        (AttrType::Int, Value::Int(i)) => WireValue::Int(*i as i32),
        (AttrType::Long, Value::Int(i)) => WireValue::Long(*i),
        (AttrType::Double, x) => WireValue::Double(x.as_f64()?),
        (AttrType::Bool, Value::Bool(b)) => WireValue::Bool(*b),
        (AttrType::String, x) => WireValue::String(x.as_str()?.to_string()),
        (AttrType::Enum(e), Value::Enum(i)) => {
            let FeatureKind::Attribute { data: simoxide_model::meta::DataKind::Enum(en) } = fid.def().kind else {
                return None;
            };
            let lit = &en.def().literals[*i as usize];
            let entry = s.enums[*e].entries.iter().find(|n| n.name == lit.name)?;
            WireValue::String(entry.name.clone())
        }
        _ => return None,
    })
}
