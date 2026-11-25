use trailhound_engine::{Index, SearchQuery, Settings};
use serde_json::{json, Value};

fn movies() -> Vec<Value> {
    serde_json::from_str(
        r#"[
        {"id": 1, "title": "Carol", "genres": ["Romance", "Drama"], "year": 2015, "rating": 7.2, "overview": "Una fotógrafa conoce a una mujer mayor en Nueva York."},
        {"id": 2, "title": "Wonder Woman", "genres": ["Action", "Adventure"], "year": 2017, "rating": 7.4, "overview": "An Amazon princess leaves her island."},
        {"id": 3, "title": "Life of Pi", "genres": ["Adventure", "Drama"], "year": 2012, "rating": 7.9, "overview": "A boy and a tiger on a boat."},
        {"id": 4, "title": "Mad Max: Fury Road", "genres": ["Adventure", "Science Fiction"], "year": 2015, "rating": 8.1, "overview": "An apocalyptic story set in a desert wasteland."},
        {"id": 5, "title": "Moana", "genres": ["Fantasy", "Action"], "year": 2016, "rating": 7.6, "overview": "An adventurous teenager sails out on a daring mission."},
        {"id": 6, "title": "Philadelphia", "genres": ["Drama"], "year": 1993, "rating": 7.7, "overview": "A lawyer is fired because of his illness.", "big": 12345678901234567890123, "price": 1.50}
    ]"#,
    )
    .unwrap()
}

fn q(v: Value) -> SearchQuery {
    serde_json::from_value(v).unwrap()
}

fn titles(res: &Value) -> Vec<String> {
    res["hits"].as_array().unwrap().iter().map(|h| h["title"].as_str().unwrap().to_string()).collect()
}

fn setup(dir: &std::path::Path) -> Index {
    let index = Index::create(dir, "movies", None, 0).unwrap();
    index.add_documents(movies(), None, false, 1).unwrap();
    let s = Settings { filterable_attributes: vec!["genres".into(), "year".into()], sortable_attributes: vec!["year".into(), "title".into(), "rating".into()], ..Default::default() };
    index.update_settings(s, 2).unwrap();
    index
}

#[test]
fn search_typos_prefix_and_diacritics() {
    let dir = tempfile::tempdir().unwrap();
    let index = setup(dir.path());
    assert_eq!(titles(&index.search(&q(json!({"q": "philadelfia"}))).unwrap()), ["Philadelphia"]);
    assert_eq!(titles(&index.search(&q(json!({"q": "wond"}))).unwrap()), ["Wonder Woman"]);
    assert_eq!(titles(&index.search(&q(json!({"q": "fotografa"}))).unwrap()), ["Carol"]);
    // Exact matches rank before typo matches; title before overview.
    let res = index.search(&q(json!({"q": "adventure"}))).unwrap();
    assert_eq!(res["estimatedTotalHits"], 4);
    let all = index.search(&q(json!({"q": ""}))).unwrap();
    assert_eq!(all["estimatedTotalHits"], 6);
}

#[test]
fn filters_sort_facets() {
    let dir = tempfile::tempdir().unwrap();
    let index = setup(dir.path());
    let res = index.search(&q(json!({"filter": "genres = Drama AND year > 2000", "sort": ["year:desc"]}))).unwrap();
    assert_eq!(titles(&res), ["Carol", "Life of Pi"]);
    let res = index.search(&q(json!({"filter": [["genres = action", "genres = fantasy"], "year 2016 TO 2017"], "sort": ["title:asc"]}))).unwrap();
    assert_eq!(titles(&res), ["Moana", "Wonder Woman"]);
    let res = index.search(&q(json!({"filter": "genres NOT IN [Drama, Action]", "sort": ["rating:asc"]}))).unwrap();
    assert_eq!(titles(&res), ["Mad Max: Fury Road"]);
    let res = index.search(&q(json!({"q": "", "facets": ["genres", "year"], "limit": 0}))).unwrap();
    assert_eq!(res["facetDistribution"]["genres"]["Adventure"], 3);
    assert_eq!(res["facetDistribution"]["genres"]["Drama"], 3);
    assert_eq!(res["facetDistribution"]["year"]["2015"], 2);
    assert_eq!(res["facetStats"]["year"]["min"], 1993.0);
    assert!(index.search(&q(json!({"filter": "rating > 5"}))).is_err());
    let res = index.search(&q(json!({"q": "tiger", "attributesToHighlight": ["overview"]}))).unwrap();
    assert_eq!(res["hits"][0]["_formatted"]["overview"], "A boy and a <em>tiger</em> on a boat.");
}

#[test]
fn documents_round_trip_exactly_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let index = setup(dir.path());
        index.delete_documents(&["2".to_string()], 3).unwrap();
        index.add_documents(vec![json!({"id": 3, "title": "Life of Pi (Extended)"})], None, true, 4).unwrap();
    }
    // A leftover from an interrupted write must be ignored and cleaned up.
    std::fs::write(dir.path().join("seg-00009999.tmp"), b"garbage").unwrap();
    std::fs::write(dir.path().join("seg-00009998.seg"), b"garbage").unwrap();
    let index = Index::open(dir.path()).unwrap();
    assert!(!dir.path().join("seg-00009998.seg").exists());
    assert_eq!(index.stats().number_of_documents, 5);
    assert!(index.get_document("2").unwrap().is_none());
    let pi = index.get_document("3").unwrap().unwrap();
    assert_eq!(pi["title"], "Life of Pi (Extended)");
    assert_eq!(pi["year"], 2012);
    // Key order, big numbers and trailing zeros come back byte for byte.
    let phil = index.get_document("6").unwrap().unwrap();
    assert_eq!(
        serde_json::to_string(&phil).unwrap(),
        r#"{"id":6,"title":"Philadelphia","genres":["Drama"],"year":1993,"rating":7.7,"overview":"A lawyer is fired because of his illness.","big":12345678901234567890123,"price":1.50}"#
    );
    assert_eq!(titles(&index.search(&q(json!({"q": "extended"}))).unwrap()), ["Life of Pi (Extended)"]);
    assert_eq!(index.search(&q(json!({"q": "wonder"}))).unwrap()["estimatedTotalHits"], 0);
}

#[test]
fn many_batches_merge_and_stay_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "n", Some("id".into()), 0).unwrap();
    for batch in 0..30u64 {
        let docs: Vec<Value> = (0..50).map(|i| json!({"id": batch * 25 + i, "text": format!("documento numero {} lote {batch}", batch * 25 + i)})).collect();
        index.add_documents(docs, None, false, batch + 1).unwrap();
    }
    let stats = index.stats();
    assert_eq!(stats.number_of_documents, 29 * 25 + 50);
    assert!(stats.segments <= 9, "segments: {}", stats.segments);
    let res = index.search(&q(json!({"q": "documento", "limit": 1}))).unwrap();
    assert_eq!(res["estimatedTotalHits"], 29 * 25 + 50);
    let (docs, total) = index.list_documents(10, 5, None).unwrap();
    assert_eq!((docs.len(), total), (5, 29 * 25 + 50));
    // Corrupting a stored block must be detected, not served.
    let seg = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "seg")).unwrap();
    drop(index);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes[20] ^= 0xff;
    std::fs::write(&seg, bytes).unwrap();
    let index = Index::open(dir.path()).unwrap();
    assert!(index.list_documents(0, 100_000, None).is_err());
}

#[test]
fn phrases_and_proximity() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "p", Some("id".into()), 0).unwrap();
    let docs = [json!({"id": 1, "title": "motor de busqueda rapido"}),
        json!({"id": 2, "title": "busqueda con un motor lento"}),
        json!({"id": 3, "title": "motor electrico", "tags": ["de", "busqueda"]}),
        json!({"id": 4, "title": "el motor muy rapido y silencioso de busqueda"}),
        json!({"id": 5, "title": "motor busqueda"})];
    // Two batches, so phrases and proximity also work across segments.
    index.add_documents(docs[..3].to_vec(), None, false, 1).unwrap();
    index.add_documents(docs[3..].to_vec(), None, false, 2).unwrap();
    let ids = |v: Value| -> Vec<u64> { index.search(&q(v)).unwrap()["hits"].as_array().unwrap().iter().map(|h| h["id"].as_u64().unwrap()).collect() };

    // A phrase needs the words adjacent, in order, in the same value.
    assert_eq!(ids(json!({"q": "\"motor de busqueda\""})), [1]);
    assert_eq!(ids(json!({"q": "\"motor busqueda\""})), [5]);
    assert_eq!(ids(json!({"q": "\"de busqueda\""})), [1, 4]);
    assert_eq!(ids(json!({"q": "\"busqueda motor\""})), Vec::<u64>::new());
    // No typo tolerance and no prefix inside quotes.
    assert_eq!(ids(json!({"q": "\"busqued\""})), Vec::<u64>::new());
    // A phrase combined with a free word.
    assert_eq!(ids(json!({"q": "\"de busqueda\" silencioso", "matchingStrategy": "all"})), [4]);

    // Proximity: adjacent first, then one word apart. Past that, and across
    // fields, every distance counts the same; word position then puts first
    // the documents whose words sit nearer the start of an attribute.
    assert_eq!(ids(json!({"q": "motor busqueda", "matchingStrategy": "all"})), [5, 1, 2, 3, 4]);
    // Swapped letters are one typo; a typo on the first letter counts double.
    assert_eq!(ids(json!({"q": "motro"})).len(), 5);
    assert_eq!(ids(json!({"q": "notor"})), Vec::<u64>::new());
    assert_eq!(ids(json!({"q": "silenciozo"})), [4]);
    assert_eq!(ids(json!({"q": "zilencioso"})), [4]);
}

#[test]
fn large_writes_are_chunked_and_keep_the_last_version() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "c", Some("id".into()), 0).unwrap();
    // 25,000 documents over 12,000 ids: the same id comes back in a later
    // chunk, while its first version sits in a segment that is not finished.
    let docs: Vec<Value> = (0..25_000u64).map(|i| json!({"id": i % 12_000, "v": i, "text": format!("palabra{}", i)})).collect();
    index.add_documents(docs, None, false, 1).unwrap();
    assert_eq!(index.stats().number_of_documents, 12_000);
    assert_eq!(index.get_document("5").unwrap().unwrap()["v"], 24_005);
    assert_eq!(index.get_document("11999").unwrap().unwrap()["v"], 23_999);
    assert_eq!(index.search(&q(json!({"q": "\"palabra5\""}))).unwrap()["estimatedTotalHits"], 0);
    assert_eq!(index.search(&q(json!({"q": "\"palabra24005\""}))).unwrap()["estimatedTotalHits"], 1);

    // Partial updates that hit the same id twice across chunks, and ids stored earlier.
    let patch: Vec<Value> = (0..22_000u64).map(|i| if i < 11_000 { json!({"id": i, "a": i}) } else { json!({"id": i - 11_000, "b": i}) }).collect();
    index.add_documents(patch, None, true, 2).unwrap();
    assert_eq!(index.stats().number_of_documents, 12_000);
    let d = index.get_document("7").unwrap().unwrap();
    assert_eq!((d["v"].as_u64(), d["a"].as_u64(), d["b"].as_u64()), (Some(24_007), Some(7), Some(11_007)));
    let d = index.get_document("10999").unwrap().unwrap();
    assert_eq!((d["v"].as_u64(), d["a"].as_u64(), d["b"].as_u64()), (Some(22_999), Some(10_999), Some(21_999)));

    // A bad document anywhere in the stream leaves the index as it was.
    let mut bad: Vec<Value> = (0..15_000u64).map(|i| json!({"id": 50_000 + i})).collect();
    bad.push(json!({"id": "no valido"}));
    assert!(index.add_documents(bad, None, false, 3).is_err());
    assert_eq!(index.stats().number_of_documents, 12_000);
    drop(index);
    assert_eq!(Index::open(dir.path()).unwrap().stats().number_of_documents, 12_000);
}

/// An index that went through many merges, deletions and updates must answer
/// exactly like one built from the same final documents in a single write.
#[test]
fn merged_index_matches_a_fresh_one() {
    let settings = || {
        Settings { filterable_attributes: vec!["tag".into(), "price".into(), "extra".into()], sortable_attributes: vec!["price".into(), "name".into()], ..Default::default() }
    };
    let make = |i: u64| json!({"id": i, "title": format!("alfa beta {i} gamma"), "tag": [(["x", "y", "z"][(i % 3) as usize]), "todos"], "price": i as f64 / 2.0, "name": format!("nombre {:04}", (i * 37) % 1000)});
    let dir = tempfile::tempdir().unwrap();
    let merged = Index::create(dir.path(), "m", Some("id".into()), 0).unwrap();
    merged.update_settings(settings(), 1).unwrap();
    let mut expected: std::collections::BTreeMap<u64, Value> = Default::default();
    let mut task = 2;
    for batch in 0..20u64 {
        let docs: Vec<Value> = (0..30).map(|i| make(batch * 30 + i)).collect();
        for d in &docs {
            expected.insert(d["id"].as_u64().unwrap(), d.clone());
        }
        merged.add_documents(docs, None, false, task).unwrap();
        task += 1;
    }
    let gone: Vec<String> = (0..600u64).filter(|i| i % 7 == 0).map(|i| i.to_string()).collect();
    for id in &gone {
        expected.remove(&id.parse().unwrap());
    }
    merged.delete_documents(&gone, task).unwrap();
    for batch in 0..12u64 {
        // Partial updates of live documents, re-creation of deleted ones.
        let ids: Vec<u64> = (0..20).map(|i| (batch * 53 + i * 11) % 600).collect();
        let docs: Vec<Value> = ids.iter().map(|i| json!({"id": i, "extra": format!("lote {batch}"), "title": format!("delta beta {i} epsilon"), "price": *i as f64 / 2.0})).collect();
        for d in &docs {
            let id = d["id"].as_u64().unwrap();
            let slot = expected.entry(id).or_insert_with(|| json!({}));
            for (k, v) in d.as_object().unwrap() {
                slot[k] = v.clone();
            }
        }
        task += 1;
        merged.add_documents(docs, None, true, task).unwrap();
    }
    assert!(merged.stats().segments <= 9, "segments: {}", merged.stats().segments);

    let dir2 = tempfile::tempdir().unwrap();
    let fresh = Index::create(dir2.path(), "f", Some("id".into()), 0).unwrap();
    fresh.update_settings(settings(), 1).unwrap();
    fresh.add_documents(expected.values().cloned().collect(), None, false, 2).unwrap();
    assert_eq!(fresh.stats().segments, 1);

    let (a, b) = (merged.stats(), fresh.stats());
    assert_eq!(a.number_of_documents, b.number_of_documents);
    for (id, doc) in &expected {
        assert_eq!(serde_json::to_string(&merged.get_document(&id.to_string()).unwrap().unwrap()).unwrap(), serde_json::to_string(doc).unwrap());
    }
    for id in &gone {
        assert_eq!(merged.get_document(id).unwrap().is_some(), expected.contains_key(&id.parse().unwrap()));
    }
    let queries = [
        json!({"q": "alfa", "sort": ["price:asc"]}),
        json!({"q": "delta beta", "sort": ["price:desc"]}),
        json!({"q": "\"beta 17 gamma\""}),
        json!({"q": "\"delta beta\"", "sort": ["price:asc"]}),
        json!({"q": "epsilom delt", "sort": ["price:asc"], "matchingStrategy": "all"}),
        json!({"q": "", "filter": "tag = x AND price > 100", "sort": ["name:desc", "price:asc"]}),
        json!({"q": "beta", "filter": "extra EXISTS AND tag != z", "sort": ["name:asc", "price:asc"]}),
        json!({"q": "", "filter": "price 10 TO 20.5 OR extra = 'lote 3'", "sort": ["price:asc"]}),
        json!({"q": "", "facets": ["tag", "extra", "price"], "sort": ["price:asc"]}),
    ];
    for mut query in queries {
        query["limit"] = json!(1000);
        query["attributesToRetrieve"] = json!(["id"]);
        let (mut x, mut y) = (merged.search(&q(query.clone())).unwrap(), fresh.search(&q(query.clone())).unwrap());
        x["processingTimeMs"] = json!(0);
        y["processingTimeMs"] = json!(0);
        assert!(x["estimatedTotalHits"].as_u64().unwrap() > 0, "{query}");
        assert_eq!(x, y, "{query}");
    }
    // And it reads the same after a restart.
    let before = merged.search(&q(json!({"q": "beta", "sort": ["price:asc"], "limit": 1000, "attributesToRetrieve": ["id"]}))).unwrap()["hits"].clone();
    drop(merged);
    let reopened = Index::open(dir.path()).unwrap();
    assert_eq!(reopened.search(&q(json!({"q": "beta", "sort": ["price:asc"], "limit": 1000, "attributesToRetrieve": ["id"]}))).unwrap()["hits"], before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count(), 0);
}

/// Field ids are assigned by a reader that does not build the documents;
/// the indexing threads must end up with exactly the same fields.
#[test]
fn nested_fields_survive_parallel_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "n", Some("id".into()), 0).unwrap();
    let s = Settings { filterable_attributes: vec!["meta".into(), "tags".into(), "vacio".into()], sortable_attributes: vec!["meta.peso".into()], ..Default::default() };
    index.update_settings(s, 1).unwrap();
    // New fields keep appearing deep into the stream, in every shape.
    let docs: Vec<Value> = (0..6_000u64)
        .map(|i| {
            let mut d = json!({"id": i, "titulo": format!("articulo {i}"), "meta": {"peso": i as f64 * 1.5, "origen": {"pais": (["cl", "ar", "pe"][(i % 3) as usize])}, "nada": null}, "tags": [{"k": "a", "n": 1e3}, "suelto", []], "vacio": {}, "grande": 123456789012345678901234567890u128.to_string().parse::<serde_json::Number>().unwrap(), "ok": true});
            d[format!("extra_{}", i / 500)] = json!({"x": [i]});
            d
        })
        .collect();
    index.add_documents(docs.clone(), None, false, 2).unwrap();
    let stats = index.stats();
    assert_eq!(stats.number_of_documents, 6_000);
    let mut expected: Vec<String> = ["id", "titulo", "meta", "meta.peso", "meta.origen", "meta.origen.pais", "meta.nada", "tags", "tags.k", "tags.n", "vacio", "grande", "ok"].iter().map(|s| s.to_string()).collect();
    for e in 0..12 {
        expected.push(format!("extra_{e}"));
        expected.push(format!("extra_{e}.x"));
    }
    expected.sort();
    assert_eq!(index.field_distribution().unwrap().keys().cloned().collect::<Vec<_>>(), expected);
    assert_eq!(index.field_distribution().unwrap()["meta.origen.pais"], 6_000);
    assert_eq!(index.field_distribution().unwrap()["extra_3.x"], 500);
    let count = |v: Value| index.search(&q(v)).unwrap()["estimatedTotalHits"].as_u64().unwrap();
    assert_eq!(count(json!({"filter": "meta.origen.pais = ar"})), 2_000);
    assert_eq!(count(json!({"filter": "tags.k = a AND meta.peso < 15"})), 10);
    assert_eq!(count(json!({"filter": "vacio EXISTS"})), 6_000);
    assert_eq!(count(json!({"filter": "meta.nada IS NULL"})), 6_000);
    assert_eq!(count(json!({"q": "\"articulo 4321\""})), 1);
    let top = index.search(&q(json!({"q": "articulo", "sort": ["meta.peso:desc"], "limit": 2}))).unwrap();
    assert_eq!(top["hits"][0]["id"], 5_999);
    assert_eq!(serde_json::to_string(&index.get_document("4321").unwrap().unwrap()).unwrap(), serde_json::to_string(&docs[4321]).unwrap());
    // A scalar where a document should be is refused, and nothing changes.
    assert!(index.add_documents(vec![json!({"id": 7_000}), json!(5)], None, false, 3).is_err());
    assert_eq!(index.stats().number_of_documents, 6_000);
}

/// Range filters go through precomputed runs of values; every answer must
/// match a plain scan of the documents, before and after merging.
#[test]
fn range_filters_match_a_full_scan() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "r", Some("id".into()), 0).unwrap();
    let s = Settings { filterable_attributes: vec!["n".into(), "few".into()], ..Default::default() };
    index.update_settings(s, 1).unwrap();
    // 9,000 distinct values (two levels of runs), negatives, repeats and gaps.
    let value = |i: u64| -> f64 { ((i * 7919) % 9000) as f64 * 0.25 - 600.0 };
    let docs: Vec<Value> = (0..12_000u64).map(|i| if i % 50 == 49 { json!({"id": i, "few": i % 7}) } else { json!({"id": i, "n": [value(i), value(i + 3)], "few": i % 7}) }).collect();
    let mut task = 2;
    for batch in docs.chunks(1_000) {
        index.add_documents(batch.to_vec(), None, false, task).unwrap();
        task += 1;
    }
    let gone: Vec<String> = (0..12_000u64).filter(|i| i % 11 == 0).map(|i| i.to_string()).collect();
    index.delete_documents(&gone, task).unwrap();
    let live: Vec<u64> = (0..12_000u64).filter(|i| i % 11 != 0).collect();
    let values = |i: u64| -> Vec<f64> { if i % 50 == 49 { vec![] } else { vec![value(i), value(i + 3)] } };
    let count = |filter: String| index.search(&q(json!({"filter": filter, "limit": 0}))).unwrap()["estimatedTotalHits"].as_u64().unwrap();
    let bounds = [-700.0, -600.0, -599.75, -100.5, 0.0, 0.25, 15.75, 16.0, 500.0, 1000.0, 1649.75, 1650.0, 2000.0];
    for &a in &bounds {
        let want = |pred: &dyn Fn(f64) -> bool| live.iter().filter(|i| values(**i).into_iter().any(pred)).count() as u64;
        assert_eq!(count(format!("n < {a}")), want(&|v| v < a), "n < {a}");
        assert_eq!(count(format!("n <= {a}")), want(&|v| v <= a), "n <= {a}");
        assert_eq!(count(format!("n > {a}")), want(&|v| v > a), "n > {a}");
        assert_eq!(count(format!("n >= {a}")), want(&|v| v >= a), "n >= {a}");
        for &b in &bounds {
            assert_eq!(count(format!("n {a} TO {b}")), want(&|v| v >= a && v <= b), "n {a} TO {b}");
            assert_eq!(count(format!("n > {a} AND n < {b}")), live.iter().filter(|i| values(**i).into_iter().any(|v| v > a) && values(**i).into_iter().any(|v| v < b)).count() as u64);
        }
    }
    assert_eq!(count("few >= 2 AND few < 5".to_string()), live.iter().filter(|i| (2..5).contains(&(**i % 7))).count() as u64);
    assert_eq!(count("n = 16".to_string()), live.iter().filter(|i| values(**i).contains(&16.0)).count() as u64);
}

/// Typo tolerance walks the term dictionary with an on-the-fly automaton;
/// it must find exactly the terms that a plain edit-distance check accepts,
/// for plain words and prefixes, ASCII or not.
#[test]
fn typo_matching_agrees_with_edit_distance() {
    use trailhound_engine::tokenizer::{tokenize, typo_count};
    let syll = ["ma", "ri", "to", "ñe", "lú", "pa", "se", "co", "東", "京", "ba", "di", "gü", "xo", "ne", "ka"];
    let mut seed = 7u64;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let mut vocab: Vec<String> = Vec::new();
    while vocab.len() < 4_000 {
        let w: String = (0..2 + next(5)).map(|_| syll[next(syll.len())]).collect();
        let norm = tokenize(&w)[0].text.clone();
        if !vocab.contains(&norm) {
            vocab.push(norm);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "t", Some("id".into()), 0).unwrap();
    // Two writes, so there is more than one segment to walk.
    let docs: Vec<Value> = vocab.iter().enumerate().map(|(i, w)| json!({"id": i, "w": w, "fijo": "comun"})).collect();
    index.add_documents(docs[..2_500].to_vec(), None, false, 1).unwrap();
    index.add_documents(docs[2_500..].to_vec(), None, false, 2).unwrap();
    let count = |v: Value| index.search(&q(v)).unwrap()["estimatedTotalHits"].as_u64().unwrap();
    let allowed = |chars: usize| if chars >= 9 { 2 } else if chars >= 5 { 1 } else { 0 };
    let mut checked = 0;
    for i in 0..300 {
        // A stored word, damaged: one character dropped, replaced or added.
        let mut chars: Vec<char> = vocab[(i * 13) % vocab.len()].chars().collect();
        let at = next(chars.len());
        match i % 4 {
            0 => {
                chars.remove(at);
            }
            1 => chars[at] = ['z', 'ñ', '京', 'q'][next(4)],
            2 => chars.insert(at, ['w', 'é', '東'][next(3)]),
            _ => {}
        }
        let query = tokenize(&chars.iter().collect::<String>()).first().map(|t| t.text.clone()).unwrap_or_default();
        let n = query.chars().count();
        if n < 2 {
            continue;
        }
        let max = allowed(n);
        // Last word of the query: a prefix.
        let as_prefix = vocab.iter().filter(|w| w.starts_with(&query) || typo_count(&query, w, true, max).is_some()).count() as u64;
        // Followed by another word: a whole word.
        let as_word = vocab.iter().filter(|w| **w == query || typo_count(&query, w, false, max).is_some()).count() as u64;
        if as_prefix > 250 {
            continue; // past the cap on derivations, where the engine stops early
        }
        assert_eq!(count(json!({"q": query, "limit": 0})), as_prefix, "prefix `{query}`");
        assert_eq!(count(json!({"q": format!("{query} comun"), "matchingStrategy": "all", "limit": 0})), as_word, "word `{query}`");
        checked += 1;
    }
    assert!(checked > 200, "only {checked} queries were checked");
}

/// Sorting picks each segment's best candidates before comparing across
/// segments; a page must be the same as when every document is sorted.
#[test]
fn sorted_pages_match_a_full_sort() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "s", Some("id".into()), 0).unwrap();
    let s = Settings { sortable_attributes: vec!["n".into(), "name".into(), "mixed".into()], ..Default::default() };
    index.update_settings(s, 1).unwrap();
    // Few distinct values, so that ties cross the edge of every page; some
    // documents have no value, and `mixed` holds numbers and strings.
    let n = |i: u64| (i * 7919) % 40;
    let name = |i: u64| format!("n{:02}", (i * 104729) % 55);
    let docs: Vec<Value> = (0..900u64)
        .map(|i| {
            let mut d = json!({"id": i, "w": "comun"});
            if i % 9 != 0 {
                d["n"] = json!(n(i));
                d["name"] = json!(name(i));
            }
            if i % 4 != 0 {
                d["mixed"] = if i % 3 == 0 { json!(name(i)) } else { json!(n(i)) };
            }
            d
        })
        .collect();
    for (task, batch) in docs.chunks(150).enumerate() {
        index.add_documents(batch.to_vec(), None, false, task as u64 + 2).unwrap();
    }
    for (field, dir) in [("n", "asc"), ("n", "desc"), ("name", "asc"), ("name", "desc"), ("mixed", "asc"), ("mixed", "desc")] {
        let all = index.search(&q(json!({"q": "comun", "sort": [format!("{field}:{dir}")], "limit": 900, "attributesToRetrieve": ["id", field]}))).unwrap();
        let all = all["hits"].as_array().unwrap();
        assert_eq!(all.len(), 900);
        for (offset, limit) in [(0, 1), (0, 20), (15, 30), (490, 25), (700, 200)] {
            let page = index.search(&q(json!({"q": "comun", "sort": [format!("{field}:{dir}")], "offset": offset, "limit": limit, "attributesToRetrieve": ["id", field]}))).unwrap();
            let page = page["hits"].as_array().unwrap();
            assert_eq!(page.len(), limit);
            // Ties may come out in another order, never with another value.
            for (got, want) in page.iter().zip(&all[offset..]) {
                assert_eq!(got.get(field), want.get(field), "{field}:{dir} offset {offset} limit {limit}");
            }
        }
    }
}

/// The end of a sentence or of a clause keeps two words apart, as in
/// Meilisearch: no phrase across it, and no better proximity than words
/// that are far from each other.
#[test]
fn hard_separators_keep_words_apart() {
    use trailhound_engine::tokenizer::tokenize;
    let breaks = |text: &str| tokenize(text).iter().map(|t| t.after_break).collect::<Vec<_>>();
    assert_eq!(breaks("red fox. Blue, bird; grey-wolf (old) cat"), [false, false, true, true, true, false, false, false]);
    // A full stop or a comma inside a word or a number is not a break.
    assert_eq!(breaks("version 3.5 of a.b,c"), [false; 7]);
    assert_eq!(breaks("...what? Yes"), [false, true]);

    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "b", Some("id".into()), 0).unwrap();
    let docs = json!([
        {"id": 1, "t": "the red fox jumps"},
        {"id": 2, "t": "it was red. Fox hunting is banned"},
        {"id": 3, "t": "red and then some more words fox"}
    ]);
    index.add_documents(docs.as_array().unwrap().clone(), None, false, 1).unwrap();
    let ids = |v: Value| index.search(&q(v)).unwrap()["hits"].as_array().unwrap().iter().map(|h| h["id"].as_u64().unwrap()).collect::<Vec<_>>();
    assert_eq!(ids(json!({"q": "\"red fox\""})), [1]);
    assert_eq!(ids(json!({"q": "red fox", "matchingStrategy": "all"}))[0], 1);
}

#[test]
fn deeply_nested_filters_are_refused() {
    use trailhound_engine::filter::parse_str;
    // Each level is a stack frame: without a limit a long enough filter
    // would overflow the stack and abort the process.
    assert!(parse_str(&format!("{}a = 1", "NOT ".repeat(60))).is_ok());
    assert!(parse_str(&format!("{}a = 1{}", "(".repeat(60), ")".repeat(60))).is_ok());
    assert!(parse_str(&format!("{}a = 1", "NOT ".repeat(20_000))).is_err());
    assert!(parse_str(&format!("{}a = 1{}", "(".repeat(20_000), ")".repeat(20_000))).is_err());
    // Width is not depth: a long flat filter is fine.
    assert!(parse_str(&vec!["a = 1"; 5_000].join(" OR ")).is_ok());
}

#[test]
fn field_distribution_counts_only_live_documents() {
    let dir = tempfile::tempdir().unwrap();
    let index = setup(dir.path());
    let dist = |index: &Index| index.field_distribution().unwrap();
    assert_eq!(dist(&index)["title"], 6);
    assert_eq!(dist(&index)["price"], 1);

    // A deleted document stays in its segment, but is no longer counted.
    index.delete_documents(&["6".to_string()], 3).unwrap();
    assert_eq!(index.stats().number_of_documents, 5);
    assert_eq!(dist(&index)["title"], 5);
    assert!(!dist(&index).contains_key("price"), "{:?}", dist(&index));

    // A replaced document is counted once, with the fields of its new version.
    index.add_documents(vec![json!({"id": 1, "title": "Carol", "director": "Todd Haynes"})], None, false, 4).unwrap();
    assert_eq!(dist(&index)["title"], 5);
    assert_eq!(dist(&index)["overview"], 4);
    assert_eq!(dist(&index)["director"], 1);

    // A partial update keeps the fields it does not mention.
    index.add_documents(vec![json!({"id": 2, "seen": true})], None, true, 5).unwrap();
    assert_eq!(dist(&index)["title"], 5);
    assert_eq!(dist(&index)["seen"], 1);

    index.delete_by_filter(&json!("year < 2016"), 6).unwrap();
    assert_eq!(index.stats().number_of_documents, 3);
    assert_eq!(dist(&index)["title"], 3);

    // The answer is the same after the index is opened again.
    let before = dist(&index);
    drop(index);
    assert_eq!(dist(&Index::open(dir.path()).unwrap()), before);
}

#[test]
fn rfc3339_dates_compare_across_offsets() {
    use trailhound_engine::rfc3339_nanos as at;
    assert_eq!(at("1970-01-01T00:00:01Z"), Some(1_000_000_000));
    assert_eq!(at("2026-01-31T12:00:00+02:00"), at("2026-01-31T10:00:00Z"));
    assert_eq!(at("2026-01-31 10:00:00Z"), at("2026-01-31T10:00:00Z"));
    assert_eq!(at("2026-01-31"), at("2026-01-31T00:00:00Z"));
    assert!(at("2026-01-31T10:00:00.5Z") > at("2026-01-31T10:00:00Z"));
    assert!(at(&trailhound_engine::now_rfc3339()) > at("2026-01-01"));
    for bad in ["", "yesterday", "2026-13-01", "2026-01-31T10:00:00"] {
        assert_eq!(at(bad), None, "{bad}");
    }
}

/// The value a document is sorted by: of several, the first.
fn sort_value(v: &Value) -> &Value {
    match v {
        Value::Array(items) => items.first().unwrap_or(&Value::Null),
        other => other,
    }
}

/// What a sorted search must return for `field` over `docs`: numbers before
/// strings when ascending and the reverse when descending, and documents
/// without a value last either way. `None` stands for no value.
fn sorted_values(docs: &[&Value], field: &str, asc: bool) -> Vec<Option<Value>> {
    let mut nums: Vec<f64> = docs.iter().filter_map(|d| sort_value(&d[field]).as_f64()).collect();
    let mut strs: Vec<&str> = docs.iter().filter_map(|d| sort_value(&d[field]).as_str()).collect();
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap());
    strs.sort();
    let mut out: Vec<Option<Value>> = nums.into_iter().map(|n| Some(json!(n))).chain(strs.into_iter().map(|s| Some(json!(s)))).collect();
    if !asc {
        out.reverse();
    }
    out.resize(docs.len(), None);
    out
}

/// Compare pages of sorted searches with a sort done here, value by value:
/// documents with the same value may come out in any order.
fn assert_sorted(index: &Index, docs: &[&Value], filter: Option<&str>, fields: &[&str], label: &str) {
    for field in fields {
        for asc in [true, false] {
            let expected = sorted_values(docs, field, asc);
            for (offset, limit) in [(0usize, 1usize), (0, 10), (7, 25), (120, 40)] {
                let sort = format!("{field}:{}", if asc { "asc" } else { "desc" });
                let res = index.search(&q(json!({"q": "", "filter": filter, "sort": [sort], "offset": offset, "limit": limit, "attributesToRetrieve": ["id", field]}))).unwrap();
                let hits = res["hits"].as_array().unwrap();
                let got: Vec<Option<Value>> = hits.iter().map(|h| h.get(*field).map(sort_value).map(|v| if v.is_number() { json!(v.as_f64().unwrap()) } else { v.clone() })).collect();
                let want: Vec<Option<Value>> = expected.iter().skip(offset).take(limit).cloned().collect();
                assert_eq!(got, want, "{label}: filter {filter:?}, {sort}, offset {offset}, limit {limit}");
                let mut ids: Vec<u64> = hits.iter().map(|h| h["id"].as_u64().unwrap()).collect();
                ids.sort_unstable();
                ids.dedup();
                assert_eq!(ids.len(), hits.len(), "{label}: a document came twice");
                assert!(ids.iter().all(|id| docs.iter().any(|d| d["id"] == *id)), "{label}: a document outside the filter");
            }
        }
    }
}

/// A sorted search reads each segment's stored order until the page is full,
/// or ranks every candidate when they are few or far from the start. Both
/// must give what sorting the matching documents gives: with many, few and
/// no candidates, with a filter on the field that is sorted, with documents
/// deleted, and after the segments have been merged and opened again.
#[test]
fn sorted_searches_match_a_sort_of_the_documents() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "s", Some("id".into()), 0).unwrap();
    let s = Settings { filterable_attributes: vec!["k".into(), "n".into()], sortable_attributes: vec!["n".into(), "name".into(), "mixed".into(), "rare".into()], ..Default::default() };
    index.update_settings(s, 1).unwrap();
    let make = |i: u64| {
        let mut d = json!({"id": i, "k": i % 10});
        if !i.is_multiple_of(9) {
            d["n"] = json!((i * 7919) % 977);
            d["name"] = json!(format!("n{:03}", (i * 104729) % 350));
        }
        if !i.is_multiple_of(4) {
            d["mixed"] = if i.is_multiple_of(3) { json!(format!("m{:02}", i % 61)) } else { json!((i * 31) % 211) };
        }
        if i % 50 == 3 {
            d["rare"] = json!(i as f64 / 8.0);
        }
        d
    };
    let mut docs: std::collections::BTreeMap<u64, Value> = (0..6000u64).map(|i| (i, make(i))).collect();
    let all: Vec<Value> = docs.values().cloned().collect();
    let mut task = 2;
    for batch in all.chunks(1500) {
        index.add_documents(batch.to_vec(), None, false, task).unwrap();
        task += 1;
    }
    let gone: Vec<String> = (0..6000u64).filter(|i| i % 13 == 0).map(|i| i.to_string()).collect();
    index.delete_documents(&gone, task).unwrap();
    docs.retain(|i, _| i % 13 != 0);

    type Keep = fn(&Value) -> bool;
    let filters: [(Option<&str>, Keep); 10] = [
        (None, |_| true),
        (Some("n >= 300 AND n < 400"), |d| d["n"].as_u64().is_some_and(|n| (300..400).contains(&n))),
        (Some("n > 100 AND k = 3 AND n <= 160"), |d| d["k"] == 3 && d["n"].as_u64().is_some_and(|n| n > 100 && n <= 160)),
        (Some("n >= 400 AND n < 300"), |_| false),
        (Some("k = 3"), |d| d["k"] == 3),
        (Some("k < 5"), |d| d["k"].as_u64().unwrap() < 5),
        (Some("n >= 600"), |d| d["n"].as_u64().is_some_and(|n| n >= 600)),
        (Some("n < 100"), |d| d["n"].as_u64().is_some_and(|n| n < 100)),
        (Some("n = 5"), |d| d["n"] == 5),
        (Some("k = 3 AND n > 5000"), |_| false),
    ];
    let fields = ["n", "name", "mixed", "rare"];
    let check = |index: &Index, docs: &std::collections::BTreeMap<u64, Value>, label: &str| {
        for (filter, keep) in &filters {
            let matching: Vec<&Value> = docs.values().filter(|d| keep(d)).collect();
            assert_sorted(index, &matching, *filter, &fields, label);
        }
    };
    assert_eq!(index.stats().segments, 4);
    check(&index, &docs, "four segments");

    // Small writes until the segments are merged, twice over.
    for round in 0..12u64 {
        let batch: Vec<Value> = (0..40u64).map(|i| make(6000 + round * 40 + i)).collect();
        for d in &batch {
            docs.insert(d["id"].as_u64().unwrap(), d.clone());
        }
        task += 1;
        index.add_documents(batch, None, false, task).unwrap();
    }
    assert!(index.stats().segments <= 8, "segments: {}", index.stats().segments);
    check(&index, &docs, "after merges");
    drop(index);
    check(&Index::open(dir.path()).unwrap(), &docs, "reopened");
}

/// A data folder written in the previous segment format, `THSEG001`, which
/// has no stored order: it is opened and sorted as before, and its segments
/// get the order when they are merged.
#[test]
fn segments_of_the_previous_format_are_read_and_merged() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/thseg001");
    let dir = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(&fixture).unwrap().flatten() {
        std::fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
    }
    // The documents the fixture was built from.
    let make = |i: u64| {
        let mut d = json!({"id": i, "w": "comun"});
        if !i.is_multiple_of(9) {
            d["n"] = json!((i * 7919) % 13);
            d["name"] = json!(format!("n{:02}", (i * 104729) % 17));
        }
        if !i.is_multiple_of(4) {
            d["mixed"] = if i.is_multiple_of(3) { json!(format!("m{:02}", i % 5)) } else { json!((i * 31) % 7) };
        }
        d
    };
    let mut docs: std::collections::BTreeMap<u64, Value> = (0..60u64).filter(|i| *i != 7 && *i != 33).map(|i| (i, make(i))).collect();
    let fields = ["n", "name", "mixed"];
    let check = |index: &Index, docs: &std::collections::BTreeMap<u64, Value>, label: &str| {
        assert_eq!(index.stats().number_of_documents, docs.len() as u64);
        assert_sorted(index, &docs.values().collect::<Vec<_>>(), None, &fields, label);
        let some: Vec<&Value> = docs.values().filter(|d| d["n"].as_u64().is_some_and(|n| n >= 4)).collect();
        assert_sorted(index, &some, Some("n >= 4"), &fields, label);
    };
    let index = Index::open(dir.path()).unwrap();
    assert_eq!(index.stats().segments, 3);
    check(&index, &docs, "as written by the previous version");
    assert_eq!(index.get_document("12").unwrap().unwrap(), make(12));

    // New writes go next to the old segments, and merges take them in.
    for round in 0..7u64 {
        let batch: Vec<Value> = (0..30u64).map(|i| make(100 + round * 30 + i)).collect();
        for d in &batch {
            docs.insert(d["id"].as_u64().unwrap(), d.clone());
        }
        index.add_documents(batch, None, false, 10 + round).unwrap();
        check(&index, &docs, "old and new segments together");
    }
    assert!(index.stats().segments <= 8, "segments: {}", index.stats().segments);
    drop(index);
    check(&Index::open(dir.path()).unwrap(), &docs, "reopened");
}

/// Two bounds on a field are read as one range, and a sorted search starts
/// at the bound, only where no document has two numbers in the field. A
/// document with `[5, 900]` has a number from 800 up and a number below 10,
/// and it is sorted by 5 whichever of the two let it through.
#[test]
fn bounds_on_a_field_with_several_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let index = Index::create(dir.path(), "b", Some("id".into()), 0).unwrap();
    let s = Settings { filterable_attributes: vec!["n".into()], sortable_attributes: vec!["n".into()], ..Default::default() };
    index.update_settings(s, 1).unwrap();
    let make = |i: u64| {
        let (a, b) = ((i * 7919) % 1000, (i * 104729) % 1000);
        // The first 1,500 documents, one segment, have one number each.
        if i >= 1500 && i.is_multiple_of(3) { json!({"id": i, "n": [a, b]}) } else { json!({"id": i, "n": a}) }
    };
    let docs: Vec<Value> = (0..6000u64).map(make).collect();
    for (task, batch) in docs.chunks(1500).enumerate() {
        index.add_documents(batch.to_vec(), None, false, task as u64 + 2).unwrap();
    }
    assert_eq!(index.stats().segments, 4);
    let numbers = |d: &Value| -> Vec<u64> {
        match &d["n"] {
            Value::Array(items) => items.iter().filter_map(|v| v.as_u64()).collect(),
            other => other.as_u64().into_iter().collect(),
        }
    };
    for (lo, hi) in [(800u64, 10u64), (300, 400), (0, 1000), (990, 995), (500, 500)] {
        let matching: Vec<&Value> = docs.iter().filter(|d| numbers(d).iter().any(|n| *n >= lo) && numbers(d).iter().any(|n| *n < hi)).collect();
        let filter = format!("n >= {lo} AND n < {hi}");
        let res = index.search(&q(json!({"q": "", "filter": filter, "limit": 0}))).unwrap();
        assert_eq!(res["estimatedTotalHits"].as_u64().unwrap(), matching.len() as u64, "{filter}");
        assert_sorted(&index, &matching, Some(&filter), &["n"], "several numbers");
    }
    for (filter, keep) in [("n >= 800", (|n| n >= 800) as fn(u64) -> bool), ("n < 10", |n| n < 10), ("n 20 TO 30", |n| (20..=30).contains(&n))] {
        let matching: Vec<&Value> = docs.iter().filter(|d| numbers(d).into_iter().any(keep)).collect();
        assert_sorted(&index, &matching, Some(filter), &["n"], "several numbers");
    }
}
