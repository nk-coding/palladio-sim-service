//! One `script-functions` session: the state the protocol keeps per connection (the model, the
//! collections the client believes the service holds) and the answers to its messages.
//!
//! Messages are handled in order. A call waits for its simulation while the connection keeps
//! reading, so keepalives are answered however long a run takes; other sessions run their
//! simulations at the same time on the shared pool.

use crate::functions;
use crate::config::Config;
use crate::protocol::{ClientMessage, HeapObject, ServiceMessage, UNKNOWN_MODEL, UNKNOWN_OBJECT, WireModel};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info};

struct HeldModel {
    id: i64,
    model: Arc<WireModel>,
    /// The plain PCM of the model and its evaluations (`pcm*` functions).
    pcm: Option<Arc<crate::pcm::PcmCache>>,
    /// Where the model was dumped (`SIM_DUMP_DIR`).
    dumped: Option<std::path::PathBuf>,
}

pub struct Session {
    config: Arc<Config>,
    label: String,
    metamodels: HashSet<String>,
    model: Option<HeldModel>,
    /// Collections held for the client, by id, with the version they were sent at.
    heap: HashMap<i64, i64>,
    /// The elements of the lists the client sent (arguments of the `pcm*` functions).
    lists: HashMap<i64, Vec<crate::protocol::WireValue>>,
    next_id: i64,
    models_seen: u64,
    calls: u64,
}

impl Session {
    pub fn new(config: Arc<Config>, label: String) -> Session {
        Session {
            config,
            label,
            metamodels: HashSet::new(),
            model: None,
            heap: HashMap::new(),
            lists: HashMap::new(),
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
                self.model = Some(HeldModel {
                    id: model_id,
                    model: Arc::new(model),
                    pcm: None,
                    dumped: None,
                });
                None
            }
            ClientMessage::Release { ids, .. } => {
                for id in ids {
                    self.heap.remove(&id);
                    self.lists.remove(&id);
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
        self.lists.clear();
    }

    async fn call(
        &mut self,
        call_id: i64,
        operation: &str,
        objects: Vec<HeapObject>,
        args: &[crate::protocol::WireValue],
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
            if let Some(e) = &o.elements {
                self.lists.insert(o.id, e.clone());
            }
        }
        if !functions::is_operation(operation) {
            return Err((format!("this service has no operation '{operation}'"), None));
        }
        let held = match (&mut self.model, model_id) {
            (Some(m), Some(id)) if m.id == id => m,
            (_, None) => return Err(("the call names no model to simulate".into(), None)),
            _ => return Err(("the model is not held".into(), Some(UNKNOWN_MODEL))),
        };
        let cache = match &held.pcm {
            Some(c) => c.clone(),
            None => {
                let model = held.model.clone();
                if let Some(dir) = &self.config.dump_dir {
                    held.dumped = crate::dump(dir, &model);
                }
                let c = tokio::task::spawn_blocking(move || crate::pcm::PcmCache::new(&model))
                    .await
                    .map_err(|e| (format!("reading the model's PCM failed: {e}"), None))?;
                let c = Arc::new(c);
                held.pcm = Some(c.clone());
                c
            }
        };
        let mut next = self.next_id;
        let dumped = held.dumped.clone();
        let answer = functions::call(&self.config, operation, args, &self.lists, &cache, dumped.as_deref(), &mut || {
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
