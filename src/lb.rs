//! Load balancers for the dynamic runs (`crate::evaluate`): Slingshot's pattern on plain PCM.
//! An SPD service group needs one in front of each assembly of a scaled container: a component
//! that provides and requires the assembly's interface, one SEFF per signature with a branch of
//! one transition that calls the signature (Slingshot adds a transition per pod when it scales
//! out); the callers' connectors and the system's delegations point at it. They are SPD's
//! realisation of a service group, Palladio, not a layer above it. All of them run on one
//! container `LoadBalancerServer` without work (joined to the first linking resource).

use crate::graph::Graph;
use crate::protocol::{WireModel, WireValue};
use std::collections::BTreeSet;

struct Builder {
    g: Graph,
    repo: String,
    system: String,
    allocation: String,
}

fn s(v: impl Into<String>) -> WireValue {
    WireValue::String(v.into())
}

fn one(g: &Graph, x: &str, feature: &str) -> Result<String, String> {
    g.ref1(x, feature).ok_or_else(|| format!("{x} ({}) has no {feature}", g.class(x)))
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

impl Builder {
    fn named(&mut self, base: &str, class: &str, entity: Option<&str>) -> String {
        let n = self.g.create(base, class);
        if let Some(e) = entity {
            self.g.set_attr(&n, "entityName", s(e));
        }
        n
    }

    /// The BasicComponent whose SEFF contains `action`.
    fn component_of(&self, action: &str) -> Option<String> {
        let mut cur = action.to_string();
        for _ in 0..64 {
            let up = self.g.container(&cur)?;
            if self.g.is_a(&up, "BasicComponent") {
                return Some(up);
            }
            cur = up;
        }
        None
    }

    /// Puts `actions` into behaviour `beh` between a new start and stop action.
    fn fill(&mut self, beh: &str, actions: &[String]) {
        let start = self.named(&format!("{beh}_start"), "StartAction", None);
        let stop = self.named(&format!("{beh}_stop"), "StopAction", None);
        let mut chain = vec![start];
        chain.extend(actions.iter().cloned());
        chain.push(stop);
        for a in &chain {
            self.g.link(beh, "steps", a);
        }
        for w in chain.windows(2) {
            self.g.link(&w[0], "successor", &w[1]);
        }
    }

    fn component(&mut self, base: &str) -> String {
        let c = self.named(base, "BasicComponent", Some(base));
        let repo = self.repo.clone();
        self.g.link(&repo, "components", &c);
        c
    }

    fn role(&mut self, comp: &str, iface: &str, provided: bool) -> String {
        let (class, feature, iface_feature, kind) = if provided {
            ("OperationProvidedRole", "providedRoles", "providedInterface", "provides")
        } else {
            ("OperationRequiredRole", "requiredRoles", "requiredInterface", "requires")
        };
        let r = self.named(&format!("{comp}_{kind}_{iface}"), class, None);
        self.g.link(comp, feature, &r);
        self.g.link(&r, iface_feature, iface);
        r
    }

    fn seff(&mut self, comp: &str, sig: &str) -> String {
        let name = format!("{comp}_{}", self.g.entity(sig));
        let x = self.named(&name, "ResourceDemandingSEFF", None);
        self.g.link(comp, "serviceEffectSpecifications", &x);
        self.g.link(&x, "describedService", sig);
        x
    }

    fn assembly(&mut self, comp: &str, container: &str) -> String {
        let a = self.named(&format!("Assembly_{comp}"), "AssemblyContext", Some(&format!("Assembly_{comp}")));
        let system = self.system.clone();
        self.g.link(&system, "assemblyContexts", &a);
        self.g.link(&a, "encapsulatedComponent", comp);
        let ac = self.named(&format!("alloc_{a}"), "AllocationContext", None);
        let alloc = self.allocation.clone();
        self.g.link(&alloc, "allocationContexts", &ac);
        self.g.link(&ac, "assemblyContext", &a);
        self.g.link(&ac, "resourceContainer", container);
        a
    }

    fn connector(&mut self, req: &str, req_role: &str, prov: &str, prov_role: &str) -> String {
        let c = self.named(&format!("{req}_to_{prov}"), "AssemblyConnector", None);
        let system = self.system.clone();
        self.g.link(&system, "connectors", &c);
        self.g.link(&c, "requiringAssemblyContext", req);
        self.g.link(&c, "requiredRole", req_role);
        self.g.link(&c, "providingAssemblyContext", prov);
        self.g.link(&c, "providedRole", prov_role);
        c
    }

    fn external(&mut self, base: &str, role: &str, sig: &str) -> String {
        let a = self.named(base, "ExternalCallAction", None);
        self.g.link(&a, "role", role);
        self.g.link(&a, "calledService", sig);
        a
    }

    /// A VariableUsage `name` with one characterisation `ty` = `spec`.
    fn usage(&mut self, owner: &str, feature: &str, name: &str, ty: &str, spec: &str) {
        let u = self.named(&format!("{owner}_{name}"), "VariableUsage", None);
        self.g.set_attr(&u, "namedReference", s(name));
        let ch = self.named(&format!("{u}_{}", ty.to_lowercase()), "VariableCharacterisation", None);
        self.g.set_attr(&ch, "type", s(ty));
        self.g.set_attr(&ch, "specification", s(spec));
        self.g.link(&u, "variableCharacterisation", &ch);
        self.g.link(owner, feature, &u);
    }

    /// What callers of `sig` through `role` of `comp` pass and read back: input
    /// characterisations (name, type) and RETURN characterisation types.
    fn forwarding(&self, comp: &str, role: &str, sig: &str) -> (BTreeSet<(String, String)>, BTreeSet<String>) {
        let mut inputs = BTreeSet::new();
        let mut returns = BTreeSet::new();
        for call in self.g.referrers(role, "role") {
            if self.g.ref1(&call, "calledService").as_deref() != Some(sig) || self.component_of(&call).as_deref() != Some(comp) {
                continue;
            }
            for u in self.g.refs(&call, "inputVariableUsages") {
                let Some(n) = self.g.str_attr(&u, "namedReference") else { continue };
                for ch in self.g.refs(&u, "variableCharacterisation") {
                    inputs.insert((n.clone(), self.g.str_attr(&ch, "type").unwrap_or_else(|| "VALUE".into())));
                }
            }
            for u in self.g.refs(&call, "returnVariableUsage") {
                for ch in self.g.refs(&u, "variableCharacterisation") {
                    let spec = self.g.str_attr(&ch, "specification").unwrap_or_default();
                    for ty in ["VALUE", "BYTESIZE", "NUMBER_OF_ELEMENTS", "TYPE", "STRUCTURE"] {
                        if spec.contains(&format!("RETURN.{ty}")) {
                            returns.insert(ty.to_string());
                        }
                    }
                }
            }
        }
        (inputs, returns)
    }

    /// A load balancer in front of assembly `asm`, on container `host`. Returns its assembly.
    fn load_balancer(&mut self, asm: &str, host: &str) -> Result<String, String> {
        let comp = one(&self.g, asm, "encapsulatedComponent")?;
        let provs: Vec<String> = self.g.refs(&comp, "providedRoles").into_iter().filter(|r| self.g.class(r) == "OperationProvidedRole").collect();
        let [prole] = provs.as_slice() else {
            return Err(format!("{} provides {} interfaces; a load balancer serves one", self.g.entity(&comp), provs.len()));
        };
        let prole = prole.clone();
        let iface = one(&self.g, &prole, "providedInterface")?;
        let conns: Vec<String> =
            sorted(self.g.referrers(asm, "providingAssemblyContext").into_iter().filter(|c| self.g.class(c) == "AssemblyConnector").collect());
        let delegs: Vec<String> =
            sorted(self.g.referrers(asm, "assemblyContext").into_iter().filter(|c| self.g.class(c) == "ProvidedDelegationConnector").collect());
        let label = self.g.entity(asm);
        let label = label.strip_prefix("Assembly_").unwrap_or(&label).to_string();
        let lb = self.component(&format!("LoadBalancer_{label}"));
        let lp = self.role(&lb, &iface, true);
        let lr = self.role(&lb, &iface, false);
        let served: BTreeSet<String> =
            self.g.refs(&comp, "serviceEffectSpecifications").iter().filter_map(|x| self.g.ref1(x, "describedService")).collect();
        for sig in self.g.refs(&iface, "signatures").into_iter().filter(|x| served.contains(x)) {
            let mut inputs = BTreeSet::new();
            for conn in &conns {
                let (Some(req), Some(role)) = (self.g.ref1(conn, "requiringAssemblyContext"), self.g.ref1(conn, "requiredRole")) else { continue };
                let caller = one(&self.g, &req, "encapsulatedComponent")?;
                let (i, r) = self.forwarding(&caller, &role, &sig);
                if !r.is_empty() {
                    return Err(format!(
                        "{}: a caller reads the RETURN of {}; Slingshot's load balancers hand nothing back",
                        self.g.entity(&comp),
                        self.g.entity(&sig)
                    ));
                }
                inputs.extend(i);
            }
            let seff = self.seff(&lb, &sig);
            let branch = self.named(&format!("{seff}_balance"), "BranchAction", Some("balance"));
            let t = self.named(&format!("{branch}_1"), "ProbabilisticBranchTransition", Some("pod 1"));
            self.g.set_attr(&t, "branchProbability", WireValue::Double(1.0));
            self.g.link(&branch, "branches", &t);
            let body = self.named(&format!("{t}_body"), "ResourceDemandingBehaviour", None);
            self.g.link(&t, "branchBehaviour", &body);
            let call = self.external(&format!("{seff}_call"), &lr, &sig);
            for (n, ty) in &inputs {
                self.usage(&call, "inputVariableUsages", n, ty, &format!("{n}.{ty}"));
            }
            self.fill(&body, &[call]);
            self.fill(&seff, &[branch]);
        }
        let lb_asm = self.assembly(&lb, host);
        for conn in &conns {
            self.g.set_ref(conn, "providingAssemblyContext", &lb_asm);
            self.g.set_ref(conn, "providedRole", &lp);
        }
        for dc in &delegs {
            self.g.set_ref(dc, "assemblyContext", &lb_asm);
            self.g.set_ref(dc, "innerProvidedRole", &lp);
        }
        self.connector(&lb_asm, &lr, asm, &prole);
        Ok(lb_asm)
    }
}

/// Name of the container the load balancers run on.
pub const HOST: &str = "LoadBalancerServer";

/// Slingshot's load balancers on plain PCM: the container [`HOST`] (one core, no work) and a
/// load balancer in front of every assembly allocated on each of `containers`. Returns the model
/// and (container, assembly, load balancer assembly) per assembly.
pub fn load_balanced(model: &WireModel, containers: &[String]) -> Result<(WireModel, Vec<(String, String, String)>), String> {
    let g = Graph::new(model);
    let first = |class: &str| g.all(class).into_iter().next().ok_or_else(|| format!("the model has no {class}"));
    let (repo, system, allocation, resenv) = (first("Repository")?, first("System")?, first("Allocation")?, first("ResourceEnvironment")?);
    let mut b = Builder { g, repo, system, allocation };
    let host = b.named(HOST, "ResourceContainer", Some(HOST));
    b.g.link(&resenv, "resourceContainer", &host);
    let cpu = b.named(&format!("{host}_cpu"), "ProcessingResourceSpecification", None);
    b.g.set_attr(&cpu, "schedulingPolicy", s("PROCESSOR_SHARING"));
    b.g.set_attr(&cpu, "activeResourceType", s("CPU"));
    b.g.set_attr(&cpu, "processingRate", s("1000000.0"));
    b.g.set_attr(&cpu, "numberOfReplicas", WireValue::Int(1));
    b.g.link(&host, "activeResourceSpecifications", &cpu);
    if let Some(l) = sorted(b.g.all("LinkingResource")).into_iter().next() {
        b.g.link(&l, "connectedResourceContainers", &host);
    }
    let mut out = Vec::new();
    for c in containers {
        let mut on_pod: Vec<String> = b
            .g
            .referrers(c, "resourceContainer")
            .into_iter()
            .filter(|ac| b.g.class(ac) == "AllocationContext")
            .filter_map(|ac| b.g.ref1(&ac, "assemblyContext"))
            .collect();
        on_pod.sort();
        for asm in on_pod {
            let lb = b.load_balancer(&asm, &host)?;
            out.push((c.clone(), asm, lb));
        }
    }
    Ok((b.g.to_wire(|_| true), out))
}
