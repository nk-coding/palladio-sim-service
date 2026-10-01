use palladio_sim_service::schema::schema;
use std::collections::{BTreeMap, BTreeSet};
fn find(p: &mut Vec<usize>, x: usize) -> usize { if p[x] != x { let r = find(p, p[x]); p[x] = r; } p[x] }
fn main() {
    let s = schema();
    let n = s.classes.len();
    let mut p: Vec<usize> = (0..n).collect();
    for a in &s.assocs { if a.target_prop.is_some() { let (x, y) = (find(&mut p, a.source), find(&mut p, a.target)); p[x] = y; } }
    // dependency edges between groups: needs[x] -> y
    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    for ci in 0..n { for &sp in &s.classes[ci].supers { edges.insert((find(&mut p, ci), find(&mut p, sp))); } }
    for a in &s.assocs { if a.target_prop.is_none() { edges.insert((find(&mut p, a.source), find(&mut p, a.target))); } }
    // SCC by repeated reachability (small graph)
    let groups: BTreeSet<usize> = (0..n).map(|c| find(&mut p, c)).collect();
    let reach = |from: usize| -> BTreeSet<usize> { let mut seen = BTreeSet::new(); let mut st = vec![from]; while let Some(x) = st.pop() { for &(a, b) in &edges { if a == x && seen.insert(b) { st.push(b); } } } seen };
    let r: BTreeMap<usize, BTreeSet<usize>> = groups.iter().map(|&g| (g, reach(g))).collect();
    let mut scc_of: BTreeMap<usize, usize> = BTreeMap::new();
    for &g in &groups { let rep = groups.iter().copied().find(|&h| h == g || (r[&g].contains(&h) && r[&h].contains(&g))).unwrap(); scc_of.insert(g, rep); }
    let mut sccs: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for ci in 0..n { let g = find(&mut p, ci); sccs.entry(scc_of[&g]).or_default().push(format!("{}[{}]", s.classes[ci].name, s.classes[ci].eclass.def().package.def().name.trim_start_matches("pcm."))); }
    let mut v: Vec<_> = sccs.into_values().collect(); v.sort_by_key(|x| std::cmp::Reverse(x.len()));
    for x in v.iter().filter(|x| x.len() > 2) { println!("{}: {}\n", x.len(), x.join(" ")); }
    println!("sccs: {} (size<=2: {})", v.len(), v.iter().filter(|x| x.len() <= 2).count());
}
