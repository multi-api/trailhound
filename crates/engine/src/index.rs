//! An index: a manifest plus a set of immutable segments.
//!
//! Every write builds new segment files and then swaps the manifest with an
//! atomic rename. Readers work on an `Arc<State>` snapshot, so searches never
//! block on writes and never see a half-applied change.

use crate::filter;
use crate::segment::{self, MergeInput, Schema, Segment, SegmentBuilder};
use crate::settings::Settings;
use crate::util::{atomic_write, now_rfc3339};
use crate::{Error, Result};
use base64::Engine as _;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock, Weak};

const MAX_SEGMENT_DOCS: u32 = 200_000;
const MAX_SEGMENT_RAW_BYTES: u64 = 128 * 1024 * 1024;
/// Incoming documents are parsed and indexed this many at a time, so the
/// memory used by a write does not depend on the size of the request.
const CHUNK_DOCS: usize = 5_000;
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// A segment under construction is written out once its in-memory index
/// reaches this estimated size, whatever the number of documents.
/// The budget is shared by the indexing threads of a write.
static INDEX_MEM: AtomicUsize = AtomicUsize::new(24 * 1024 * 1024);
/// 0 means: one per available core, up to four.
static INDEX_THREADS: AtomicUsize = AtomicUsize::new(0);
const MIN_FLUSH_MEM: usize = 4 * 1024 * 1024;
/// A write with fewer documents than this is not worth splitting.
const SMALL_WRITE: usize = 2_000;

/// Set how many threads a write may use and how much memory, in bytes, its
/// in-memory index may take before segments are written out.
pub fn configure_indexing(threads: Option<usize>, memory: Option<usize>) {
    if let Some(t) = threads {
        INDEX_THREADS.store(t, Ordering::Relaxed);
    }
    if let Some(m) = memory {
        INDEX_MEM.store(m.max(MIN_FLUSH_MEM), Ordering::Relaxed);
    }
}

fn index_threads() -> usize {
    match INDEX_THREADS.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()).min(4),
        n => n,
    }
}
/// Merging stops growing a segment past this many documents. On 6.4 million
/// Wikipedia articles, segments of up to 4 and of up to 64 million made the
/// light searches faster (a word, a title, a sort: 20 to 60% at the median)
/// and the heavy ones slower (several common words: 10 to 15% under load,
/// and up to twice as long for a search running alone, which spreads its
/// segments over threads), with 1.5 to 2.2 GB more disk in use while merging.
const MAX_MERGED_DOCS: u64 = 1_000_000;
const MERGE_TRIGGER: usize = 8;
/// The most segments joined in one merge.
const MERGE_WIDTH: usize = 16;
const MERGE_RATIO: u64 = 2;
/// zstd level of the document blocks. They are 16 KB each, too small for the
/// highest levels to pay: on Wikipedia text, level 19 took eleven times as
/// long as this one to make the documents 2% smaller, and compressing them
/// again in every large merge was most of the time spent indexing.
const LEVEL: i32 = 6;

#[derive(Serialize, Deserialize, Clone)]
pub struct SegMeta {
    pub id: u64,
    pub docs: u32,
    pub deleted: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub version: u32,
    pub uid: String,
    pub primary_key: Option<String>,
    pub fields: Vec<String>,
    pub settings: Settings,
    pub segments: Vec<SegMeta>,
    pub next_seg: u64,
    /// Highest task id whose effects are contained in this manifest.
    pub applied_task: u64,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SegHandle {
    pub seg: Arc<Segment>,
    pub deleted: RoaringBitmap,
}

impl SegHandle {
    pub fn live(&self) -> RoaringBitmap {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..self.seg.doc_count);
        all -= &self.deleted;
        all
    }

    pub fn live_count(&self) -> u64 {
        self.seg.doc_count as u64 - self.deleted.len()
    }

    fn share(&self) -> SegHandle {
        SegHandle { seg: self.seg.clone(), deleted: self.deleted.clone() }
    }
}

pub struct State {
    pub manifest: Manifest,
    pub segs: Vec<SegHandle>,
    pub schema: Schema,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub number_of_documents: u64,
    pub disk_bytes: u64,
    pub segments: usize,
}

/// A field distribution and the snapshot it was computed from.
type Distribution = (Weak<State>, BTreeMap<String, u64>);

pub struct Index {
    dir: PathBuf,
    state: RwLock<Arc<State>>,
    wlock: Mutex<()>,
    /// The field distribution of one snapshot, kept until the index changes.
    distribution: Mutex<Option<Distribution>>,
}

fn seg_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("seg-{id:08}.seg"))
}

fn doc_id(v: Option<&Value>, pk: &str) -> Result<String> {
    match v {
        None | Some(Value::Null) => Err(Error::user("missing_document_id", format!("Document doesn't have a `{pk}` attribute."))),
        Some(Value::Number(n)) if n.is_u64() || n.is_i64() => Ok(n.to_string()),
        Some(Value::String(s)) if !s.is_empty() && s.len() <= 511 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => Ok(s.clone()),
        Some(other) => Err(Error::user(
            "invalid_document_id",
            format!("Document identifier `{other}` is invalid. A document identifier can be of type integer or string, only composed of alphanumeric characters (a-z A-Z 0-9), hyphens (-) and underscores (_), and can not be more than 511 bytes."),
        )),
    }
}

fn infer_primary_key(doc: &Map<String, Value>, uid: &str) -> Result<String> {
    let cands: Vec<&String> = doc.keys().filter(|k| k.to_lowercase().ends_with("id")).collect();
    match cands.len() {
        0 => Err(Error::user(
            "index_primary_key_no_candidate_found",
            format!("The primary key inference failed as the engine did not find any field ending with `id` in its name. Please specify the primary key manually using the `primaryKey` query parameter. Index `{uid}`."),
        )),
        1 => Ok(cands[0].clone()),
        n => Err(Error::user(
            "index_primary_key_multiple_candidates_found",
            format!("The primary key inference failed as the engine found {n} fields ending with `id` in their names. Please specify the primary key manually using the `primaryKey` query parameter."),
        )),
    }
}

/// A batch of documents for one indexing thread, still as JSON text: the
/// thread that indexes a document is the one that parses it and frees it.
struct Job {
    docs: Vec<(String, String)>,
    schema: Arc<Schema>,
}

struct PartOutcome {
    writer: Writer,
    deleted: Vec<RoaringBitmap>,
    count: u64,
    result: Result<()>,
}

/// Cuts a stream of JSON documents into chunks of `(id, text)`, giving every
/// field its id on the way.
struct ChunkReader<'a> {
    docs: &'a mut dyn Iterator<Item = Result<String>>,
    pk: Option<String>,
    uid: &'a str,
    done: bool,
}

impl ChunkReader<'_> {
    fn next_chunk(&mut self, schema: &mut Schema) -> Result<Vec<(String, String)>> {
        let mut list = Vec::new();
        let mut bytes = 0usize;
        while !self.done && list.len() < CHUNK_DOCS && bytes < CHUNK_BYTES {
            let Some(line) = self.docs.next().transpose()? else {
                self.done = true;
                break;
            };
            if self.pk.is_none() {
                let doc: Value = serde_json::from_str(&line)?;
                let obj = doc.as_object().ok_or_else(|| Error::user("invalid_document_fields", "A document must be a JSON object."))?;
                self.pk = Some(infer_primary_key(obj, self.uid)?);
            }
            let key = self.pk.as_deref().unwrap();
            let id = doc_id(segment::scan_document(&line, schema, Some(key))?.as_ref(), key)?;
            bytes += line.len();
            list.push((id, line));
        }
        Ok(list)
    }
}

/// Builds one or more segment files from a stream of documents.
struct Writer {
    dir: PathBuf,
    next_seg: Arc<AtomicU64>,
    schema: Schema,
    level: i32,
    flush_mem: usize,
    /// The segment being built, with the documents of it that were
    /// superseded before it was even finished.
    cur: Option<(u64, SegmentBuilder, RoaringBitmap)>,
    out: Vec<SegHandle>,
}

impl Writer {
    fn new(dir: &Path, manifest: &Manifest, level: i32) -> Self {
        let schema = Schema::new(manifest.fields.clone(), &manifest.settings);
        Writer::shared(dir, schema, level, Arc::new(AtomicU64::new(manifest.next_seg)), INDEX_MEM.load(Ordering::Relaxed))
    }

    /// A writer that takes its segment ids from a counter shared with others.
    fn shared(dir: &Path, schema: Schema, level: i32, next_seg: Arc<AtomicU64>, flush_mem: usize) -> Self {
        Writer { dir: dir.to_path_buf(), next_seg, schema, level, flush_mem, cur: None, out: Vec::new() }
    }

    fn add(&mut self, pk: &str, doc: &Value) -> Result<()> {
        if self.cur.is_none() {
            let id = self.next_seg.fetch_add(1, Ordering::Relaxed);
            self.cur = Some((id, SegmentBuilder::new(&seg_path(&self.dir, id), self.level)?, RoaringBitmap::new()));
        }
        let (_, builder, _) = self.cur.as_mut().unwrap();
        builder.add_doc(&mut self.schema, pk, doc)?;
        if builder.doc_count() >= MAX_SEGMENT_DOCS || builder.raw_bytes() >= MAX_SEGMENT_RAW_BYTES || builder.mem() >= self.flush_mem {
            self.seal()?;
        }
        Ok(())
    }

    fn seal(&mut self) -> Result<()> {
        if let Some((id, builder, deleted)) = self.cur.take() {
            builder.finish()?;
            let seg = Segment::open(&seg_path(&self.dir, id), id)?;
            self.out.push(SegHandle { seg: Arc::new(seg), deleted });
        }
        Ok(())
    }

    /// Whether the unfinished segment holds a live document with this id.
    fn in_current(&self, id: &str) -> bool {
        self.cur.as_ref().is_some_and(|(_, b, del)| b.pk_lookup(id).is_some_and(|d| !del.contains(d)))
    }

    /// Mark a document of the unfinished segment as superseded.
    fn drop_from_current(&mut self, id: &str) -> bool {
        match self.cur.as_mut() {
            Some((_, b, del)) => b.pk_lookup(id).is_some_and(|d| del.insert(d)),
            None => false,
        }
    }

    /// Discard everything written so far. The unfinished file, if any, is a
    /// `.tmp` that the next open removes.
    fn abort(self) {
        for s in &self.out {
            s.seg.mark_obsolete();
        }
    }

    /// Returns the new segments and updates the manifest's field list and
    /// segment counter.
    fn finish(mut self, manifest: &mut Manifest) -> Result<Vec<SegHandle>> {
        self.seal()?;
        manifest.fields = self.schema.fields;
        manifest.next_seg = self.next_seg.load(Ordering::Relaxed);
        Ok(self.out)
    }
}

impl Index {
    pub fn create(dir: &Path, uid: &str, primary_key: Option<String>, task: u64) -> Result<Index> {
        std::fs::create_dir_all(dir)?;
        let now = now_rfc3339();
        let manifest = Manifest {
            version: 1,
            uid: uid.to_string(),
            primary_key,
            fields: vec![],
            settings: Settings::default(),
            segments: vec![],
            next_seg: 1,
            applied_task: task,
            created_at: now.clone(),
            updated_at: now,
        };
        atomic_write(&dir.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
        Index::open(dir)
    }

    pub fn exists(dir: &Path) -> bool {
        dir.join("manifest.json").is_file()
    }

    pub fn open(dir: &Path) -> Result<Index> {
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
        let mut segs = Vec::new();
        for meta in &manifest.segments {
            let seg = Segment::open(&seg_path(dir, meta.id), meta.id)?;
            let raw = base64::engine::general_purpose::STANDARD.decode(&meta.deleted).map_err(|_| Error::corrupt("bad deletion bitmap"))?;
            let deleted = if raw.is_empty() { RoaringBitmap::new() } else { RoaringBitmap::deserialize_from(&raw[..]).map_err(|_| Error::corrupt("bad deletion bitmap"))? };
            segs.push(SegHandle { seg: Arc::new(seg), deleted });
        }
        // Remove leftovers of writes that were interrupted before their
        // manifest swap: they were never visible, so nothing is lost.
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let orphan_seg = name.starts_with("seg-") && name.ends_with(".seg") && !manifest.segments.iter().any(|m| seg_path(dir, m.id).file_name() == Some(entry.file_name().as_os_str()));
            if name.ends_with(".tmp") || orphan_seg {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let schema = Schema::new(manifest.fields.clone(), &manifest.settings);
        Ok(Index { dir: dir.to_path_buf(), state: RwLock::new(Arc::new(State { manifest, segs, schema })), wlock: Mutex::new(()), distribution: Mutex::new(None) })
    }

    pub fn snapshot(&self) -> Arc<State> {
        self.state.read().unwrap().clone()
    }

    pub fn applied_task(&self) -> u64 {
        self.snapshot().manifest.applied_task
    }

    fn commit(&self, mut manifest: Manifest, segs: Vec<SegHandle>, task: u64) -> Result<()> {
        manifest.segments = segs
            .iter()
            .map(|s| {
                let mut raw = Vec::new();
                if !s.deleted.is_empty() {
                    s.deleted.serialize_into(&mut raw).expect("write to vec");
                }
                SegMeta { id: s.seg.id, docs: s.seg.doc_count, deleted: base64::engine::general_purpose::STANDARD.encode(raw) }
            })
            .collect();
        manifest.updated_at = now_rfc3339();
        manifest.applied_task = manifest.applied_task.max(task);
        atomic_write(&self.dir.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
        let schema = Schema::new(manifest.fields.clone(), &manifest.settings);
        let new = Arc::new(State { manifest, segs, schema });
        let old = std::mem::replace(&mut *self.state.write().unwrap(), new.clone());
        for s in &old.segs {
            if !new.segs.iter().any(|n| n.seg.id == s.seg.id) {
                s.seg.mark_obsolete();
            }
        }
        Ok(())
    }

    /// Mark the index as gone. Segment files are removed as readers finish.
    pub fn destroy(&self) -> Result<()> {
        let _w = self.wlock.lock().unwrap();
        let _ = std::fs::remove_file(self.dir.join("manifest.json"));
        let st = self.snapshot();
        for s in &st.segs {
            s.seg.mark_obsolete();
        }
        let mut manifest = st.manifest.clone();
        manifest.segments.clear();
        let schema = Schema::new(vec![], &manifest.settings);
        *self.state.write().unwrap() = Arc::new(State { manifest, segs: vec![], schema });
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn set_primary_key(&self, pk: String, task: u64) -> Result<()> {
        let _w = self.wlock.lock().unwrap();
        let st = self.snapshot();
        if st.manifest.primary_key.as_deref() == Some(pk.as_str()) {
            return Ok(());
        }
        if st.segs.iter().any(|s| s.live_count() > 0) {
            return Err(Error::user("index_primary_key_already_exists", format!("Index `{}` already has a primary key.", st.manifest.uid)));
        }
        let mut manifest = st.manifest.clone();
        manifest.primary_key = Some(pk);
        self.commit(manifest, vec![], task)
    }

    /// Add or replace documents. With `partial`, the fields of an incoming
    /// document are merged into the stored document with the same id.
    pub fn add_documents(&self, docs: Vec<Value>, pk_hint: Option<String>, partial: bool, task: u64) -> Result<u64> {
        self.add_documents_stream(&mut docs.into_iter().map(|d| Ok(d.to_string())), pk_hint, partial, task)
    }

    /// Like `add_documents`, reading one JSON document per item. Documents
    /// are read a chunk at a time and spread over several threads by the
    /// hash of their id, each thread parsing its share and building its own
    /// segments; nothing becomes visible until the single commit at the end,
    /// and an error leaves the index untouched.
    pub fn add_documents_stream(&self, docs: &mut dyn Iterator<Item = Result<String>>, pk_hint: Option<String>, partial: bool, task: u64) -> Result<u64> {
        let _w = self.wlock.lock().unwrap();
        let st = self.snapshot();
        let mut manifest = st.manifest.clone();
        let mut schema = Schema::new(manifest.fields.clone(), &manifest.settings);
        let mut reader = ChunkReader { docs, pk: manifest.primary_key.clone().or(pk_hint), uid: &st.manifest.uid, done: false };
        // Field ids are handed out by the reader, in document order, so every
        // thread agrees on them.
        let mut chunk = reader.next_chunk(&mut schema)?;
        if chunk.is_empty() {
            return Ok(0);
        }
        let parts = if reader.done && chunk.len() < SMALL_WRITE { 1 } else { index_threads() };
        let flush_mem = (INDEX_MEM.load(Ordering::Relaxed) / parts).max(MIN_FLUSH_MEM);
        let next_seg = Arc::new(AtomicU64::new(manifest.next_seg));

        let (fed, outs) = std::thread::scope(|scope| {
            let mut txs = Vec::with_capacity(parts);
            let mut handles = Vec::with_capacity(parts);
            for _ in 0..parts {
                // No buffering: a thread takes its next batch when it is ready for it.
                let (tx, rx) = mpsc::sync_channel::<Job>(0);
                let writer = Writer::shared(&self.dir, schema.clone(), LEVEL, next_seg.clone(), flush_mem);
                let st = &st;
                handles.push(scope.spawn(move || self.index_part(st, rx, writer, partial)));
                txs.push(tx);
            }
            let mut shared = Arc::new(schema.clone());
            let fed = (|| -> Result<()> {
                loop {
                    if schema.fields.len() != shared.fields.len() {
                        shared = Arc::new(schema.clone());
                    }
                    let mut split: Vec<Vec<(String, String)>> = (0..parts).map(|_| Vec::new()).collect();
                    for (id, line) in chunk.drain(..) {
                        let part = if parts == 1 { 0 } else { ((segment::fx_hash(id.as_bytes()) >> 32) % parts as u64) as usize };
                        split[part].push((id, line));
                    }
                    for (part, docs) in split.into_iter().enumerate() {
                        if !docs.is_empty() && txs[part].send(Job { docs, schema: shared.clone() }).is_err() {
                            return Err(Error::corrupt("an indexing thread stopped early"));
                        }
                    }
                    if reader.done {
                        return Ok(());
                    }
                    chunk = reader.next_chunk(&mut schema)?;
                    if chunk.is_empty() {
                        return Ok(());
                    }
                }
            })();
            drop(txs);
            let outs: Vec<PartOutcome> = handles.into_iter().map(|h| h.join().expect("indexing thread panicked")).collect();
            (fed, outs)
        });

        // The error of a thread is the cause; the feeder only notices it.
        if fed.is_err() || outs.iter().any(|o| o.result.is_err()) {
            let mut first = None;
            for out in outs {
                out.writer.abort();
                if first.is_none() {
                    first = out.result.err();
                }
            }
            return Err(first.or(fed.err()).unwrap_or_else(|| Error::corrupt("indexing failed")));
        }

        let mut deleted: Vec<RoaringBitmap> = st.segs.iter().map(|s| s.deleted.clone()).collect();
        let mut new = Vec::new();
        let mut total = 0u64;
        for out in outs {
            for (all, mine) in deleted.iter_mut().zip(&out.deleted) {
                *all |= mine;
            }
            total += out.count;
            new.extend(out.writer.out);
        }
        new.sort_by_key(|s| s.seg.id);
        manifest.primary_key = reader.pk;
        manifest.fields = schema.fields;
        manifest.next_seg = next_seg.load(Ordering::Relaxed);
        let mut segs: Vec<SegHandle> = st.segs.iter().zip(deleted).filter(|(s, d)| d.len() < s.seg.doc_count as u64).map(|(s, d)| SegHandle { seg: s.seg.clone(), deleted: d }).collect();
        for s in new {
            if s.deleted.len() < s.seg.doc_count as u64 {
                segs.push(s);
            } else {
                s.seg.mark_obsolete();
            }
        }
        self.commit(manifest, segs, task)?;
        self.maybe_merge(task)?;
        Ok(total)
    }

    /// One indexing thread: parses the documents it is sent, writes them
    /// into its own segments, and records which stored documents they replace.
    fn index_part(&self, st: &State, rx: mpsc::Receiver<Job>, mut writer: Writer, partial: bool) -> PartOutcome {
        let mut deleted: Vec<RoaringBitmap> = st.segs.iter().map(|s| s.deleted.clone()).collect();
        let mut count = 0u64;
        let result = (|| -> Result<()> {
            let mut current: Option<Arc<Schema>> = None;
            for job in rx.iter() {
                if current.as_ref().is_some_and(|c| !Arc::ptr_eq(c, &job.schema)) {
                    writer.schema = (*job.schema).clone();
                }
                current = Some(job.schema);
                // One version per id within the batch.
                let mut list: Vec<(String, Value)> = Vec::with_capacity(job.docs.len());
                let mut pos: HashMap<String, usize> = HashMap::new();
                for (id, line) in job.docs {
                    let doc: Value = serde_json::from_str(&line)?;
                    match pos.get(&id) {
                        Some(&i) if partial => merge_into(&mut list[i].1, doc),
                        Some(&i) => list[i].1 = doc,
                        None => {
                            pos.insert(id.clone(), list.len());
                            list.push((id, doc));
                        }
                    }
                }
                drop(pos);
                for (id, mut doc) in list {
                    self.supersede(st, &mut deleted, &mut writer, &id, &mut doc, partial)?;
                    writer.add(&id, &doc)?;
                    count += 1;
                }
            }
            writer.seal()
        })();
        PartOutcome { writer, deleted, count, result }
    }

    /// Find the stored version of a document that is about to be written
    /// again, mark it deleted and, for a partial update, merge the incoming
    /// fields into it. The newest version is looked for first.
    fn supersede(&self, st: &State, deleted: &mut [RoaringBitmap], writer: &mut Writer, id: &str, doc: &mut Value, partial: bool) -> Result<()> {
        if writer.in_current(id) {
            if partial {
                // The old version must be readable: finish its segment first.
                writer.seal()?;
            } else {
                writer.drop_from_current(id);
                return Ok(());
            }
        }
        let fresh = writer.out.iter_mut().rev().map(|s| (s.seg.clone(), &mut s.deleted));
        let stored = st.segs.iter().zip(deleted.iter_mut()).map(|(s, d)| (s.seg.clone(), d));
        for (seg, del) in fresh.chain(stored) {
            if let Some(d) = seg.pk_lookup(id)
                && !del.contains(d) {
                    if partial {
                        let mut old = seg.doc(d)?;
                        merge_into(&mut old, std::mem::take(doc));
                        *doc = old;
                    }
                    del.insert(d);
                    return Ok(());
                }
        }
        Ok(())
    }

    fn apply_deletions(&self, st: &State, deleted: Vec<RoaringBitmap>, task: u64) -> Result<()> {
        let segs: Vec<SegHandle> = st.segs.iter().zip(deleted).filter(|(s, d)| d.len() < s.seg.doc_count as u64).map(|(s, d)| SegHandle { seg: s.seg.clone(), deleted: d }).collect();
        self.commit(st.manifest.clone(), segs, task)
    }

    pub fn delete_documents(&self, ids: &[String], task: u64) -> Result<u64> {
        let _w = self.wlock.lock().unwrap();
        let st = self.snapshot();
        let mut deleted: Vec<RoaringBitmap> = st.segs.iter().map(|s| s.deleted.clone()).collect();
        let mut n = 0;
        for id in ids {
            for (i, s) in st.segs.iter().enumerate() {
                if let Some(d) = s.seg.pk_lookup(id)
                    && deleted[i].insert(d) {
                        n += 1;
                    }
            }
        }
        self.apply_deletions(&st, deleted, task)?;
        Ok(n)
    }

    pub fn delete_by_filter(&self, filter: &Value, task: u64) -> Result<u64> {
        let _w = self.wlock.lock().unwrap();
        let st = self.snapshot();
        let matched = universe(&st, Some(filter))?;
        let n = matched.iter().map(|b| b.len()).sum();
        let deleted = st.segs.iter().zip(matched).map(|(s, m)| &s.deleted | m).collect();
        self.apply_deletions(&st, deleted, task)?;
        Ok(n)
    }

    pub fn clear(&self, task: u64) -> Result<u64> {
        let _w = self.wlock.lock().unwrap();
        let st = self.snapshot();
        let n = st.segs.iter().map(|s| s.live_count()).sum();
        let mut manifest = st.manifest.clone();
        manifest.fields.clear();
        self.commit(manifest, vec![], task)?;
        Ok(n)
    }

    pub fn update_settings(&self, new: Settings, task: u64) -> Result<()> {
        let _w = self.wlock.lock().unwrap();
        let st = self.snapshot();
        let mut manifest = st.manifest.clone();
        let reindex = manifest.settings.needs_reindex(&new);
        manifest.settings = new;
        if !reindex || st.segs.is_empty() {
            let segs = st.segs.iter().map(|s| s.share()).collect();
            return self.commit(manifest, segs, task);
        }
        manifest.fields.clear();
        let all: Vec<usize> = (0..st.segs.len()).collect();
        let segs = self.rewrite(&st, &all, &mut manifest)?;
        self.commit(manifest, segs, task)?;
        self.maybe_merge(task)
    }

    /// Re-index the live documents of the chosen segments into new ones.
    fn rewrite(&self, st: &State, which: &[usize], manifest: &mut Manifest) -> Result<Vec<SegHandle>> {
        let pk = manifest.primary_key.clone().ok_or_else(|| Error::corrupt("documents without a primary key"))?;
        let mut writer = Writer::new(&self.dir, manifest, LEVEL);
        for &i in which {
            let handle = &st.segs[i];
            handle.seg.for_each_doc(|docid, bytes| {
                if handle.deleted.contains(docid) {
                    return Ok(());
                }
                let doc: Value = serde_json::from_slice(bytes)?;
                let id = doc_id(doc.get(&pk), &pk)?;
                writer.add(&id, &doc)
            })?;
        }
        writer.finish(manifest)
    }

    /// Keep the number of segments bounded by merging the smallest ones.
    /// Merging also drops deleted documents.
    fn maybe_merge(&self, task: u64) -> Result<()> {
        loop {
            let st = self.snapshot();
            let sizes: Vec<(u64, u64)> = st.segs.iter().map(|s| (s.live_count(), s.seg.sort_bytes())).collect();
            let Some(mut picked) = pick_merge(&sizes) else { return Ok(()) };
            let total: u64 = picked.iter().map(|i| sizes[*i].0).sum();
            picked.sort_unstable();
            let mut manifest = st.manifest.clone();
            let mut segs: Vec<SegHandle> = st.segs.iter().enumerate().filter(|(i, _)| !picked.contains(i)).map(|(_, s)| s.share()).collect();
            if total > 0 {
                let id = manifest.next_seg;
                manifest.next_seg += 1;
                let inputs: Vec<MergeInput> = picked.iter().map(|i| MergeInput { seg: &st.segs[*i].seg, deleted: &st.segs[*i].deleted }).collect();
                segment::merge(&inputs, &st.schema, &seg_path(&self.dir, id), LEVEL)?;
                segs.push(SegHandle { seg: Arc::new(Segment::open(&seg_path(&self.dir, id), id)?), deleted: RoaringBitmap::new() });
            }
            self.commit(manifest, segs, task)?;
        }
    }

    pub fn get_document(&self, id: &str) -> Result<Option<Value>> {
        let st = self.snapshot();
        for s in &st.segs {
            if let Some(d) = s.seg.pk_lookup(id)
                && !s.deleted.contains(d) {
                    return Ok(Some(s.seg.doc(d)?));
                }
        }
        Ok(None)
    }

    pub fn list_documents(&self, offset: usize, limit: usize, filter: Option<&Value>) -> Result<(Vec<Value>, u64)> {
        let st = self.snapshot();
        let cands = universe(&st, filter)?;
        let total = cands.iter().map(|b| b.len()).sum();
        let mut out = Vec::new();
        let mut skipped = 0usize;
        'outer: for (i, bm) in cands.iter().enumerate() {
            if skipped + bm.len() as usize <= offset {
                skipped += bm.len() as usize;
                continue;
            }
            for d in bm {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if out.len() >= limit {
                    break 'outer;
                }
                out.push(st.segs[i].seg.doc(d)?);
            }
        }
        Ok((out, total))
    }

    pub fn stats(&self) -> IndexStats {
        let st = self.snapshot();
        IndexStats {
            number_of_documents: st.segs.iter().map(|s| s.live_count()).sum(),
            disk_bytes: st.segs.iter().map(|s| s.seg.file_size).sum(),
            segments: st.segs.len(),
        }
    }

    /// How many documents hold each field. Deleted and replaced documents
    /// are not counted, although they stay in their segment until a merge.
    pub fn field_distribution(&self) -> Result<BTreeMap<String, u64>> {
        let st = self.snapshot();
        if let Some((of, dist)) = self.distribution.lock().unwrap().as_ref()
            && of.upgrade().is_some_and(|s| Arc::ptr_eq(&s, &st)) {
                return Ok(dist.clone());
            }
        let mut counts: BTreeMap<u16, u64> = BTreeMap::new();
        for s in &st.segs {
            for (fid, n) in &s.seg.field_counts {
                *counts.entry(*fid).or_insert(0) += *n as u64;
            }
            if !s.deleted.is_empty() {
                for (fid, n) in s.seg.field_counts_of(&s.deleted, &st.schema)? {
                    let c = counts.entry(fid).or_insert(0);
                    *c = c.saturating_sub(n as u64);
                }
            }
        }
        let dist: BTreeMap<String, u64> = counts.into_iter().filter(|(_, n)| *n > 0).filter_map(|(fid, n)| st.schema.fields.get(fid as usize).map(|name| (name.clone(), n))).collect();
        *self.distribution.lock().unwrap() = Some((Arc::downgrade(&st), dist.clone()));
        Ok(dist)
    }

    pub fn search(&self, q: &crate::SearchQuery) -> Result<Value> {
        crate::search::search(&self.snapshot(), q)
    }
}

/// Which segments to merge next, given the live documents and the bytes of
/// sort columns of each; `None` when there is nothing to do.
///
/// Above `MERGE_TRIGGER` segments the smallest are merged, as many as are of
/// a size: a segment joins only while it is at most `MERGE_RATIO` times what
/// has been gathered below it. A write that leaves a dozen small segments
/// has them joined in one pass instead of three, and a large segment is not
/// written again to take in a few documents.
fn pick_merge(sizes: &[(u64, u64)]) -> Option<Vec<usize>> {
    if sizes.len() <= MERGE_TRIGGER {
        return None;
    }
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|i| sizes[*i].0);
    let mut picked = vec![order[0]];
    let (mut docs, mut sort) = sizes[order[0]];
    for &i in &order[1..] {
        let (d, s) = sizes[i];
        // The strings of a sort column are addressed with 32 bits.
        if picked.len() >= MERGE_WIDTH || docs + d > MAX_MERGED_DOCS || sort + s > u32::MAX as u64 {
            break;
        }
        if picked.len() >= 2 && d > MERGE_RATIO * docs {
            break;
        }
        picked.push(i);
        docs += d;
        sort += s;
    }
    (picked.len() >= 2).then_some(picked)
}

/// Live documents of every segment, restricted by an optional filter.
pub fn universe(st: &State, filter: Option<&Value>) -> Result<Vec<RoaringBitmap>> {
    let parsed = match filter {
        Some(v) => filter::parse_value(v)?,
        None => None,
    };
    universe_of(st, parsed.as_ref())
}

/// Like `universe`, for a filter that has been parsed already.
pub fn universe_of(st: &State, filter: Option<&filter::Filter>) -> Result<Vec<RoaringBitmap>> {
    let mut out = Vec::with_capacity(st.segs.len());
    for s in &st.segs {
        let mut live = s.live();
        if let Some(f) = filter {
            live &= filter::eval(f, &s.seg, &st.schema, &st.manifest.settings.filterable_attributes)?;
        }
        out.push(live);
    }
    Ok(out)
}

fn merge_into(target: &mut Value, patch: Value) {
    if let (Value::Object(t), Value::Object(p)) = (target, patch) {
        for (k, v) in p {
            t.insert(k, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(docs: &[u64]) -> Option<Vec<usize>> {
        let sizes: Vec<(u64, u64)> = docs.iter().map(|d| (*d, 0)).collect();
        pick_merge(&sizes).map(|mut p| {
            p.sort_unstable();
            p
        })
    }

    #[test]
    fn merges_take_the_small_segments_and_leave_the_large() {
        // Nothing to do up to the trigger.
        assert_eq!(pick(&[5; 8]), None);
        // The small ones of a write go in one merge, without the large ones.
        assert_eq!(pick(&[900_000, 6_000, 6_000, 400_000, 6_000, 6_000, 6_000, 6_000, 6_000, 6_000]), Some(vec![1, 2, 4, 5, 6, 7, 8, 9]));
        // A larger segment joins once those below it add up to half of it.
        assert_eq!(pick(&[300, 100, 100, 100, 50_000, 50_000, 50_000, 50_000, 50_000]), Some(vec![0, 1, 2, 3]));
        assert_eq!(pick(&[700, 100, 100, 100, 50_000, 50_000, 50_000, 50_000, 50_000]), Some(vec![1, 2, 3]));
        // Never more than the width, never past the cap.
        assert_eq!(pick(&[10; 40]).map(|p| p.len()), Some(MERGE_WIDTH));
        assert_eq!(pick(&[400_000; 9]).map(|p| p.len()), Some(2));
        assert_eq!(pick(&[600_000; 9]), None);
        // The two smallest are merged whatever their sizes, to get back under the trigger.
        assert_eq!(pick(&[1, 100, 900, 900, 900, 900, 900, 900, 900]), Some(vec![0, 1]));
        // Sort columns that would not fit in one segment stay apart.
        assert_eq!(pick_merge(&[(10, 3 << 30); 9]), None);
    }
}
