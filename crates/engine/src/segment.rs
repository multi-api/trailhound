//! Immutable segment files.
//!
//! Layout: MAGIC, ten sections, footer. The footer holds the offset, length
//! and CRC32 of every section, its own CRC32, and MAGIC again. Documents are
//! stored as zstd blocks, each with its own CRC, so a damaged block is
//! detected when it is read instead of being served.

use crate::settings::{attr_matches, Settings};
use crate::tokenizer::{normalize_facet, tokenize, BREAK_GAP};
use crate::util::{encode_f64, read_docset, read_varint, write_docset, write_varint};
use crate::{Error, Result};
use fst::map::OpBuilder;
use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use memmap2::Mmap;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAGIC: &[u8; 8] = b"THSEG002";
/// The previous format, which is still read. Its sort columns come without
/// the documents in sorted order; a merge writes them.
const MAGIC_V1: &[u8; 8] = b"THSEG001";
/// Reading one document decompresses its whole block: small blocks keep
/// that cheap, at the price of a slightly lower compression ratio.
const BLOCK_SIZE: usize = 16 * 1024;
const N_SECTIONS: usize = 10;
const FOOTER_LEN: usize = N_SECTIONS * 20 + 4 + 8;
const S_DOCS: usize = 0;
const S_DOCIDX: usize = 1;
const S_TERMS: usize = 2;
const S_TERMS_POST: usize = 3;
const S_FACETS: usize = 4;
const S_FACETS_POST: usize = 5;
const S_PKS: usize = 6;
const S_SORT: usize = 7;
const S_META: usize = 8;
const S_POS: usize = 9;
/// Position gap between two values of the same field, so that phrases and
/// proximity never span separate values.
const VALUE_GAP: u32 = 8;
/// A merge lets go of the pages of its inputs every time it has written
/// this much.
const RELIEF_EVERY: u64 = 32 << 20;

pub const TAG_EXISTS: u8 = 1;
pub const TAG_NULL: u8 = 2;
pub const TAG_NUM: u8 = 3;
pub const TAG_STR: u8 = 4;

/// Unions of runs of consecutive numeric values of a field, so a range
/// filter reads a few precomputed sets instead of one per distinct value.
/// Keys: field, tag, first value of the run, last value of the run.
pub const TAG_NUM_L1: u8 = 5;
pub const TAG_NUM_L2: u8 = 6;
const LEVEL_FANOUT: usize = 64;

#[derive(Default)]
struct RangeLevels {
    groups: Vec<([u8; 8], [u8; 8], RoaringBitmap)>,
    in_last: usize,
    values: usize,
}

impl RangeLevels {
    /// Values must arrive in ascending order.
    fn push(&mut self, value: &[u8], docs: &RoaringBitmap) {
        let Ok(v) = <[u8; 8]>::try_from(value) else { return };
        if self.in_last == 0 || self.in_last == LEVEL_FANOUT {
            self.groups.push((v, v, docs.clone()));
            self.in_last = 1;
        } else {
            let last = self.groups.last_mut().unwrap();
            last.1 = v;
            last.2 |= docs;
            self.in_last += 1;
        }
        self.values += 1;
    }

    /// The level entries of a field, in key order, and reset for the next.
    fn finish(&mut self, fid: u16) -> Vec<(Vec<u8>, RoaringBitmap)> {
        let groups = std::mem::take(&mut self.groups);
        let values = std::mem::take(&mut self.values);
        self.in_last = 0;
        if values <= LEVEL_FANOUT {
            return Vec::new();
        }
        let key = |tag: u8, lo: &[u8; 8], hi: &[u8; 8]| facet_key(fid, tag, &[lo.as_slice(), hi.as_slice()].concat());
        let mut upper = Vec::new();
        if groups.len() > LEVEL_FANOUT {
            for chunk in groups.chunks(LEVEL_FANOUT) {
                let mut all = RoaringBitmap::new();
                for g in chunk {
                    all |= &g.2;
                }
                upper.push((key(TAG_NUM_L2, &chunk[0].0, &chunk[chunk.len() - 1].1), all));
            }
        }
        let mut out: Vec<(Vec<u8>, RoaringBitmap)> = groups.into_iter().map(|(lo, hi, bm)| (key(TAG_NUM_L1, &lo, &hi), bm)).collect();
        out.extend(upper);
        out
    }
}

pub const F_SEARCH: u8 = 1;
pub const F_FILTER: u8 = 2;
pub const F_SORT: u8 = 4;

pub fn facet_key(fid: u16, tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(3 + payload.len());
    k.extend_from_slice(&fid.to_be_bytes());
    k.push(tag);
    k.extend_from_slice(payload);
    k
}

/// Maps flattened field names (`a.b.c`) to small ids and records which
/// fields are searchable, filterable and sortable under the given settings.
#[derive(Clone)]
pub struct Schema {
    pub fields: Vec<String>,
    ids: HashMap<String, u16>,
    flags: Vec<u8>,
    searchable: Vec<String>,
    filterable: Vec<String>,
    sortable: Vec<String>,
}

impl Schema {
    pub fn new(fields: Vec<String>, s: &Settings) -> Self {
        let mut schema = Schema {
            fields: Vec::new(),
            ids: HashMap::new(),
            flags: Vec::new(),
            searchable: s.searchable_attributes.clone(),
            filterable: s.filterable_attributes.clone(),
            sortable: s.sortable_attributes.clone(),
        };
        for f in fields {
            let _ = schema.id_or_insert(&f);
        }
        schema
    }

    pub fn id(&self, name: &str) -> Option<u16> {
        self.ids.get(name).copied()
    }

    pub fn id_or_insert(&mut self, name: &str) -> Result<u16> {
        if let Some(id) = self.ids.get(name) {
            return Ok(*id);
        }
        if self.fields.len() >= u16::MAX as usize {
            return Err(Error::user("max_fields_limit_exceeded", "A document cannot contain more than 65,535 fields."));
        }
        let id = self.fields.len() as u16;
        let mut flags = 0;
        if attr_matches(&self.searchable, name) {
            flags |= F_SEARCH;
        }
        if attr_matches(&self.filterable, name) {
            flags |= F_FILTER;
        }
        if attr_matches(&self.sortable, name) {
            flags |= F_SORT;
        }
        self.fields.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        self.flags.push(flags);
        Ok(id)
    }

    pub fn flags(&self, fid: u16) -> u8 {
        self.flags[fid as usize]
    }

    /// Field ids covered by an attribute name, including nested fields.
    pub fn ids_for(&self, attr: &str) -> Vec<u16> {
        let list = [attr.to_string()];
        (0..self.fields.len()).filter(|i| attr_matches(&list, &self.fields[*i])).map(|i| i as u16).collect()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
struct SortColMeta {
    num: Option<(u64, u64)>,
    ord: Option<(u64, u64)>,
    stroffs: (u64, u64),
    strdata: (u64, u64),
    /// The documents that have a value, as `u32` ids in ascending order of
    /// it: numbers, then strings, and by id where the values are equal.
    #[serde(default)]
    order: Option<(u64, u64)>,
}

/// A document's place in the order of a sort column: 0 and its number, 1 and
/// the rank of its string among the segment's strings, or 2 without a value.
pub type Rank = (u8, f64);

pub fn cmp_rank(a: &Rank, b: &Rank) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
}

fn col_num(buf: &[u8], col: &SortColMeta, d: usize) -> f64 {
    match col.num {
        Some((off, _)) => {
            let p = off as usize + d * 8;
            f64::from_le_bytes(buf[p..p + 8].try_into().unwrap())
        }
        None => f64::NAN,
    }
}

fn col_ord(buf: &[u8], col: &SortColMeta, d: usize) -> u32 {
    match col.ord {
        Some((off, _)) => {
            let p = off as usize + d * 4;
            u32::from_le_bytes(buf[p..p + 4].try_into().unwrap())
        }
        None => u32::MAX,
    }
}

fn col_rank(buf: &[u8], col: &SortColMeta, d: usize) -> Rank {
    let f = col_num(buf, col, d);
    if !f.is_nan() {
        return (0, f);
    }
    match col_ord(buf, col, d) {
        u32::MAX => (2, 0.0),
        ord => (1, ord as f64),
    }
}

/// The documents of a column that have a value, in the order of `col_rank`.
fn sorted_docs(buf: &[u8], col: &SortColMeta, n: usize) -> Vec<u32> {
    let mut order: Vec<u32> = (0..n as u32).filter(|d| col_rank(buf, col, *d as usize).0 != 2).collect();
    order.sort_unstable_by(|a, b| cmp_rank(&col_rank(buf, col, *a as usize), &col_rank(buf, col, *b as usize)).then(a.cmp(b)));
    order
}

/// The sorted order of one column: read from the file, or worked out for a
/// segment written before the order was stored.
enum Order<'a> {
    Stored(&'a [u8]),
    Built(Vec<u32>),
}

impl Order<'_> {
    fn len(&self) -> usize {
        match self {
            Order::Stored(b) => b.len() / 4,
            Order::Built(v) => v.len(),
        }
    }

    fn get(&self, i: usize) -> u32 {
        match self {
            Order::Stored(b) => u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()),
            Order::Built(v) => v[i],
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct MetaFile {
    doc_count: u32,
    field_counts: Vec<(u16, u32)>,
    sort: Vec<(u16, SortColMeta)>,
    /// The filterable fields in which some document holds more than one
    /// number. Absent from segments written before it was recorded.
    #[serde(default)]
    multi_num: Option<Vec<u16>>,
}

/// Postings of one term in one field while a segment is being built, in a
/// single byte buffer: per document, the docid delta as a varint followed by
/// its token positions as varints of `(delta << 1) | more`. The first
/// position delta is the position itself, and `more` says whether another
/// position of the same document follows.
#[derive(Default)]
struct TermField {
    data: Vec<u8>,
    n: u32,
    last_doc: u32,
    last_pos: u32,
    /// Where the most recent position varint starts, to set its `more` bit.
    last_at: u32,
}

impl TermField {
    /// Returns the number of bytes the buffer grew by.
    fn add(&mut self, docid: u32, pos: u32) -> usize {
        let before = self.data.len();
        let delta = if self.n == 0 || self.last_doc != docid {
            write_varint(&mut self.data, (docid - self.last_doc) as u64);
            self.n += 1;
            self.last_doc = docid;
            pos
        } else {
            self.data[self.last_at as usize] |= 1;
            pos - self.last_pos
        };
        self.last_at = self.data.len() as u32;
        write_varint(&mut self.data, (delta as u64) << 1);
        self.last_pos = pos;
        self.data.len() - before
    }

    /// The documents, and the positions in the on-disk encoding.
    fn split(&self) -> Result<(RoaringBitmap, Vec<u8>)> {
        let mut docs = RoaringBitmap::new();
        let mut positions = Vec::with_capacity(self.data.len());
        let (mut at, mut doc) = (0usize, 0u32);
        for _ in 0..self.n {
            doc += read_varint(&self.data, &mut at)? as u32;
            docs.push(doc);
            loop {
                let start = at;
                let v = read_varint(&self.data, &mut at)?;
                positions.extend_from_slice(&self.data[start..at]);
                if v & 1 == 0 {
                    break;
                }
            }
        }
        Ok((docs, positions))
    }
}

/// An ascending list of docids as delta varints.
#[derive(Default)]
struct DocList {
    data: Vec<u8>,
    n: u32,
    last: u32,
}

impl DocList {
    fn insert(&mut self, docid: u32) -> usize {
        if self.n > 0 && self.last == docid {
            return 0;
        }
        let before = self.data.len();
        write_varint(&mut self.data, (docid - self.last) as u64);
        self.n += 1;
        self.last = docid;
        self.data.len() - before
    }

    fn to_bitmap(&self) -> Result<RoaringBitmap> {
        let mut bm = RoaringBitmap::new();
        let (mut at, mut doc) = (0usize, 0u32);
        for _ in 0..self.n {
            doc += read_varint(&self.data, &mut at)? as u32;
            bm.push(doc);
        }
        Ok(bm)
    }
}

/// A fast non-cryptographic hasher for the term table of a segment under
/// construction, which is looked up once per token.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(b)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub fn fx_hash(bytes: &[u8]) -> u64 {
    let mut h = FxHasher::default();
    std::hash::Hasher::write(&mut h, bytes);
    std::hash::Hasher::finish(&h)
}

type TermMap = HashMap<(String, u16), TermField, std::hash::BuildHasherDefault<FxHasher>>;

/// Give every field of a document its id, in the order indexing would.
pub fn register_fields(schema: &mut Schema, name: &str, v: &Value) -> Result<()> {
    match v {
        Value::Object(map) => {
            if !name.is_empty() {
                schema.id_or_insert(name)?;
            }
            for (k, sub) in map {
                if name.is_empty() {
                    register_fields(schema, k, sub)?;
                } else {
                    register_fields(schema, &format!("{name}.{k}"), sub)?;
                }
            }
        }
        Value::Array(items) => {
            schema.id_or_insert(name)?;
            for item in items {
                register_fields(schema, name, item)?;
            }
        }
        _ => {
            schema.id_or_insert(name)?;
        }
    }
    Ok(())
}

/// Reads one JSON document without building it: registers its fields in the
/// schema, in document order, and picks up the value of the primary key.
/// With `pk` unknown nothing is picked up.
pub fn scan_document(line: &str, schema: &mut Schema, pk: Option<&str>) -> Result<Option<Value>> {
    let mut id = None;
    let mut failure = None;
    let mut de = serde_json::Deserializer::from_str(line);
    let seed = Scan { schema, name: "", pk, id: &mut id, failure: &mut failure, top: true };
    let parsed = serde::de::DeserializeSeed::deserialize(seed, &mut de).and_then(|_| de.end());
    match (failure, parsed) {
        (Some(e), _) => Err(e),
        (None, Err(e)) => Err(e.into()),
        (None, Ok(())) => Ok(id),
    }
}

/// How serde_json presents a number when `arbitrary_precision` is on.
const NUMBER_TOKEN: &str = "$serde_json::private::Number";

struct Scan<'a> {
    schema: &'a mut Schema,
    name: &'a str,
    pk: Option<&'a str>,
    id: &'a mut Option<Value>,
    failure: &'a mut Option<Error>,
    top: bool,
}

impl Scan<'_> {
    fn leaf<E: serde::de::Error>(self) -> std::result::Result<(), E> {
        if self.top {
            *self.failure = Some(Error::user("invalid_document_fields", "A document must be a JSON object."));
            return Err(E::custom("not an object"));
        }
        self.register()
    }

    fn register<E: serde::de::Error>(self) -> std::result::Result<(), E> {
        match self.schema.id_or_insert(self.name) {
            Ok(_) => Ok(()),
            Err(e) => {
                *self.failure = Some(e);
                Err(E::custom("too many fields"))
            }
        }
    }
}

impl<'de> serde::de::DeserializeSeed<'de> for Scan<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Scan<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<(), E> {
        self.leaf()
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<(), E> {
        self.leaf()
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<(), E> {
        self.leaf()
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<(), E> {
        self.leaf()
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<(), E> {
        self.leaf()
    }

    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<(), E> {
        self.leaf()
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        if self.top {
            return self.leaf();
        }
        let Scan { schema, name, id, failure, .. } = self;
        Scan { schema: &mut *schema, name, pk: None, id: &mut *id, failure: &mut *failure, top: false }.register()?;
        while seq.next_element_seed(Scan { schema: &mut *schema, name, pk: None, id: &mut *id, failure: &mut *failure, top: false })?.is_some() {}
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        let Scan { schema, name, pk, id, failure, top } = self;
        let mut first = true;
        let mut child = String::new();
        while let Some(key) = map.next_key::<std::borrow::Cow<str>>()? {
            if first && key == NUMBER_TOKEN {
                map.next_value::<serde::de::IgnoredAny>()?;
                return Scan { schema, name, pk: None, id, failure, top }.leaf();
            }
            if first && !top {
                Scan { schema: &mut *schema, name, pk: None, id: &mut *id, failure: &mut *failure, top: false }.register()?;
            }
            first = false;
            child.clear();
            if !top {
                child.push_str(name);
                child.push('.');
            }
            child.push_str(&key);
            if top && pk == Some(&*key) {
                let value: Value = map.next_value()?;
                if let Err(e) = register_fields(schema, &child, &value) {
                    *failure = Some(e);
                    return Err(serde::de::Error::custom("too many fields"));
                }
                *id = Some(value);
            } else {
                map.next_value_seed(Scan { schema: &mut *schema, name: &child, pk: None, id: &mut *id, failure: &mut *failure, top: false })?;
            }
        }
        if first && !top {
            // An empty nested object still counts as a field.
            Scan { schema, name, pk: None, id, failure, top: false }.register()?;
        }
        Ok(())
    }
}

/// Rough cost in bytes of one more entry in a builder map, on top of its key.
const ENTRY_COST: usize = 112;

#[derive(Default)]
struct SortBuild {
    nums: Vec<(u32, f64)>,
    strs: Vec<(u32, String)>,
}

pub struct SegmentBuilder {
    tmp: PathBuf,
    path: PathBuf,
    file: BufWriter<File>,
    pos: u64,
    docs_crc: crc32fast::Hasher,
    level: i32,
    block: Vec<u8>,
    block_first: u32,
    docidx: Vec<u8>,
    n: u32,
    raw_bytes: u64,
    terms: TermMap,
    next_pos: HashMap<u16, u32>,
    facets: BTreeMap<Vec<u8>, (String, DocList)>,
    /// Estimate of the memory held by the maps above.
    mem: usize,
    pks: BTreeMap<String, u32>,
    sort: BTreeMap<u16, SortBuild>,
    counts: BTreeMap<u16, u32>,
    last_seen: HashMap<u16, u32>,
    /// Per filterable field, the last document that held a number in it.
    last_num: HashMap<u16, u32>,
    multi_num: BTreeSet<u16>,
}

impl SegmentBuilder {
    pub fn new(path: &Path, level: i32) -> Result<Self> {
        let tmp = path.with_extension("tmp");
        let mut file = BufWriter::new(File::create(&tmp)?);
        file.write_all(MAGIC)?;
        Ok(Self {
            tmp,
            path: path.to_path_buf(),
            file,
            pos: MAGIC.len() as u64,
            docs_crc: crc32fast::Hasher::new(),
            level,
            block: Vec::with_capacity(BLOCK_SIZE + 4096),
            block_first: 0,
            docidx: Vec::new(),
            n: 0,
            raw_bytes: 0,
            terms: TermMap::default(),
            mem: 0,
            next_pos: HashMap::new(),
            facets: BTreeMap::new(),
            pks: BTreeMap::new(),
            sort: BTreeMap::new(),
            counts: BTreeMap::new(),
            last_seen: HashMap::new(),
            last_num: HashMap::new(),
            multi_num: BTreeSet::new(),
        })
    }

    pub fn doc_count(&self) -> u32 {
        self.n
    }

    pub fn raw_bytes(&self) -> u64 {
        self.raw_bytes
    }

    /// Approximate memory held by this unfinished segment.
    pub fn mem(&self) -> usize {
        self.mem
    }

    /// The docid of a document already added to this unfinished segment.
    pub fn pk_lookup(&self, pk: &str) -> Option<u32> {
        self.pks.get(pk).copied()
    }

    pub fn add_doc(&mut self, schema: &mut Schema, pk: &str, doc: &Value) -> Result<()> {
        let docid = self.n;
        let bytes = serde_json::to_vec(doc)?;
        write_varint(&mut self.block, bytes.len() as u64);
        self.block.extend_from_slice(&bytes);
        self.raw_bytes += bytes.len() as u64;
        self.pks.insert(pk.to_string(), docid);
        self.mem += pk.len() + 64;
        self.n += 1;
        self.next_pos.clear();
        self.walk(schema, "", doc, docid)?;
        if self.block.len() >= BLOCK_SIZE {
            self.flush_block()?;
        }
        Ok(())
    }

    fn touch(&mut self, schema: &mut Schema, name: &str, docid: u32) -> Result<(u16, u8)> {
        let fid = schema.id_or_insert(name)?;
        let flags = schema.flags(fid);
        let seen = self.last_seen.entry(fid).or_insert(u32::MAX);
        if *seen != docid {
            *seen = docid;
            *self.counts.entry(fid).or_insert(0) += 1;
            if flags & F_FILTER != 0 {
                self.add_facet(fid, TAG_EXISTS, &[], "", docid);
            }
        }
        Ok((fid, flags))
    }

    fn add_facet(&mut self, fid: u16, tag: u8, payload: &[u8], orig: &str, docid: u32) {
        let key = facet_key(fid, tag, payload);
        let cost = key.len() + orig.len() + ENTRY_COST;
        let mem = &mut self.mem;
        let entry = self.facets.entry(key).or_insert_with(|| {
            *mem += cost;
            (orig.to_string(), DocList::default())
        });
        self.mem += entry.1.insert(docid) * 2;
    }

    fn walk(&mut self, schema: &mut Schema, name: &str, v: &Value, docid: u32) -> Result<()> {
        match v {
            Value::Object(map) => {
                if !name.is_empty() {
                    self.touch(schema, name, docid)?;
                }
                for (k, sub) in map {
                    if name.is_empty() {
                        self.walk(schema, k, sub, docid)?;
                    } else {
                        self.walk(schema, &format!("{name}.{k}"), sub, docid)?;
                    }
                }
            }
            Value::Array(items) => {
                self.touch(schema, name, docid)?;
                for item in items {
                    self.walk(schema, name, item, docid)?;
                }
            }
            leaf => self.leaf(schema, name, leaf, docid)?,
        }
        Ok(())
    }

    fn leaf(&mut self, schema: &mut Schema, name: &str, v: &Value, docid: u32) -> Result<()> {
        let (fid, flags) = self.touch(schema, name, docid)?;
        if flags & F_SEARCH != 0 {
            let text = match v {
                Value::String(s) => Some(std::borrow::Cow::Borrowed(s.as_str())),
                Value::Number(n) => Some(std::borrow::Cow::Owned(n.to_string())),
                _ => None,
            };
            if let Some(text) = text {
                let base = self.next_pos.get(&fid).copied().unwrap_or(0);
                let mut count = 0u32;
                for tok in tokenize(&text) {
                    if tok.after_break {
                        count += BREAK_GAP - 1;
                    }
                    let pos = base + count;
                    count += 1;
                    let cost = tok.text.len() + ENTRY_COST;
                    let mem = &mut self.mem;
                    let tf = self.terms.entry((tok.text, fid)).or_insert_with(|| {
                        *mem += cost;
                        TermField::default()
                    });
                    self.mem += tf.add(docid, pos) * 2;
                }
                self.next_pos.insert(fid, base + count + VALUE_GAP);
            }
        }
        if flags & F_FILTER != 0 {
            match v {
                Value::Null => self.add_facet(fid, TAG_NULL, &[], "", docid),
                Value::Number(n) => {
                    if let Some(f) = n.as_f64() {
                        let f = if f == 0.0 { 0.0 } else { f };
                        self.add_facet(fid, TAG_NUM, &encode_f64(f), "", docid);
                        if self.last_num.insert(fid, docid) == Some(docid) {
                            self.multi_num.insert(fid);
                        }
                    }
                }
                Value::String(s) => {
                    let norm = normalize_facet(s);
                    let mut orig = s.trim();
                    if orig.len() > 480 {
                        let mut cut = 480;
                        while !orig.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        orig = &orig[..cut];
                    }
                    self.add_facet(fid, TAG_STR, norm.as_bytes(), orig, docid);
                }
                Value::Bool(b) => {
                    let s = if *b { "true" } else { "false" };
                    self.add_facet(fid, TAG_STR, s.as_bytes(), s, docid);
                }
                _ => {}
            }
        }
        if flags & F_SORT != 0 {
            let col = self.sort.entry(fid).or_default();
            match v {
                Value::Number(n) => {
                    if let Some(f) = n.as_f64()
                        && col.nums.last().map(|l| l.0) != Some(docid) {
                            col.nums.push((docid, f));
                            self.mem += 16;
                        }
                }
                Value::String(s) if col.strs.last().map(|l| l.0) != Some(docid) => {
                    let norm = normalize_facet(s);
                    self.mem += norm.len() + 40;
                    col.strs.push((docid, norm));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.block.is_empty() {
            return Ok(());
        }
        let comp = zstd::bulk::compress(&self.block, self.level)?;
        let crc = crc32fast::hash(&comp);
        self.docidx.extend_from_slice(&(self.pos - MAGIC.len() as u64).to_le_bytes());
        self.docidx.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        self.docidx.extend_from_slice(&self.block_first.to_le_bytes());
        self.docidx.extend_from_slice(&crc.to_le_bytes());
        self.file.write_all(&comp)?;
        self.docs_crc.update(&comp);
        self.pos += comp.len() as u64;
        self.block.clear();
        self.block_first = self.n;
        Ok(())
    }

    fn section(&mut self, data: &[u8]) -> Result<(u64, u64, u32)> {
        let off = self.pos;
        self.file.write_all(data)?;
        self.pos += data.len() as u64;
        Ok((off, data.len() as u64, crc32fast::hash(data)))
    }

    /// Write every section, fsync, and move the file to its final name.
    pub fn finish(mut self) -> Result<u32> {
        self.flush_block()?;
        let mut secs = [(0u64, 0u64, 0u32); N_SECTIONS];
        let docs_crc = std::mem::take(&mut self.docs_crc).finalize();
        secs[S_DOCS] = (MAGIC.len() as u64, self.pos - MAGIC.len() as u64, docs_crc);
        let docidx = std::mem::take(&mut self.docidx);
        secs[S_DOCIDX] = self.section(&docidx)?;

        let mut post = Vec::new();
        let mut positions = Vec::new();
        let mut fst = MapBuilder::memory();
        let mut terms: Vec<((String, u16), TermField)> = std::mem::take(&mut self.terms).into_iter().collect();
        terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut terms = terms.into_iter().peekable();
        while let Some(((term, fid), tf)) = terms.next() {
            let mut fields = vec![(fid, tf)];
            while terms.peek().is_some_and(|((t, _), _)| *t == term) {
                let ((_, fid), tf) = terms.next().unwrap();
                fields.push((fid, tf));
            }
            fst.insert(term.as_bytes(), post.len() as u64)?;
            write_varint(&mut post, fields.len() as u64);
            for (fid, tf) in fields {
                let (docs, pos) = tf.split()?;
                write_varint(&mut post, fid as u64);
                write_docset(&mut post, &docs);
                write_varint(&mut post, positions.len() as u64);
                positions.extend_from_slice(&pos);
            }
        }
        secs[S_TERMS] = self.section(&fst.into_inner()?)?;
        secs[S_TERMS_POST] = self.section(&post)?;

        let mut post = Vec::new();
        let mut fst = MapBuilder::memory();
        let mut levels = RangeLevels::default();
        let mut current: Option<u16> = None;
        let facets = std::mem::take(&mut self.facets);
        // One sentinel past the end flushes the levels of the last field.
        for entry in facets.into_iter().map(Some).chain(std::iter::once(None)) {
            let fid = entry.as_ref().map(|(key, _)| u16::from_be_bytes([key[0], key[1]]));
            if fid != current {
                if let Some(done) = current {
                    for (key, docs) in levels.finish(done) {
                        fst.insert(&key, post.len() as u64)?;
                        write_varint(&mut post, 0);
                        write_docset(&mut post, &docs);
                    }
                }
                current = fid;
            }
            let Some((key, (orig, docs))) = entry else { break };
            let docs = docs.to_bitmap()?;
            if key[2] == TAG_NUM {
                levels.push(&key[3..], &docs);
            }
            fst.insert(&key, post.len() as u64)?;
            write_varint(&mut post, orig.len() as u64);
            post.extend_from_slice(orig.as_bytes());
            write_docset(&mut post, &docs);
        }
        secs[S_FACETS] = self.section(&fst.into_inner()?)?;
        secs[S_FACETS_POST] = self.section(&post)?;

        let mut fst = MapBuilder::memory();
        for (pk, docid) in std::mem::take(&mut self.pks) {
            fst.insert(pk.as_bytes(), docid as u64)?;
        }
        secs[S_PKS] = self.section(&fst.into_inner()?)?;

        let (sort, sort_meta) = encode_sort(self.n as usize, std::mem::take(&mut self.sort));
        let meta = MetaFile { doc_count: self.n, field_counts: self.counts.iter().map(|(k, v)| (*k, *v)).collect(), sort: sort_meta, multi_num: Some(self.multi_num.iter().copied().collect()) };
        secs[S_SORT] = self.section(&sort)?;
        secs[S_META] = self.section(&serde_json::to_vec(&meta)?)?;
        secs[S_POS] = self.section(&positions)?;

        let mut footer = Vec::with_capacity(FOOTER_LEN);
        for (off, len, crc) in secs {
            footer.extend_from_slice(&off.to_le_bytes());
            footer.extend_from_slice(&len.to_le_bytes());
            footer.extend_from_slice(&crc.to_le_bytes());
        }
        let crc = crc32fast::hash(&footer);
        footer.extend_from_slice(&crc.to_le_bytes());
        footer.extend_from_slice(MAGIC);
        self.file.write_all(&footer)?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        drop(self.file);
        std::fs::rename(&self.tmp, &self.path)?;
        Ok(self.n)
    }
}

/// Lay out the sort columns of a segment with `n` documents.
fn encode_sort(n: usize, cols: BTreeMap<u16, SortBuild>) -> (Vec<u8>, Vec<(u16, SortColMeta)>) {
    let mut sort = Vec::new();
    let mut metas = Vec::new();
    for (fid, col) in cols {
        let mut m = SortColMeta::default();
        if !col.nums.is_empty() {
            let off = sort.len();
            sort.resize(off + n * 8, 0);
            for i in 0..n {
                sort[off + i * 8..off + i * 8 + 8].copy_from_slice(&f64::NAN.to_le_bytes());
            }
            for (d, f) in &col.nums {
                let p = off + *d as usize * 8;
                sort[p..p + 8].copy_from_slice(&f.to_le_bytes());
            }
            m.num = Some((off as u64, (n * 8) as u64));
        }
        if !col.strs.is_empty() {
            let distinct: Vec<&str> = col.strs.iter().map(|(_, s)| s.as_str()).collect::<BTreeSet<_>>().into_iter().collect();
            let off = sort.len();
            sort.resize(off + n * 4, 0xff);
            for (d, s) in &col.strs {
                let ord = distinct.binary_search(&s.as_str()).unwrap() as u32;
                let p = off + *d as usize * 4;
                sort[p..p + 4].copy_from_slice(&ord.to_le_bytes());
            }
            m.ord = Some((off as u64, (n * 4) as u64));
            let offs_at = sort.len();
            let mut data = Vec::new();
            for s in &distinct {
                sort.extend_from_slice(&(data.len() as u32).to_le_bytes());
                data.extend_from_slice(s.as_bytes());
            }
            sort.extend_from_slice(&(data.len() as u32).to_le_bytes());
            m.stroffs = (offs_at as u64, (sort.len() - offs_at) as u64);
            m.strdata = (sort.len() as u64, data.len() as u64);
            sort.extend_from_slice(&data);
        }
        let order = sorted_docs(&sort, &m, n);
        m.order = Some((sort.len() as u64, (order.len() * 4) as u64));
        for d in order {
            sort.extend_from_slice(&d.to_le_bytes());
        }
        metas.push((fid, m));
    }
    (sort, metas)
}

/// The output file of a merge: tracks the position and the checksum of the
/// section being written.
struct Sink {
    file: BufWriter<File>,
    pos: u64,
    start: u64,
    crc: crc32fast::Hasher,
}

impl Sink {
    fn begin(&mut self) {
        self.start = self.pos;
        self.crc = crc32fast::Hasher::new();
    }

    fn end(&mut self) -> (u64, u64, u32) {
        (self.start, self.pos - self.start, std::mem::take(&mut self.crc).finalize())
    }

    fn put(&mut self, data: &[u8]) -> Result<(u64, u64, u32)> {
        self.begin();
        self.write_all(data)?;
        Ok(self.end())
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write_all(buf)?;
        self.crc.update(buf);
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// A section that is produced alongside another one: it goes to a side file
/// first and is appended to the segment afterwards.
struct Spill {
    path: PathBuf,
    file: BufWriter<File>,
    len: u64,
}

impl Spill {
    fn create(path: PathBuf) -> Result<Spill> {
        Ok(Spill { file: BufWriter::new(File::create(&path)?), path, len: 0 })
    }

    fn put(&mut self, data: &[u8]) -> Result<()> {
        self.file.write_all(data)?;
        self.len += data.len() as u64;
        Ok(())
    }

    fn move_into(self, sink: &mut Sink) -> Result<(u64, u64, u32)> {
        sink.begin();
        self.append_to(sink)?;
        Ok(sink.end())
    }

    /// Copy the side file to the end of the section being written.
    fn append_to(mut self, sink: &mut Sink) -> Result<u64> {
        self.file.flush()?;
        drop(self.file);
        let mut src = File::open(&self.path)?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sink.write_all(&buf[..n])?;
        }
        drop(src);
        let _ = std::fs::remove_file(&self.path);
        Ok(self.len)
    }
}

pub struct MergeInput<'a> {
    pub seg: &'a Segment,
    pub deleted: &'a RoaringBitmap,
}

fn mark_fields(schema: &Schema, name: &str, v: &Value, seen: &mut BTreeSet<u16>) {
    let mut mark = |name: &str| {
        if let Some(fid) = schema.id(name) {
            seen.insert(fid);
        }
    };
    match v {
        Value::Object(map) => {
            if !name.is_empty() {
                mark(name);
            }
            for (k, sub) in map {
                if name.is_empty() {
                    mark_fields(schema, k, sub, seen);
                } else {
                    mark_fields(schema, &format!("{name}.{k}"), sub, seen);
                }
            }
        }
        Value::Array(items) => {
            mark(name);
            for item in items {
                mark_fields(schema, name, item, seen);
            }
        }
        _ => mark(name),
    }
}

fn flush_merged_block(sink: &mut Sink, block: &mut Vec<u8>, docidx: &mut Vec<u8>, first: u32, level: i32) -> Result<()> {
    if block.is_empty() {
        return Ok(());
    }
    let comp = zstd::bulk::compress(block, level)?;
    docidx.extend_from_slice(&(sink.pos - sink.start).to_le_bytes());
    docidx.extend_from_slice(&(comp.len() as u32).to_le_bytes());
    docidx.extend_from_slice(&first.to_le_bytes());
    docidx.extend_from_slice(&crc32fast::hash(&comp).to_le_bytes());
    sink.write_all(&comp)?;
    block.clear();
    Ok(())
}

/// Write one sort column of a merge. New ids follow the inputs one after
/// another, so the values are copied input by input and the sorted order is
/// the inputs' orders merged; the distinct strings are merged the same way.
/// Nothing is held per document: only, per input, where each of its distinct
/// strings went.
fn merge_sort_column(inputs: &[MergeInput], remap: &[Vec<u32>], fid: u16, sink: &mut Sink, path: &Path) -> Result<SortColMeta> {
    const CHUNK: usize = 1 << 16;
    let k = inputs.len();
    let none = SortColMeta::default();
    let cols: Vec<&SortColMeta> = inputs.iter().map(|inp| inp.seg.sortcols.get(&fid).unwrap_or(&none)).collect();
    let bufs: Vec<&[u8]> = inputs.iter().map(|inp| inp.seg.sort.as_ref()).collect();
    let live = |i: usize| remap[i].iter().enumerate().filter(|(_, new)| **new != u32::MAX).map(|(d, _)| d);
    let mut m = SortColMeta::default();
    let mut out: Vec<u8> = Vec::with_capacity(CHUNK + 8);

    // Numbers.
    if (0..k).any(|i| cols[i].num.is_some() && live(i).any(|d| !col_num(bufs[i], cols[i], d).is_nan())) {
        let off = sink.pos - sink.start;
        for i in 0..k {
            for d in live(i) {
                out.extend_from_slice(&col_num(bufs[i], cols[i], d).to_le_bytes());
                if out.len() >= CHUNK {
                    sink.write_all(&out)?;
                    out.clear();
                }
            }
        }
        sink.write_all(&out)?;
        out.clear();
        m.num = Some((off, sink.pos - sink.start - off));
    }

    // Strings. `ords[i][old]` is the new place of a string of input `i`, or
    // u32::MAX when no surviving document has it.
    let string_at = |i: usize, ord: usize| -> &[u8] {
        let o = cols[i].stroffs.0 as usize + ord * 4;
        let a = u32::from_le_bytes(bufs[i][o..o + 4].try_into().unwrap()) as usize;
        let b = u32::from_le_bytes(bufs[i][o + 4..o + 8].try_into().unwrap()) as usize;
        let base = cols[i].strdata.0 as usize;
        &bufs[i][base + a..base + b]
    };
    let mut ords: Vec<Vec<u32>> = Vec::with_capacity(k);
    for i in 0..k {
        let mut used = Vec::new();
        if cols[i].ord.is_some() {
            used = vec![u32::MAX; (cols[i].stroffs.1 as usize / 4).saturating_sub(1)];
            for d in live(i) {
                let ord = col_ord(bufs[i], cols[i], d);
                if ord != u32::MAX {
                    *used.get_mut(ord as usize).ok_or_else(|| Error::corrupt("bad sort value"))? = 0;
                }
            }
        }
        ords.push(used);
    }
    if ords.iter().any(|used| used.contains(&0)) {
        let mut offs = Spill::create(path.with_extension("soff.tmp"))?;
        let mut data = Spill::create(path.with_extension("sstr.tmp"))?;
        let skip = |used: &[u32], mut at: usize| {
            while at < used.len() && used[at] == u32::MAX {
                at += 1;
            }
            at
        };
        let mut heads: Vec<usize> = ords.iter().map(|used| skip(used, 0)).collect();
        let mut next = 0u32;
        loop {
            let mut best: Option<&[u8]> = None;
            for i in 0..k {
                if heads[i] < ords[i].len() {
                    let s = string_at(i, heads[i]);
                    if best.is_none_or(|b| s < b) {
                        best = Some(s);
                    }
                }
            }
            let Some(best) = best else { break };
            if data.len + best.len() as u64 > u32::MAX as u64 {
                return Err(Error::corrupt("the strings of a sortable field do not fit in a segment"));
            }
            offs.put(&(data.len as u32).to_le_bytes())?;
            data.put(best)?;
            for i in 0..k {
                if heads[i] < ords[i].len() && string_at(i, heads[i]) == best {
                    ords[i][heads[i]] = next;
                    heads[i] = skip(&ords[i], heads[i] + 1);
                }
            }
            next += 1;
        }
        offs.put(&(data.len as u32).to_le_bytes())?;

        let off = sink.pos - sink.start;
        for i in 0..k {
            for d in live(i) {
                let new = match col_ord(bufs[i], cols[i], d) {
                    u32::MAX => u32::MAX,
                    ord => ords[i][ord as usize],
                };
                out.extend_from_slice(&new.to_le_bytes());
                if out.len() >= CHUNK {
                    sink.write_all(&out)?;
                    out.clear();
                }
            }
        }
        sink.write_all(&out)?;
        out.clear();
        m.ord = Some((off, sink.pos - sink.start - off));
        let off = sink.pos - sink.start;
        m.stroffs = (off, offs.append_to(sink)?);
        let off = sink.pos - sink.start;
        m.strdata = (off, data.append_to(sink)?);
    }

    // The sorted order: the inputs' orders, merged. A string compares by its
    // place among the merged strings.
    let orders: Vec<Order> = inputs.iter().map(|inp| inp.seg.order(fid)).collect();
    let mut at = vec![0usize; k];
    let mut advance = |i: usize| -> Option<(Rank, u32)> {
        while at[i] < orders[i].len() {
            let d = orders[i].get(at[i]) as usize;
            at[i] += 1;
            let new = remap[i][d];
            if new != u32::MAX {
                let rank = match col_rank(bufs[i], cols[i], d) {
                    (1, ord) => (1, ords[i][ord as usize] as f64),
                    other => other,
                };
                return Some((rank, new));
            }
        }
        None
    };
    let mut heads: Vec<Option<(Rank, u32)>> = (0..k).map(&mut advance).collect();
    let off = sink.pos - sink.start;
    loop {
        let mut best: Option<usize> = None;
        for i in 0..k {
            if let Some(h) = &heads[i] {
                // Equal values keep the order of the inputs, which is that of the new ids.
                if best.is_none_or(|b| cmp_rank(&h.0, &heads[b].as_ref().unwrap().0) == std::cmp::Ordering::Less) {
                    best = Some(i);
                }
            }
        }
        let Some(i) = best else { break };
        out.extend_from_slice(&heads[i].unwrap().1.to_le_bytes());
        if out.len() >= CHUNK {
            sink.write_all(&out)?;
            out.clear();
        }
        heads[i] = advance(i);
    }
    sink.write_all(&out)?;
    m.order = Some((off, sink.pos - sink.start - off));
    Ok(m)
}

/// Merge segments into one without re-indexing: documents are copied block
/// by block and the term, filter and key dictionaries are merged as sorted
/// streams, dropping deleted documents on the way. Memory use depends on the
/// largest single posting list, not on the size of the segments.
///
/// The document blocks of inputs without deletions are copied as they are,
/// still compressed; the documents of the others are compressed at `level`.
pub fn merge(inputs: &[MergeInput], schema: &Schema, path: &Path, level: i32) -> Result<u32> {
    // Old docid -> new docid per input; u32::MAX for deleted documents.
    let mut remap: Vec<Vec<u32>> = Vec::with_capacity(inputs.len());
    let mut n = 0u32;
    for inp in inputs {
        let mut m = vec![u32::MAX; inp.seg.doc_count as usize];
        for (d, slot) in m.iter_mut().enumerate() {
            if !inp.deleted.contains(d as u32) {
                *slot = n;
                n += 1;
            }
        }
        remap.push(m);
    }

    let tmp = path.with_extension("tmp");
    let mut sink = Sink { file: BufWriter::new(File::create(&tmp)?), pos: 0, start: 0, crc: crc32fast::Hasher::new() };
    sink.write_all(MAGIC)?;
    let mut secs = [(0u64, 0u64, 0u32); N_SECTIONS];
    // A merge reads its inputs from end to end. The pages it has been
    // through are let go as it advances, so that it holds a few megabytes of
    // them whatever the size of the segments.
    let mut released = 0u64;
    let mut relief = |written: u64| {
        if written.saturating_sub(released) >= RELIEF_EVERY {
            released = written;
            for inp in inputs {
                inp.seg.release_pages();
            }
        }
    };

    // Documents.
    sink.begin();
    let mut block = Vec::with_capacity(BLOCK_SIZE + 4096);
    let mut docidx = Vec::new();
    let (mut written, mut block_first) = (0u32, 0u32);
    let mut counts: BTreeMap<u16, u32> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (i, inp) in inputs.iter().enumerate() {
        // Field counts add up, unless some documents are gone: then the
        // surviving ones are counted again.
        let recount = !inp.deleted.is_empty();
        if !recount {
            for (fid, c) in &inp.seg.field_counts {
                *counts.entry(*fid).or_insert(0) += c;
            }
        }
        if !recount {
            flush_merged_block(&mut sink, &mut block, &mut docidx, block_first, level)?;
            for b in 0..inp.seg.block_count() {
                let (off, len, first, crc) = inp.seg.block_entry(b);
                let comp = inp.seg.docs.as_ref().get(off..off + len).ok_or_else(|| Error::corrupt("document block out of bounds"))?;
                docidx.extend_from_slice(&(sink.pos - sink.start).to_le_bytes());
                docidx.extend_from_slice(&(len as u32).to_le_bytes());
                docidx.extend_from_slice(&(written + first).to_le_bytes());
                docidx.extend_from_slice(&crc.to_le_bytes());
                sink.write_all(comp)?;
                relief(sink.pos);
            }
            written += inp.seg.doc_count;
            block_first = written;
            continue;
        }
        inp.seg.for_each_doc(|docid, bytes| {
            if remap[i][docid as usize] == u32::MAX {
                return Ok(());
            }
            if recount {
                mark_fields(schema, "", &serde_json::from_slice(bytes)?, &mut seen);
                for fid in std::mem::take(&mut seen) {
                    *counts.entry(fid).or_insert(0) += 1;
                }
            }
            write_varint(&mut block, bytes.len() as u64);
            block.extend_from_slice(bytes);
            written += 1;
            if block.len() >= BLOCK_SIZE {
                flush_merged_block(&mut sink, &mut block, &mut docidx, block_first, level)?;
                block_first = written;
                relief(sink.pos);
            }
            Ok(())
        })?;
    }
    flush_merged_block(&mut sink, &mut block, &mut docidx, block_first, level)?;
    secs[S_DOCS] = sink.end();
    secs[S_DOCIDX] = sink.put(&docidx)?;

    // Terms: the dictionary goes straight to the file, postings and
    // positions to side files.
    let docs_end = sink.pos;
    let mut post = Spill::create(path.with_extension("post.tmp"))?;
    let mut positions = Spill::create(path.with_extension("pos.tmp"))?;
    sink.begin();
    {
        let mut fst = MapBuilder::new(&mut sink)?;
        let mut op = OpBuilder::new();
        for inp in inputs {
            op.push(inp.seg.terms.stream());
        }
        let mut union = op.union();
        let mut rec = Vec::new();
        let mut body = Vec::new();
        while let Some((term, ivs)) = union.next() {
            let mut ivs: Vec<(usize, u64)> = ivs.iter().map(|iv| (iv.index, iv.value)).collect();
            ivs.sort_unstable();
            let mut lists = Vec::with_capacity(ivs.len());
            for (i, off) in ivs {
                lists.push((i, inputs[i].seg.postings(off)?));
            }
            // Field by field, the positions of each input go straight to
            // the side file: new ids follow the inputs in order, so nothing
            // but the set of documents has to be put together in memory.
            let fids: BTreeSet<u16> = lists.iter().flat_map(|(_, list)| list.iter().map(|p| p.0)).collect();
            let mut fields = 0u64;
            body.clear();
            for fid in fids {
                let start = positions.len;
                let mut docs = RoaringBitmap::new();
                let mut failed = None;
                for (i, list) in &lists {
                    for (_, theirs, pos_off) in list.iter().filter(|p| p.0 == fid) {
                        inputs[*i].seg.raw_positions(*pos_off, theirs, |doc, bytes| {
                            let new = remap[*i][doc as usize];
                            if new != u32::MAX && failed.is_none() {
                                docs.push(new);
                                failed = positions.put(bytes).err();
                            }
                        })?;
                    }
                }
                if let Some(e) = failed {
                    return Err(e);
                }
                if docs.is_empty() {
                    continue;
                }
                fields += 1;
                write_varint(&mut body, fid as u64);
                write_docset(&mut body, &docs);
                write_varint(&mut body, start);
            }
            if fields == 0 {
                continue;
            }
            fst.insert(term, post.len)?;
            rec.clear();
            write_varint(&mut rec, fields);
            rec.extend_from_slice(&body);
            post.put(&rec)?;
            relief(docs_end + post.len + positions.len);
        }
        fst.finish()?;
    }
    secs[S_TERMS] = sink.end();
    secs[S_TERMS_POST] = post.move_into(&mut sink)?;

    // Filter values.
    let mut post = Spill::create(path.with_extension("post.tmp"))?;
    sink.begin();
    {
        let mut fst = MapBuilder::new(&mut sink)?;
        let mut op = OpBuilder::new();
        for inp in inputs {
            op.push(inp.seg.facets.stream());
        }
        let mut union = op.union();
        let mut rec = Vec::new();
        let mut levels = RangeLevels::default();
        let mut current: Option<u16> = None;
        loop {
            let entry = union.next();
            let fid = entry.as_ref().map(|(key, _)| u16::from_be_bytes([key[0], key[1]]));
            if fid != current {
                if let Some(done) = current {
                    for (key, docs) in levels.finish(done) {
                        fst.insert(&key, post.len)?;
                        rec.clear();
                        write_varint(&mut rec, 0);
                        write_docset(&mut rec, &docs);
                        post.put(&rec)?;
                    }
                }
                current = fid;
            }
            let Some((key, ivs)) = entry else { break };
            // The runs of the inputs do not line up: they are built again.
            if key[2] >= TAG_NUM_L1 {
                continue;
            }
            let mut ivs: Vec<(usize, u64)> = ivs.iter().map(|iv| (iv.index, iv.value)).collect();
            ivs.sort_unstable();
            let mut merged = RoaringBitmap::new();
            let mut orig: Option<&str> = None;
            for (i, off) in ivs {
                let (o, docs) = inputs[i].seg.facet_record(off)?;
                let before = merged.len();
                for d in docs {
                    let new = remap[i][d as usize];
                    if new != u32::MAX {
                        merged.push(new);
                    }
                }
                if orig.is_none() && merged.len() > before {
                    orig = Some(o);
                }
            }
            if merged.is_empty() {
                continue;
            }
            let orig = orig.unwrap_or("");
            if key[2] == TAG_NUM {
                levels.push(&key[3..], &merged);
            }
            fst.insert(key, post.len)?;
            rec.clear();
            write_varint(&mut rec, orig.len() as u64);
            rec.extend_from_slice(orig.as_bytes());
            write_docset(&mut rec, &merged);
            post.put(&rec)?;
        }
        fst.finish()?;
    }
    secs[S_FACETS] = sink.end();
    secs[S_FACETS_POST] = post.move_into(&mut sink)?;

    // Primary keys.
    sink.begin();
    {
        let mut fst = MapBuilder::new(&mut sink)?;
        let mut op = OpBuilder::new();
        for inp in inputs {
            op.push(inp.seg.pks.stream());
        }
        let mut union = op.union();
        while let Some((key, ivs)) = union.next() {
            if let Some(new) = ivs.iter().map(|iv| remap[iv.index][iv.value as usize]).find(|d| *d != u32::MAX) {
                fst.insert(key, new as u64)?;
            }
        }
        fst.finish()?;
    }
    secs[S_PKS] = sink.end();

    // Sort columns.
    sink.begin();
    let mut sort_meta = Vec::new();
    let fids: BTreeSet<u16> = inputs.iter().flat_map(|inp| inp.seg.sortcols.keys().copied()).collect();
    for fid in fids {
        sort_meta.push((fid, merge_sort_column(inputs, &remap, fid, &mut sink, path)?));
    }
    secs[S_SORT] = sink.end();
    // Known only when every input knows it; a document that was deleted may
    // have been the only one with several numbers, which errs on the safe side.
    let multi_num = inputs.iter().map(|inp| inp.seg.multi_num.as_ref()).collect::<Option<Vec<_>>>().map(|sets| sets.into_iter().flatten().copied().collect::<BTreeSet<u16>>().into_iter().collect());
    let meta = MetaFile { doc_count: n, field_counts: counts.into_iter().collect(), sort: sort_meta, multi_num };
    secs[S_META] = sink.put(&serde_json::to_vec(&meta)?)?;
    secs[S_POS] = positions.move_into(&mut sink)?;

    let mut footer = Vec::with_capacity(FOOTER_LEN);
    for (off, len, crc) in secs {
        footer.extend_from_slice(&off.to_le_bytes());
        footer.extend_from_slice(&len.to_le_bytes());
        footer.extend_from_slice(&crc.to_le_bytes());
    }
    let crc = crc32fast::hash(&footer);
    footer.extend_from_slice(&crc.to_le_bytes());
    footer.extend_from_slice(MAGIC);
    sink.file.write_all(&footer)?;
    sink.file.flush()?;
    sink.file.get_ref().sync_all()?;
    drop(sink);
    std::fs::rename(&tmp, path)?;
    Ok(n)
}

#[derive(Clone)]
pub struct Section {
    mmap: Arc<Mmap>,
    off: usize,
    len: usize,
}

impl AsRef<[u8]> for Section {
    fn as_ref(&self) -> &[u8] {
        &self.mmap[self.off..self.off + self.len]
    }
}

/// CRC32 of a stretch of a mapped file. Reading it maps its pages into the
/// process; they are let go piece by piece, so that checking a file of any
/// size takes a few megabytes and a server at rest holds only what its
/// searches touch. The pages stay in the system's cache.
fn checksum(mmap: &Mmap, off: usize, len: usize) -> u32 {
    const PIECE: usize = 4 << 20;
    let mut crc = crc32fast::Hasher::new();
    let mut at = off;
    while at < off + len {
        let n = PIECE.min(off + len - at);
        crc.update(&mmap[at..at + n]);
        #[cfg(unix)]
        unsafe {
            let _ = mmap.unchecked_advise_range(memmap2::UncheckedAdvice::DontNeed, at, n);
        }
        at += n;
    }
    crc.finalize()
}

/// Removes the segment file once the last reader has released it.
struct Deleter {
    path: PathBuf,
    armed: AtomicBool,
}

impl Drop for Deleter {
    fn drop(&mut self) {
        if self.armed.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SortKey {
    Num(f64),
    Str(Vec<u8>),
    Missing,
}

type BlockCache = Mutex<Option<(usize, Arc<Vec<u8>>)>>;

// Field order matters: `deleter` is declared last so every mapping of the
// file is released before the file is removed.
pub struct Segment {
    pub id: u64,
    pub doc_count: u32,
    pub file_size: u64,
    pub field_counts: Vec<(u16, u32)>,
    docs: Section,
    docidx: Section,
    terms: Map<Section>,
    terms_post: Section,
    positions: Section,
    facets: Map<Section>,
    facets_post: Section,
    pks: Map<Section>,
    sort: Section,
    sortcols: HashMap<u16, SortColMeta>,
    multi_num: Option<Vec<u16>>,
    cache: BlockCache,
    deleter: Deleter,
}

/// Steps over the positions of `docs` documents starting at `at`, and says
/// where the next document's begin. A number's first byte says in its low
/// bit whether the document has another one, and a byte is a first byte when
/// the one before it does not carry on, so the documents that end in eight
/// bytes are counted at once.
fn skip_positions(buf: &[u8], mut at: usize, mut docs: u64) -> Result<usize> {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    const LOW: u64 = 0x0101_0101_0101_0101;
    let truncated = || Error::corrupt("truncated positions");
    // Whether `at` is in the middle of a number.
    let mut inside = false;
    while docs > 0 {
        if let Some(chunk) = buf.get(at..at + 8) {
            let word = u64::from_le_bytes(chunk.try_into().unwrap());
            let carried = ((word & HIGH) << 8) | if inside { 0x80 } else { 0 };
            let ended = (!carried & ((!word & LOW) << 7) & HIGH).count_ones() as u64;
            // Strictly fewer: the bytes after the last end belong to a
            // document that is stepped over too.
            if ended < docs {
                docs -= ended;
                at += 8;
                inside = word >> 63 == 1;
                continue;
            }
        }
        // The rest of a number whose first byte was already counted.
        while inside {
            inside = *buf.get(at).ok_or_else(truncated)? & 0x80 != 0;
            at += 1;
        }
        loop {
            let first = *buf.get(at).ok_or_else(truncated)?;
            at += 1;
            let mut byte = first;
            while byte & 0x80 != 0 {
                byte = *buf.get(at).ok_or_else(truncated)?;
                at += 1;
            }
            if first & 1 == 0 {
                break;
            }
        }
        docs -= 1;
    }
    Ok(at)
}

impl Segment {
    pub fn open(path: &Path, id: u64) -> Result<Segment> {
        let file = File::open(path)?;
        let mmap = Arc::new(unsafe { Mmap::map(&file)? });
        let size = mmap.len();
        let bad = |m: &str| Error::corrupt(format!("{}: {m}", path.display()));
        if size < MAGIC.len() + FOOTER_LEN || (&mmap[..8] != MAGIC && &mmap[..8] != MAGIC_V1) || mmap[size - 8..] != mmap[..8] {
            return Err(bad("not a complete segment file"));
        }
        let footer = &mmap[size - FOOTER_LEN..size - 12];
        let stored = u32::from_le_bytes(mmap[size - 12..size - 8].try_into().unwrap());
        if crc32fast::hash(footer) != stored {
            return Err(bad("footer checksum mismatch"));
        }
        let mut secs = Vec::with_capacity(N_SECTIONS);
        for i in 0..N_SECTIONS {
            let e = &footer[i * 20..i * 20 + 20];
            let off = u64::from_le_bytes(e[0..8].try_into().unwrap()) as usize;
            let len = u64::from_le_bytes(e[8..16].try_into().unwrap()) as usize;
            let crc = u32::from_le_bytes(e[16..20].try_into().unwrap());
            if off.checked_add(len).is_none_or(|end| end > size - FOOTER_LEN) {
                return Err(bad("section out of bounds"));
            }
            // Document blocks carry their own checksum and are verified on read.
            if i != S_DOCS && checksum(&mmap, off, len) != crc {
                return Err(bad("section checksum mismatch"));
            }
            secs.push(Section { mmap: mmap.clone(), off, len });
        }
        // The pieces do not end on page boundaries: let go of what is left.
        #[cfg(unix)]
        unsafe {
            let _ = mmap.unchecked_advise(memmap2::UncheckedAdvice::DontNeed);
        }
        let meta: MetaFile = serde_json::from_slice(secs[S_META].as_ref())?;
        Ok(Segment {
            id,
            doc_count: meta.doc_count,
            file_size: size as u64,
            field_counts: meta.field_counts,
            docs: secs[S_DOCS].clone(),
            docidx: secs[S_DOCIDX].clone(),
            terms: Map::new(secs[S_TERMS].clone())?,
            terms_post: secs[S_TERMS_POST].clone(),
            positions: secs[S_POS].clone(),
            facets: Map::new(secs[S_FACETS].clone())?,
            facets_post: secs[S_FACETS_POST].clone(),
            pks: Map::new(secs[S_PKS].clone())?,
            sort: secs[S_SORT].clone(),
            sortcols: meta.sort.into_iter().collect(),
            multi_num: meta.multi_num,
            cache: Mutex::new(None),
            deleter: Deleter { path: path.to_path_buf(), armed: AtomicBool::new(false) },
        })
    }

    /// Whether no document of this segment holds more than one number in
    /// the field. Then two bounds on the field are one range, and a document
    /// that passes a range filter is sorted by the number that passed it.
    pub fn one_number_each(&self, fid: u16) -> bool {
        self.multi_num.as_ref().is_some_and(|multi| !multi.contains(&fid))
    }

    /// Bytes of the sort columns.
    pub fn sort_bytes(&self) -> u64 {
        self.sort.len as u64
    }

    /// Let go of the pages of this file that the process has mapped. They
    /// stay in the system's cache and come back when they are read again.
    pub fn release_pages(&self) {
        #[cfg(unix)]
        unsafe {
            let _ = self.docs.mmap.unchecked_advise(memmap2::UncheckedAdvice::DontNeed);
        }
    }

    /// Delete the file when the last reader drops this segment.
    pub fn mark_obsolete(&self) {
        self.deleter.armed.store(true, Ordering::Relaxed);
    }

    fn block_count(&self) -> usize {
        self.docidx.len / 20
    }

    fn block_entry(&self, i: usize) -> (usize, usize, u32, u32) {
        let e = &self.docidx.as_ref()[i * 20..i * 20 + 20];
        (
            u64::from_le_bytes(e[0..8].try_into().unwrap()) as usize,
            u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize,
            u32::from_le_bytes(e[12..16].try_into().unwrap()),
            u32::from_le_bytes(e[16..20].try_into().unwrap()),
        )
    }

    fn block(&self, i: usize) -> Result<Arc<Vec<u8>>> {
        if let Some((cached, data)) = self.cache.lock().unwrap().as_ref()
            && *cached == i {
                return Ok(data.clone());
            }
        let (off, len, _, crc) = self.block_entry(i);
        let docs = self.docs.as_ref();
        let comp = docs.get(off..off + len).ok_or_else(|| Error::corrupt("document block out of bounds"))?;
        if crc32fast::hash(comp) != crc {
            return Err(Error::corrupt(format!("document block {i} of segment {} failed its checksum", self.id)));
        }
        let data = Arc::new(zstd::stream::decode_all(comp)?);
        *self.cache.lock().unwrap() = Some((i, data.clone()));
        Ok(data)
    }

    pub fn doc_bytes(&self, docid: u32) -> Result<Vec<u8>> {
        if docid >= self.doc_count {
            return Err(Error::corrupt("document id out of range"));
        }
        let n = self.block_count();
        let (mut lo, mut hi) = (0usize, n);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.block_entry(mid).2 <= docid {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let first = self.block_entry(lo).2;
        let data = self.block(lo)?;
        let mut pos = 0usize;
        for _ in first..docid {
            let len = read_varint(&data, &mut pos)? as usize;
            pos += len;
        }
        let len = read_varint(&data, &mut pos)? as usize;
        data.get(pos..pos + len).map(|s| s.to_vec()).ok_or_else(|| Error::corrupt("truncated document"))
    }

    pub fn doc(&self, docid: u32) -> Result<Value> {
        Ok(serde_json::from_slice(&self.doc_bytes(docid)?)?)
    }

    /// Visit every stored document in id order, decompressing block by block.
    pub fn for_each_doc(&self, mut f: impl FnMut(u32, &[u8]) -> Result<()>) -> Result<()> {
        for b in 0..self.block_count() {
            let (off, len, first, crc) = self.block_entry(b);
            let comp = &self.docs.as_ref()[off..off + len];
            if crc32fast::hash(comp) != crc {
                return Err(Error::corrupt(format!("document block {b} of segment {} failed its checksum", self.id)));
            }
            let data = zstd::stream::decode_all(comp)?;
            let mut pos = 0usize;
            let mut docid = first;
            while pos < data.len() {
                let len = read_varint(&data, &mut pos)? as usize;
                let bytes = data.get(pos..pos + len).ok_or_else(|| Error::corrupt("truncated document"))?;
                f(docid, bytes)?;
                pos += len;
                docid += 1;
            }
        }
        Ok(())
    }

    /// For each field, how many of the documents in `docs` hold it.
    pub fn field_counts_of(&self, docs: &RoaringBitmap, schema: &Schema) -> Result<BTreeMap<u16, u32>> {
        let mut counts: BTreeMap<u16, u32> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut count = |bytes: &[u8]| -> Result<()> {
            mark_fields(schema, "", &serde_json::from_slice(bytes)?, &mut seen);
            for fid in std::mem::take(&mut seen) {
                *counts.entry(fid).or_insert(0) += 1;
            }
            Ok(())
        };
        // Reading a few documents one by one is cheaper than walking every
        // block; past that, one pass over the segment is.
        if docs.len() * 8 < self.doc_count as u64 {
            for d in docs {
                count(&self.doc_bytes(d)?)?;
            }
        } else {
            self.for_each_doc(|docid, bytes| if docs.contains(docid) { count(bytes) } else { Ok(()) })?;
        }
        Ok(counts)
    }

    pub fn pk_lookup(&self, pk: &str) -> Option<u32> {
        self.pks.get(pk.as_bytes()).map(|v| v as u32)
    }

    pub fn terms(&self) -> &Map<Section> {
        &self.terms
    }

    /// For one term: per field, the documents and the offset of their positions.
    pub fn postings(&self, off: u64) -> Result<Vec<(u16, RoaringBitmap, u64)>> {
        let buf = self.terms_post.as_ref();
        let mut pos = off as usize;
        let n = read_varint(buf, &mut pos)?;
        let mut out = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let fid = read_varint(buf, &mut pos)? as u16;
            let docs = read_docset(buf, &mut pos)?;
            out.push((fid, docs, read_varint(buf, &mut pos)?));
        }
        Ok(out)
    }

    /// Visit the still-encoded positions of each document of a posting list.
    fn raw_positions(&self, pos_off: u64, docs: &RoaringBitmap, mut f: impl FnMut(u32, &[u8])) -> Result<()> {
        let buf = self.positions.as_ref();
        let mut at = pos_off as usize;
        for doc in docs {
            let start = at;
            while read_varint(buf, &mut at)? & 1 == 1 {}
            f(doc, &buf[start..at]);
        }
        Ok(())
    }

    /// Visit the token positions of a posting list for the documents in
    /// `want` (ascending), giving each one's index in `want`. Documents that
    /// are not wanted are stepped over without decoding, and the scan stops
    /// after the last wanted one.
    pub fn scan_positions(&self, pos_off: u64, docs: &RoaringBitmap, want: &[u32], mut f: impl FnMut(usize, u32)) -> Result<()> {
        let buf = self.positions.as_ref();
        let Some(&first) = want.first() else {
            return Ok(());
        };
        let mut at = pos_off as usize;
        let mut w = 0usize;
        // Documents of the list met since the last wanted one: their
        // positions are stepped over together.
        let mut unwanted = docs.rank(first) - docs.contains(first) as u64;
        let mut iter = docs.iter();
        iter.advance_to(first);
        for doc in iter {
            while w < want.len() && want[w] < doc {
                w += 1;
            }
            if w == want.len() {
                break;
            }
            if want[w] != doc {
                unwanted += 1;
                continue;
            }
            at = skip_positions(buf, at, std::mem::take(&mut unwanted))?;
            let mut pos = 0u32;
            loop {
                let v = read_varint(buf, &mut at)?;
                pos = pos.wrapping_add((v >> 1) as u32);
                f(w, pos);
                if v & 1 == 0 {
                    break;
                }
            }
        }
        Ok(())
    }

    fn facet_record(&self, off: u64) -> Result<(&str, RoaringBitmap)> {
        let buf = self.facets_post.as_ref();
        let mut pos = off as usize;
        let len = read_varint(buf, &mut pos)? as usize;
        let orig = buf.get(pos..pos + len).and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(|| Error::corrupt("bad facet value"))?;
        pos += len;
        Ok((orig, read_docset(buf, &mut pos)?))
    }

    pub fn facet_get(&self, key: &[u8]) -> Result<RoaringBitmap> {
        match self.facets.get(key) {
            Some(off) => Ok(self.facet_record(off)?.1),
            None => Ok(RoaringBitmap::new()),
        }
    }

    /// Documents with a numeric value of the field inside the range.
    pub fn num_range(&self, fid: u16, lo: [u8; 8], lo_incl: bool, hi: [u8; 8], hi_incl: bool) -> Result<RoaringBitmap> {
        // The sets are joined in one go: adding them one by one to a set
        // that grows costs several times as much.
        let mut parts = Vec::new();
        self.range_level(fid, TAG_NUM_L2, lo, lo_incl, hi, hi_incl, &mut parts)?;
        Ok(roaring::MultiOps::union(parts))
    }

    /// Take every run of this level that lies wholly inside the range, then
    /// cover what is left at both ends with the level below.
    #[allow(clippy::too_many_arguments)]
    fn range_level(&self, fid: u16, tag: u8, lo: [u8; 8], lo_incl: bool, hi: [u8; 8], hi_incl: bool, acc: &mut Vec<RoaringBitmap>) -> Result<()> {
        if lo > hi || (lo == hi && !(lo_incl && hi_incl)) {
            return Ok(());
        }
        if tag == TAG_NUM {
            let (lo_key, hi_key) = (facet_key(fid, TAG_NUM, &lo), facet_key(fid, TAG_NUM, &hi));
            let mut stream = self.facets.range().ge(&lo_key).le(&hi_key).into_stream();
            while let Some((key, off)) = stream.next() {
                if (!lo_incl && key == lo_key.as_slice()) || (!hi_incl && key == hi_key.as_slice()) {
                    continue;
                }
                acc.push(self.facet_record(off)?.1);
            }
            return Ok(());
        }
        let mut full: Option<([u8; 8], [u8; 8])> = None;
        {
            let from = facet_key(fid, tag, &lo);
            let to = facet_key(fid, tag, &[hi.as_slice(), [0xffu8; 8].as_slice()].concat());
            let mut stream = self.facets.range().ge(&from).le(&to).into_stream();
            while let Some((key, off)) = stream.next() {
                if key.len() != 19 {
                    continue;
                }
                let run_lo: [u8; 8] = key[3..11].try_into().unwrap();
                let run_hi: [u8; 8] = key[11..19].try_into().unwrap();
                if run_lo == lo && !lo_incl {
                    continue;
                }
                if run_hi > hi || (run_hi == hi && !hi_incl) {
                    break;
                }
                acc.push(self.facet_record(off)?.1);
                full = Some((full.map_or(run_lo, |f| f.0), run_hi));
            }
        }
        let below = if tag == TAG_NUM_L2 { TAG_NUM_L1 } else { TAG_NUM };
        match full {
            None => self.range_level(fid, below, lo, lo_incl, hi, hi_incl, acc),
            Some((first, last)) => {
                self.range_level(fid, below, lo, lo_incl, first, false, acc)?;
                self.range_level(fid, below, last, false, hi, hi_incl, acc)
            }
        }
    }

    /// Visit facet entries with keys in `[ge, le]`, in key order.
    pub fn facet_scan(&self, ge: &[u8], le: &[u8], mut f: impl FnMut(&[u8], &str, RoaringBitmap) -> Result<()>) -> Result<()> {
        let mut stream = self.facets.range().ge(ge).le(le).into_stream();
        while let Some((key, off)) = stream.next() {
            let (orig, bm) = self.facet_record(off)?;
            f(key, orig, bm)?;
        }
        Ok(())
    }

    /// The place of each of `docs` in the order of one sort column,
    /// comparable only inside this segment. Unlike `sort_key` it copies no
    /// string.
    pub fn sort_ranks(&self, fid: u16, docs: &RoaringBitmap) -> Vec<(Rank, u32)> {
        let buf = self.sort.as_ref();
        match self.sortcols.get(&fid) {
            Some(col) => docs.iter().map(|d| (col_rank(buf, col, d as usize), d)).collect(),
            None => docs.iter().map(|d| ((2, 0.0), d)).collect(),
        }
    }

    fn order(&self, fid: u16) -> Order<'_> {
        let buf = self.sort.as_ref();
        match self.sortcols.get(&fid) {
            Some(SortColMeta { order: Some((off, len)), .. }) => Order::Stored(&buf[*off as usize..(*off + *len) as usize]),
            Some(col) => Order::Built(sorted_docs(buf, col, self.doc_count as usize)),
            None => Order::Built(Vec::new()),
        }
    }

    /// The documents of `docs` that come first in the order of a sort
    /// column, descending when `asc` is false: the first `want` of them and
    /// every one tied with the last. The stored order is walked from its
    /// end, so the cost is the number of entries read, not the number of
    /// candidates. `from` is a number that no document of `docs` comes
    /// before: the walk starts there instead of at the end.
    ///
    /// `None` when the segment has no stored order for the column, or when
    /// more than `budget` entries would have to be read: the caller then
    /// ranks the candidates one by one.
    pub fn sort_head(&self, fid: u16, docs: &RoaringBitmap, want: usize, asc: bool, from: Option<f64>, budget: usize) -> Option<RoaringBitmap> {
        let col = self.sortcols.get(&fid)?;
        let (off, len) = col.order?;
        let buf = self.sort.as_ref();
        let order = &buf[off as usize..(off + len) as usize];
        let n = order.len() / 4;
        let doc_at = |i: usize| u32::from_le_bytes(order[i * 4..i * 4 + 4].try_into().unwrap());
        // How many entries, from the end the walk starts at, come before `from`.
        let skipped = match from {
            None => 0,
            Some(v) => {
                let before = |i: usize| {
                    let ord = cmp_rank(&col_rank(buf, col, doc_at(i) as usize), &(0, v));
                    if asc { ord == std::cmp::Ordering::Less } else { ord == std::cmp::Ordering::Greater }
                };
                let (mut lo, mut hi) = (0, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if before(if asc { mid } else { n - 1 - mid }) {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                lo
            }
        };
        let mut picked: Vec<u32> = Vec::with_capacity(want.min(1024) + 8);
        let mut last: Rank = (2, 0.0);
        for step in skipped..n {
            if step - skipped >= budget {
                return None;
            }
            let d = doc_at(if asc { step } else { n - 1 - step });
            if picked.len() < want {
                if docs.contains(d) {
                    picked.push(d);
                    last = col_rank(buf, col, d as usize);
                }
            } else if cmp_rank(&col_rank(buf, col, d as usize), &last) != std::cmp::Ordering::Equal {
                return Some(picked.into_iter().collect());
            } else if docs.contains(d) {
                picked.push(d);
            }
        }
        if picked.len() < want {
            // Every candidate with a value is in: the rest have none, and tie.
            return Some(docs.clone());
        }
        Some(picked.into_iter().collect())
    }

    pub fn sort_key(&self, fid: u16, docid: u32) -> SortKey {
        let Some(col) = self.sortcols.get(&fid) else { return SortKey::Missing };
        let buf = self.sort.as_ref();
        let d = docid as usize;
        if let Some((off, _)) = col.num {
            let p = off as usize + d * 8;
            let f = f64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
            if !f.is_nan() {
                return SortKey::Num(f);
            }
        }
        if let Some((off, _)) = col.ord {
            let p = off as usize + d * 4;
            let ord = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
            if ord != u32::MAX {
                let o = col.stroffs.0 as usize + ord as usize * 4;
                let a = u32::from_le_bytes(buf[o..o + 4].try_into().unwrap()) as usize;
                let b = u32::from_le_bytes(buf[o + 4..o + 8].try_into().unwrap()) as usize;
                let base = col.strdata.0 as usize;
                return SortKey::Str(buf[base + a..base + b].to_vec());
            }
        }
        SortKey::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::write_varint;

    #[test]
    fn positions_are_stepped_over_as_they_are_read() {
        // Documents with one to five positions, some of them needing two or
        // three bytes, written the way a segment stores them.
        let mut buf = Vec::new();
        let mut starts = vec![0usize];
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        for _ in 0..500 {
            let count = 1 + next() % 5;
            for i in 0..count {
                let delta = match next() % 4 {
                    0 => next() % 40_000,
                    1 => next() % 300,
                    _ => next() % 60,
                };
                write_varint(&mut buf, (delta << 1) | (i + 1 < count) as u64);
            }
            starts.push(buf.len());
        }
        for from in [0, 1, 2, 7, 63, 250, 499] {
            for docs in [0, 1, 2, 3, 8, 9, 100, 500] {
                if from + docs <= 500 {
                    assert_eq!(skip_positions(&buf, starts[from], docs as u64).unwrap(), starts[from + docs], "from {from}, {docs} documents");
                }
            }
        }
        assert!(skip_positions(&buf, starts[499], 2).is_err());
    }
}
