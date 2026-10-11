//! Typed, transferable IPC for the Rust codec worker. WebRTC and engine mutation stay on Window.
use photocraft_collab::wire::{self, Decoder, Wire};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
#[path = "worker_policy.rs"]
pub mod policy;
pub const CODEC_VERSION: u32 = 20261010;
#[derive(Serialize, Deserialize)]
pub enum Task {
    Encode(Box<Wire>),
    Decode {
        peer: String,
        reliable: bool,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    Remove(String),
}
#[derive(Serialize, Deserialize)]
pub struct Request {
    pub generation: u64,
    pub id: u64,
    pub task: Task,
}
#[derive(Serialize, Deserialize)]
pub enum ResultData {
    Ready { protocol: u32 },
    Encoded(#[serde(with = "serde_bytes")] Vec<u8>),
    Decoded(Option<Box<Wire>>),
    Removed,
    Failed(String),
}
#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub generation: u64,
    pub id: u64,
    pub elapsed_ms: f64,
    pub result: ResultData,
}
pub fn js_error(value: JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}
fn serialize(value: &impl Serialize) -> Result<JsValue, String> {
    value.serialize(&serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true)).map_err(|e| e.to_string())
}
/// Count structured-clone memory conservatively and collect transferable byte buffers.
/// Trees come exclusively from our serializer; bounded traversal guards oversized local documents.
fn envelope(value: &JsValue) -> Result<(usize, js_sys::Array), String> {
    let mut stack = vec![value.clone()];
    let mut nodes = 0usize;
    let mut size = 0usize;
    let transfer = js_sys::Array::new();
    while let Some(value) = stack.pop() {
        nodes = nodes.saturating_add(1);
        size = size.saturating_add(64);
        if nodes > 1_000_000 || size > policy::MAX_SINGLE {
            return Err("Collaboration worker message exceeds protocol limit plus bounded metadata".into());
        }
        if value.is_instance_of::<js_sys::Uint8Array>() {
            let array = js_sys::Uint8Array::new(&value);
            size = size.saturating_add(array.byte_length() as usize);
            transfer.push(&array.buffer());
        } else if let Some(text) = value.as_string() {
            size = size.saturating_add(text.len().saturating_mul(2));
        } else if js_sys::Array::is_array(&value) {
            let array = js_sys::Array::from(&value);
            if array.length() as usize > 1_000_000usize.saturating_sub(nodes) {
                return Err("Collaboration worker tree too large".into());
            }
            stack.extend(array.iter());
        } else if value.is_object() {
            // Wire maps serialize to Map; integer keys retain precision across structured cloning.
            if value.is_instance_of::<js_sys::Map>() {
                let iter = js_sys::try_iter(&value).map_err(js_error)?.ok_or("Invalid worker map")?;
                for entry in iter {
                    stack.extend(js_sys::Array::from(&entry.map_err(js_error)?).iter());
                }
            } else {
                let keys = js_sys::Object::keys(&js_sys::Object::from(value.clone()));
                for key in keys.iter() {
                    stack.push(js_sys::Reflect::get(&value, &key).map_err(js_error)?);
                }
            }
        }
    }
    if size > policy::MAX_SINGLE {
        return Err("Collaboration worker message exceeds protocol limit plus bounded metadata".into());
    }
    Ok((size, transfer))
}
// This shared module is compiled separately for Window and the worker binary.
#[allow(dead_code)]
pub struct Prepared {
    id: u64,
    value: JsValue,
    transfer: js_sys::Array,
    pub charge: usize,
}
struct Pending {
    charge: usize,
    posted: f64,
    received: bool,
}
#[allow(dead_code)]
pub struct Codec {
    worker: web_sys::Worker,
    pub ready: Rc<Cell<bool>>,
    pending: Rc<RefCell<BTreeMap<u64, Pending>>>,
    bytes: Cell<usize>,
    next: Cell<u64>,
    last_progress: Rc<Cell<f64>>,
    highest_reply: Rc<Cell<u64>>,
    pub started: f64,
    _message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _error: Closure<dyn FnMut(web_sys::Event)>,
}
#[allow(dead_code)]
impl Codec {
    pub fn new(callback: impl Fn(Reply) + 'static) -> Result<Rc<Self>, String> {
        let worker = web_sys::Worker::new("photocraft-wire-worker_loader.js").map_err(js_error)?;
        let ready = Rc::new(Cell::new(false));
        let ready_callback = ready.clone();
        let pending = Rc::new(RefCell::new(BTreeMap::<u64, Pending>::new()));
        let callback_pending = pending.clone();
        let last_progress = Rc::new(Cell::new(js_sys::Date::now()));
        let callback_progress = last_progress.clone();
        let highest_reply = Rc::new(Cell::new(0u64));
        let callback_highest = highest_reply.clone();
        let callback = Rc::new(callback);
        let error_callback = callback.clone();
        let message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            callback_progress.set(js_sys::Date::now());
            let reply = serde_wasm_bindgen::from_value::<Reply>(event.data()).unwrap_or_else(|error| Reply {
                generation: 0,
                id: 0,
                elapsed_ms: 0.0,
                result: ResultData::Failed(error.to_string()),
            });
            if reply.id != 0 {
                callback_highest.set(callback_highest.get().max(reply.id));
                if let Some(task) = callback_pending.borrow_mut().get_mut(&reply.id) {
                    task.received = true;
                }
            }
            match reply.result {
                ResultData::Ready { protocol } if protocol == CODEC_VERSION => ready_callback.set(true),
                ResultData::Ready { .. } => {
                    callback(Reply { generation: 0, id: 0, elapsed_ms: 0.0, result: ResultData::Failed("Collaboration worker version mismatch".into()) });
                    return;
                }
                _ => {}
            }
            callback(reply);
        });
        let error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            error_callback(Reply { generation: 0, id: 0, elapsed_ms: 0.0, result: ResultData::Failed("Collaboration codec worker stopped".into()) })
        });
        worker.set_onmessage(Some(message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(error.as_ref().unchecked_ref()));
        Ok(Rc::new(Self {
            worker,
            ready,
            pending,
            last_progress,
            highest_reply,
            bytes: Cell::new(0),
            next: Cell::new(0),
            started: js_sys::Date::now(),
            _message: message,
            _error: error,
        }))
    }
    pub fn submit(&self, generation: u64, task: Task) -> Result<(u64, usize), String> {
        let id = self.next.get().checked_add(1).ok_or("Worker id exhausted")?;
        let value = serialize(&Request { generation, id, task })?;
        self.post_value(id, value)
    }
    pub fn prepare_encode(&self, generation: u64, message: &Wire) -> Result<Prepared, String> {
        // Serialize by reference: cloning a checkpoint would copy its full baseline on Window.
        #[derive(Serialize)]
        enum TaskRef<'a> {
            Encode(&'a Wire),
        }
        #[derive(Serialize)]
        struct RequestRef<'a> {
            generation: u64,
            id: u64,
            task: TaskRef<'a>,
        }
        let id = self.next.get().checked_add(1).ok_or("Worker id exhausted")?;
        let value = serialize(&RequestRef { generation, id, task: TaskRef::Encode(message) })?;
        self.prepare_value(id, value)
    }
    pub fn submit_packet(&self, generation: u64, peer: String, reliable: bool, buffer: &js_sys::ArrayBuffer) -> Result<(u64, usize), String> {
        let id = self.next.get().checked_add(1).ok_or("Worker id exhausted")?;
        let value = serialize(&Request { generation, id, task: Task::Decode { peer, reliable, bytes: vec![] } })?;
        let task = js_sys::Reflect::get(&value, &JsValue::from_str("task")).map_err(js_error)?;
        let decode = js_sys::Reflect::get(&task, &JsValue::from_str("Decode")).map_err(js_error)?;
        js_sys::Reflect::set(&decode, &JsValue::from_str("bytes"), &js_sys::Uint8Array::new(buffer)).map_err(js_error)?;
        self.post_value(id, value)
    }
    fn post_value(&self, id: u64, value: JsValue) -> Result<(u64, usize), String> {
        let prepared = self.prepare_value(id, value)?;
        self.post(prepared)
    }
    fn prepare_value(&self, id: u64, value: JsValue) -> Result<Prepared, String> {
        let (charge, transfer) = envelope(&value)?;
        if !policy::accepts(self.pending.borrow().len(), self.bytes.get(), charge) {
            return Err("Collaboration send queue full: worker busy".into());
        }
        Ok(Prepared { id, value, transfer, charge })
    }
    pub fn post(&self, prepared: Prepared) -> Result<(u64, usize), String> {
        let Prepared { id, value, transfer, charge } = prepared;
        // No callback can interleave synchronous prepare/reserve/post on Window.
        self.worker.post_message_with_transfer(&value, &transfer).map_err(js_error)?;
        self.pending.borrow_mut().insert(id, Pending { charge, posted: js_sys::Date::now(), received: false });
        self.bytes.set(self.bytes.get().saturating_add(charge));
        self.next.set(id);
        Ok((id, charge))
    }
    pub fn acknowledge(&self, id: u64) {
        if let Some(task) = self.pending.borrow_mut().remove(&id) {
            self.bytes.set(self.bytes.get().saturating_sub(task.charge));
        }
    }
    pub fn stalled(&self) -> bool {
        let now = js_sys::Date::now();
        self.pending
            .borrow()
            .iter()
            .any(|(id, task)| policy::worker_stalled(now, task.posted, task.received, self.last_progress.get(), *id, self.highest_reply.get()))
    }
    pub fn completion_metrics(&self) -> serde_json::Value {
        let now = js_sys::Date::now();
        let pending = self.pending.borrow();
        let received = pending.values().filter(|task| task.received).count();
        let oldest = |received| pending.values().filter(|task| task.received == received).map(|task| (now - task.posted).max(0.0)).fold(0.0, f64::max);
        serde_json::json!({"awaitingResults":pending.len().saturating_sub(received),"receivedResultsQueued":received,"oldestUnansweredMs":oldest(false),"oldestReceivedMs":oldest(true),"workerSilenceMs":(now-self.last_progress.get()).max(0.0)})
    }
    pub fn queued(&self) -> usize {
        self.pending.borrow().len()
    }
    pub fn bytes(&self) -> usize {
        self.bytes.get()
    }
}
impl Drop for Codec {
    fn drop(&mut self) {
        self.worker.set_onmessage(None);
        self.worker.set_onerror(None);
        self.worker.terminate();
    }
}
/// Worker-side callback owns the decoder for the lifetime of this dedicated worker.
#[allow(dead_code)]
pub fn run() -> Result<(), String> {
    let scope = js_sys::global().dyn_into::<web_sys::DedicatedWorkerGlobalScope>().map_err(|e| js_error(e.into()))?;
    let decoder = Rc::new(RefCell::new(Decoder::default()));
    let generation = Rc::new(Cell::new(0));
    let buffered = Rc::new(RefCell::new(BTreeMap::<String, usize>::new()));
    let reply_scope = scope.clone();
    let callback = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
        let started = js_sys::Date::now();
        let request = serde_wasm_bindgen::from_value::<Request>(event.data());
        let (gen_id, id, result) = match request {
            Ok(request) => {
                if generation.get() != request.generation {
                    generation.set(request.generation);
                    *decoder.borrow_mut() = Decoder::default();
                    buffered.borrow_mut().clear();
                }
                let result = (|| -> Result<ResultData, String> {
                    match request.task {
                        Task::Encode(message) => {
                            let bytes = wire::encode_message(&message).map_err(|e| e.to_string())?;
                            if bytes.len() > wire::MAX_TRANSFER {
                                return Err("Collaboration worker encoded message exceeds protocol limit plus bounded metadata".into());
                            }
                            Ok(ResultData::Encoded(bytes))
                        }
                        Task::Decode { peer, reliable, bytes } => {
                            if bytes.len() > wire::MAX_PACKET {
                                return Err("Worker packet too large".into());
                            }
                            if !reliable {
                                return wire::decode_unreliable(&bytes).map(|wire| ResultData::Decoded(Some(Box::new(wire)))).map_err(|e| e.to_string());
                            }
                            let mut buffered = buffered.borrow_mut();
                            let total = buffered.values().copied().fold(0usize, usize::saturating_add);
                            if total.saturating_add(bytes.len().saturating_sub(24)) > wire::MAX_TRANSFER {
                                return Err("Collaboration worker fragment budget exceeded".into());
                            }
                            let count = buffered.entry(peer.clone()).or_default();
                            *count = count.saturating_add(bytes.len().saturating_sub(24));
                            let result = decoder.borrow_mut().push(&peer, &bytes).map_err(|e| e.to_string());
                            if !matches!(result, Ok(None)) {
                                buffered.remove(&peer);
                            }
                            result.map(|message| ResultData::Decoded(message.map(Box::new)))
                        }
                        Task::Remove(peer) => {
                            decoder.borrow_mut().remove(&peer);
                            buffered.borrow_mut().remove(&peer);
                            Ok(ResultData::Removed)
                        }
                    }
                })();
                (request.generation, request.id, result.unwrap_or_else(ResultData::Failed))
            }
            Err(error) => (0, 0, ResultData::Failed(error.to_string())),
        };
        let reply = Reply { generation: gen_id, id, elapsed_ms: js_sys::Date::now() - started, result };
        let sent = serialize(&reply).and_then(|value| {
            let (_, transfer) = envelope(&value)?;
            reply_scope.post_message_with_transfer(&value, &transfer).map_err(js_error)
        });
        if let Err(error) = sent {
            // A tiny reply retains the request identity and releases Window credits. If even
            // this cannot be posted, the Window completion watchdog stops the room safely.
            let failed = Reply {
                generation: gen_id,
                id,
                elapsed_ms: js_sys::Date::now() - started,
                result: ResultData::Failed(format!("Worker reply failed: {error}")),
            };
            if let Ok(value) = serialize(&failed) {
                let _ = reply_scope.post_message(&value);
            }
        }
    });
    scope.set_onmessage(Some(callback.as_ref().unchecked_ref()));
    // This callback is the worker's only entry point and lives until Worker::terminate.
    callback.forget();
    let ready = serialize(&Reply { generation: 0, id: 0, elapsed_ms: 0.0, result: ResultData::Ready { protocol: CODEC_VERSION } })?;
    scope.post_message(&ready).map_err(js_error)
}
