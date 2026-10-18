//! A mutable instance graph over the platform's model data ([`WireModel`]): instances with
//! attributes and references by name, as the platform sends them.
//!
//! The platform sends both ends of a containment and of a pair of opposite references. Edits
//! keep that: [`Graph::link`] and [`Graph::unlink`] update the opposite end too, when the
//! Palladio metamodel names one. Instances are kept in declaration order; new ones are appended,
//! so the PCM lists they join stay in a deterministic order.

use palladio_sim_service::protocol::{WireInstance, WireModel, WireValue};
use palladio_sim_service::schema::{PropKind, Schema, schema};
use simoxide_model::meta::prelude::PcmClass;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct Graph {
    pub metamodel_path: String,
    pub insts: Vec<WireInstance>,
    index: HashMap<String, usize>,
    removed: HashSet<usize>,
}

/// The opposite end of a Palladio reference, if the metamodel names one.
pub fn opposite(class: &str, feature: &str) -> Option<&'static str> {
    let s: &'static Schema = schema();
    let ci = s.class(class)?;
    let pi = s.prop(ci, feature)?;
    for a in &s.assocs {
        match (a.source_prop, a.target_prop) {
            (Some(x), Some(y)) if x == pi => return Some(s.props[y].name.as_str()),
            (Some(x), Some(y)) if y == pi => return Some(s.props[x].name.as_str()),
            _ => {}
        }
    }
    None
}

/// Is `feature` of `class` a Palladio containment (the container's end)?
pub fn is_containment(class: &str, feature: &str) -> bool {
    let s = schema();
    s.class(class)
        .and_then(|ci| s.prop(ci, feature))
        .is_some_and(|pi| matches!(s.props[pi].kind, PropKind::Ref { containment: true, .. }))
}

/// Is `class` a Palladio class?
pub fn is_pcm(class: &str) -> bool {
    schema().class(class).is_some()
}

/// Is `class` the Palladio class `sup` or a subclass of it?
pub fn is_a(class: &str, sup: &str) -> bool {
    let s = schema();
    let (Some(c), Some(p)) = (s.class(class), s.class(sup)) else { return class == sup };
    s.classes[c].eclass.is_a(s.classes[p].eclass)
}

impl Graph {
    pub fn new(model: &WireModel) -> Graph {
        let insts = model.instances.clone();
        let index = insts.iter().enumerate().map(|(i, x)| (x.name.clone(), i)).collect();
        Graph { metamodel_path: model.metamodel_path.clone(), insts, index, removed: HashSet::new() }
    }

    pub fn to_wire(&self, keep: impl Fn(&WireInstance) -> bool) -> WireModel {
        let live: Vec<&WireInstance> =
            self.insts.iter().enumerate().filter(|(i, _)| !self.removed.contains(i)).map(|(_, x)| x).filter(|x| keep(x)).collect();
        let names: HashSet<&str> = live.iter().map(|x| x.name.as_str()).collect();
        let instances = live
            .iter()
            .map(|x| {
                let mut y = (*x).clone();
                for (_, ts) in &mut y.references {
                    ts.retain(|t| names.contains(t.as_str()));
                }
                y
            })
            .collect();
        WireModel { metamodel_path: self.metamodel_path.clone(), instances }
    }

    /// Instance names in declaration order.
    pub fn names(&self) -> Vec<String> {
        self.insts.iter().enumerate().filter(|(i, _)| !self.removed.contains(i)).map(|(_, x)| x.name.clone()).collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.index.get(name).is_some_and(|i| !self.removed.contains(i))
    }

    pub fn get(&self, name: &str) -> Option<&WireInstance> {
        self.index.get(name).filter(|i| !self.removed.contains(i)).map(|&i| &self.insts[i])
    }

    fn get_mut(&mut self, name: &str) -> &mut WireInstance {
        let i = self.index[name];
        &mut self.insts[i]
    }

    pub fn class(&self, name: &str) -> &str {
        self.get(name).map_or("", |x| x.class_name.as_str())
    }

    pub fn is_a(&self, name: &str, sup: &str) -> bool {
        is_a(self.class(name), sup)
    }

    /// Instances of `class` (or of a subclass, for Palladio classes), in declaration order.
    pub fn all(&self, class: &str) -> Vec<String> {
        self.insts
            .iter()
            .enumerate()
            .filter(|(i, x)| !self.removed.contains(i) && (x.class_name == class || is_a(&x.class_name, class)))
            .map(|(_, x)| x.name.clone())
            .collect()
    }

    pub fn attr(&self, name: &str, key: &str) -> Option<&WireValue> {
        self.get(name)?.attributes.iter().find(|(k, _)| k == key)?.1.first()
    }

    pub fn str_attr(&self, name: &str, key: &str) -> Option<String> {
        match self.attr(name, key)? {
            WireValue::String(s) => Some(s.clone()),
            WireValue::Enum { entry, .. } => Some(entry.clone()),
            _ => None,
        }
    }

    pub fn num_attr(&self, name: &str, key: &str) -> Option<f64> {
        match self.attr(name, key)? {
            WireValue::Int(i) => Some(*i as f64),
            WireValue::Long(i) => Some(*i as f64),
            WireValue::Double(d) => Some(*d),
            WireValue::Float(d) => Some(*d as f64),
            _ => None,
        }
    }

    pub fn bool_attr(&self, name: &str, key: &str) -> Option<bool> {
        match self.attr(name, key)? {
            WireValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn entity(&self, name: &str) -> String {
        self.str_attr(name, "entityName").unwrap_or_else(|| name.to_string())
    }

    pub fn set_attr(&mut self, name: &str, key: &str, value: WireValue) {
        let x = self.get_mut(name);
        match x.attributes.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = vec![value],
            None => x.attributes.push((key.to_string(), vec![value])),
        }
    }

    pub fn refs(&self, name: &str, feature: &str) -> Vec<String> {
        self.get(name)
            .and_then(|x| x.references.iter().find(|(k, _)| k == feature))
            .map(|(_, v)| v.iter().filter(|t| self.has(t)).cloned().collect())
            .unwrap_or_default()
    }

    pub fn ref1(&self, name: &str, feature: &str) -> Option<String> {
        self.refs(name, feature).into_iter().next()
    }

    /// Who refers to `name` through `feature`.
    pub fn referrers(&self, name: &str, feature: &str) -> Vec<String> {
        self.insts
            .iter()
            .enumerate()
            .filter(|(i, x)| {
                !self.removed.contains(i) && x.references.iter().any(|(k, v)| k == feature && v.iter().any(|t| t == name))
            })
            .map(|(_, x)| x.name.clone())
            .collect()
    }

    fn add_end(&mut self, a: &str, feature: &str, b: &str) {
        let x = self.get_mut(a);
        match x.references.iter_mut().find(|(k, _)| k == feature) {
            Some((_, v)) => {
                if !v.iter().any(|t| t == b) {
                    v.push(b.to_string());
                    v.sort();
                }
            }
            None => x.references.push((feature.to_string(), vec![b.to_string()])),
        }
    }

    fn remove_end(&mut self, a: &str, feature: &str, b: &str) {
        if !self.index.contains_key(a) {
            return;
        }
        let x = self.get_mut(a);
        if let Some((_, v)) = x.references.iter_mut().find(|(k, _)| k == feature) {
            v.retain(|t| t != b);
        }
    }

    /// Adds the link `a.feature -- b`, and its opposite end.
    pub fn link(&mut self, a: &str, feature: &str, b: &str) {
        self.add_end(a, feature, b);
        if let Some(op) = opposite(self.class(a), feature) {
            self.add_end(b, op, a);
        }
    }

    pub fn unlink(&mut self, a: &str, feature: &str, b: &str) {
        self.remove_end(a, feature, b);
        if let Some(op) = opposite(self.class(a), feature) {
            self.remove_end(b, op, a);
        }
    }

    /// Replaces every target of `a.feature` by `b`.
    pub fn set_ref(&mut self, a: &str, feature: &str, b: &str) {
        for t in self.refs(a, feature) {
            self.unlink(a, feature, &t);
        }
        self.link(a, feature, b);
    }

    /// A fresh instance named `base` (or `base_2`, ...).
    pub fn create(&mut self, base: &str, class: &str) -> String {
        let mut name = sanitize(base);
        let mut k = 2;
        while self.index.contains_key(&name) {
            name = format!("{}_{k}", sanitize(base));
            k += 1;
        }
        self.index.insert(name.clone(), self.insts.len());
        self.insts.push(WireInstance {
            name: name.clone(),
            class_name: class.to_string(),
            attributes: Vec::new(),
            references: Vec::new(),
        });
        name
    }

    /// Removes an instance and every link to it.
    pub fn remove(&mut self, name: &str) {
        let Some(&i) = self.index.get(name) else { return };
        self.removed.insert(i);
        for x in &mut self.insts {
            for (_, v) in &mut x.references {
                v.retain(|t| t != name);
            }
        }
    }

    /// Removes the instances `names` and every link to them, in one pass.
    pub fn remove_all(&mut self, names: &HashSet<String>) {
        for n in names {
            if let Some(&i) = self.index.get(n.as_str()) {
                self.removed.insert(i);
            }
        }
        for x in &mut self.insts {
            for (_, v) in &mut x.references {
                v.retain(|t| !names.contains(t));
            }
        }
    }

    /// `name` and everything it contains (Palladio containments), depth first.
    pub fn subtree(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![name.to_string()];
        while let Some(n) = stack.pop() {
            let Some(x) = self.get(&n) else { continue };
            for (f, ts) in &x.references {
                if is_containment(&x.class_name, f) {
                    stack.extend(ts.iter().filter(|t| self.has(t)).cloned());
                }
            }
            out.push(n);
        }
        out
    }

    /// Completes the opposite ends of every Palladio reference (a model read from a `.m` file
    /// names each link once; the platform sends both ends).
    pub fn complete_opposites(&mut self) {
        let mut add = Vec::new();
        for x in &self.insts {
            for (f, ts) in &x.references {
                if let Some(op) = opposite(&x.class_name, f) {
                    for t in ts {
                        add.push((t.clone(), op, x.name.clone()));
                    }
                }
            }
        }
        for (a, f, b) in add {
            if self.index.contains_key(&a) {
                self.add_end(&a, f, &b);
            }
        }
    }

    /// The container of `name` and the containment feature, from the child's container end.
    pub fn container(&self, name: &str) -> Option<String> {
        let x = self.get(name)?;
        let s = schema();
        let ci = s.class(&x.class_name)?;
        for (f, ts) in &x.references {
            if let Some(pi) = s.prop(ci, f)
                && matches!(s.props[pi].kind, PropKind::Container { .. })
                && let Some(t) = ts.first()
                && self.has(t)
            {
                return Some(t.clone());
            }
        }
        None
    }
}

/// An instance name from any text: letters, digits and `_`.
pub fn sanitize(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("x{out}") } else { out }
}
