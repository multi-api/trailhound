use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

pub const MAX_TERM_BYTES: usize = 250;

#[derive(Debug, Clone)]
pub struct Token {
    /// Byte range of the token in the original text.
    pub start: usize,
    pub end: usize,
    /// Lowercased text without diacritics.
    pub text: String,
}

fn push_normalized(c: char, out: &mut String) {
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
        return;
    }
    for d in c.nfd() {
        if !is_combining_mark(d) {
            for l in d.to_lowercase() {
                out.push(l);
            }
        }
    }
}

/// Split text into alphanumeric tokens, lowercased and without diacritics.
pub fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            if cur.is_empty() {
                start = i;
            }
            if cur.len() < MAX_TERM_BYTES {
                push_normalized(c, &mut cur);
            }
        } else if !cur.is_empty() {
            out.push(Token { start, end: i, text: std::mem::take(&mut cur) });
        }
    }
    if !cur.is_empty() {
        out.push(Token { start, end: text.len(), text: cur });
    }
    out
}

/// Normalization used for filter and facet string values.
pub fn normalize_facet(s: &str) -> String {
    let mut out = s.trim().to_lowercase();
    if out.len() > 480 {
        let mut cut = 480;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

/// Edit distance between `a` and `b`, where swapping two neighbouring
/// characters counts as one edit. With `prefix`, the distance between `a`
/// and the closest prefix of `b`.
pub fn edit_distance(a: &str, b: &str, prefix: bool) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut before: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut best = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(before[j - 2] + 1);
            }
            cur[j] = best;
        }
        std::mem::swap(&mut before, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    if prefix {
        prev.iter().copied().min().unwrap_or(0)
    } else {
        prev[b.len()]
    }
}

/// The typos charged for matching `term` against the query word, the way
/// Meilisearch counts them, or `None` when it is not a match: a typo on the
/// first character counts double, so it needs a word that allows two.
pub fn typo_count(query: &str, term: &str, prefix: bool, max: usize) -> Option<usize> {
    if max == 0 {
        return None;
    }
    let distance = edit_distance(query, term, prefix);
    if query.chars().next() == term.chars().next() {
        (distance <= max).then_some(distance)
    } else {
        (max >= 2 && distance <= 1).then_some(2)
    }
}
