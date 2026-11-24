import { LogOutIcon, PlusIcon } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { ConfirmDialog, type Confirmation } from '@/components/confirm-dialog'
import { Login } from '@/components/login'
import { Logo } from '@/components/logo'
import { NewTenantDialog } from '@/components/new-tenant-dialog'
import { TenantDetail } from '@/components/tenant-detail'
import { TenantsTable } from '@/components/tenants-table'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { Toaster } from '@/components/ui/sonner'
import { adminApi, ApiError, type Tenant } from '@/lib/api'
import { formatBytes, formatNumber } from '@/lib/format'
import { useI18n } from '@/lib/i18n'
import { LanguageButton } from '@/components/language-button'

const STORAGE_KEY = 'trailhound-admin'
const REFRESH_MS = 10_000

function storedKey() {
  try {
    return sessionStorage.getItem(STORAGE_KEY) ?? ''
  } catch {
    return ''
  }
}

function storeKey(key: string) {
  try {
    if (key) sessionStorage.setItem(STORAGE_KEY, key)
    else sessionStorage.removeItem(STORAGE_KEY)
  } catch {
    // Without session storage the key just lasts until the page is reloaded.
  }
}

export default function App() {
  const { t } = useI18n()
  const [adminKey, setAdminKey] = useState(storedKey)
  const [tenants, setTenants] = useState<Tenant[] | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [loginError, setLoginError] = useState(false)
  const [creating, setCreating] = useState(false)
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null)
  const api = useMemo(() => adminApi(adminKey), [adminKey])

  const logout = useCallback((invalidKey = false) => {
    storeKey('')
    setAdminKey('')
    setTenants(null)
    setSelectedId(null)
    setLoginError(invalidKey)
  }, [])

  const refresh = useCallback(async () => {
    try {
      setTenants(await api.list())
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) logout(true)
      else toast.error(e instanceof Error ? e.message : String(e), { id: 'refresh' })
    }
  }, [api, logout])

  useEffect(() => {
    if (!adminKey) return
    refresh()
    const timer = setInterval(() => !document.hidden && refresh(), REFRESH_MS)
    return () => clearInterval(timer)
  }, [adminKey, refresh])

  /** Runs a change, reports how it went, and reloads the list. */
  async function act(change: () => Promise<unknown>, done: string) {
    let ok = true
    try {
      await change()
      toast.success(done)
    } catch (e) {
      ok = false
      toast.error(e instanceof Error ? e.message : String(e))
    }
    await refresh()
    return ok
  }

  if (!adminKey) {
    return (
      <Login
        error={loginError ? t.invalidKey : ''}
        onLogin={(key) => {
          storeKey(key)
          setLoginError(false)
          setAdminKey(key)
        }}
      />
    )
  }

  const selected = tenants?.find((x) => x.id === selectedId) ?? null
  const sum = (pick: (t: Tenant) => number) => (tenants ?? []).reduce((total, x) => total + Number(pick(x) || 0), 0)
  const tiles = [
    [t.tenants, formatNumber(tenants?.length)],
    [t.indexes, formatNumber(sum((x) => x.usage.indexes))],
    [t.documents, formatNumber(sum((x) => x.usage.documents))],
    [t.onDisk, formatBytes(sum((x) => x.usage.storageBytes))],
    [t.searches, formatNumber(sum((x) => x.usage.searches))],
  ]

  return (
    <div className="min-h-svh">
      <header className="border-b bg-card">
        <div className="mx-auto flex max-w-6xl items-center gap-3 px-4 py-3">
          <Logo />
          <span className="border-l pl-3 text-sm text-muted-foreground">{t.admin}</span>
          <LanguageButton className="ml-auto" />
          <Button variant="outline" onClick={() => logout()}>
            <LogOutIcon /> {t.logout}
          </Button>
        </div>
      </header>

      <main className="mx-auto grid max-w-6xl gap-4 p-4">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
          {tiles.map(([label, value]) => (
            <Card key={label} size="sm">
              <CardContent>
                <div className="text-sm text-muted-foreground">{label}</div>
                {tenants ? <div className="text-2xl font-semibold tabular-nums">{value}</div> : <Skeleton className="mt-1 h-7 w-16" />}
              </CardContent>
            </Card>
          ))}
        </div>

        <Card>
          <CardHeader>
            <CardTitle>{t.tenants}</CardTitle>
            <CardDescription>{t.tenantsHint}</CardDescription>
            <CardAction>
              <Button onClick={() => setCreating(true)}>
                <PlusIcon /> {t.newTenant}
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent>
            {tenants === null ? (
              <div className="grid gap-2">
                <Skeleton className="h-8" />
                <Skeleton className="h-8" />
              </div>
            ) : tenants.length === 0 ? (
              <div className="grid justify-items-center gap-3 py-10 text-center">
                <p className="max-w-md text-sm text-muted-foreground">{t.noTenants}</p>
                <Button variant="outline" onClick={() => setCreating(true)}>
                  <PlusIcon /> {t.createFirst}
                </Button>
              </div>
            ) : (
              <TenantsTable
                tenants={tenants}
                selectedId={selectedId}
                onSelect={setSelectedId}
                onToggleSuspended={(x) => act(() => api.update(x.id, { suspended: !x.suspended }), x.suspended ? t.resumedDone(x.name) : t.suspendedDone(x.name))}
                onDelete={(x) =>
                  setConfirmation({
                    title: t.confirmDeleteTitle(x.name),
                    description: t.confirmDeleteText,
                    action: t.confirmDeleteAction,
                    run: () => act(() => api.remove(x.id), t.deleted(x.name)),
                  })
                }
              />
            )}
          </CardContent>
        </Card>

        {selected && (
          <TenantDetail
            key={selected.id}
            tenant={selected}
            onClose={() => setSelectedId(null)}
            onRotateKey={(x) =>
              setConfirmation({
                title: t.confirmRotateTitle,
                description: t.confirmRotateText(x.name),
                action: t.confirmRotateAction,
                run: () => act(() => api.rotateKey(x.id), t.rotated),
              })
            }
            onSaveQuotas={(x, quotas) => act(() => api.update(x.id, { quotas }), t.quotasSaved)}
          />
        )}
      </main>

      <NewTenantDialog
        open={creating}
        onOpenChange={setCreating}
        onCreate={(name, quotas) =>
          act(async () => {
            const tenant = await api.create(name, quotas)
            setSelectedId(tenant.id)
          }, t.created(name))
        }
      />
      <ConfirmDialog confirmation={confirmation} onClose={() => setConfirmation(null)} />
      <Toaster position="bottom-right" />
    </div>
  )
}
