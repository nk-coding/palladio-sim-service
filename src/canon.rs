//! A canonical form of a PCM model: the same instances with names and order that follow from
//! the model's structure, not from the names and order it was sent with. Two models that differ
//! only in instance names (ids), entity names and the order of instances and of reference lists
//! get the same canonical model, so SimOxide, whose event order and random streams follow ids
//! and list order, simulates them identically.
//!
//! - **Structure.** An instance's initial colour is its class and its attribute values
//!   (`entityName` left out); references are labelled edges in both directions.
//! - **Colour refinement** (1-dimensional Weisfeiler-Leman): an instance's next colour is its
//!   colour and the multiset of (edge label, direction, neighbour colour), until the partition
//!   is stable. Colours are ranks of canonical keys, so they do not depend on names or order.
//! - **Ties.** Instances left with equal colours are individualised class by class: the members
//!   of the tied class with the smallest colour get colours in the order of their original
//!   names, and the refinement runs again. For instances that are interchangeable (an
//!   automorphism of the model maps one to the other: two identical branches, pods, signatures)
//!   the choice does not change the canonical model. Instances that colour refinement cannot
//!   tell apart without being interchangeable (rare, regular structures) are told apart by their
//!   names: the residual name dependence, deterministic.
//! - **Result.** Instances in colour order, named `_c<colour>`, with `entityName` set to that
//!   name; every reference list sorted by colour. The maps lead back to the caller's names.

use crate::protocol::{WireInstance, WireModel, WireValue};
use std::collections::HashMap;

/// A canonical model and the way back.
#[derive(Clone, Debug)]
pub struct Canonical {
    pub model: WireModel,
    /// The caller's name → the canonical name, and back.
    pub to_canon: HashMap<String, String>,
    pub to_orig: HashMap<String, String>,
    /// Canonical name → the caller's `entityName` (if it had one).
    pub entity: HashMap<String, String>,
    /// Tied classes individualised by name (colour refinement could not tell their members
    /// apart).
    pub ties: usize,
    /// Canonical names of the instances that were tied after colour refinement: which of the
    /// caller's instances they stand for depends on names (they are interchangeable, or nearly).
    pub tied: std::collections::HashSet<String>,
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fnv(s: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The canonical name of colour `c`.
pub fn name_of(c: usize) -> String {
    format!("_c{c}")
}

/// Is `s` a canonical name?
pub fn is_canonical(s: &str) -> bool {
    s.strip_prefix("_c").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Ranks of `keys`: equal keys equal ranks, ranks in key order.
fn ranks<K: Ord + Clone>(keys: &[K]) -> (Vec<u32>, usize) {
    let mut sorted: Vec<K> = keys.to_vec();
    sorted.sort();
    sorted.dedup();
    let r = keys.iter().map(|k| sorted.binary_search(k).unwrap() as u32).collect();
    (r, sorted.len())
}

/// Refines `colour` until the partition is stable; returns the number of classes.
fn refine(colour: &mut Vec<u32>, edges: &[Vec<(u64, u32)>], mut classes: usize) -> usize {
    let n = colour.len();
    let mut keys: Vec<(u32, u64)> = Vec::with_capacity(n);
    loop {
        keys.clear();
        for i in 0..n {
            // the multiset of (label, neighbour colour), hashed order-free
            let mut h = 0u64;
            for &(label, j) in &edges[i] {
                h = h.wrapping_add(mix(label ^ mix(u64::from(colour[j as usize]) + 1)));
            }
            keys.push((colour[i], mix(h)));
        }
        let (next, k) = ranks(&keys);
        *colour = next;
        if k == classes {
            return k;
        }
        classes = k;
    }
}

/// The canonical form of `model`.
pub fn canonicalise(model: &WireModel) -> Canonical {
    let insts = &model.instances;
    let n = insts.len();
    let index: HashMap<&str, u32> = insts.iter().enumerate().map(|(i, x)| (x.name.as_str(), i as u32)).collect();
    // initial colours: class and attribute values, entity names left out
    let init: Vec<u64> = insts
        .iter()
        .map(|x| {
            let mut attrs: Vec<String> = x.attributes.iter().filter(|(k, _)| k != "entityName").map(|(k, v)| format!("{k}={v:?}")).collect();
            attrs.sort();
            let mut h = fnv(x.class_name.as_bytes());
            for a in &attrs {
                h = mix(h ^ fnv(a.as_bytes()));
            }
            h
        })
        .collect();
    // edges in both directions, labelled by feature and direction
    let mut edges: Vec<Vec<(u64, u32)>> = vec![Vec::new(); n];
    for (i, x) in insts.iter().enumerate() {
        for (f, targets) in &x.references {
            let out = fnv(f.as_bytes());
            let inn = mix(out ^ 0x494e);
            for t in targets {
                if let Some(&j) = index.get(t.as_str()) {
                    edges[i].push((out, j));
                    edges[j as usize].push((inn, i as u32));
                }
            }
        }
    }
    let (mut colour, mut classes) = ranks(&init);
    let t0 = std::time::Instant::now();
    classes = refine(&mut colour, &edges, classes);
    if std::env::var_os("PCM_CANON_DEBUG").is_some() {
        eprintln!("canon: {n} instances, first refinement {:.1} ms, {classes} classes", t0.elapsed().as_secs_f64() * 1e3);
    }
    let mut ties = 0;
    // instances that colour refinement leaves tied: their identity follows from names
    let mut size = vec![0u32; classes];
    for &c in &colour {
        size[c as usize] += 1;
    }
    let tied_after_refinement: Vec<bool> = colour.iter().map(|&c| size[c as usize] > 1).collect();
    while classes < n {
        // the tied class with the smallest colour, its members ordered by their original names
        let mut size = vec![0u32; classes];
        for &c in &colour {
            size[c as usize] += 1;
        }
        let tied = (0..classes).find(|&c| size[c] > 1).unwrap() as u32;
        let mut members: Vec<usize> = (0..n).filter(|&i| colour[i] == tied).collect();
        members.sort_by(|&a, &b| insts[a].name.cmp(&insts[b].name));
        let mut pos = vec![0u32; n];
        for (k, &i) in members.iter().enumerate() {
            pos[i] = k as u32;
        }
        let keys: Vec<(u32, u32)> = (0..n).map(|i| (colour[i], pos[i])).collect();
        let (c, k) = ranks(&keys);
        colour = c;
        classes = refine(&mut colour, &edges, k);
        ties += 1;
    }
    // the canonical model
    let canon: Vec<String> = colour.iter().map(|&c| name_of(c as usize)).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| colour[i]);
    let mut to_canon = HashMap::with_capacity(n);
    let mut to_orig = HashMap::with_capacity(n);
    let mut entity = HashMap::new();
    for (i, x) in insts.iter().enumerate() {
        to_canon.insert(x.name.clone(), canon[i].clone());
        to_orig.insert(canon[i].clone(), x.name.clone());
        if let Some((_, v)) = x.attributes.iter().find(|(k, _)| k == "entityName")
            && let Some(WireValue::String(s)) = v.first()
        {
            entity.insert(canon[i].clone(), s.clone());
        }
    }
    let instances = order
        .iter()
        .map(|&i| {
            let x = &insts[i];
            let attributes = x
                .attributes
                .iter()
                .map(|(k, v)| if k == "entityName" { (k.clone(), vec![WireValue::String(canon[i].clone())]) } else { (k.clone(), v.clone()) })
                .collect();
            let references = x
                .references
                .iter()
                .map(|(f, ts)| {
                    let mut ts: Vec<(u32, String)> = ts.iter().filter_map(|t| index.get(t.as_str()).map(|&j| (colour[j as usize], canon[j as usize].clone()))).collect();
                    ts.sort();
                    (f.clone(), ts.into_iter().map(|(_, t)| t).collect())
                })
                .collect();
            WireInstance { name: canon[i].clone(), class_name: x.class_name.clone(), attributes, references }
        })
        .collect();
    let tied = (0..n).filter(|&i| tied_after_refinement[i]).map(|i| canon[i].clone()).collect();
    Canonical { model: WireModel { metamodel_path: model.metamodel_path.clone(), instances }, to_canon, to_orig, entity, ties, tied }
}

impl Canonical {
    /// The caller's name of canonical name `s` (`s` itself if it is none).
    pub fn orig(&self, s: &str) -> String {
        self.to_orig.get(s).cloned().unwrap_or_else(|| s.to_string())
    }

    /// The canonical name of the caller's `s` (`s` itself if it is none).
    pub fn canon(&self, s: &str) -> String {
        self.to_canon.get(s).cloned().unwrap_or_else(|| s.to_string())
    }

    /// `s` with every canonical name in it replaced by the caller's name (`names`: by its entity
    /// name).
    pub fn orig_text(&self, s: &str, names: bool) -> String {
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'_' && b.get(i + 1) == Some(&b'c') && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
                let mut j = i + 2;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 2 && (j == b.len() || !b[j].is_ascii_alphanumeric()) {
                    let c = &s[i..j];
                    let r = if names { self.entity.get(c).cloned().unwrap_or_else(|| self.orig(c)) } else { self.orig(c) };
                    out.push_str(&r);
                    i = j;
                    continue;
                }
            }
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// A JSON value with the canonical names in it replaced by the caller's: values of keys
    /// `name` and `…Name` by entity names, every other string by instance names.
    pub fn orig_json(&self, v: &mut serde_json::Value) {
        self.orig_json_in(v, false);
    }

    fn orig_json_in(&self, v: &mut serde_json::Value, names: bool) {
        match v {
            serde_json::Value::String(s) => *s = self.orig_text(s, names),
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| self.orig_json_in(x, names)),
            serde_json::Value::Object(o) => {
                let mut renamed = serde_json::Map::new();
                for (k, mut x) in std::mem::take(o) {
                    let names = k == "name" || k.ends_with("Name");
                    self.orig_json_in(&mut x, names);
                    renamed.insert(self.orig_text(&k, false), x);
                }
                *o = renamed;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(name: &str, class: &str, entity: &str, refs: &[(&str, &[&str])]) -> WireInstance {
        WireInstance {
            name: name.into(),
            class_name: class.into(),
            attributes: vec![("entityName".into(), vec![WireValue::String(entity.into())]), ("x".into(), vec![WireValue::Int(1)])],
            references: refs.iter().map(|(f, ts)| (f.to_string(), ts.iter().map(|t| t.to_string()).collect())).collect(),
        }
    }

    #[test]
    fn names_and_order_do_not_matter() {
        let a = WireModel {
            metamodel_path: "m".into(),
            instances: vec![
                inst("root", "R", "r", &[("kids", &["k1", "k2", "k3"])]),
                inst("k1", "K", "one", &[("next", &["k2"])]),
                inst("k2", "K", "two", &[("next", &["k3"])]),
                inst("k3", "K", "three", &[]),
            ],
        };
        let b = WireModel {
            metamodel_path: "m".into(),
            instances: vec![
                inst("z", "K", "c", &[]),
                inst("y", "K", "b", &[("next", &["z"])]),
                inst("top", "R", "q", &[("kids", &["z", "x", "y"])]),
                inst("x", "K", "a", &[("next", &["y"])]),
            ],
        };
        let (ca, cb) = (canonicalise(&a), canonicalise(&b));
        assert_eq!(format!("{:?}", ca.model), format!("{:?}", cb.model));
        assert_eq!(ca.ties, 0);
        // the chain's first element in both
        assert_eq!(ca.canon("k1"), cb.canon("x"));
        assert_eq!(cb.orig(&ca.canon("k3")), "z");
        let text = format!("{} and {}", ca.canon("k1"), ca.canon("root"));
        assert_eq!(ca.orig_text(&text, false), "k1 and root");
        assert_eq!(ca.orig_text(&text, true), "one and r");
    }

    #[test]
    fn interchangeable_instances_give_one_canonical_model() {
        // two identical children: a tie, broken by name, the same model either way
        let m = |n1: &str, n2: &str| WireModel {
            metamodel_path: "m".into(),
            instances: vec![inst("p", "P", "p", &[("kids", &[n1, n2])]), inst(n1, "K", "", &[]), inst(n2, "K", "", &[])],
        };
        let (a, b) = (canonicalise(&m("a", "b")), canonicalise(&m("b", "a")));
        assert_eq!(a.ties, 1);
        assert_eq!(format!("{:?}", a.model), format!("{:?}", b.model));
    }
}
