//! Platform administration: tenants, quotas, usage, and the embedded panel.

use crate::error::ApiError;
use crate::platform::{new_key, Platform, Quotas, Tenant};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

type P = State<Arc<Platform>>;
type R = Result<Response, ApiError>;

fn authorize(p: &Platform, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    // Compare every byte so the time taken does not reveal a matching prefix.
    let expected = p.admin_key.as_bytes();
    let same = !expected.is_empty() && token.len() == expected.len() && token.bytes().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
    if same {
        Ok(())
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "invalid_admin_key", "The provided admin key is invalid."))
    }
}

/// Proves the request carries the admin key. As an extractor it runs before
/// the body is read, so a caller without the key cannot make the server
/// buffer anything.
struct Admin;

#[axum::async_trait]
impl FromRequestParts<Arc<Platform>> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, platform: &Arc<Platform>) -> Result<Self, ApiError> {
        authorize(platform, &parts.headers)?;
        Ok(Admin)
    }
}

fn tenant_json(t: &Tenant) -> Value {
    let meta = t.meta.read().unwrap().clone();
    let indexes: Vec<Value> = t
        .indexes
        .read()
        .unwrap()
        .iter()
        .map(|(uid, ix)| {
            let s = ix.stats();
            json!({ "uid": uid, "documents": s.number_of_documents, "storageBytes": s.disk_bytes, "segments": s.segments })
        })
        .collect();
    let tasks = t.tasks.lock().unwrap();
    let pending = tasks.tasks.values().filter(|t| t.status == "enqueued" || t.status == "processing").count();
    let failed = tasks.tasks.values().filter(|t| t.status == "failed").count();
    let search_key = t.keys.read().unwrap().iter().find(|k| k.actions == ["search"]).map(|k| k.key.clone());
    json!({
        "id": meta.id,
        "name": meta.name,
        "masterKey": meta.master_key,
        "searchKey": search_key,
        "createdAt": meta.created_at,
        "suspended": meta.suspended,
        "quotas": meta.quotas,
        "usage": t.usage(),
        "indexes": indexes,
        "tasks": { "total": tasks.tasks.len(), "pending": pending, "failed": failed },
    })
}

fn parse(body: &Bytes) -> Result<Value, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::bad("malformed_payload", e.to_string()))
}

fn parse_quotas(v: &Value) -> Result<Quotas, ApiError> {
    serde_json::from_value(v.clone()).map_err(|e| ApiError::bad("invalid_quotas", e.to_string()))
}

async fn list_tenants(State(p): P, _admin: Admin) -> R {
    let tenants: Vec<Arc<Tenant>> = p.tenants.read().unwrap().values().cloned().collect();
    Ok(Json(json!({ "results": tenants.iter().map(|t| tenant_json(t)).collect::<Vec<_>>() })).into_response())
}

async fn create_tenant(State(p): P, _admin: Admin, body: Bytes) -> R {
    let body = parse(&body)?;
    let name = body["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| ApiError::bad("missing_name", "Missing field `name`."))?;
    let quotas = if body["quotas"].is_null() { Quotas::default() } else { parse_quotas(&body["quotas"])? };
    let tenant = p.create_tenant(name.to_string(), quotas)?;
    Ok((StatusCode::CREATED, Json(tenant_json(&tenant))).into_response())
}

fn find(p: &Platform, id: &str) -> Result<Arc<Tenant>, ApiError> {
    p.tenant(id).ok_or_else(|| ApiError::not_found("tenant_not_found", format!("Tenant `{id}` not found.")))
}

async fn update_tenant(State(p): P, _admin: Admin, Path(id): Path<String>, body: Bytes) -> R {
    let tenant = find(&p, &id)?;
    let body = parse(&body)?;
    {
        let mut meta = tenant.meta.write().unwrap();
        if let Some(name) = body["name"].as_str() {
            meta.name = name.to_string();
        }
        if let Some(suspended) = body["suspended"].as_bool() {
            meta.suspended = suspended;
        }
        if !body["quotas"].is_null() {
            meta.quotas = parse_quotas(&body["quotas"])?;
        }
    }
    p.save()?;
    Ok(Json(tenant_json(&tenant)).into_response())
}

async fn delete_tenant(State(p): P, _admin: Admin, Path(id): Path<String>) -> R {
    find(&p, &id)?;
    tokio::task::spawn_blocking(move || p.delete_tenant(&id)).await.map_err(|_| ApiError::internal("delete failed"))??;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn rotate_key(State(p): P, _admin: Admin, Path(id): Path<String>) -> R {
    let tenant = find(&p, &id)?;
    tenant.meta.write().unwrap().master_key = new_key();
    p.save()?;
    p.rebuild_keymap();
    Ok(Json(tenant_json(&tenant)).into_response())
}

async fn panel() -> Html<&'static str> {
    Html(include_str!("panel.html"))
}

pub fn router() -> Router<Arc<Platform>> {
    Router::new()
        .route("/_admin", get(panel))
        .route("/_admin/api/tenants", get(list_tenants).post(create_tenant))
        .route("/_admin/api/tenants/:id", patch(update_tenant).delete(delete_tenant))
        .route("/_admin/api/tenants/:id/rotate-key", post(rotate_key))
        // Admin requests are a name and a few numbers.
        .layer(DefaultBodyLimit::max(64 * 1024))
}
