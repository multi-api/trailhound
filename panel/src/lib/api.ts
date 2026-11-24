export type Quotas = {
  maxIndexes: number | null
  maxDocuments: number | null
  maxStorageBytes: number | null
}

export type TenantIndex = { uid: string; documents: number; storageBytes: number; segments: number }

export type Tenant = {
  id: string
  name: string
  masterKey: string
  searchKey: string | null
  createdAt: string
  suspended: boolean
  quotas: Quotas
  usage: { indexes: number; documents: number; storageBytes: number; searches: number }
  indexes: TenantIndex[]
  tasks: { total: number; pending: number; failed: number }
}

export type SearchResult = {
  hits: Record<string, unknown>[]
  estimatedTotalHits?: number
  totalHits?: number
  processingTimeMs: number
}

export class ApiError extends Error {
  status: number
  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

async function request<T>(key: string, method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: { Authorization: 'Bearer ' + key, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (res.status === 204) return null as T
  const data = await res.json().catch(() => ({}))
  if (!res.ok) throw new ApiError(data.message || 'Error ' + res.status, res.status)
  return data as T
}

/** The administration API, authenticated with the platform's admin key. */
export function adminApi(adminKey: string) {
  const base = '/_admin/api/tenants'
  return {
    list: () => request<{ results: Tenant[] }>(adminKey, 'GET', base).then((r) => r.results),
    create: (name: string, quotas: Partial<Quotas>) => request<Tenant>(adminKey, 'POST', base, { name, quotas }),
    update: (id: string, patch: { name?: string; suspended?: boolean; quotas?: Quotas }) =>
      request<Tenant>(adminKey, 'PATCH', `${base}/${id}`, patch),
    remove: (id: string) => request<null>(adminKey, 'DELETE', `${base}/${id}`),
    rotateKey: (id: string) => request<Tenant>(adminKey, 'POST', `${base}/${id}/rotate-key`),
  }
}

export type AdminApi = ReturnType<typeof adminApi>

/** A search against one tenant's index, authenticated with that tenant's key. */
export function search(tenantKey: string, index: string, q: string) {
  return request<SearchResult>(tenantKey, 'POST', `/indexes/${encodeURIComponent(index)}/search`, { q, limit: 5 })
}
