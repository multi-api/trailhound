use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MinWordSizeForTypos {
    pub one_typo: usize,
    pub two_typos: usize,
}

impl Default for MinWordSizeForTypos {
    fn default() -> Self {
        Self { one_typo: 5, two_typos: 9 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TypoTolerance {
    pub enabled: bool,
    pub min_word_size_for_typos: MinWordSizeForTypos,
    pub disable_on_words: Vec<String>,
    pub disable_on_attributes: Vec<String>,
}

impl Default for TypoTolerance {
    fn default() -> Self {
        Self { enabled: true, min_word_size_for_typos: Default::default(), disable_on_words: vec![], disable_on_attributes: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Faceting {
    pub max_values_per_facet: usize,
    pub sort_facet_values_by: BTreeMap<String, String>,
}

impl Default for Faceting {
    fn default() -> Self {
        let mut by = BTreeMap::new();
        by.insert("*".to_string(), "alpha".to_string());
        Self { max_values_per_facet: 100, sort_facet_values_by: by }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Pagination {
    pub max_total_hits: usize,
}

impl Default for Pagination {
    fn default() -> Self {
        Self { max_total_hits: 1000 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub displayed_attributes: Vec<String>,
    pub searchable_attributes: Vec<String>,
    pub filterable_attributes: Vec<String>,
    pub sortable_attributes: Vec<String>,
    pub ranking_rules: Vec<String>,
    pub stop_words: BTreeSet<String>,
    pub synonyms: BTreeMap<String, Vec<String>>,
    pub distinct_attribute: Option<String>,
    pub typo_tolerance: TypoTolerance,
    pub faceting: Faceting,
    pub pagination: Pagination,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            displayed_attributes: vec!["*".into()],
            searchable_attributes: vec!["*".into()],
            filterable_attributes: vec![],
            sortable_attributes: vec![],
            ranking_rules: ["words", "typo", "proximity", "attributeRank", "sort", "wordPosition", "exactness"].iter().map(|s| s.to_string()).collect(),
            stop_words: BTreeSet::new(),
            synonyms: BTreeMap::new(),
            distinct_attribute: None,
            typo_tolerance: Default::default(),
            faceting: Default::default(),
            pagination: Default::default(),
        }
    }
}

impl Settings {
    /// True when moving to `other` changes what is stored in segments.
    pub fn needs_reindex(&self, other: &Settings) -> bool {
        self.searchable_attributes != other.searchable_attributes
            || self.filterable_attributes != other.filterable_attributes
            || self.sortable_attributes != other.sortable_attributes
    }
}

/// `list` selects `name` when it holds `*`, the name itself, or a parent of it.
pub fn attr_matches(list: &[String], name: &str) -> bool {
    list.iter().any(|p| p == "*" || p == name || (name.len() > p.len() && name.starts_with(p.as_str()) && name.as_bytes()[p.len()] == b'.'))
}
