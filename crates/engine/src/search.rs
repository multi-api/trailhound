//! Query execution: term lookup with typo tolerance, bucket-sort ranking,
//! sorting, facets and hit formatting.

use crate::index::{universe_of, State};
use crate::segment::{cmp_rank, facet_key, Rank, Segment, SortKey, F_FILTER, TAG_NUM, TAG_STR};
use crate::settings::{attr_matches, Settings};
use crate::tokenizer::{tokenize, typo_count};
use crate::util::decode_f64;
use crate::{Error, Result};
use fst::automaton::{Automaton, Str};
use fst::{IntoStreamer, Streamer};
use roaring::RoaringBitmap;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_QUERY_WORDS: usize = 10;
/// Quoted phrases beyond these are ignored: each one costs a positional scan.
const MAX_PHRASES: usize = 10;
const MAX_PHRASE_WORDS: usize = 16;
/// Facets a single search may ask for.
const MAX_FACETS: usize = 100;
/// A query word follows at most this many terms of a segment's dictionary,
/// or the segment's share of `DERIVATION_BUDGET` by documents if that is
/// more: an index in one segment follows as many terms as the same index in
/// eight, so what a search sees does not shrink when segments are merged.
const MAX_DERIVATIONS: usize = 300;
const DERIVATION_BUDGET: usize = 2400;

/// How many dictionary terms a query word may follow in a segment holding
/// `docs` of the `total` documents of its index.
fn derivation_limit(docs: u64, total: u64) -> usize {
    MAX_DERIVATIONS.max((DERIVATION_BUDGET as u64 * docs / total.max(1)) as usize)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub page: Option<usize>,
    pub hits_per_page: Option<usize>,
    pub filter: Option<Value>,
    pub sort: Option<Vec<String>>,
    pub facets: Option<Vec<String>>,
    pub attributes_to_retrieve: Option<Vec<String>>,
    pub attributes_to_highlight: Option<Vec<String>>,
    pub attributes_to_crop: Option<Vec<String>>,
    pub crop_length: Option<usize>,
    pub crop_marker: Option<String>,
    pub highlight_pre_tag: Option<String>,
    pub highlight_post_tag: Option<String>,
    pub matching_strategy: Option<String>,
    pub attributes_to_search_on: Option<Vec<String>>,
    pub show_matches_position: Option<bool>,
    pub index_uid: Option<String>,
}

#[derive(Debug, Clone)]
struct QWord {
    text: String,
    prefix: bool,
    max_typo: usize,
}

/// Returns the query words and the quoted phrases (each a list of words).
fn parse_query(q: &str, settings: &Settings) -> (Vec<QWord>, Vec<Vec<String>>) {
    let typo = &settings.typo_tolerance;
    let mut words = Vec::new();
    let mut phrases: Vec<Vec<String>> = Vec::new();
    let mut in_quote = false;
    let parts: Vec<&str> = q.split('"').collect();
    for (i, part) in parts.iter().enumerate() {
        let last_part = i == parts.len() - 1;
        if words.len() >= MAX_QUERY_WORDS && phrases.len() >= MAX_PHRASES {
            break;
        }
        let toks = tokenize(part);
        let n = toks.len();
        // An unclosed quote is treated as plain text.
        let quoted = in_quote && !last_part;
        if quoted && !toks.is_empty() && phrases.len() < MAX_PHRASES {
            phrases.push(toks.iter().take(MAX_PHRASE_WORDS).map(|t| t.text.clone()).collect());
        }
        for (j, tok) in toks.into_iter().enumerate() {
            if !quoted && settings.stop_words.contains(&tok.text) {
                continue;
            }
            let prefix = !quoted && last_part && j == n - 1 && tok.end == part.len();
            let chars = tok.text.chars().count();
            let max_typo = if quoted || !typo.enabled || typo.disable_on_words.iter().any(|w| w.to_lowercase() == tok.text) {
                0
            } else if chars >= typo.min_word_size_for_typos.two_typos {
                2
            } else if chars >= typo.min_word_size_for_typos.one_typo {
                1
            } else {
                0
            };
            words.push(QWord { text: tok.text, prefix, max_typo });
        }
        in_quote = !in_quote;
    }
    words.truncate(MAX_QUERY_WORDS);
    (words, phrases)
}

/// Documents of `cands` in which the words appear next to each other, in
/// order, inside one field value.
fn phrase_docs(seg: &Segment, phrase: &[String], cands: &RoaringBitmap, allowed: Option<&HashSet<u16>>) -> Result<RoaringBitmap> {
    let mut found = RoaringBitmap::new();
    let mut lists = Vec::with_capacity(phrase.len());
    for word in phrase {
        match seg.terms().get(word.as_bytes()) {
            Some(off) => lists.push(seg.postings(off)?),
            None => return Ok(found),
        }
    }
    for (fid, first_docs, _) in &lists[0] {
        if allowed.is_some_and(|a| !a.contains(fid)) {
            continue;
        }
        let mut docs = cands & first_docs;
        let mut in_field = Vec::with_capacity(lists.len());
        for list in &lists {
            match list.iter().find(|(f, _, _)| f == fid) {
                Some(entry) => {
                    docs &= &entry.1;
                    in_field.push(entry);
                }
                None => docs.clear(),
            }
        }
        if docs.is_empty() {
            continue;
        }
        if phrase.len() == 1 {
            found |= docs;
            continue;
        }
        // Per word, its (document, position) pairs in document order; the
        // lists are then walked side by side.
        let want: Vec<u32> = docs.iter().collect();
        let mut positions: Vec<Vec<(u32, u32)>> = Vec::with_capacity(in_field.len());
        for (_, bm, pos_off) in &in_field {
            let mut list = Vec::new();
            seg.scan_positions(*pos_off, bm, &want, |w, p| list.push((want[w], p)))?;
            positions.push(list);
        }
        let mut at = vec![0usize; positions.len()];
        let mut slices: Vec<&[(u32, u32)]> = Vec::with_capacity(positions.len());
        for d in &docs {
            slices.clear();
            for (i, list) in positions.iter().enumerate() {
                while at[i] < list.len() && list[at[i]].0 < d {
                    at[i] += 1;
                }
                let start = at[i];
                while at[i] < list.len() && list[at[i]].0 == d {
                    at[i] += 1;
                }
                slices.push(&list[start..at[i]]);
            }
            if slices[0].iter().any(|(_, p)| (1..slices.len()).all(|i| slices[i].binary_search_by_key(&(p + i as u32), |x| x.1).is_ok())) {
                found.insert(d);
            }
        }
    }
    Ok(found)
}

impl QWord {
    fn matches(&self, token: &str) -> bool {
        if token == self.text || (self.prefix && token.starts_with(&self.text)) {
            return true;
        }
        typo_count(&self.text, token, self.prefix, self.max_typo).is_some()
    }
}

/// Where one query word matches inside one segment.
#[derive(Default)]
struct WordHits {
    /// Documents by number of typos of their best match (disjoint).
    lvl: [RoaringBitmap; 3],
    any: RoaringBitmap,
    per_field: HashMap<u16, RoaringBitmap>,
    /// The same, for the terms that match the word without a typo.
    per_field_exact: HashMap<u16, RoaringBitmap>,
    /// Posting offsets of every term this word matched.
    terms: Vec<u64>,
    /// Documents that contain the word exactly as typed, and its postings.
    exact: RoaringBitmap,
    exact_term: Option<u64>,
}

fn collect<A: Automaton>(seg: &Segment, aut: A, limit: usize, mut f: impl FnMut(&[u8], u64)) {
    let mut stream = seg.terms().search(aut).into_stream();
    let mut n = 0;
    while let Some((key, off)) = stream.next() {
        f(key, off);
        n += 1;
        if n >= limit {
            break;
        }
    }
}

/// Below this many terms the postings of a word are read in one go.
const TERMS_PER_PIECE: usize = 64;

/// The dictionary terms a query word follows in one segment: the posting
/// offset of each one with its number of typos, in file order.
struct Derived {
    exact_term: Option<u64>,
    terms: Vec<(u64, usize)>,
}

fn derive(seg: &Segment, word: &QWord, lev: Option<&Typos>, settings: &Settings, limit: usize) -> Derived {
    let mut derivs: HashMap<u64, usize> = HashMap::new();
    let mut note = |off: u64, typos: usize| {
        let e = derivs.entry(off).or_insert(typos);
        *e = (*e).min(typos);
    };
    let exact_term = seg.terms().get(word.text.as_bytes());
    if let Some(off) = exact_term {
        note(off, 0);
    }
    if word.prefix {
        collect(seg, Str::new(&word.text).starts_with(), limit, |_, off| note(off, 0));
    }
    {
        if let Some(lev) = lev {
            let mut classify = |key: &[u8], off: u64| {
                let term = std::str::from_utf8(key).unwrap_or("");
                if let Some(typos) = typo_count(&word.text, term, word.prefix, word.max_typo) {
                    note(off, typos.min(2));
                }
            };
            collect(seg, lev, limit, &mut classify);
        }
    }
    if let Some(syns) = settings.synonyms.get(&word.text) {
        for syn in syns {
            let toks = tokenize(syn);
            if toks.len() == 1
                && let Some(off) = seg.terms().get(toks[0].text.as_bytes()) {
                    note(off, 0);
                }
        }
    }
    let mut terms: Vec<(u64, usize)> = derivs.into_iter().collect();
    terms.sort_unstable();
    Derived { exact_term, terms }
}

impl WordHits {
    /// The documents of some of a word's terms. The typo levels still
    /// overlap: `finish` settles them once every term has been read.
    fn read(seg: &Segment, exact_term: Option<u64>, terms: &[(u64, usize)], allowed: Option<&HashSet<u16>>) -> Result<WordHits> {
        let mut hits = WordHits::default();
        for &(off, typos) in terms {
            hits.terms.push(off);
            for (fid, bm, _) in seg.postings(off)? {
                if allowed.is_some_and(|a| !a.contains(&fid)) {
                    continue;
                }
                if exact_term == Some(off) {
                    hits.exact |= &bm;
                }
                hits.lvl[typos] |= &bm;
                if typos == 0 {
                    *hits.per_field_exact.entry(fid).or_default() |= &bm;
                }
                *hits.per_field.entry(fid).or_default() |= bm;
            }
        }
        Ok(hits)
    }

    fn absorb(&mut self, other: WordHits) {
        if self.terms.is_empty() {
            let exact_term = self.exact_term;
            *self = other;
            self.exact_term = exact_term;
            return;
        }
        for (mine, theirs) in self.lvl.iter_mut().zip(&other.lvl) {
            *mine |= theirs;
        }
        self.exact |= other.exact;
        self.terms.extend(other.terms);
        for (fid, bm) in other.per_field {
            *self.per_field.entry(fid).or_default() |= bm;
        }
        for (fid, bm) in other.per_field_exact {
            *self.per_field_exact.entry(fid).or_default() |= bm;
        }
    }

    /// A document counts for the fewest typos it matches with.
    fn finish(&mut self) {
        let l0 = self.lvl[0].clone();
        self.lvl[1] -= &l0;
        let l01 = &l0 | &self.lvl[1];
        self.lvl[2] -= &l01;
        self.any = l01 | &self.lvl[2];
    }
}

/// Typo tolerance as an automaton over the term dictionary, computed on the
/// fly: its state is one row of the edit-distance table. Unlike a
/// precompiled automaton it costs nothing to set up, which matters because
/// every query brings new words.
struct Typos {
    query: Vec<char>,
    max: u8,
    /// Accept every term that starts with something close enough.
    prefix: bool,
}

const TYPO_MAX_CHARS: usize = 32;

#[derive(Clone, Copy)]
struct TypoState {
    row: [u8; TYPO_MAX_CHARS + 1],
    /// The row before the last character, and that character: a swap of two
    /// neighbouring characters counts as one edit.
    before: [u8; TYPO_MAX_CHARS + 1],
    last: u32,
    /// A UTF-8 character being read: its bits so far and the bytes missing.
    acc: u32,
    missing: u8,
    accepted: bool,
    dead: bool,
    /// Typos still allowed on this path. A term that starts with another
    /// letter than the query is only accepted one edit away, and only for
    /// words that allow two (see `typo_count`), so most of the dictionary is
    /// given up on at its first letter.
    max: u8,
    first: bool,
}

impl Typos {
    fn new(word: &QWord) -> Option<Typos> {
        let query: Vec<char> = word.text.chars().collect();
        if word.max_typo == 0 || query.len() > TYPO_MAX_CHARS {
            return None;
        }
        Some(Typos { query, max: word.max_typo as u8, prefix: word.prefix })
    }

    fn step(&self, state: &mut TypoState, c: char) {
        let n = self.query.len();
        if std::mem::take(&mut state.first) && c != self.query[0] {
            if state.max < 2 {
                state.dead = true;
                return;
            }
            state.max = 1;
        }
        let old = state.row;
        let mut prev_diag = state.row[0];
        state.row[0] = state.row[0].saturating_add(1);
        for j in 1..=n {
            let above = state.row[j];
            let cost = if self.query[j - 1] == c { 0 } else { 1 };
            let mut best = (above.saturating_add(1)).min(state.row[j - 1].saturating_add(1)).min(prev_diag.saturating_add(cost));
            if j > 1 && state.last == self.query[j - 1] as u32 && self.query[j - 2] == c {
                best = best.min(state.before[j - 2].saturating_add(1));
            }
            state.row[j] = best;
            prev_diag = above;
        }
        state.before = old;
        state.last = c as u32;
        if self.prefix && state.row[n] <= state.max {
            state.accepted = true;
        }
    }
}

impl Automaton for Typos {
    type State = TypoState;

    fn start(&self) -> TypoState {
        let mut row = [0u8; TYPO_MAX_CHARS + 1];
        for (j, slot) in row.iter_mut().enumerate() {
            *slot = j as u8;
        }
        let accepted = self.prefix && row[self.query.len()] <= self.max;
        TypoState { row, before: row, last: u32::MAX, acc: 0, missing: 0, accepted, dead: false, max: self.max, first: true }
    }

    fn is_match(&self, s: &TypoState) -> bool {
        !s.dead && s.missing == 0 && (s.accepted || s.row[self.query.len()] <= s.max)
    }

    fn can_match(&self, s: &TypoState) -> bool {
        !s.dead && (s.accepted || s.row[..=self.query.len()].iter().any(|d| *d <= s.max))
    }

    fn will_always_match(&self, s: &TypoState) -> bool {
        s.accepted && s.missing == 0 && !s.dead
    }

    fn accept(&self, s: &TypoState, byte: u8) -> TypoState {
        let mut next = *s;
        if next.dead {
            return next;
        }
        if next.missing > 0 {
            if byte & 0xC0 != 0x80 {
                next.dead = true;
                return next;
            }
            next.acc = (next.acc << 6) | (byte & 0x3F) as u32;
            next.missing -= 1;
        } else if byte < 0x80 {
            next.acc = byte as u32;
        } else if byte & 0xE0 == 0xC0 {
            next.acc = (byte & 0x1F) as u32;
            next.missing = 1;
        } else if byte & 0xF0 == 0xE0 {
            next.acc = (byte & 0x0F) as u32;
            next.missing = 2;
        } else if byte & 0xF8 == 0xF0 {
            next.acc = (byte & 0x07) as u32;
            next.missing = 3;
        } else {
            next.dead = true;
            return next;
        }
        if next.missing == 0 {
            match char::from_u32(next.acc) {
                Some(c) => self.step(&mut next, c),
                None => next.dead = true,
            }
        }
        next
    }
}

/// Searches running right now. One search alone may spread over the cores;
/// several at once already fill them, and extra threads would only add cost.
static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct ActiveGuard;

impl ActiveGuard {
    fn enter() -> ActiveGuard {
        ACTIVE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ActiveGuard
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn alone() -> bool {
    ACTIVE.load(std::sync::atomic::Ordering::Relaxed) <= 1
}

type Cands = Vec<RoaringBitmap>;
static PROF: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];
fn prof(i: usize, t: std::time::Instant) { PROF[i].fetch_add(t.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed); PROF[i + 3].fetch_add(1, std::sync::atomic::Ordering::Relaxed); }

/// Below this many candidate documents a stage runs on the calling thread:
/// starting threads would cost more than the work itself.
const PARALLEL_MIN_DOCS: u64 = 1_500;

/// Threads a search running alone spreads its heavy stages over.
const SPREAD: usize = 8;

fn spread() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        std::env::var("TRAILHOUND_SEARCH_THREADS").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n > 0).unwrap_or(SPREAD).min(cores)
    })
}

/// The candidates cut into pieces of work: one per segment, or, when the
/// work is worth spreading, runs of documents of about the same size, so
/// that a search uses as many threads on an index kept in one segment as on
/// one kept in eight.
fn pieces(cands: &Cands, parallel: bool) -> Vec<(usize, Cow<'_, RoaringBitmap>)> {
    cut(cands, if parallel && alone() { spread() as u64 } else { 1 })
}

fn cut(cands: &Cands, threads: u64) -> Vec<(usize, Cow<'_, RoaringBitmap>)> {
    let per = count(cands).div_ceil(threads).max(PARALLEL_MIN_DOCS / 2);
    let mut out = Vec::new();
    for (s, bm) in cands.iter().enumerate() {
        let len = bm.len();
        if len == 0 {
            continue;
        }
        let n = if threads == 1 { 1 } else { ((len + per / 2) / per).max(1) };
        if n == 1 {
            out.push((s, Cow::Borrowed(bm)));
            continue;
        }
        let (mut from, last) = (0u32, bm.max().unwrap_or(0));
        for i in 1..=n {
            // Up to the first document of the next piece, or to the end.
            let mut piece = RoaringBitmap::new();
            match bm.select((i * len / n) as u32).filter(|_| i < n) {
                Some(to) => {
                    piece.insert_range(from..to);
                    from = to;
                }
                None => {
                    piece.insert_range(from..=last);
                }
            }
            piece &= bm;
            out.push((s, Cow::Owned(piece)));
        }
    }
    out
}

/// Run `f` for every piece of work, each on its own thread when asked to.
fn per_piece<T: Send>(n: usize, parallel: bool, f: impl Fn(usize) -> Result<T> + Sync) -> Result<Vec<T>> {
    if !parallel || n < 2 || spread() < 2 || !alone() {
        return (0..n).map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut slots: Vec<Option<Result<T>>> = (0..n).map(|_| None).collect();
    std::thread::scope(|scope| {
        let (f, next) = (&f, &next);
        let work = move || {
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= n {
                    return done;
                }
                done.push((i, f(i)));
            }
        };
        // The calling thread takes its share of the pieces.
        let helpers: Vec<_> = (1..n.min(spread())).map(|_| scope.spawn(work)).collect();
        let mut done = work();
        for h in helpers {
            done.extend(h.join().unwrap_or_default());
        }
        for (i, r) in done {
            slots[i] = Some(r);
        }
    });
    slots.into_iter().map(|r| r.unwrap_or_else(|| Err(Error::corrupt("a search thread panicked")))).collect()
}

fn count(c: &Cands) -> u64 {
    c.iter().map(|b| b.len()).sum()
}

#[derive(Debug, Clone)]
enum Rule {
    Words,
    Typo,
    Proximity,
    AttributeRank,
    WordPosition,
    Exactness,
    Sort(Vec<u16>, bool),
}

struct Ctx<'a> {
    st: &'a State,
    /// `words[word][segment]`
    words: Vec<Vec<WordHits>>,
    /// Weight of every field under `searchableAttributes`, by field id; the
    /// lower the better. `None` when all fields weigh the same.
    weights: Option<Vec<u32>>,
    /// Word-position cost of the bucket being refined, when that rule has
    /// run (`u32::MAX` otherwise). Above zero, no document of the bucket has
    /// every word in its place, so none starts an attribute with the query.
    position_cost: std::sync::atomic::AtomicU32,
    /// Whether the bucket being refined is the one the typo rule made of
    /// the documents that match every word without a typo.
    no_typos: std::sync::atomic::AtomicBool,
    match_all: bool,
    allowed: Option<HashSet<u16>>,
    /// Per field, the lowest and the highest number the filter lets through.
    bounds: HashMap<u16, (Option<f64>, Option<f64>)>,
    /// What was read of the positions of the buckets being refined, the
    /// innermost last: a rule that reads positions leaves it here for the
    /// rules after it, which see parts of the same candidates.
    read: std::sync::Mutex<Vec<std::sync::Arc<Positions>>>,
}

/// What the rules that read positions need to know of a set of candidates
/// holding `k` of the query words. It comes from one pass over the posting
/// lists: proximity, word position and exactness used to make one each, and
/// the lists of common words are long.
struct Positions {
    k: usize,
    /// By segment.
    segs: Vec<SegPositions>,
}

#[derive(Default)]
struct SegPositions {
    /// The candidates, in ascending order.
    docs: Vec<u32>,
    /// For each one: how far its words are from their place in the query,
    /// added up, and their proximity when it was asked for.
    cost: Vec<u8>,
    prox: Vec<u32>,
    /// Those that hold every query word, exactly as typed, at its place in
    /// the query. Empty when `k` is not the whole query.
    starts: RoaringBitmap,
}

impl Positions {
    /// One pass over the positions of `cands`.
    fn read(ctx: &Ctx, cands: &Cands, k: usize, with_proximity: bool) -> Result<Positions> {
        let spreading = count(cands) >= PARALLEL_MIN_DOCS;
        let whole = k == ctx.words.len();
        let parts = pieces(cands, spreading);
        let read = per_piece(parts.len(), spreading, |t| -> Result<SegPositions> {
            let (s, piece) = (parts[t].0, &*parts[t].1);
            let seg = &ctx.st.segs[s].seg;
            let want: Vec<u32> = piece.iter().collect();
            let size = want.len();
            let mut cost = vec![0u8; size];
            let mut starts = vec![whole; size];
            // Per word: every (document, field, position) where it matches.
            let mut occ: Vec<Vec<(u32, u16, u32)>> = Vec::new();
            for (i, w) in ctx.words[..k].iter().enumerate() {
                let place = bucketed_position(i as u32);
                let mut best = vec![POSITION_MAX_COST; size];
                let mut placed = vec![false; size];
                let mut found = Vec::new();
                for off in &w[s].terms {
                    let exact = whole && w[s].exact_term == Some(*off);
                    for (fid, docs, pos_off) in seg.postings(*off)? {
                        if ctx.allowed.as_ref().is_some_and(|a| !a.contains(&fid)) || docs.is_disjoint(piece) {
                            continue;
                        }
                        seg.scan_positions(pos_off, &docs, &want, |idx, p| {
                            let at = bucketed_position(p);
                            let far = position_cost(at.abs_diff(i as u32));
                            if far < best[idx] {
                                best[idx] = far;
                            }
                            if exact && at == place {
                                placed[idx] = true;
                            }
                            if with_proximity {
                                found.push((want[idx], fid, p));
                            }
                        })?;
                    }
                }
                for idx in 0..size {
                    cost[idx] += best[idx];
                    starts[idx] &= placed[idx];
                }
                if with_proximity {
                    if found.windows(2).any(|w| w[0] > w[1]) {
                        found.sort_unstable();
                    }
                    occ.push(found);
                }
            }
            let mut prox = Vec::new();
            if with_proximity {
                prox.reserve(size);
                let mut at = vec![0usize; k];
                let mut slices: Vec<&[(u32, u16, u32)]> = Vec::with_capacity(k);
                for &d in &want {
                    slices.clear();
                    for (i, list) in occ.iter().enumerate() {
                        while at[i] < list.len() && list[at[i]].0 < d {
                            at[i] += 1;
                        }
                        let start = at[i];
                        while at[i] < list.len() && list[at[i]].0 == d {
                            at[i] += 1;
                        }
                        slices.push(&list[start..at[i]]);
                    }
                    prox.push(proximity(&slices));
                }
            }
            let starts = want.iter().zip(&starts).filter(|(_, placed)| **placed).map(|(d, _)| *d).collect();
            Ok(SegPositions { docs: want, cost, prox, starts })
        })?;
        // The pieces of a segment come in ascending order of documents.
        let mut segs: Vec<SegPositions> = cands.iter().map(|_| SegPositions::default()).collect();
        for ((s, _), part) in parts.iter().zip(read) {
            let seg = &mut segs[*s];
            if seg.docs.is_empty() {
                *seg = part;
            } else {
                seg.docs.extend(part.docs);
                seg.cost.extend(part.cost);
                seg.prox.extend(part.prox);
                seg.starts |= part.starts;
            }
        }
        Ok(Positions { k, segs })
    }

    /// `cands` split by a value read for each of them, or `None` if one of
    /// them was not read.
    fn split(&self, cands: &Cands, value: impl Fn(&SegPositions, usize) -> u32) -> Option<BTreeMap<u32, Cands>> {
        let nseg = cands.len();
        let mut buckets: BTreeMap<u32, Cands> = BTreeMap::new();
        for (s, bm) in cands.iter().enumerate() {
            let seg = &self.segs[s];
            let mut at = 0;
            for d in bm {
                while at < seg.docs.len() && seg.docs[at] < d {
                    at += 1;
                }
                if seg.docs.get(at) != Some(&d) {
                    return None;
                }
                // Documents come in ascending order: append, do not search.
                buckets.entry(value(seg, at)).or_insert_with(|| vec![RoaringBitmap::new(); nseg])[s].push(d);
            }
        }
        Some(buckets)
    }
}

impl Ctx<'_> {
    /// What an earlier rule read of the candidates being refined, when it
    /// read them for the same number of words.
    fn positions(&self, k: usize) -> Option<std::sync::Arc<Positions>> {
        self.read.lock().unwrap().last().filter(|p| p.k == k).cloned()
    }

    /// Runs `f` with `read` at hand for the rules it calls.
    fn with_positions<T>(&self, read: std::sync::Arc<Positions>, f: impl FnOnce() -> T) -> T {
        self.read.lock().unwrap().push(read);
        let out = f();
        self.read.lock().unwrap().pop();
        out
    }
}

/// Above this many candidates in a bucket the proximity rule is skipped:
/// reading positions for all of them would cost more than it is worth.
const PROXIMITY_MAX_DOCS: u64 = 100_000;
/// As in Meilisearch, words four or more positions apart, or in different
/// attributes, all count the same.
const MAX_PROXIMITY: u32 = 4;

/// Meilisearch-style proximity of one document: for each pair of
/// consecutive query words, the smallest distance between them (capped, and
/// one more when they appear in reverse order), summed over the pairs.
fn proximity(occ: &[&[(u32, u16, u32)]]) -> u32 {
    let mut total = 0;
    for pair in occ.windows(2) {
        // Both lists are sorted by field and position, so the closest
        // partner of an occurrence is the last one at or before it or the
        // first one after it.
        let (first, second) = (pair[0], pair[1]);
        let mut best = MAX_PROXIMITY;
        let mut next = 0;
        for a in first {
            while next < second.len() && (second[next].1, second[next].2) <= (a.1, a.2) {
                next += 1;
            }
            if let Some(b) = second.get(next).filter(|b| b.1 == a.1) {
                best = best.min(b.2 - a.2);
            }
            if let Some(b) = next.checked_sub(1).map(|at| second[at]).filter(|b| b.1 == a.1) {
                best = best.min(a.2 - b.2 + 1);
            }
            if best <= 1 {
                break;
            }
        }
        total += best;
    }
    total
}

/// Positions are compared in coarse steps, the way Meilisearch stores them:
/// exact up to 15, then 24, then powers of two.
fn bucketed_position(relative: u32) -> u32 {
    if relative < 16 {
        relative
    } else if relative < 24 {
        24
    } else {
        relative.next_power_of_two()
    }
}

const POSITION_MAX_COST: u8 = 10;

fn position_cost(distance: u32) -> u8 {
    match distance {
        0 => 0,
        1 => 1,
        2..=4 => 2,
        5..=7 => 3,
        8..=11 => 4,
        12..=16 => 5,
        17..=24 => 6,
        25..=64 => 7,
        65..=256 => 8,
        257..=1024 => 9,
        _ => POSITION_MAX_COST,
    }
}

/// A sorted search walks a segment's stored order instead of ranking every
/// candidate when the walk is expected to read at most a `WALK_MARGIN`th as
/// many entries as there are candidates, and gives up after reading one
/// entry for every `WALK_COST` candidates.
const WALK_MARGIN: u64 = 8;
const WALK_COST: usize = 4;

fn cmp_keys(a: &SortKey, b: &SortKey, asc: bool) -> Ordering {
    let rank = |k: &SortKey| match k {
        SortKey::Num(_) => 0,
        SortKey::Str(_) => 1,
        SortKey::Missing => 2,
    };
    match (a, b) {
        (SortKey::Missing, SortKey::Missing) => Ordering::Equal,
        (SortKey::Missing, _) => Ordering::Greater,
        (_, SortKey::Missing) => Ordering::Less,
        _ => {
            let ord = match (a, b) {
                (SortKey::Num(x), SortKey::Num(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
                (SortKey::Str(x), SortKey::Str(y)) => x.cmp(y),
                _ => rank(a).cmp(&rank(b)),
            };
            if asc {
                ord
            } else {
                ord.reverse()
            }
        }
    }
}

/// Bucket sort: each rule splits the candidates into ordered buckets and the
/// next rule only refines buckets that are still needed to fill the page.
fn rank(ctx: &Ctx, rules: &[Rule], cands: Cands, k: usize, out: &mut Vec<(usize, u32)>, need: usize) -> Result<()> {
    if out.len() >= need {
        return Ok(());
    }
    let total = count(&cands);
    if total == 0 {
        return Ok(());
    }
    if rules.is_empty() || total == 1 {
        for (s, bm) in cands.iter().enumerate() {
            for d in bm {
                if out.len() >= need {
                    return Ok(());
                }
                out.push((s, d));
            }
        }
        return Ok(());
    }
    let rest = &rules[1..];
    let nseg = cands.len();
    match &rules[0] {
        Rule::Words => {
            let n = ctx.words.len();
            if n == 0 {
                return rank(ctx, rest, cands, 0, out, need);
            }
            let mut seen: Cands = vec![RoaringBitmap::new(); nseg];
            let lowest = if ctx.match_all { n } else { 1 };
            for kk in (lowest..=n).rev() {
                let mut bucket = cands.clone();
                for s in 0..nseg {
                    for w in &ctx.words[..kk] {
                        bucket[s] &= &w[s].any;
                    }
                    bucket[s] -= &seen[s];
                    seen[s] |= &bucket[s];
                }
                rank(ctx, rest, bucket, kk, out, need)?;
                if out.len() >= need {
                    return Ok(());
                }
            }
        }
        Rule::Typo => {
            if k == 0 {
                return rank(ctx, rest, cands, k, out, need);
            }
            // by_sum[t][segment]: documents whose matches add up to t typos.
            let mut by_sum: Vec<Cands> = vec![cands.clone()];
            for w in &ctx.words[..k] {
                let mut next: Vec<Cands> = vec![vec![RoaringBitmap::new(); nseg]; by_sum.len() + 2];
                for (sum, bucket) in by_sum.iter().enumerate() {
                    for s in 0..nseg {
                        if bucket[s].is_empty() {
                            continue;
                        }
                        for t in 0..3 {
                            next[sum + t][s] |= &bucket[s] & &w[s].lvl[t];
                        }
                    }
                }
                by_sum = next;
            }
            for (sum, bucket) in by_sum.into_iter().enumerate() {
                ctx.no_typos.store(sum == 0, std::sync::atomic::Ordering::Relaxed);
                let done = rank(ctx, rest, bucket, k, out, need);
                ctx.no_typos.store(false, std::sync::atomic::Ordering::Relaxed);
                done?;
                if out.len() >= need {
                    return Ok(());
                }
            }
        }
        Rule::Proximity => {
            if k < 2 || total > PROXIMITY_MAX_DOCS {
                return rank(ctx, rest, cands, k, out, need);
            }
            let t_prof = std::time::Instant::now();
            let read = std::sync::Arc::new(Positions::read(ctx, &cands, k, true)?);
            prof(0, t_prof);
            let buckets = read.split(&cands, |seg, at| seg.prox[at]).ok_or_else(|| Error::corrupt("a candidate was not read"))?;
            drop(cands);
            return ctx.with_positions(read, || {
                for (_, bucket) in buckets {
                    rank(ctx, rest, bucket, k, out, need)?;
                    if out.len() >= need {
                        break;
                    }
                }
                Ok(())
            });
        }
        Rule::AttributeRank => {
            // Cost of a document: for every query word, the weight of the
            // best-placed attribute it matches in, added up.
            let Some(weights) = ctx.weights.as_ref().filter(|_| k > 0) else {
                return rank(ctx, rest, cands, k, out, need);
            };
            let worst = weights.iter().filter(|w| **w != u32::MAX).max().map_or(0, |w| w + 1);
            let no_typos = ctx.no_typos.load(std::sync::atomic::Ordering::Relaxed);
            let mut parts: BTreeMap<u32, Cands> = BTreeMap::from([(0, cands)]);
            for w in &ctx.words[..k] {
                // This word's documents per weight, per segment.
                let mut by_weight: Vec<BTreeMap<u32, RoaringBitmap>> = vec![BTreeMap::new(); nseg];
                for (s, slot) in by_weight.iter_mut().enumerate() {
                    // A document that matches without typos is placed by
                    // the attributes where it does, not by one that holds
                    // a misspelling of the word.
                    let per_field = if no_typos { &w[s].per_field_exact } else { &w[s].per_field };
                    for (fid, bm) in per_field {
                        let weight = weights.get(*fid as usize).copied().unwrap_or(u32::MAX);
                        if weight != u32::MAX {
                            *slot.entry(weight).or_default() |= bm;
                        }
                    }
                }
                let mut next: BTreeMap<u32, Cands> = BTreeMap::new();
                for (cost, bucket) in parts {
                    for s in 0..nseg {
                        let mut left = bucket[s].clone();
                        for (weight, bm) in &by_weight[s] {
                            if left.is_empty() {
                                break;
                            }
                            let here = &left & bm;
                            if !here.is_empty() {
                                left -= &here;
                                next.entry(cost + weight).or_insert_with(|| vec![RoaringBitmap::new(); nseg])[s] |= here;
                            }
                        }
                        if !left.is_empty() {
                            next.entry(cost + worst).or_insert_with(|| vec![RoaringBitmap::new(); nseg])[s] |= left;
                        }
                    }
                }
                parts = next;
            }
            for (_, bucket) in parts {
                rank(ctx, rest, bucket, k, out, need)?;
                if out.len() >= need {
                    return Ok(());
                }
            }
        }
        Rule::WordPosition => {
            if k == 0 || total > PROXIMITY_MAX_DOCS {
                return rank(ctx, rest, cands, k, out, need);
            }
            // Cost of a document: for every query word, how far its best
            // occurrence is from where the word sits in the query, added up.
            let t_prof = std::time::Instant::now();
            let known = ctx.positions(k).and_then(|read| read.split(&cands, |seg, at| seg.cost[at] as u32).map(|buckets| (None, buckets)));
            let (read, buckets) = match known {
                Some(found) => found,
                None => {
                    let read = std::sync::Arc::new(Positions::read(ctx, &cands, k, false)?);
                    let buckets = read.split(&cands, |seg, at| seg.cost[at] as u32).ok_or_else(|| Error::corrupt("a candidate was not read"))?;
                    (Some(read), buckets)
                }
            };
            prof(1, t_prof);
            drop(cands);
            let refine = || {
                for (cost, bucket) in buckets {
                    ctx.position_cost.store(cost, std::sync::atomic::Ordering::Relaxed);
                    let done = rank(ctx, rest, bucket, k, out, need);
                    ctx.position_cost.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
                    done?;
                    if out.len() >= need {
                        break;
                    }
                }
                Ok(())
            };
            return match read {
                Some(read) => ctx.with_positions(read, refine),
                None => refine(),
            };
        }
        Rule::Exactness => {
            if k == 0 {
                return rank(ctx, rest, cands, k, out, need);
            }
            // First, documents with the query words, exactly as typed, at
            // the start of an attribute. As in Meilisearch the positions of
            // different attributes are not told apart.
            let t_prof = std::time::Instant::now();
            let placed = matches!(ctx.position_cost.load(std::sync::atomic::Ordering::Relaxed), 0 | u32::MAX);
            let whole_query = placed && k == ctx.words.len() && total <= PROXIMITY_MAX_DOCS;
            let mut starts: Cands = vec![RoaringBitmap::new(); nseg];
            if let Some(read) = ctx.positions(k).filter(|_| whole_query) {
                for (s, known) in read.segs.iter().enumerate() {
                    starts[s] = &cands[s] & &known.starts;
                }
            } else if whole_query {
                let parts = pieces(&cands, total >= PARALLEL_MIN_DOCS);
                let found = per_piece(parts.len(), total >= PARALLEL_MIN_DOCS, |t| -> Result<RoaringBitmap> {
                    let s = parts[t].0;
                    let seg = &ctx.st.segs[s].seg;
                    let mut want: Vec<u32> = parts[t].1.iter().collect();
                    for (i, w) in ctx.words.iter().enumerate() {
                        let Some(off) = w[s].exact_term.filter(|_| !want.is_empty()) else {
                            return Ok(RoaringBitmap::new());
                        };
                        let wanted = bucketed_position(i as u32);
                        let mut placed = vec![false; want.len()];
                        for (fid, docs, pos_off) in seg.postings(off)? {
                            if ctx.allowed.as_ref().is_some_and(|a| !a.contains(&fid)) {
                                continue;
                            }
                            seg.scan_positions(pos_off, &docs, &want, |idx, p| {
                                if bucketed_position(p) == wanted {
                                    placed[idx] = true;
                                }
                            })?;
                        }
                        let mut keep = placed.iter();
                        want.retain(|_| *keep.next().unwrap());
                    }
                    Ok(want.into_iter().collect())
                })?;
                for ((s, _), docs) in parts.iter().zip(found) {
                    starts[*s] |= docs;
                }
            }
            prof(2, t_prof);
            for at_start in [true, false] {
                // Then, by how many words are present exactly as typed.
                let mut by_missing: Vec<Cands> = vec![(0..nseg).map(|s| if at_start { &cands[s] & &starts[s] } else { &cands[s] - &starts[s] }).collect()];
                for w in &ctx.words[..k] {
                    let mut next: Vec<Cands> = vec![vec![RoaringBitmap::new(); nseg]; by_missing.len() + 1];
                    for (missing, bucket) in by_missing.iter().enumerate() {
                        for s in 0..nseg {
                            if !bucket[s].is_empty() {
                                next[missing][s] |= &bucket[s] & &w[s].exact;
                                next[missing + 1][s] |= &bucket[s] - &w[s].exact;
                            }
                        }
                    }
                    by_missing = next;
                }
                for bucket in by_missing {
                    rank(ctx, rest, bucket, k, out, need)?;
                    if out.len() >= need {
                        return Ok(());
                    }
                }
            }
        }
        Rule::Sort(fids, asc) => {
            // Only the documents that can reach the page need their key read
            // in full. Inside a segment the order is known from the column's
            // numbers and string ranks, so each segment first keeps its best
            // `want` candidates, and whatever ties with the last of them.
            let want = need - out.len();
            let mut cands = cands;
            if let [fid] = fids[..] {
                let by_rank = |a: &Rank, b: &Rank| {
                    let ord = cmp_rank(a, b);
                    if *asc || a.0 == 2 || b.0 == 2 { ord } else { ord.reverse() }
                };
                for (s, bm) in cands.iter_mut().enumerate() {
                    let have = bm.len() as usize;
                    if have <= want {
                        continue;
                    }
                    let seg = &ctx.st.segs[s].seg;
                    // The segment keeps the documents of the field in sorted
                    // order. Walking it meets a candidate every `doc_count /
                    // have` entries when the candidates are spread evenly;
                    // with few of them, or all of them far from the start,
                    // reading the rank of each one is cheaper. A filter on
                    // the same field says where they start, as long as a
                    // document is sorted by the number that passed it.
                    let from = ctx.bounds.get(&fid).and_then(|(lo, hi)| if *asc { *lo } else { *hi }).filter(|_| seg.one_number_each(fid));
                    let expected = (want as u64).saturating_mul(seg.doc_count as u64) / have as u64;
                    if (from.is_some() || expected.saturating_mul(WALK_MARGIN) <= have as u64)
                        && let Some(head) = seg.sort_head(fid, bm, want, *asc, from, have / WALK_COST) {
                            *bm = head;
                            continue;
                        }
                    let mut ranked = seg.sort_ranks(fid, bm);
                    ranked.select_nth_unstable_by(want - 1, |a, b| by_rank(&a.0, &b.0));
                    let last = ranked[want - 1].0;
                    *bm = ranked[..want].iter().chain(ranked[want..].iter().filter(|r| by_rank(&r.0, &last) == Ordering::Equal)).map(|r| r.1).collect();
                }
            }
            let total = count(&cands);
            let mut keyed: Vec<(SortKey, usize, u32)> = Vec::with_capacity(total as usize);
            for (s, bm) in cands.iter().enumerate() {
                let seg = &ctx.st.segs[s].seg;
                for d in bm {
                    let key = fids.iter().map(|f| seg.sort_key(*f, d)).find(|k| *k != SortKey::Missing).unwrap_or(SortKey::Missing);
                    keyed.push((key, s, d));
                }
            }
            keyed.sort_by(|a, b| cmp_keys(&a.0, &b.0, *asc).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            let mut i = 0;
            while i < keyed.len() && out.len() < need {
                let mut j = i + 1;
                while j < keyed.len() && keyed[j].0 == keyed[i].0 {
                    j += 1;
                }
                if j - i == 1 || rest.is_empty() {
                    for item in &keyed[i..j] {
                        if out.len() >= need {
                            break;
                        }
                        out.push((item.1, item.2));
                    }
                } else {
                    let mut group: Cands = vec![RoaringBitmap::new(); nseg];
                    for item in &keyed[i..j] {
                        group[item.1].insert(item.2);
                    }
                    rank(ctx, rest, group, k, out, need)?;
                }
                i = j;
            }
        }
    }
    Ok(())
}

fn parse_sort(spec: &str, st: &State) -> Result<Rule> {
    let bad = || Error::user("invalid_search_sort", format!("Invalid syntax for the sort parameter: expected expression ending by `:asc` or `:desc`, found `{spec}`."));
    let (field, dir) = spec.rsplit_once(':').ok_or_else(bad)?;
    let asc = match dir {
        "asc" => true,
        "desc" => false,
        _ => return Err(bad()),
    };
    let sortable = &st.manifest.settings.sortable_attributes;
    if !attr_matches(sortable, field) {
        return Err(Error::user(
            "invalid_search_sort",
            format!("Attribute `{field}` is not sortable. Available sortable attributes are: `{}`.", sortable.join(", ")),
        ));
    }
    Ok(Rule::Sort(st.schema.id(field).into_iter().collect(), asc))
}

fn format_number(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn facet_distribution(st: &State, cands: &Cands, facets: &[String]) -> Result<(Value, Value)> {
    let settings = &st.manifest.settings;
    let mut dist = Map::new();
    let mut stats = Map::new();
    let names: Vec<String> = if facets.iter().any(|f| f == "*") {
        st.schema.fields.iter().enumerate().filter(|(i, _)| st.schema.flags(*i as u16) & F_FILTER != 0).map(|(_, n)| n.clone()).collect()
    } else {
        facets.to_vec()
    };
    let mut seen = std::collections::HashSet::new();
    let names: Vec<String> = names.into_iter().filter(|n| seen.insert(n.clone())).collect();
    if names.len() > MAX_FACETS {
        return Err(Error::user("invalid_search_facets", format!("A search can ask for at most {MAX_FACETS} facets.")));
    }
    for name in names {
        if !attr_matches(&settings.filterable_attributes, &name) {
            return Err(Error::user(
                "invalid_search_facets",
                format!("Invalid facet distribution, attribute `{name}` is not filterable. The available filterable attributes are `{}`.", settings.filterable_attributes.join(", ")),
            ));
        }
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
        if let Some(fid) = st.schema.id(&name) {
            for (s, handle) in st.segs.iter().enumerate() {
                if cands[s].is_empty() {
                    continue;
                }
                handle.seg.facet_scan(&facet_key(fid, TAG_NUM, &[]), &facet_key(fid, TAG_STR, &[0xff; 4]), |key, orig, bm| {
                    let n = bm.intersection_len(&cands[s]);
                    if n == 0 {
                        return Ok(());
                    }
                    if key[2] == TAG_NUM {
                        let f = decode_f64(&key[3..]);
                        min = min.min(f);
                        max = max.max(f);
                        *counts.entry(format_number(f)).or_insert(0) += n;
                    } else {
                        *counts.entry(orig.to_string()).or_insert(0) += n;
                    }
                    Ok(())
                })?;
            }
        }
        let by_count = settings.faceting.sort_facet_values_by.get(&name).or_else(|| settings.faceting.sort_facet_values_by.get("*")).is_some_and(|v| v == "count");
        let mut entries: Vec<(String, u64)> = counts.into_iter().collect();
        if by_count {
            entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        entries.truncate(settings.faceting.max_values_per_facet);
        dist.insert(name.clone(), Value::Object(entries.into_iter().map(|(k, v)| (k, json!(v))).collect()));
        if min.is_finite() {
            stats.insert(name, json!({ "min": min, "max": max }));
        }
    }
    Ok((Value::Object(dist), Value::Object(stats)))
}

struct Format<'a> {
    words: &'a [QWord],
    highlight: Vec<String>,
    crop: Vec<(String, Option<usize>)>,
    crop_length: usize,
    crop_marker: String,
    pre: String,
    post: String,
}

impl Format<'_> {
    fn is_match(&self, token: &str) -> bool {
        self.words.iter().any(|w| w.matches(token))
    }

    fn highlight(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 16);
        let mut last = 0;
        for tok in tokenize(text) {
            if self.is_match(&tok.text) {
                out.push_str(&text[last..tok.start]);
                out.push_str(&self.pre);
                out.push_str(&text[tok.start..tok.end]);
                out.push_str(&self.post);
                last = tok.end;
            }
        }
        out.push_str(&text[last..]);
        out
    }

    fn crop(&self, text: &str, len: usize) -> String {
        let toks = tokenize(text);
        if toks.len() <= len {
            return text.to_string();
        }
        if len == 0 {
            return self.crop_marker.clone();
        }
        let first = toks.iter().position(|t| self.is_match(&t.text)).unwrap_or(0);
        let start = first.saturating_sub(len / 2).min(toks.len() - len);
        let end = start + len;
        let mut out = String::new();
        if start > 0 {
            out.push_str(&self.crop_marker);
        }
        out.push_str(&text[toks[start].start..toks[end - 1].end]);
        if end < toks.len() {
            out.push_str(&self.crop_marker);
        }
        out
    }

    fn value(&self, path: &str, v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(m.iter().map(|(k, sub)| (k.clone(), self.value(&format!("{path}.{k}"), sub))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(|sub| self.value(path, sub)).collect()),
            Value::String(s) => {
                let mut text = s.clone();
                if let Some((_, len)) = self.crop.iter().find(|(attr, _)| attr_matches(std::slice::from_ref(attr), path)) {
                    text = self.crop(&text, len.unwrap_or(self.crop_length));
                }
                if attr_matches(&self.highlight, path) {
                    text = self.highlight(&text);
                }
                Value::String(text)
            }
            Value::Number(n) => Value::String(n.to_string()),
            other => other.clone(),
        }
    }

    fn positions(&self, path: &str, v: &Value, out: &mut Map<String, Value>) {
        match v {
            Value::Object(m) => {
                for (k, sub) in m {
                    let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    self.positions(&child, sub, out);
                }
            }
            Value::Array(a) => {
                for sub in a {
                    self.positions(path, sub, out);
                }
            }
            Value::String(s) => {
                let found: Vec<Value> = tokenize(s).iter().filter(|t| self.is_match(&t.text)).map(|t| json!({ "start": t.start, "length": t.end - t.start })).collect();
                if !found.is_empty() {
                    match out.get_mut(path) {
                        Some(Value::Array(existing)) => existing.extend(found),
                        _ => {
                            out.insert(path.to_string(), Value::Array(found));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn search(st: &State, q: &SearchQuery) -> Result<Value> {
    let _active = ActiveGuard::enter();
    let started = std::time::Instant::now();
    let settings = &st.manifest.settings;
    let paged = q.page.is_some() || q.hits_per_page.is_some();
    let (offset, limit) = if paged {
        let per_page = q.hits_per_page.unwrap_or(20);
        (q.page.unwrap_or(1).saturating_sub(1).saturating_mul(per_page), per_page)
    } else {
        (q.offset.unwrap_or(0), q.limit.unwrap_or(20))
    };
    let need = offset.saturating_add(limit).min(settings.pagination.max_total_hits);

    let filter = match &q.filter {
        Some(v) => crate::filter::parse_value(v)?,
        None => None,
    };
    let mut cands = universe_of(st, filter.as_ref())?;
    let t_filter = started.elapsed();
    let query = q.q.clone().unwrap_or_default();
    let (words, phrases) = parse_query(&query, settings);

    let allowed: Option<HashSet<u16>> = match &q.attributes_to_search_on {
        Some(attrs) if !attrs.iter().any(|a| a == "*") => Some(attrs.iter().flat_map(|a| st.schema.ids_for(a)).collect()),
        _ => None,
    };
    let levs: Vec<Option<Typos>> = words.iter().map(Typos::new).collect();
    let nseg = st.segs.len();
    let heavy = !phrases.is_empty() || words.len() > 1;
    let total_docs: u64 = st.segs.iter().map(|s| s.seg.doc_count as u64).sum();
    let spreading = count(&cands) >= PARALLEL_MIN_DOCS;
    if !phrases.is_empty() {
        let parts = pieces(&cands, spreading);
        let kept = per_piece(parts.len(), spreading, |t| -> Result<RoaringBitmap> {
            let seg = &st.segs[parts[t].0].seg;
            let mut cand = Cow::Borrowed(&*parts[t].1);
            for phrase in &phrases {
                if !cand.is_empty() {
                    cand = Cow::Owned(phrase_docs(seg, phrase, &cand, allowed.as_ref())?);
                }
            }
            Ok(cand.into_owned())
        })?;
        let mut left: Cands = vec![RoaringBitmap::new(); nseg];
        for ((s, _), docs) in parts.iter().zip(kept) {
            left[*s] |= docs;
        }
        drop(parts);
        cands = left;
    }
    let t_phrase = started.elapsed();
    // First the terms each word follows in each segment that still has
    // candidates, then their postings, a run of terms at a time.
    let pairs: Vec<(usize, usize)> = (0..nseg).filter(|s| !cands[*s].is_empty()).flat_map(|s| (0..words.len()).map(move |w| (s, w))).collect();
    let walks = words.iter().any(|w| w.prefix || w.max_typo > 0);
    let derived = per_piece(pairs.len(), spreading && (heavy || walks), |t| -> Result<Derived> {
        let (s, w) = pairs[t];
        let seg = &st.segs[s].seg;
        Ok(derive(seg, &words[w], levs[w].as_ref(), settings, derivation_limit(seg.doc_count as u64, total_docs)))
    })?;
    let followed: usize = derived.iter().map(|d| d.terms.len()).sum();
    let per = followed.div_ceil(spread()).max(TERMS_PER_PIECE);
    let runs: Vec<(usize, &[(u64, usize)])> = derived.iter().enumerate().flat_map(|(t, d)| d.terms.chunks(per).map(move |run| (t, run))).collect();
    let read = per_piece(runs.len(), spreading && (heavy || followed >= 2 * TERMS_PER_PIECE), |r| -> Result<WordHits> {
        let (t, run) = runs[r];
        WordHits::read(&st.segs[pairs[t].0].seg, derived[t].exact_term, run, allowed.as_ref())
    })?;
    // hits[word][segment]
    let mut hits: Vec<Vec<WordHits>> = words.iter().map(|_| (0..nseg).map(|_| WordHits::default()).collect()).collect();
    for (&(s, w), d) in pairs.iter().zip(&derived) {
        hits[w][s].exact_term = d.exact_term;
    }
    for ((t, _), part) in runs.iter().zip(read) {
        let (s, w) = pairs[*t];
        hits[w][s].absorb(part);
    }
    for h in hits.iter_mut().flatten() {
        h.finish();
    }
    let match_all = q.matching_strategy.as_deref() == Some("all");
    if !words.is_empty() {
        for (s, cand) in cands.iter_mut().enumerate() {
            if match_all {
                for w in &hits {
                    *cand &= &w[s].any;
                }
            } else {
                *cand &= &hits[0][s].any;
            }
        }
    }
    let total = count(&cands);
    let t_words = started.elapsed();

    let sort_rules: Vec<Rule> = q.sort.iter().flatten().map(|s| parse_sort(s, st)).collect::<Result<_>>()?;
    let mut rules = Vec::new();
    let mut has_words = false;
    for r in &settings.ranking_rules {
        match r.as_str() {
            "words" => {
                has_words = true;
                rules.push(Rule::Words);
            }
            "typo" => rules.push(Rule::Typo),
            // The legacy name stands for both of the rules it was split into.
            "attribute" => {
                rules.push(Rule::AttributeRank);
                rules.push(Rule::WordPosition);
            }
            "attributeRank" => rules.push(Rule::AttributeRank),
            "wordPosition" => rules.push(Rule::WordPosition),
            "sort" => rules.extend(sort_rules.iter().cloned()),
            "proximity" => rules.push(Rule::Proximity),
            "exactness" => rules.push(Rule::Exactness),
            custom => {
                if let Some((field, dir)) = custom.rsplit_once(':') {
                    rules.push(Rule::Sort(st.schema.id(field).into_iter().collect(), dir != "desc"));
                }
            }
        }
    }
    if !has_words {
        rules.insert(0, Rule::Words);
    }

    let weights: Option<Vec<u32>> = if settings.searchable_attributes.iter().any(|a| a == "*") {
        None
    } else {
        Some(st.schema.fields.iter().map(|name| settings.searchable_attributes.iter().position(|a| attr_matches(std::slice::from_ref(a), name)).map_or(u32::MAX, |i| i as u32)).collect())
    };
    // Like Meilisearch: whichever of the legacy and the split names comes first wins.
    let mut seen = [false; 3];
    rules.retain(|r| {
        let slot = match r {
            Rule::AttributeRank => 0,
            Rule::WordPosition => 1,
            Rule::Exactness => 2,
            _ => return true,
        };
        !std::mem::replace(&mut seen[slot], true)
    });
    let bounds = filter.iter().flat_map(crate::filter::bounds).filter_map(|(field, lo, hi)| Some((st.schema.id(field)?, (lo, hi)))).collect();
    let ctx = Ctx { st, words: hits, weights, position_cost: std::sync::atomic::AtomicU32::new(u32::MAX), no_typos: std::sync::atomic::AtomicBool::new(false), match_all, allowed, bounds, read: Default::default() };
    let mut ranked = Vec::new();
    let facet_cands = if q.facets.is_some() { Some(cands.clone()) } else { None };
    rank(&ctx, &rules, cands, 0, &mut ranked, need)?;
    let t_rank = started.elapsed();

    let format = Format {
        words: &words,
        highlight: q.attributes_to_highlight.clone().unwrap_or_default(),
        crop: q
            .attributes_to_crop
            .iter()
            .flatten()
            .map(|c| match c.rsplit_once(':') {
                Some((attr, len)) if len.parse::<usize>().is_ok() => (attr.to_string(), len.parse().ok()),
                _ => (c.clone(), None),
            })
            .collect(),
        crop_length: q.crop_length.unwrap_or(10),
        crop_marker: q.crop_marker.clone().unwrap_or_else(|| "…".to_string()),
        pre: q.highlight_pre_tag.clone().unwrap_or_else(|| "<em>".to_string()),
        post: q.highlight_post_tag.clone().unwrap_or_else(|| "</em>".to_string()),
    };
    let want_formatted = q.attributes_to_highlight.is_some() || q.attributes_to_crop.is_some();
    let retrieve = q.attributes_to_retrieve.clone().unwrap_or_else(|| vec!["*".to_string()]);

    let mut out_hits = Vec::new();
    for (s, d) in ranked.into_iter().skip(offset) {
        let doc = st.segs[s].seg.doc(d)?;
        let Value::Object(full) = doc else { continue };
        let shown: Map<String, Value> = full.iter().filter(|(k, _)| attr_matches(&settings.displayed_attributes, k)).map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut hit: Map<String, Value> = shown.iter().filter(|(k, _)| attr_matches(&retrieve, k)).map(|(k, v)| (k.clone(), v.clone())).collect();
        if want_formatted {
            let formatted: Map<String, Value> = hit.iter().map(|(k, v)| (k.clone(), format.value(k, v))).collect();
            hit.insert("_formatted".into(), Value::Object(formatted));
        }
        if q.show_matches_position == Some(true) {
            let mut pos = Map::new();
            format.positions("", &Value::Object(shown), &mut pos);
            hit.insert("_matchesPosition".into(), Value::Object(pos));
        }
        out_hits.push(Value::Object(hit));
    }

    let t_hyd = started.elapsed();
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *TRACE.get_or_init(|| std::env::var_os("TRAILHOUND_TRACE").is_some()) {
        eprintln!("[trailhound] search filter={:.2} match={:.2} merge={:.2} rank={:.2} hydrate={:.2} ms prox={}us/{} pos={}us/{} exact={}us/{} q={}", t_filter.as_secs_f64()*1e3, (t_phrase-t_filter).as_secs_f64()*1e3, (t_words-t_phrase).as_secs_f64()*1e3, (t_rank-t_words).as_secs_f64()*1e3, (t_hyd-t_rank).as_secs_f64()*1e3, PROF[0].swap(0, std::sync::atomic::Ordering::Relaxed), PROF[3].swap(0, std::sync::atomic::Ordering::Relaxed), PROF[1].swap(0, std::sync::atomic::Ordering::Relaxed), PROF[4].swap(0, std::sync::atomic::Ordering::Relaxed), PROF[2].swap(0, std::sync::atomic::Ordering::Relaxed), PROF[5].swap(0, std::sync::atomic::Ordering::Relaxed), query);
    }
    let mut res = Map::new();
    res.insert("hits".into(), Value::Array(out_hits));
    res.insert("query".into(), json!(query));
    res.insert("processingTimeMs".into(), json!(started.elapsed().as_millis() as u64));
    if paged {
        let capped = (total as usize).min(settings.pagination.max_total_hits);
        res.insert("hitsPerPage".into(), json!(limit));
        res.insert("page".into(), json!(q.page.unwrap_or(1)));
        res.insert("totalPages".into(), json!(if limit == 0 { 0 } else { capped.div_ceil(limit) }));
        res.insert("totalHits".into(), json!(capped));
    } else {
        res.insert("limit".into(), json!(limit));
        res.insert("offset".into(), json!(offset));
        res.insert("estimatedTotalHits".into(), json!(total));
    }
    if let (Some(facets), Some(fc)) = (&q.facets, &facet_cands) {
        let (dist, stats) = facet_distribution(st, fc, facets)?;
        res.insert("facetDistribution".into(), dist);
        res.insert("facetStats".into(), stats);
    }
    Ok(Value::Object(res))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_followed_do_not_depend_on_how_an_index_is_split() {
        // One segment follows what eight equal ones follow together.
        assert_eq!(derivation_limit(1_000_000, 1_000_000), 8 * derivation_limit(125_000, 1_000_000));
        assert_eq!(derivation_limit(125_000, 1_000_000), MAX_DERIVATIONS);
        // A small segment next to a large one keeps the floor.
        assert_eq!(derivation_limit(10, 1_000_000), MAX_DERIVATIONS);
        assert_eq!(derivation_limit(900_000, 1_000_000), 2160);
        assert_eq!(derivation_limit(0, 0), MAX_DERIVATIONS);
    }

    #[test]
    fn proximity_is_the_closest_pair_of_each_two_words() {
        // Every pair in the same field, the slow way.
        let slow = |occ: &[&[(u32, u16, u32)]]| -> u32 {
            occ.windows(2)
                .map(|pair| {
                    let mut best = MAX_PROXIMITY;
                    for a in pair[0] {
                        for b in pair[1].iter().filter(|b| b.1 == a.1) {
                            best = best.min(if b.2 > a.2 { b.2 - a.2 } else { a.2 - b.2 + 1 });
                        }
                    }
                    best
                })
                .sum()
        };
        let mut seed = 11u64;
        let mut next = |below: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % below
        };
        for _ in 0..2000 {
            let words: Vec<Vec<(u32, u16, u32)>> = (0..2 + next(3))
                .map(|_| {
                    let mut list: Vec<(u32, u16, u32)> = (0..next(6)).map(|_| (7, next(3) as u16, next(12) as u32)).collect();
                    list.sort_unstable();
                    list.dedup();
                    list
                })
                .collect();
            let slices: Vec<&[(u32, u16, u32)]> = words.iter().map(|w| w.as_slice()).collect();
            assert_eq!(proximity(&slices), slow(&slices), "{words:?}");
        }
    }

    #[test]
    fn candidates_are_cut_into_runs_that_cover_them_once() {
        let large: RoaringBitmap = (0..200_000u32).filter(|d| d % 3 != 1).chain(4_000_000_000..4_000_000_500).collect();
        let small: RoaringBitmap = (10..40u32).collect();
        let cands = vec![large.clone(), RoaringBitmap::new(), small.clone()];
        // On one thread every segment that has candidates is one piece.
        let whole = cut(&cands, 1);
        assert_eq!(whole.iter().map(|(s, p)| (*s, p.len())).collect::<Vec<_>>(), [(0, large.len()), (2, small.len())]);
        // On eight, the large segment is cut into even runs of documents in
        // ascending order and the small one is left alone.
        let parts = cut(&cands, 8);
        assert_eq!(parts.last().map(|(s, p)| (*s, p.as_ref())), Some((2, &small)));
        let runs: Vec<&RoaringBitmap> = parts.iter().filter(|(s, _)| *s == 0).map(|(_, p)| p.as_ref()).collect();
        assert_eq!(runs.len(), 8);
        let mut joined = RoaringBitmap::new();
        for run in &runs {
            assert!(run.len().abs_diff(large.len() / 8) <= 1, "{} documents", run.len());
            assert!(joined.max() < run.min());
            joined |= *run;
        }
        assert_eq!(joined, large);
        // Too few candidates to be worth a second thread.
        assert_eq!(cut(&vec![small.clone()], 8).len(), 1);
    }
}
