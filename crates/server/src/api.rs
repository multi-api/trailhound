//! The Meilisearch-compatible HTTP API.

use crate::error::ApiError;
use crate::platform::{apply_settings_patch, new_key, valid_uid, ApiKey, Kind, Platform, Tenant};
use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use trailhound_engine::{now_rfc3339, Index, SearchQuery, Settings};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use uuid::Uuid;

type P = State<Arc<Platform>>;
type R = Result<Response, ApiError>;
type Params = Query<HashMap<String, String>>;

fn ok(v: Value) -> R {
    Ok(Json(v).into_response())
}

fn accepted(v: Value) -> R {
    Ok((StatusCode::ACCEPTED, Json(v)).into_response())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, ApiError> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|_| ApiError::internal("request handler panicked"))?
}

fn parse_body(body: &Bytes) -> Result<Value, ApiError> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad("malformed_payload", format!("The json payload provided is malformed. `{e}`.")))
}

pub struct Auth {
    pub tenant: Arc<Tenant>,
    /// `None` when the request used the tenant's master key.
    pub key: Option<ApiKey>,
}

#[axum::async_trait]
impl FromRequestParts<Arc<Platform>> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, platform: &Arc<Platform>) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing_authorization_header", "The Authorization header is missing. It must use the bearer authorization method."))?;
        let invalid = || ApiError::new(StatusCode::FORBIDDEN, "invalid_api_key", "The provided API key is invalid.");
        let (tenant, key) = platform.resolve(token.trim()).ok_or_else(invalid)?;
        if tenant.meta.read().unwrap().suspended {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "tenant_suspended", "This account is suspended."));
        }
        if let Some(expires) = key.as_ref().and_then(|k| k.expires_at.as_ref()) {
            if expires.as_str() < now_rfc3339().as_str() {
                return Err(invalid());
            }
        }
        Ok(Auth { tenant, key })
    }
}

impl Auth {
    fn check(&self, action: &str, index: Option<&str>) -> Result<(), ApiError> {
        let Some(key) = &self.key else { return Ok(()) };
        let group = action.split('.').next().unwrap_or("");
        let action_ok = key.actions.iter().any(|a| a == "*" || a == action || a.strip_suffix(".*") == Some(group));
        let index_ok = index.map_or(true, |uid| key.indexes.iter().any(|p| p == "*" || p == uid || p.strip_suffix('*').map_or(false, |pre| uid.starts_with(pre))));
        if action_ok && index_ok {
            Ok(())
        } else {
            Err(ApiError::new(StatusCode::FORBIDDEN, "invalid_api_key", "The provided API key is invalid."))
        }
    }

    fn allows_index(&self, uid: &str) -> bool {
        self.key.as_ref().map_or(true, |k| k.indexes.iter().any(|p| p == "*" || p == uid || p.strip_suffix('*').map_or(false, |pre| uid.starts_with(pre))))
    }
}

fn index_json(ix: &Index) -> Value {
    let st = ix.snapshot();
    json!({ "uid": st.manifest.uid, "createdAt": st.manifest.created_at, "updatedAt": st.manifest.updated_at, "primaryKey": st.manifest.primary_key })
}

fn num(params: &HashMap<String, String>, name: &str, default: usize) -> Result<usize, ApiError> {
    match params.get(name) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| ApiError::bad("bad_request", format!("Invalid value in parameter `{name}`: could not parse `{v}` as a positive integer."))),
    }
}

fn check_uid(uid: &str) -> Result<(), ApiError> {
    if valid_uid(uid) {
        Ok(())
    } else {
        Err(ApiError::bad("invalid_index_uid", format!("`{uid}` is not a valid index uid. Index uid can be an integer or a string containing only alphanumeric characters, hyphens (-) and underscores (_), and can not be more than 400 bytes.")))
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "available" }))
}

async fn version(_auth: Auth) -> Json<Value> {
    // Reported so Meilisearch SDKs enable their v1 feature set.
    Json(json!({ "commitSha": "trailhound", "commitDate": "", "pkgVersion": "1.12.0", "engine": concat!("trailhound ", env!("CARGO_PKG_VERSION")) }))
}

async fn list_indexes(auth: Auth, Query(params): Params) -> R {
    auth.check("indexes.get", None)?;
    let (offset, limit) = (num(&params, "offset", 0)?, num(&params, "limit", 20)?);
    let all: Vec<Value> = auth.tenant.indexes.read().unwrap().iter().filter(|(uid, _)| auth.allows_index(uid)).map(|(_, ix)| index_json(ix)).collect();
    let total = all.len();
    ok(json!({ "results": all.into_iter().skip(offset).take(limit).collect::<Vec<_>>(), "offset": offset, "limit": limit, "total": total }))
}

async fn create_index(State(p): P, auth: Auth, body: Bytes) -> R {
    let body = parse_body(&body)?;
    let uid = body["uid"].as_str().ok_or_else(|| ApiError::bad("missing_index_uid", "Missing field `uid`."))?.to_string();
    check_uid(&uid)?;
    auth.check("indexes.create", Some(&uid))?;
    let primary_key = body["primaryKey"].as_str().map(|s| s.to_string());
    let details = json!({ "primaryKey": primary_key });
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::IndexCreation { primary_key }, details)?)
}

async fn get_index(auth: Auth, Path(uid): Path<String>) -> R {
    auth.check("indexes.get", Some(&uid))?;
    ok(index_json(&*auth.tenant.require_index(&uid)?))
}

async fn update_index(State(p): P, auth: Auth, Path(uid): Path<String>, body: Bytes) -> R {
    auth.check("indexes.update", Some(&uid))?;
    let body = parse_body(&body)?;
    let primary_key = body["primaryKey"].as_str().map(|s| s.to_string());
    let details = json!({ "primaryKey": primary_key });
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::IndexUpdate { primary_key }, details)?)
}

async fn delete_index(State(p): P, auth: Auth, Path(uid): Path<String>) -> R {
    auth.check("indexes.delete", Some(&uid))?;
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::IndexDeletion, json!({ "deletedDocuments": null }))?)
}

async fn index_stats(auth: Auth, Path(uid): Path<String>) -> R {
    auth.check("stats.get", Some(&uid))?;
    let s = auth.tenant.require_index(&uid)?.stats();
    ok(json!({ "numberOfDocuments": s.number_of_documents, "isIndexing": false, "fieldDistribution": s.field_distribution, "rawDocumentDbSize": s.disk_bytes, "segments": s.segments }))
}

async fn stats(auth: Auth) -> R {
    auth.check("stats.get", None)?;
    let mut indexes = Map::new();
    let mut size = 0;
    let mut last: Option<String> = None;
    for (uid, ix) in auth.tenant.indexes.read().unwrap().iter().filter(|(uid, _)| auth.allows_index(uid)) {
        let s = ix.stats();
        size += s.disk_bytes;
        let updated = ix.snapshot().manifest.updated_at.clone();
        if last.as_ref().map_or(true, |l| *l < updated) {
            last = Some(updated);
        }
        indexes.insert(uid.clone(), json!({ "numberOfDocuments": s.number_of_documents, "isIndexing": false, "fieldDistribution": s.field_distribution }));
    }
    ok(json!({ "databaseSize": size, "usedDatabaseSize": size, "lastUpdate": last, "indexes": indexes }))
}

fn pick_fields(doc: Value, fields: Option<&Vec<String>>) -> Value {
    match (doc, fields) {
        (Value::Object(m), Some(fields)) if !fields.iter().any(|f| f == "*") => Value::Object(m.into_iter().filter(|(k, _)| fields.contains(k)).collect()),
        (doc, _) => doc,
    }
}

fn string_list(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::String(s) => Some(s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()),
        Value::Array(a) => Some(a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()),
        _ => None,
    }
}

async fn fetch_documents(auth: Auth, uid: String, opts: Value) -> R {
    auth.check("documents.get", Some(&uid))?;
    let ix = auth.tenant.require_index(&uid)?;
    let offset = opts["offset"].as_u64().unwrap_or(0) as usize;
    let limit = opts["limit"].as_u64().unwrap_or(20) as usize;
    let fields = string_list(&opts["fields"]);
    let filter = match &opts["filter"] {
        Value::Null => None,
        f => Some(f.clone()),
    };
    let (docs, total) = blocking(move || Ok(ix.list_documents(offset, limit, filter.as_ref())?)).await?;
    let results: Vec<Value> = docs.into_iter().map(|d| pick_fields(d, fields.as_ref())).collect();
    ok(json!({ "results": results, "offset": offset, "limit": limit, "total": total }))
}

async fn get_documents(auth: Auth, Path(uid): Path<String>, Query(params): Params) -> R {
    let mut opts = Map::new();
    opts.insert("offset".into(), json!(num(&params, "offset", 0)?));
    opts.insert("limit".into(), json!(num(&params, "limit", 20)?));
    for name in ["fields", "filter"] {
        if let Some(v) = params.get(name) {
            opts.insert(name.into(), json!(v));
        }
    }
    fetch_documents(auth, uid, Value::Object(opts)).await
}

async fn post_fetch_documents(auth: Auth, Path(uid): Path<String>, body: Bytes) -> R {
    fetch_documents(auth, uid, parse_body(&body)?).await
}

async fn get_document(auth: Auth, Path((uid, id)): Path<(String, String)>, Query(params): Params) -> R {
    auth.check("documents.get", Some(&uid))?;
    let ix = auth.tenant.require_index(&uid)?;
    let fields = params.get("fields").and_then(|f| string_list(&json!(f)));
    let lookup = id.clone();
    let doc = blocking(move || Ok(ix.get_document(&lookup)?)).await?;
    match doc {
        Some(d) => ok(pick_fields(d, fields.as_ref())),
        None => Err(ApiError::not_found("document_not_found", format!("Document `{id}` not found."))),
    }
}

/// Queries one `multi-search` request may carry.
const MAX_MULTI_SEARCH_QUERIES: usize = 100;

/// Largest accepted document payload.
const MAX_PAYLOAD_BYTES: u64 = 256 * 1024 * 1024;

async fn write_documents(p: Arc<Platform>, auth: Auth, uid: String, params: HashMap<String, String>, headers: HeaderMap, body: Body, partial: bool) -> R {
    check_uid(&uid)?;
    auth.check("documents.add", Some(&uid))?;
    let content_type = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let ndjson = content_type.contains("ndjson");
    if !ndjson && !content_type.contains("json") {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_content_type",
            format!("The Content-Type `{content_type}` is invalid. Accepted values for the Content-Type header are: `application/json`, `application/x-ndjson`"),
        ));
    }
    let primary_key = params.get("primaryKey").cloned();
    let tenant = auth.tenant.clone();
    // A tenant already over its quota is told before it uploads anything.
    tenant.check_document_quota(0, 0)?;
    let id = Uuid::new_v4().simple().to_string();
    // The body goes to disk as it arrives and is checked from there, one
    // document at a time: the memory a request needs does not grow with it.
    let upload = tenant.dir.join("payloads").join(format!("{id}.upload"));
    if let Err(e) = receive(body, &upload).await {
        let _ = std::fs::remove_file(&upload);
        return Err(e);
    }
    let summary = blocking(move || {
        let name = format!("{id}.ndjson");
        let path = tenant.dir.join("payloads").join(&name);
        let checked = check_upload(&upload, &path, ndjson);
        let _ = std::fs::remove_file(&upload);
        let received = match checked {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        };
        let details = json!({ "receivedDocuments": received, "indexedDocuments": null });
        tenant.enqueue(&p, Some(&uid), Kind::DocumentAddition { payload: name, primary_key, partial }, details)
    })
    .await?;
    accepted(summary)
}

async fn receive(mut body: Body, path: &std::path::Path) -> Result<(), ApiError> {
    use http_body_util::BodyExt;
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path).await?;
    let mut total = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| ApiError::bad("malformed_payload", format!("The request body could not be read. `{e}`.")))?;
        if let Ok(data) = frame.into_data() {
            total += data.len() as u64;
            if total > MAX_PAYLOAD_BYTES {
                return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", "The provided payload reached the size limit. The maximum accepted payload size is 256 MiB."));
            }
            file.write_all(&data).await?;
        }
    }
    file.flush().await?;
    Ok(())
}

/// Validate an uploaded body and leave it at `path` as one document per
/// line, fsynced. Returns the number of documents.
fn check_upload(upload: &std::path::Path, path: &std::path::Path, ndjson: bool) -> Result<u64, ApiError> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(upload)?);
    if ndjson {
        let mut n = 0u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let doc: Value = serde_json::from_slice(&line).map_err(|e| ApiError::bad("malformed_payload", format!("The `ndjson` payload provided is malformed. `{e}`.")))?;
            if !doc.is_object() {
                return Err(ApiError::bad("malformed_payload", "Every document must be a JSON object."));
            }
            n += 1;
        }
        drop(reader);
        // Already one document per line: the upload itself is the payload.
        std::fs::OpenOptions::new().append(true).open(upload)?.sync_all()?;
        std::fs::rename(upload, path)?;
        return Ok(n);
    }
    let first = loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break None;
        }
        match buf.iter().position(|b| !b.is_ascii_whitespace()) {
            Some(i) => {
                let b = buf[i];
                reader.consume(i);
                break Some(b);
            }
            None => {
                let len = buf.len();
                reader.consume(len);
            }
        }
    };
    let malformed = |e: serde_json::Error| ApiError::bad("malformed_payload", format!("The json payload provided is malformed. `{e}`."));
    let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let received = match first {
        Some(b'[') => {
            let mut de = serde_json::Deserializer::from_reader(&mut reader);
            let mut failure = None;
            let parsed = serde::Deserializer::deserialize_seq(&mut de, DocumentArray { out: &mut out, failure: &mut failure }).and_then(|n| de.end().map(|_| n));
            match (parsed, failure) {
                (_, Some(e)) => return Err(e),
                (Err(e), None) => return Err(malformed(e)),
                (Ok(n), None) => n,
            }
        }
        Some(b'{') => {
            let mut de = serde_json::Deserializer::from_reader(&mut reader);
            let doc = <Value as serde::Deserialize>::deserialize(&mut de).map_err(malformed)?;
            de.end().map_err(malformed)?;
            write_document(&mut out, &doc)?;
            1
        }
        _ => return Err(ApiError::bad("malformed_payload", "The `json` payload must be an object or an array of objects.")),
    };
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(received)
}

fn write_document(out: &mut impl Write, doc: &Value) -> Result<(), ApiError> {
    if !doc.is_object() {
        return Err(ApiError::bad("malformed_payload", "Every document must be a JSON object."));
    }
    serde_json::to_writer(&mut *out, doc)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Reads a JSON array one element at a time, writing each document as a line.
struct DocumentArray<'a, W: Write> {
    out: &'a mut W,
    failure: &'a mut Option<ApiError>,
}

impl<'de, W: Write> serde::de::Visitor<'de> for DocumentArray<'_, W> {
    type Value = u64;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array of documents")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<u64, A::Error> {
        let mut n = 0;
        while let Some(doc) = seq.next_element::<Value>()? {
            if let Err(e) = write_document(self.out, &doc) {
                *self.failure = Some(e);
                return Err(serde::de::Error::custom("rejected document"));
            }
            n += 1;
        }
        Ok(n)
    }
}

async fn add_documents(State(p): P, auth: Auth, Path(uid): Path<String>, Query(params): Params, headers: HeaderMap, body: Body) -> R {
    write_documents(p, auth, uid, params, headers, body, false).await
}

async fn update_documents(State(p): P, auth: Auth, Path(uid): Path<String>, Query(params): Params, headers: HeaderMap, body: Body) -> R {
    write_documents(p, auth, uid, params, headers, body, true).await
}

async fn clear_documents(State(p): P, auth: Auth, Path(uid): Path<String>) -> R {
    auth.check("documents.delete", Some(&uid))?;
    auth.tenant.require_index(&uid)?;
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::DocumentClear, json!({ "deletedDocuments": null }))?)
}

async fn delete_document(State(p): P, auth: Auth, Path((uid, id)): Path<(String, String)>) -> R {
    auth.check("documents.delete", Some(&uid))?;
    auth.tenant.require_index(&uid)?;
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::DocumentDeletion { ids: vec![id] }, json!({ "providedIds": 1, "deletedDocuments": null }))?)
}

async fn delete_batch(State(p): P, auth: Auth, Path(uid): Path<String>, body: Bytes) -> R {
    auth.check("documents.delete", Some(&uid))?;
    auth.tenant.require_index(&uid)?;
    let Value::Array(items) = parse_body(&body)? else {
        return Err(ApiError::bad("bad_request", "Expected an array of document ids."));
    };
    let ids: Vec<String> = items.iter().map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())).collect();
    let details = json!({ "providedIds": ids.len(), "deletedDocuments": null });
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::DocumentDeletion { ids }, details)?)
}

async fn delete_by_filter(State(p): P, auth: Auth, Path(uid): Path<String>, body: Bytes) -> R {
    auth.check("documents.delete", Some(&uid))?;
    auth.tenant.require_index(&uid)?;
    let filter = parse_body(&body)?["filter"].clone();
    if filter.is_null() {
        return Err(ApiError::bad("missing_document_filter", "Missing field `filter`."));
    }
    // Checked now, so a filter that cannot be parsed never reaches the task log.
    trailhound_engine::filter::parse_value(&filter)?;
    let details = json!({ "providedIds": 0, "deletedDocuments": null, "originalFilter": filter.to_string() });
    accepted(auth.tenant.enqueue(&p, Some(&uid), Kind::DocumentDeletionByFilter { filter }, details)?)
}

fn query_from_params(params: &HashMap<String, String>) -> Result<SearchQuery, ApiError> {
    let mut v = Map::new();
    for (k, raw) in params {
        let value = match k.as_str() {
            "offset" | "limit" | "page" | "hitsPerPage" | "cropLength" => json!(num(params, k, 0)?),
            "showMatchesPosition" => json!(raw == "true"),
            "attributesToRetrieve" | "attributesToHighlight" | "attributesToCrop" | "sort" | "facets" | "attributesToSearchOn" => json!(string_list(&json!(raw))),
            _ => json!(raw),
        };
        v.insert(k.clone(), value);
    }
    parse_query(Value::Object(v))
}

fn parse_query(v: Value) -> Result<SearchQuery, ApiError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| ApiError::bad("bad_request", format!("Invalid search request: {e}")))
}

async fn run_search(auth: &Auth, uid: &str, query: SearchQuery) -> Result<Value, ApiError> {
    auth.check("search", Some(uid))?;
    let ix = auth.tenant.require_index(uid)?;
    auth.tenant.count_search();
    crate::platform::SEARCHED.store(true, std::sync::atomic::Ordering::Relaxed);
    blocking(move || Ok(ix.search(&query)?)).await
}

async fn search_post(auth: Auth, Path(uid): Path<String>, body: Bytes) -> R {
    ok(run_search(&auth, &uid, parse_query(parse_body(&body)?)?).await?)
}

async fn search_get(auth: Auth, Path(uid): Path<String>, Query(params): Params) -> R {
    ok(run_search(&auth, &uid, query_from_params(&params)?).await?)
}

async fn multi_search(auth: Auth, body: Bytes) -> R {
    let body = parse_body(&body)?;
    if !body["federation"].is_null() {
        return Err(ApiError::bad("feature_not_enabled", "Federated search is not supported yet."));
    }
    let queries = body["queries"].as_array().cloned().ok_or_else(|| ApiError::bad("bad_request", "Missing field `queries`."))?;
    if queries.len() > MAX_MULTI_SEARCH_QUERIES {
        return Err(ApiError::bad("bad_request", format!("A multi-search request can hold at most {MAX_MULTI_SEARCH_QUERIES} queries.")));
    }
    let mut results = Vec::with_capacity(queries.len());
    for q in queries {
        let query = parse_query(q)?;
        let uid = query.index_uid.clone().ok_or_else(|| ApiError::bad("missing_index_uid", "Missing field `indexUid`."))?;
        let mut res = run_search(&auth, &uid, query).await?;
        if let Value::Object(m) = &mut res {
            m.shift_insert(0, "indexUid".into(), json!(uid));
        }
        results.push(res);
    }
    ok(json!({ "results": results }))
}

fn camel(setting: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in setting.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn current_settings(auth: &Auth, uid: &str) -> Result<Value, ApiError> {
    auth.check("settings.get", Some(uid))?;
    Ok(serde_json::to_value(&auth.tenant.require_index(uid)?.snapshot().manifest.settings)?)
}

async fn get_settings(auth: Auth, Path(uid): Path<String>) -> R {
    ok(current_settings(&auth, &uid)?)
}

fn enqueue_settings(p: &Platform, auth: &Auth, uid: &str, patch: Value) -> R {
    check_uid(uid)?;
    auth.check("settings.update", Some(uid))?;
    apply_settings_patch(&Settings::default(), &patch)?;
    accepted(auth.tenant.enqueue(p, Some(uid), Kind::SettingsUpdate { patch: patch.clone() }, patch)?)
}

async fn patch_settings(State(p): P, auth: Auth, Path(uid): Path<String>, body: Bytes) -> R {
    enqueue_settings(&p, &auth, &uid, parse_body(&body)?)
}

async fn reset_settings(State(p): P, auth: Auth, Path(uid): Path<String>) -> R {
    let Value::Object(defaults) = serde_json::to_value(Settings::default())? else { unreachable!() };
    let patch: Map<String, Value> = defaults.keys().map(|k| (k.clone(), Value::Null)).collect();
    enqueue_settings(&p, &auth, &uid, Value::Object(patch))
}

async fn get_setting(auth: Auth, Path((uid, name)): Path<(String, String)>) -> R {
    let all = current_settings(&auth, &uid)?;
    match all.get(camel(&name)) {
        Some(v) => ok(v.clone()),
        None => Err(ApiError::not_found("not_found", format!("Unknown setting `{name}`."))),
    }
}

async fn put_setting(State(p): P, auth: Auth, Path((uid, name)): Path<(String, String)>, body: Bytes) -> R {
    let mut patch = Map::new();
    patch.insert(camel(&name), parse_body(&body)?);
    enqueue_settings(&p, &auth, &uid, Value::Object(patch))
}

async fn reset_setting(State(p): P, auth: Auth, Path((uid, name)): Path<(String, String)>) -> R {
    let mut patch = Map::new();
    patch.insert(camel(&name), Value::Null);
    enqueue_settings(&p, &auth, &uid, Value::Object(patch))
}

async fn list_tasks(auth: Auth, Query(params): Params) -> R {
    auth.check("tasks.get", None)?;
    let limit = num(&params, "limit", 20)?;
    let from = params.get("from").and_then(|v| v.parse::<u64>().ok());
    let list = |name: &str| params.get(name).map(|v| v.split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>());
    let (statuses, types, uids) = (list("statuses"), list("types"), list("indexUids"));
    let store = auth.tenant.tasks.lock().unwrap();
    let mut matching = store.tasks.values().rev().filter(|t| {
        from.map_or(true, |f| t.uid <= f)
            && statuses.as_ref().map_or(true, |s| s.contains(&t.status))
            && types.as_ref().map_or(true, |s| s.iter().any(|x| x == t.kind.type_name()))
            && uids.as_ref().map_or(true, |s| t.index_uid.as_ref().map_or(false, |u| s.contains(u)))
            && t.index_uid.as_ref().map_or(true, |u| auth.allows_index(u))
    });
    let results: Vec<Value> = matching.by_ref().take(limit).map(|t| t.view()).collect();
    let next = matching.next().map(|t| t.uid);
    // `total` counts every task matching the filters, not only this page.
    let total = results.len() + next.map_or(0, |_| 1 + matching.count());
    ok(json!({ "results": results, "total": total, "limit": limit, "from": results.first().map(|t| t["uid"].clone()), "next": next }))
}

async fn get_task(auth: Auth, Path(uid): Path<u64>) -> R {
    auth.check("tasks.get", None)?;
    match auth.tenant.tasks.lock().unwrap().tasks.get(&uid) {
        // A key limited to some indexes sees only their tasks, as in the list.
        Some(t) if t.index_uid.as_ref().map_or(true, |u| auth.allows_index(u)) => ok(t.view()),
        _ => Err(ApiError::not_found("task_not_found", format!("Task `{uid}` not found."))),
    }
}

async fn list_keys(auth: Auth, Query(params): Params) -> R {
    auth.check("keys.get", None)?;
    let (offset, limit) = (num(&params, "offset", 0)?, num(&params, "limit", 20)?);
    let keys = auth.tenant.keys.read().unwrap();
    ok(json!({ "results": keys.iter().skip(offset).take(limit).collect::<Vec<_>>(), "offset": offset, "limit": limit, "total": keys.len() }))
}

/// A key's expiry as `YYYY-MM-DDTHH:MM:SSZ`, the form it is compared in.
/// Accepts a date, or a date and time separated by `T` or a space, in UTC.
fn expiry(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches('Z');
    let (date, time) = match text.split_once(|c| c == 'T' || c == ' ') {
        Some((d, t)) => (d, t.split('.').next().unwrap_or(t)),
        None => (text, "00:00:00"),
    };
    let numbers = |s: &str, sep: char, widths: &[usize], max: &[u32]| {
        let parts: Vec<&str> = s.split(sep).collect();
        parts.len() == widths.len()
            && parts.iter().zip(widths).zip(max).all(|((p, w), m)| p.len() == *w && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().map_or(false, |n| n <= *m))
    };
    (numbers(date, '-', &[4, 2, 2], &[9999, 12, 31]) && numbers(time, ':', &[2, 2, 2], &[23, 59, 59])).then(|| format!("{date}T{time}Z"))
}

async fn create_key(State(p): P, auth: Auth, body: Bytes) -> R {
    auth.check("keys.create", None)?;
    let body = parse_body(&body)?;
    let actions = string_list(&body["actions"]).filter(|a| body["actions"].is_array() && !a.is_empty()).ok_or_else(|| ApiError::bad("missing_api_key_actions", "Missing field `actions`."))?;
    let indexes = string_list(&body["indexes"]).filter(|_| body["indexes"].is_array()).ok_or_else(|| ApiError::bad("missing_api_key_indexes", "Missing field `indexes`."))?;
    let expires_at = match body.get("expiresAt") {
        None => return Err(ApiError::bad("missing_api_key_expires_at", "Missing field `expiresAt`.")),
        Some(Value::Null) => None,
        Some(v) => Some(v.as_str().and_then(expiry).ok_or_else(|| {
            ApiError::bad("invalid_api_key_expires_at", "`expiresAt` must be null or a UTC date such as `2030-01-31` or `2030-01-31T12:00:00Z`.")
        })?),
    };
    let now = now_rfc3339();
    let key = ApiKey {
        name: body["name"].as_str().map(|s| s.to_string()),
        description: body["description"].as_str().map(|s| s.to_string()),
        key: new_key(),
        uid: body["uid"].as_str().map(|s| s.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string()),
        actions,
        indexes,
        expires_at,
        created_at: now.clone(),
        updated_at: now,
    };
    {
        let mut keys = auth.tenant.keys.write().unwrap();
        if keys.iter().any(|k| k.uid == key.uid) {
            return Err(ApiError::new(StatusCode::CONFLICT, "api_key_already_exists", format!("`uid` field value `{}` is already an existing API key.", key.uid)));
        }
        keys.push(key.clone());
    }
    auth.tenant.save_keys()?;
    p.rebuild_keymap();
    Ok((StatusCode::CREATED, Json(json!(key))).into_response())
}

fn key_missing(id: &str) -> ApiError {
    ApiError::not_found("api_key_not_found", format!("API key `{id}` not found."))
}

async fn get_key(auth: Auth, Path(id): Path<String>) -> R {
    auth.check("keys.get", None)?;
    let keys = auth.tenant.keys.read().unwrap();
    let key = keys.iter().find(|k| k.uid == id || k.key == id).ok_or_else(|| key_missing(&id))?;
    ok(json!(key))
}

async fn update_key(auth: Auth, Path(id): Path<String>, body: Bytes) -> R {
    auth.check("keys.update", None)?;
    let body = parse_body(&body)?;
    let updated = {
        let mut keys = auth.tenant.keys.write().unwrap();
        let key = keys.iter_mut().find(|k| k.uid == id || k.key == id).ok_or_else(|| key_missing(&id))?;
        if let Some(v) = body.get("name") {
            key.name = v.as_str().map(|s| s.to_string());
        }
        if let Some(v) = body.get("description") {
            key.description = v.as_str().map(|s| s.to_string());
        }
        key.updated_at = now_rfc3339();
        key.clone()
    };
    auth.tenant.save_keys()?;
    ok(json!(updated))
}

async fn delete_key(State(p): P, auth: Auth, Path(id): Path<String>) -> R {
    auth.check("keys.delete", None)?;
    {
        let mut keys = auth.tenant.keys.write().unwrap();
        let before = keys.len();
        keys.retain(|k| k.uid != id && k.key != id);
        if keys.len() == before {
            return Err(key_missing(&id));
        }
    }
    auth.tenant.save_keys()?;
    p.rebuild_keymap();
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub fn router() -> Router<Arc<Platform>> {
    Router::new()
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/stats", get(stats))
        .route("/indexes", get(list_indexes).post(create_index))
        .route("/indexes/:uid", get(get_index).patch(update_index).delete(delete_index))
        .route("/indexes/:uid/stats", get(index_stats))
        .route("/indexes/:uid/search", get(search_get).post(search_post))
        .route("/indexes/:uid/documents", get(get_documents).post(add_documents).put(update_documents).delete(clear_documents))
        .route("/indexes/:uid/documents/fetch", post(post_fetch_documents))
        .route("/indexes/:uid/documents/delete-batch", post(delete_batch))
        .route("/indexes/:uid/documents/delete", post(delete_by_filter))
        .route("/indexes/:uid/documents/:id", get(get_document).delete(delete_document))
        .route("/indexes/:uid/settings", get(get_settings).patch(patch_settings).delete(reset_settings))
        .route("/indexes/:uid/settings/:name", get(get_setting).put(put_setting).patch(put_setting).post(put_setting).delete(reset_setting))
        .route("/multi-search", post(multi_search))
        .route("/tasks", get(list_tasks))
        .route("/tasks/:uid", get(get_task))
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/:id", get(get_key).patch(update_key).delete(delete_key))
}
