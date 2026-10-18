//! One `script-functions` session: the state the protocol keeps per connection (the model, the
//! collections the client believes the service holds) and the answers to its messages.
//!
//! Messages are handled in order. A call waits for its simulation while the connection keeps
//! reading, so keepalives are answered however long a run takes; other sessions run their
//! simulations at the same time on the shared pool.

use crate::functions;
use crate::translate::{self, Translation};
use palladio_sim_service::engine::{Engine, Prepared};
use palladio_sim_service::protocol::{ClientMessage, HeapObject, ServiceMessage, UNKNOWN_MODEL, UNKNOWN_OBJECT, WireModel};
use palladio_sim_service::schema::schema;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info};

struct HeldModel {
    id: i64,
    model: Arc<WireModel>,
    translation: Option<Arc<Translation>>,
    prepared: Option<Arc<Prepared>>,
    /// The model of a dynamic run (rule 5), ready to run (`crate::dynamic`).
    dynamic: Option<Arc<crate::dynamic::Prepared>>,
}

pub struct Session {
    engine: Arc<Engine>,
    label: String,
    metamodels: HashSet<String>,
    model: Option<HeldModel>,
    /// Collections held for the client, by id, with the version they were sent at.
    heap: HashMap<i64, i64>,
    next_id: i64,
    models_seen: u64,
    calls: u64,
}

impl Session {
    pub fn new(engine: Arc<Engine>, label: String) -> Session {
        Session {
            engine,
            label,
            metamodels: HashSet::new(),
            model: None,
            heap: HashMap::new(),
            next_id: -1,
            models_seen: 0,
            calls: 0,
        }
    }

    /// Answers one message; `None` for the messages that have no answer.
    pub async fn handle(&mut self, message: ClientMessage) -> Option<ServiceMessage> {
        match message {
            ClientMessage::Metamodel(mm) => {
                debug!("{}: metamodel {} ({} classes)", self.label, mm.path, mm.classes.len());
                if self.model.as_ref().is_some_and(|m| m.model.metamodel_path == mm.path) {
                    self.drop_model();
                }
                self.metamodels.insert(mm.path);
                None
            }
            ClientMessage::Model { model_id, model } => {
                self.drop_model();
                self.models_seen += 1;
                if self.models_seen == 1 || self.models_seen % 500 == 0 {
                    info!("{}: model #{} ({} instances)", self.label, self.models_seen, model.instances.len());
                }
                self.model = Some(HeldModel { id: model_id, model: Arc::new(model), translation: None, prepared: None, dynamic: None });
                None
            }
            ClientMessage::Release { ids, .. } => {
                for id in ids {
                    self.heap.remove(&id);
                }
                None
            }
            ClientMessage::Call { call_id, operation, objects, args, model_id } => {
                self.calls += 1;
                Some(match self.call(call_id, &operation, objects, &args, model_id).await {
                    Ok(m) => m,
                    Err((message, code)) => {
                        debug!("{}: call {call_id} ({operation}) failed: {message}", self.label);
                        ServiceMessage::Failure { call_id, message, code: code.map(String::from) }
                    }
                })
            }
        }
    }

    fn drop_model(&mut self) {
        // Everything held belonged to the old model.
        self.model = None;
        self.heap.clear();
    }

    async fn call(
        &mut self,
        call_id: i64,
        operation: &str,
        objects: Vec<HeapObject>,
        args: &[palladio_sim_service::protocol::WireValue],
        model_id: Option<i64>,
    ) -> Result<ServiceMessage, (String, Option<&'static str>)> {
        // Collections the client left out must be held at the version it assumes.
        for o in &objects {
            if o.elements.is_none() && o.entries.is_none() && self.heap.get(&o.id) != Some(&o.version) {
                return Err((format!("collection {} is not held", o.id), Some(UNKNOWN_OBJECT)));
            }
        }
        for o in &objects {
            self.heap.insert(o.id, o.version);
        }
        if !functions::is_operation(operation) {
            return Err((format!("this service has no operation '{operation}'"), None));
        }
        let held = match (&mut self.model, model_id) {
            (Some(m), Some(id)) if m.id == id => m,
            (_, None) => return Err(("the call names no model to simulate".into(), None)),
            _ => return Err(("the model is not held".into(), Some(UNKNOWN_MODEL))),
        };
        if functions::is_dynamic(operation) {
            let dyn_engine = crate::dynamic::engine(&self.engine.config);
            let prepared = match &held.dynamic {
                Some(p) => p.clone(),
                None => {
                    let model = held.model.clone();
                    let max_pods = dyn_engine.max_pods;
                    let p = tokio::task::spawn_blocking(move || -> Result<crate::dynamic::Prepared, String> {
                        let (_, dm) = translate::translate_dynamic(&model)?;
                        crate::dynamic::prepare(Arc::new(dm), max_pods)
                    })
                    .await
                    .map_err(|e| (format!("the dynamic translation failed: {e}"), None))?
                    .map_err(|e| (e, None))?;
                    let p = Arc::new(p);
                    held.dynamic = Some(p.clone());
                    p
                }
            };
            let params = functions::hpa_params(operation, args).map_err(|e| (e, None))?;
            let outcome = dyn_engine.simulate(prepared, params).await.map_err(|e| (e, None))?;
            let mut next = self.next_id;
            let answer = functions::call_dynamic(operation, args, &outcome, &mut || {
                let id = next;
                next -= 1;
                id
            })
            .map_err(|e| (e, None))?;
            self.next_id = next;
            for o in &answer.objects {
                self.heap.insert(o.id, o.version);
            }
            return Ok(ServiceMessage::Result { call_id, objects: answer.objects, value: answer.value });
        }
        let translation = match &held.translation {
            Some(t) => t.clone(),
            None => {
                let model = held.model.clone();
                if let Some(dir) = &self.engine.config.dump_dir {
                    crate::dump(dir, &model);
                }
                let t = tokio::task::spawn_blocking(move || translate::translate(&model))
                    .await
                    .map_err(|e| (format!("the translation failed: {e}"), None))?
                    .map_err(|e| (e, None))?;
                let t = Arc::new(t);
                held.translation = Some(t.clone());
                t
            }
        };
        let prepared = if functions::simulates(operation) {
            match &held.prepared {
                Some(p) => Some(p.clone()),
                None => {
                    let p = self.engine.prepare(schema(), Arc::new(translation.pcm.clone())).await.map_err(|e| (e, None))?;
                    held.prepared = Some(p.clone());
                    Some(p)
                }
            }
        } else {
            None
        };
        let mut next = self.next_id;
        let answer = functions::call(&self.engine, operation, args, &translation, prepared, &mut || {
            let id = next;
            next -= 1;
            id
        })
        .await
        .map_err(|e| (e, None))?;
        self.next_id = next;
        for o in &answer.objects {
            self.heap.insert(o.id, o.version);
        }
        Ok(ServiceMessage::Result { call_id, objects: answer.objects, value: answer.value })
    }

    pub fn summary(&self) -> String {
        format!("{} models, {} calls", self.models_seen, self.calls)
    }
}
