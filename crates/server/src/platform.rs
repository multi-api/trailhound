//! Tenants, API keys and the durable task queue.
//!
//! A write request is acknowledged only after its payload and its task
//! record are fsynced. Tasks left unfinished by a crash are run again on
//! startup; every task is idempotent, so replaying is safe.

use crate::error::ApiError;
use axum::http::StatusCode;
use trailhound_engine::{atomic_write, now_rfc3339, Index, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use uuid::Uuid;

const KEPT_FINISHED_TASKS: usize = 10_000;
/// Finished tasks are also dropped, oldest first, past this many bytes.
const KEPT_FINISHED_BYTES: usize = 32 * 1024 * 1024;
/// The task log is rewritten once it has grown past this and doubled.
const LOG_COMPACT_BYTES: u64 = 16 * 1024 * 1024;

/// Set when a search ran, so the memory its threads freed is handed back
/// shortly after instead of staying in the allocator.
pub static SEARCHED: AtomicBool = AtomicBool::new(false);

/// Keep glibc from holding on to freed memory. By default it raises its
/// own thresholds after every large block it frees and gives each new
/// thread an arena of its own; with one short-lived thread per segment and
/// search, the process ends up tens of megabytes larger than what it uses.
/// Here large blocks are always mapped and unmapped one by one, the heap is
/// trimmed early, and the arenas are capped at one per core.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        let knob = |name: &str, default: i32| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
        libc::mallopt(libc::M_MMAP_THRESHOLD, knob("TRAILHOUND_MALLOC_MMAP_KB", 256) * 1024);
        libc::mallopt(libc::M_TRIM_THRESHOLD, knob("TRAILHOUND_MALLOC_TRIM_KB", 1024) * 1024);
        // One arena per core keeps concurrent searches from queueing on a lock.
        let cores = std::thread::available_parallelism().map_or(8, |n| n.get() as i32);
        libc::mallopt(libc::M_ARENA_MAX, knob("TRAILHOUND_MALLOC_ARENAS", cores.max(8)));
    }
}

/// Hand the memory freed by a finished task back to the operating system.
/// glibc otherwise keeps it in its arenas, and the process looks as large as
/// its biggest write for the rest of its life.
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

pub fn new_key() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn valid_uid(uid: &str) -> bool {
    !uid.is_empty() && uid.len() <= 400 && uid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Quotas {
    pub max_indexes: Option<u64>,
    pub max_documents: Option<u64>,
    pub max_storage_bytes: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TenantMeta {
    pub id: String,
    pub name: String,
    pub master_key: String,
    pub created_at: String,
    #[serde(default)]
    pub suspended: bool,
    #[serde(default)]
    pub quotas: Quotas,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub name: Option<String>,
    pub description: Option<String>,
    pub key: String,
    pub uid: String,
    pub actions: Vec<String>,
    pub indexes: Vec<String>,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// Externally tagged on purpose: serde's internally tagged enums buffer their
// content, which breaks exact number handling.
#[derive(Serialize, Deserialize, Clone)]
pub enum Kind {
    IndexCreation { primary_key: Option<String> },
    IndexUpdate { primary_key: Option<String> },
    IndexDeletion,
    DocumentAddition { payload: String, primary_key: Option<String>, partial: bool },
    DocumentDeletion { ids: Vec<String> },
    DocumentDeletionByFilter { filter: Value },
    DocumentClear,
    SettingsUpdate { patch: Value },
}

impl Kind {
    /// Drop the request's payload once the task has run: only its summary
    /// (`details`) is shown afterwards.
    fn slim(&mut self) {
        match self {
            Kind::DocumentDeletion { ids } => *ids = Vec::new(),
            Kind::DocumentDeletionByFilter { filter } => *filter = Value::Null,
            Kind::SettingsUpdate { patch } => *patch = Value::Null,
            _ => {}
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Kind::IndexCreation { .. } => "indexCreation",
            Kind::IndexUpdate { .. } => "indexUpdate",
            Kind::IndexDeletion => "indexDeletion",
            Kind::DocumentAddition { .. } => "documentAdditionOrUpdate",
            Kind::DocumentDeletion { .. } | Kind::DocumentDeletionByFilter { .. } | Kind::DocumentClear => "documentDeletion",
            Kind::SettingsUpdate { .. } => "settingsUpdate",
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Task {
    pub uid: u64,
    pub index_uid: Option<String>,
    pub status: String,
    pub kind: Kind,
    pub details: Value,
    pub error: Option<Value>,
    pub enqueued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    /// Set when the task was interrupted by a crash and is being replayed.
    #[serde(skip)]
    pub retry: bool,
    /// Bytes this task takes in the log, once finished.
    #[serde(skip)]
    size: usize,
}

impl Task {
    pub fn view(&self) -> Value {
        json!({
            "uid": self.uid,
            "batchUid": self.uid,
            "indexUid": self.index_uid,
            "status": self.status,
            "type": self.kind.type_name(),
            "canceledBy": null,
            "details": self.details,
            "error": self.error,
            "duration": self.duration_ms.map(|ms| format!("PT{}.{:03}S", ms / 1000, ms % 1000)),
            "enqueuedAt": self.enqueued_at,
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
        })
    }
}

pub struct TaskStore {
    pub tasks: BTreeMap<u64, Task>,
    queue: VecDeque<u64>,
    next: u64,
    path: PathBuf,
    log: File,
    /// Finished tasks, oldest first, and the bytes they take.
    finished: VecDeque<u64>,
    finished_bytes: usize,
    /// Size of the log file, and the size at which it is next rewritten.
    log_bytes: u64,
    compact_at: u64,
}

impl TaskStore {
    fn load(path: &Path) -> std::io::Result<TaskStore> {
        let mut tasks: BTreeMap<u64, Task> = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            // A torn last line (crash mid-append) fails to parse and is skipped.
            for line in text.lines() {
                if let Ok(task) = serde_json::from_str::<Task>(line) {
                    tasks.insert(task.uid, task);
                }
            }
        }
        let next = tasks.keys().next_back().map_or(0, |u| u + 1);
        let mut queue = VecDeque::new();
        let mut finished = VecDeque::new();
        let mut finished_bytes = 0;
        for task in tasks.values_mut() {
            if task.status == "processing" {
                task.status = "enqueued".into();
                task.retry = true;
            }
            if task.status == "enqueued" {
                queue.push_back(task.uid);
            } else {
                task.kind.slim();
                task.size = serde_json::to_vec(task)?.len();
                finished.push_back(task.uid);
                finished_bytes += task.size;
            }
        }
        let log = OpenOptions::new().create(true).append(true).open(path)?;
        let mut store = TaskStore { tasks, queue, next, path: path.to_path_buf(), log, finished, finished_bytes, log_bytes: 0, compact_at: 0 };
        store.prune();
        store.compact()?;
        Ok(store)
    }

    fn append(&mut self, task: &Task, sync: bool) -> std::io::Result<usize> {
        let mut line = serde_json::to_vec(task)?;
        line.push(b'\n');
        self.log.write_all(&line)?;
        if sync {
            self.log.sync_data()?;
        }
        self.log_bytes += line.len() as u64;
        Ok(line.len())
    }

    /// Record a task's outcome, then keep the history and the log bounded.
    fn finish(&mut self, mut task: Task) {
        task.size = self.append(&task, true).unwrap_or(0);
        self.finished.push_back(task.uid);
        self.finished_bytes += task.size;
        self.tasks.insert(task.uid, task);
        self.prune();
        if self.log_bytes > self.compact_at {
            if let Err(e) = self.compact() {
                eprintln!("[trailhound] cannot rewrite {}: {e}", self.path.display());
            }
        }
    }

    /// Forget the oldest finished tasks beyond the count and size kept.
    fn prune(&mut self) {
        while self.finished.len() > KEPT_FINISHED_TASKS || (self.finished_bytes > KEPT_FINISHED_BYTES && self.finished.len() > 1) {
            let Some(uid) = self.finished.pop_front() else { break };
            if let Some(task) = self.tasks.remove(&uid) {
                self.finished_bytes -= task.size;
            }
        }
    }

    /// Rewrite the log with one line per task still known.
    fn compact(&mut self) -> std::io::Result<()> {
        let mut compact = Vec::new();
        for task in self.tasks.values() {
            compact.extend_from_slice(&serde_json::to_vec(task)?);
            compact.push(b'\n');
        }
        atomic_write(&self.path, &compact).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        self.log = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.log_bytes = compact.len() as u64;
        self.compact_at = (self.log_bytes * 2).max(LOG_COMPACT_BYTES);
        Ok(())
    }
}

pub struct Tenant {
    pub dir: PathBuf,
    pub meta: RwLock<TenantMeta>,
    pub indexes: RwLock<BTreeMap<String, Arc<Index>>>,
    pub keys: RwLock<Vec<ApiKey>>,
    pub tasks: Mutex<TaskStore>,
    scheduled: AtomicBool,
    pub searches: AtomicU64,
    pub docs_indexed: AtomicU64,
    usage_dirty: AtomicBool,
}

impl Tenant {
    fn load(dir: PathBuf, meta: TenantMeta) -> Result<Tenant, ApiError> {
        std::fs::create_dir_all(dir.join("indexes"))?;
        std::fs::create_dir_all(dir.join("payloads"))?;
        // Uploads that were still being received when the process stopped.
        for entry in std::fs::read_dir(dir.join("payloads"))?.flatten() {
            if entry.path().extension().map_or(false, |e| e == "upload") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let keys: Vec<ApiKey> = std::fs::read(dir.join("keys.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut indexes = BTreeMap::new();
        for entry in std::fs::read_dir(dir.join("indexes"))?.flatten() {
            let path = entry.path();
            if !Index::exists(&path) {
                // A deleted index, or one whose creation never completed.
                let _ = std::fs::remove_dir_all(&path);
                continue;
            }
            match Index::open(&path) {
                Ok(ix) => {
                    let uid = ix.snapshot().manifest.uid.clone();
                    indexes.insert(uid, Arc::new(ix));
                }
                Err(e) => eprintln!("[trailhound] tenant {}: cannot open index at {}: {e}", meta.id, path.display()),
            }
        }
        let usage: Value = std::fs::read(dir.join("usage.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        let tasks = TaskStore::load(&dir.join("tasks.log"))?;
        Ok(Tenant {
            dir,
            meta: RwLock::new(meta),
            indexes: RwLock::new(indexes),
            keys: RwLock::new(keys),
            tasks: Mutex::new(tasks),
            scheduled: AtomicBool::new(false),
            searches: AtomicU64::new(usage["searches"].as_u64().unwrap_or(0)),
            docs_indexed: AtomicU64::new(usage["documentsIndexed"].as_u64().unwrap_or(0)),
            usage_dirty: AtomicBool::new(false),
        })
    }

    pub fn index(&self, uid: &str) -> Option<Arc<Index>> {
        self.indexes.read().unwrap().get(uid).cloned()
    }

    pub fn require_index(&self, uid: &str) -> Result<Arc<Index>, ApiError> {
        self.index(uid).ok_or_else(|| ApiError::index_not_found(uid))
    }

    pub fn save_keys(&self) -> Result<(), ApiError> {
        let keys = self.keys.read().unwrap();
        atomic_write(&self.dir.join("keys.json"), &serde_json::to_vec_pretty(&*keys)?)?;
        Ok(())
    }

    pub fn count_search(&self) {
        self.searches.fetch_add(1, Ordering::Relaxed);
        self.usage_dirty.store(true, Ordering::Relaxed);
    }

    pub fn save_usage(&self) {
        if self.usage_dirty.swap(false, Ordering::Relaxed) {
            let usage = json!({ "searches": self.searches.load(Ordering::Relaxed), "documentsIndexed": self.docs_indexed.load(Ordering::Relaxed) });
            let _ = atomic_write(&self.dir.join("usage.json"), usage.to_string().as_bytes());
        }
    }

    pub fn usage(&self) -> Value {
        let indexes = self.indexes.read().unwrap();
        let (mut docs, mut bytes) = (0u64, 0u64);
        for ix in indexes.values() {
            let s = ix.stats();
            docs += s.number_of_documents;
            bytes += s.disk_bytes;
        }
        json!({
            "indexes": indexes.len(),
            "documents": docs,
            "storageBytes": bytes,
            "searches": self.searches.load(Ordering::Relaxed),
            "documentsIndexed": self.docs_indexed.load(Ordering::Relaxed),
        })
    }

    fn create_index(&self, uid: &str, primary_key: Option<String>, task: u64) -> Result<Arc<Index>, ApiError> {
        let mut indexes = self.indexes.write().unwrap();
        if let Some(max) = self.meta.read().unwrap().quotas.max_indexes {
            if indexes.len() as u64 >= max {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "quota_exceeded", format!("This plan allows at most {max} indexes.")));
            }
        }
        // A fresh directory per creation: files of a deleted index with the
        // same uid may still be held open by a search in flight.
        let dir = self.dir.join("indexes").join(format!("{uid}-{}", &Uuid::new_v4().simple().to_string()[..8]));
        let ix = Arc::new(Index::create(&dir, uid, primary_key, task)?);
        indexes.insert(uid.to_string(), ix.clone());
        Ok(ix)
    }

    fn index_or_create(&self, uid: &str, task: u64) -> Result<Arc<Index>, ApiError> {
        match self.index(uid) {
            Some(ix) => Ok(ix),
            None => self.create_index(uid, None, task),
        }
    }

    /// Record a task durably and hand it to the workers.
    pub fn enqueue(self: &Arc<Self>, platform: &Platform, index_uid: Option<&str>, kind: Kind, details: Value) -> Result<Value, ApiError> {
        let task = {
            let mut store = self.tasks.lock().unwrap();
            let task = Task {
                uid: store.next,
                index_uid: index_uid.map(|s| s.to_string()),
                status: "enqueued".into(),
                kind,
                details,
                error: None,
                enqueued_at: now_rfc3339(),
                started_at: None,
                finished_at: None,
                duration_ms: None,
                retry: false,
                size: 0,
            };
            store.append(&task, true)?;
            store.next += 1;
            store.queue.push_back(task.uid);
            store.tasks.insert(task.uid, task.clone());
            task
        };
        platform.schedule(self);
        Ok(json!({
            "taskUid": task.uid,
            "indexUid": task.index_uid,
            "status": "enqueued",
            "type": task.kind.type_name(),
            "enqueuedAt": task.enqueued_at,
        }))
    }

    fn has_enqueued(&self) -> bool {
        !self.tasks.lock().unwrap().queue.is_empty()
    }

    fn drain(&self) {
        loop {
            let mut task = {
                let mut store = self.tasks.lock().unwrap();
                let Some(uid) = store.queue.pop_front() else { return };
                let Some(task) = store.tasks.get_mut(&uid) else { continue };
                task.status = "processing".into();
                task.started_at = Some(now_rfc3339());
                let snapshot = task.clone();
                let _ = store.append(&snapshot, false);
                snapshot
            };
            let started = std::time::Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(&task))).unwrap_or_else(|_| Err(ApiError::internal("the task panicked")));
            match result {
                Ok(details) => {
                    task.status = "succeeded".into();
                    if let (Value::Object(base), Value::Object(extra)) = (&mut task.details, details) {
                        base.extend(extra);
                    }
                }
                Err(e) => {
                    task.status = "failed".into();
                    task.error = Some(e.to_json());
                }
            }
            task.finished_at = Some(now_rfc3339());
            task.duration_ms = Some(started.elapsed().as_millis() as u64);
            release_memory();
            task.kind.slim();
            self.tasks.lock().unwrap().finish(task);
        }
    }

    /// Refuses a write of `incoming` documents taking `incoming_bytes` as
    /// uploaded when it would take the tenant past its quotas. The upload's
    /// size stands in for what it will take once indexed, which is less.
    pub fn check_document_quota(&self, incoming: u64, incoming_bytes: u64) -> Result<(), ApiError> {
        let quotas = self.meta.read().unwrap().quotas.clone();
        if quotas.max_documents.is_none() && quotas.max_storage_bytes.is_none() {
            return Ok(());
        }
        let usage = self.usage();
        if let Some(max) = quotas.max_documents {
            if usage["documents"].as_u64().unwrap_or(0) + incoming > max {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "quota_exceeded", format!("This plan allows at most {max} documents.")));
            }
        }
        if let Some(max) = quotas.max_storage_bytes {
            if usage["storageBytes"].as_u64().unwrap_or(0).saturating_add(incoming_bytes) > max {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "quota_exceeded", format!("This plan allows at most {max} bytes of storage.")));
            }
        }
        Ok(())
    }

    fn run(&self, task: &Task) -> Result<Value, ApiError> {
        let uid = task.index_uid.clone().unwrap_or_default();
        match &task.kind {
            Kind::IndexCreation { primary_key } => {
                if let Some(ix) = self.index(&uid) {
                    if task.retry && ix.applied_task() >= task.uid {
                        return Ok(json!({}));
                    }
                    return Err(ApiError::new(StatusCode::CONFLICT, "index_already_exists", format!("Index `{uid}` already exists.")));
                }
                self.create_index(&uid, primary_key.clone(), task.uid)?;
                Ok(json!({}))
            }
            Kind::IndexUpdate { primary_key } => {
                let ix = self.require_index(&uid)?;
                if let Some(pk) = primary_key {
                    ix.set_primary_key(pk.clone(), task.uid)?;
                }
                Ok(json!({}))
            }
            Kind::IndexDeletion => {
                let removed = self.indexes.write().unwrap().remove(&uid);
                match removed {
                    Some(ix) => {
                        let n = ix.stats().number_of_documents;
                        ix.destroy()?;
                        let _ = std::fs::remove_dir_all(ix.dir());
                        Ok(json!({ "deletedDocuments": n }))
                    }
                    None if task.retry => Ok(json!({})),
                    None => Err(ApiError::index_not_found(&uid)),
                }
            }
            Kind::DocumentAddition { payload, primary_key, partial } => {
                let path = self.dir.join("payloads").join(payload);
                let result = (|| {
                    let file = match File::open(&path) {
                        Ok(f) => f,
                        // Replayed after a crash that happened once the documents were already in.
                        Err(_) if task.retry => return Ok(json!({})),
                        Err(e) => return Err(ApiError::internal(format!("payload unavailable: {e}"))),
                    };
                    self.check_document_quota(task.details["receivedDocuments"].as_u64().unwrap_or(0), file.metadata().map_or(0, |m| m.len()))?;
                    let ix = self.index_or_create(&uid, task.uid)?;
                    // One line, one document: the payload is never in memory as a whole.
                    let mut docs = BufReader::with_capacity(1 << 20, file).lines().filter_map(|line| match line {
                        Ok(l) if l.trim().is_empty() => None,
                        Ok(l) => Some(Ok(l)),
                        Err(e) => Some(Err(e.into())),
                    });
                    let n = ix.add_documents_stream(&mut docs, primary_key.clone(), *partial, task.uid)?;
                    self.docs_indexed.fetch_add(n, Ordering::Relaxed);
                    self.usage_dirty.store(true, Ordering::Relaxed);
                    Ok(json!({ "indexedDocuments": n }))
                })();
                let _ = std::fs::remove_file(&path);
                result
            }
            Kind::DocumentDeletion { ids } => {
                let n = self.require_index(&uid)?.delete_documents(ids, task.uid)?;
                Ok(json!({ "deletedDocuments": n }))
            }
            Kind::DocumentDeletionByFilter { filter } => {
                let n = self.require_index(&uid)?.delete_by_filter(filter, task.uid)?;
                Ok(json!({ "deletedDocuments": n }))
            }
            Kind::DocumentClear => {
                let n = self.require_index(&uid)?.clear(task.uid)?;
                Ok(json!({ "deletedDocuments": n }))
            }
            Kind::SettingsUpdate { patch } => {
                let ix = self.index_or_create(&uid, task.uid)?;
                let current = ix.snapshot().manifest.settings.clone();
                ix.update_settings(apply_settings_patch(&current, patch)?, task.uid)?;
                Ok(json!({}))
            }
        }
    }
}

/// Merge a partial settings object into `base`. `null` resets a setting.
pub fn apply_settings_patch(base: &Settings, patch: &Value) -> Result<Settings, ApiError> {
    let Value::Object(patch) = patch else {
        return Err(ApiError::bad("bad_request", "Settings must be a JSON object."));
    };
    let Value::Object(mut merged) = serde_json::to_value(base)? else { unreachable!() };
    let Value::Object(defaults) = serde_json::to_value(Settings::default())? else { unreachable!() };
    for (key, value) in patch {
        let Some(default) = defaults.get(key) else { continue };
        let nested = matches!(key.as_str(), "typoTolerance" | "faceting" | "pagination");
        match (value, merged.get_mut(key)) {
            (Value::Null, _) => {
                merged.insert(key.clone(), default.clone());
            }
            (Value::Object(sub), Some(Value::Object(target))) if nested => {
                let empty = Map::new();
                let sub_defaults = default.as_object().unwrap_or(&empty);
                for (k, v) in sub {
                    match (v, target.get_mut(k)) {
                        (Value::Null, _) => {
                            if let Some(d) = sub_defaults.get(k) {
                                target.insert(k.clone(), d.clone());
                            }
                        }
                        (Value::Object(inner), Some(Value::Object(t))) => {
                            for (ik, iv) in inner {
                                t.insert(ik.clone(), iv.clone());
                            }
                        }
                        _ => {
                            target.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    serde_json::from_value(Value::Object(merged)).map_err(|e| ApiError::bad("invalid_settings", format!("Invalid settings: {e}")))
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PlatformFile {
    admin_key: String,
    tenants: Vec<TenantMeta>,
}

/// Which tenant a key belongs to; `None` marks the tenant's master key.
type KeyMap = HashMap<String, (String, Option<String>)>;

pub struct Platform {
    pub dir: PathBuf,
    pub admin_key: String,
    pub tenants: RwLock<BTreeMap<String, Arc<Tenant>>>,
    keymap: RwLock<KeyMap>,
    tx: Mutex<mpsc::Sender<Arc<Tenant>>>,
}

impl Platform {
    pub fn open(dir: &Path, admin_key: Option<String>, master_key: Option<String>) -> Result<Arc<Platform>, ApiError> {
        std::fs::create_dir_all(dir.join("tenants"))?;
        let file: PlatformFile = std::fs::read(dir.join("platform.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let admin_key = admin_key.unwrap_or_else(|| if file.admin_key.is_empty() { new_key() } else { file.admin_key.clone() });
        let mut tenants = BTreeMap::new();
        for meta in file.tenants {
            let id = meta.id.clone();
            match Tenant::load(dir.join("tenants").join(&id), meta) {
                Ok(t) => {
                    tenants.insert(id, Arc::new(t));
                }
                Err(e) => eprintln!("[trailhound] cannot load tenant {id}: {}", e.message),
            }
        }
        let (tx, rx) = mpsc::channel::<Arc<Tenant>>();
        let rx = Arc::new(Mutex::new(rx));
        let workers = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(2));
        for i in 0..workers {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("trailhound-worker-{i}"))
                .spawn(move || loop {
                    let next = rx.lock().unwrap().recv();
                    let Ok(tenant) = next else { return };
                    loop {
                        tenant.drain();
                        tenant.scheduled.store(false, Ordering::SeqCst);
                        // A task may have arrived between the last check and the flag reset.
                        if !tenant.has_enqueued() || tenant.scheduled.swap(true, Ordering::SeqCst) {
                            break;
                        }
                    }
                })
                .map_err(|e| ApiError::internal(e.to_string()))?;
        }
        let platform = Arc::new(Platform { dir: dir.to_path_buf(), admin_key, tenants: RwLock::new(tenants), keymap: RwLock::new(HashMap::new()), tx: Mutex::new(tx) });
        if let Some(master) = master_key {
            // Single-tenant mode: behave like one Meilisearch instance.
            let existing = platform.tenant("default");
            match existing {
                Some(t) => t.meta.write().unwrap().master_key = master,
                None => {
                    platform.create_tenant_with("default".into(), "default".into(), master, Quotas::default())?;
                }
            }
        }
        platform.save()?;
        platform.rebuild_keymap();
        let all: Vec<Arc<Tenant>> = platform.tenants.read().unwrap().values().cloned().collect();
        for t in all {
            if t.has_enqueued() {
                platform.schedule(&t);
            }
        }
        Ok(platform)
    }

    pub fn tenant(&self, id: &str) -> Option<Arc<Tenant>> {
        self.tenants.read().unwrap().get(id).cloned()
    }

    pub fn save(&self) -> Result<(), ApiError> {
        let tenants = self.tenants.read().unwrap().values().map(|t| t.meta.read().unwrap().clone()).collect();
        let file = PlatformFile { admin_key: self.admin_key.clone(), tenants };
        atomic_write(&self.dir.join("platform.json"), &serde_json::to_vec_pretty(&file)?)?;
        Ok(())
    }

    pub fn rebuild_keymap(&self) {
        let mut map = HashMap::new();
        for (id, t) in self.tenants.read().unwrap().iter() {
            map.insert(t.meta.read().unwrap().master_key.clone(), (id.clone(), None));
            for k in t.keys.read().unwrap().iter() {
                map.insert(k.key.clone(), (id.clone(), Some(k.uid.clone())));
            }
        }
        *self.keymap.write().unwrap() = map;
    }

    pub fn resolve(&self, key: &str) -> Option<(Arc<Tenant>, Option<ApiKey>)> {
        let (tenant_id, key_uid) = self.keymap.read().unwrap().get(key).cloned()?;
        let tenant = self.tenant(&tenant_id)?;
        let api_key = match key_uid {
            None => None,
            Some(uid) => Some(tenant.keys.read().unwrap().iter().find(|k| k.uid == uid).cloned()?),
        };
        Some((tenant, api_key))
    }

    fn schedule(&self, tenant: &Arc<Tenant>) {
        if !tenant.scheduled.swap(true, Ordering::SeqCst) {
            let _ = self.tx.lock().unwrap().send(tenant.clone());
        }
    }

    pub fn create_tenant(&self, name: String, quotas: Quotas) -> Result<Arc<Tenant>, ApiError> {
        let id = Uuid::new_v4().simple().to_string()[..12].to_string();
        let tenant = self.create_tenant_with(id, name, new_key(), quotas)?;
        self.save()?;
        self.rebuild_keymap();
        Ok(tenant)
    }

    fn create_tenant_with(&self, id: String, name: String, master_key: String, quotas: Quotas) -> Result<Arc<Tenant>, ApiError> {
        let now = now_rfc3339();
        let meta = TenantMeta { id: id.clone(), name, master_key, created_at: now.clone(), suspended: false, quotas };
        let tenant = Arc::new(Tenant::load(self.dir.join("tenants").join(&id), meta)?);
        let default_key = |name: &str, description: &str, actions: &[&str]| ApiKey {
            name: Some(name.into()),
            description: Some(description.into()),
            key: new_key(),
            uid: Uuid::new_v4().to_string(),
            actions: actions.iter().map(|a| a.to_string()).collect(),
            indexes: vec!["*".into()],
            expires_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        *tenant.keys.write().unwrap() = vec![
            default_key("Default Search API Key", "Use it to search from the frontend", &["search"]),
            default_key("Default Admin API Key", "Use it for anything that is not a search operation. Caution! Do not expose it on a public frontend", &["*"]),
        ];
        tenant.save_keys()?;
        self.tenants.write().unwrap().insert(id, tenant.clone());
        Ok(tenant)
    }

    pub fn delete_tenant(&self, id: &str) -> Result<bool, ApiError> {
        let Some(tenant) = self.tenants.write().unwrap().remove(id) else { return Ok(false) };
        self.save()?;
        self.rebuild_keymap();
        for ix in tenant.indexes.write().unwrap().values() {
            let _ = ix.destroy();
        }
        let _ = std::fs::remove_dir_all(&tenant.dir);
        Ok(true)
    }

    pub fn flush_usage(&self) {
        let all: Vec<Arc<Tenant>> = self.tenants.read().unwrap().values().cloned().collect();
        for t in all {
            t.save_usage();
        }
    }
}
