//! Meilisearch filter expressions: parser and evaluation against a segment.

use crate::segment::{facet_key, Schema, Segment, F_FILTER, TAG_EXISTS, TAG_NULL, TAG_NUM, TAG_STR};
use crate::settings::attr_matches;
use crate::tokenizer::normalize_facet;
use crate::util::encode_f64;
use crate::{Error, Result};
use roaring::RoaringBitmap;
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    Eq(String, String),
    /// field, lower bound, upper bound; each bound is (value, inclusive).
    Range(String, Option<(f64, bool)>, Option<(f64, bool)>),
    In(String, Vec<String>),
    Exists(String),
    IsNull(String),
}

fn err(msg: impl Into<String>) -> Error {
    Error::user("invalid_search_filter", msg)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Op(String),
    Word(String, bool),
}

fn lex(s: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1
            }
            '=' => {
                out.push(Tok::Op("=".into()));
                i += 1
            }
            '!' | '>' | '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    out.push(Tok::Op(format!("{c}=")));
                    i += 2;
                } else if c == '!' {
                    return Err(err(format!("Unexpected `!` at position {i} in filter `{s}`.")));
                } else {
                    out.push(Tok::Op(c.to_string()));
                    i += 1;
                }
            }
            '"' | '\'' => {
                let mut w = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(err(format!("Expression `{s}` is missing a closing quote."))),
                        Some('\\') if i + 1 < chars.len() => {
                            w.push(chars[i + 1]);
                            i += 2;
                        }
                        Some(q) if *q == c => {
                            i += 1;
                            break;
                        }
                        Some(ch) => {
                            w.push(*ch);
                            i += 1;
                        }
                    }
                }
                out.push(Tok::Word(w, true));
            }
            _ => {
                let start = i;
                while i < chars.len() && !chars[i].is_whitespace() && !"()[],=!<>".contains(chars[i]) {
                    i += 1;
                }
                out.push(Tok::Word(chars[start..i].iter().collect(), false));
            }
        }
    }
    Ok(out)
}

/// Deepest nesting of `NOT` and parentheses accepted. Every level is a stack
/// frame in the parser and again when the filter is evaluated, so an
/// unbounded filter could overflow the stack and take the process down.
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
    src: &'a str,
}

impl Parser<'_> {
    fn peek_kw(&self, kw: &str) -> bool {
        matches!(self.toks.get(self.pos), Some(Tok::Word(w, false)) if w.eq_ignore_ascii_case(kw))
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.peek_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn deeper(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(err(format!("The filter is nested more than {MAX_DEPTH} levels deep.")));
        }
        Ok(())
    }

    fn bad(&self) -> Error {
        err(format!("Was expecting an operation `=`, `!=`, `>=`, `>`, `<=`, `<`, `IN`, `NOT IN`, `TO`, `EXISTS`, `NOT EXISTS`, `IS NULL`, `IS NOT NULL` in filter `{}`.", self.src))
    }

    fn word(&mut self) -> Result<String> {
        match self.toks.get(self.pos) {
            Some(Tok::Word(w, _)) => {
                self.pos += 1;
                Ok(w.clone())
            }
            _ => Err(err(format!("Was expecting a value in filter `{}`.", self.src))),
        }
    }

    fn number(&mut self) -> Result<f64> {
        let w = self.word()?;
        w.parse::<f64>().map_err(|_| err(format!("Value `{w}` in filter `{}` must be a number.", self.src)))
    }

    fn or(&mut self) -> Result<Filter> {
        let mut parts = vec![self.and()?];
        while self.eat_kw("OR") {
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Filter::Or(parts) })
    }

    fn and(&mut self) -> Result<Filter> {
        let mut parts = vec![self.not()?];
        while self.eat_kw("AND") {
            parts.push(self.not()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Filter::And(parts) })
    }

    fn not(&mut self) -> Result<Filter> {
        if self.eat_kw("NOT") {
            self.deeper()?;
            let inner = self.not()?;
            self.depth -= 1;
            return Ok(Filter::Not(Box::new(inner)));
        }
        self.primary()
    }

    fn list(&mut self) -> Result<Vec<String>> {
        if self.toks.get(self.pos) != Some(&Tok::LBracket) {
            return Err(err(format!("Expected `[` after `IN` in filter `{}`.", self.src)));
        }
        self.pos += 1;
        let mut vals = Vec::new();
        loop {
            match self.toks.get(self.pos) {
                Some(Tok::RBracket) => {
                    self.pos += 1;
                    return Ok(vals);
                }
                Some(Tok::Comma) => self.pos += 1,
                Some(Tok::Word(w, _)) => {
                    vals.push(w.clone());
                    self.pos += 1;
                }
                _ => return Err(err(format!("Expression `{}` is missing a closing `]`.", self.src))),
            }
        }
    }

    fn primary(&mut self) -> Result<Filter> {
        if self.toks.get(self.pos) == Some(&Tok::LParen) {
            self.pos += 1;
            self.deeper()?;
            let inner = self.or()?;
            self.depth -= 1;
            if self.toks.get(self.pos) != Some(&Tok::RParen) {
                return Err(err(format!("Expression `{}` is missing a closing `)`.", self.src)));
            }
            self.pos += 1;
            return Ok(inner);
        }
        let field = self.word()?;
        if field.starts_with("_geo") {
            return Err(err("Geo filters are not supported yet."));
        }
        match self.toks.get(self.pos).cloned() {
            Some(Tok::Op(op)) => {
                self.pos += 1;
                match op.as_str() {
                    "=" => Ok(Filter::Eq(field, self.word()?)),
                    "!=" => Ok(Filter::Not(Box::new(Filter::Eq(field, self.word()?)))),
                    ">" => Ok(Filter::Range(field, Some((self.number()?, false)), None)),
                    ">=" => Ok(Filter::Range(field, Some((self.number()?, true)), None)),
                    "<" => Ok(Filter::Range(field, None, Some((self.number()?, false)))),
                    _ => Ok(Filter::Range(field, None, Some((self.number()?, true)))),
                }
            }
            Some(Tok::Word(_, false)) if self.eat_kw("IN") => Ok(Filter::In(field, self.list()?)),
            Some(Tok::Word(_, false)) if self.eat_kw("EXISTS") => Ok(Filter::Exists(field)),
            Some(Tok::Word(_, false)) if self.eat_kw("NOT") => {
                if self.eat_kw("IN") {
                    Ok(Filter::Not(Box::new(Filter::In(field, self.list()?))))
                } else if self.eat_kw("EXISTS") {
                    Ok(Filter::Not(Box::new(Filter::Exists(field))))
                } else {
                    Err(self.bad())
                }
            }
            Some(Tok::Word(_, false)) if self.eat_kw("IS") => {
                let negate = self.eat_kw("NOT");
                if !self.eat_kw("NULL") {
                    return Err(err("Only `IS NULL` and `IS NOT NULL` are supported."));
                }
                let f = Filter::IsNull(field);
                Ok(if negate { Filter::Not(Box::new(f)) } else { f })
            }
            Some(Tok::Word(_, _)) => {
                let lo = self.number()?;
                if !self.eat_kw("TO") {
                    return Err(self.bad());
                }
                let hi = self.number()?;
                Ok(Filter::Range(field, Some((lo, true)), Some((hi, true))))
            }
            _ => Err(self.bad()),
        }
    }
}

pub fn parse_str(s: &str) -> Result<Filter> {
    let mut p = Parser { toks: lex(s)?, pos: 0, depth: 0, src: s };
    let f = p.or()?;
    if p.pos != p.toks.len() {
        return Err(err(format!("Found unexpected characters at the end of the filter `{s}`.")));
    }
    Ok(f)
}

/// Accepts a filter string, or an array whose items are strings (ANDed) or
/// arrays of strings (ORed).
pub fn parse_value(v: &Value) -> Result<Option<Filter>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.trim().is_empty() => Ok(None),
        Value::String(s) => Ok(Some(parse_str(s)?)),
        Value::Array(items) => {
            let mut ands = Vec::new();
            for item in items {
                match item {
                    Value::String(s) => ands.push(parse_str(s)?),
                    Value::Array(ors) => {
                        let mut parts = Vec::new();
                        for o in ors {
                            let s = o.as_str().ok_or_else(|| err("Invalid syntax for the filter parameter: expected a string."))?;
                            parts.push(parse_str(s)?);
                        }
                        ands.push(Filter::Or(parts));
                    }
                    _ => return Err(err("Invalid syntax for the filter parameter: expected a string or an array.")),
                }
            }
            Ok(if ands.is_empty() { None } else { Some(Filter::And(ands)) })
        }
        _ => Err(err("Invalid syntax for the filter parameter: expected a string or an array.")),
    }
}

fn field_ids(schema: &Schema, filterable: &[String], field: &str) -> Result<Vec<u16>> {
    if !attr_matches(filterable, field) {
        return Err(err(format!(
            "Attribute `{field}` is not filterable. Available filterable attributes are: `{}`.",
            filterable.join(", ")
        )));
    }
    Ok(schema.id(field).filter(|id| schema.flags(*id) & F_FILTER != 0).into_iter().collect())
}

fn eq(seg: &Segment, fid: u16, value: &str) -> Result<RoaringBitmap> {
    let mut bm = seg.facet_get(&facet_key(fid, TAG_STR, normalize_facet(value).as_bytes()))?;
    if let Ok(f) = value.trim().parse::<f64>() {
        let f = if f == 0.0 { 0.0 } else { f };
        bm |= seg.facet_get(&facet_key(fid, TAG_NUM, &encode_f64(f)))?;
    }
    Ok(bm)
}

pub fn eval(f: &Filter, seg: &Segment, schema: &Schema, filterable: &[String]) -> Result<RoaringBitmap> {
    Ok(match f {
        Filter::And(parts) => {
            let mut acc: Option<RoaringBitmap> = None;
            for p in parts {
                let bm = eval(p, seg, schema, filterable)?;
                acc = Some(match acc {
                    Some(a) => a & bm,
                    None => bm,
                });
            }
            acc.unwrap_or_default()
        }
        Filter::Or(parts) => {
            let mut acc = RoaringBitmap::new();
            for p in parts {
                acc |= eval(p, seg, schema, filterable)?;
            }
            acc
        }
        Filter::Not(inner) => {
            let mut all = RoaringBitmap::new();
            all.insert_range(0..seg.doc_count);
            all - eval(inner, seg, schema, filterable)?
        }
        Filter::Eq(field, value) => {
            let mut acc = RoaringBitmap::new();
            for fid in field_ids(schema, filterable, field)? {
                acc |= eq(seg, fid, value)?;
            }
            acc
        }
        Filter::In(field, values) => {
            let mut acc = RoaringBitmap::new();
            for fid in field_ids(schema, filterable, field)? {
                for v in values {
                    acc |= eq(seg, fid, v)?;
                }
            }
            acc
        }
        Filter::Exists(field) => {
            let mut acc = RoaringBitmap::new();
            for fid in field_ids(schema, filterable, field)? {
                acc |= seg.facet_get(&facet_key(fid, TAG_EXISTS, &[]))?;
            }
            acc
        }
        Filter::IsNull(field) => {
            let mut acc = RoaringBitmap::new();
            for fid in field_ids(schema, filterable, field)? {
                acc |= seg.facet_get(&facet_key(fid, TAG_NULL, &[]))?;
            }
            acc
        }
        Filter::Range(field, lo, hi) => {
            let mut acc = RoaringBitmap::new();
            for fid in field_ids(schema, filterable, field)? {
                let (lo_key, lo_incl) = lo.map_or(([0u8; 8], true), |(v, incl)| (encode_f64(v), incl));
                let (hi_key, hi_incl) = hi.map_or(([0xffu8; 8], true), |(v, incl)| (encode_f64(v), incl));
                acc |= seg.num_range(fid, lo_key, lo_incl, hi_key, hi_incl)?;
            }
            acc
        }
    })
}
