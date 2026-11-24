import { CopyIcon, EyeIcon, EyeOffIcon, RefreshCwIcon, SearchIcon, XIcon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { StatusBadge } from '@/components/tenants-table'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { search, type Quotas, type SearchResult, type Tenant } from '@/lib/api'
import { formatBytes, formatDate, formatNumber, numberOrNull } from '@/lib/format'
import { useI18n } from '@/lib/i18n'

const MB = 1024 * 1024

type Props = {
  tenant: Tenant
  onClose: () => void
  onRotateKey: (tenant: Tenant) => void
  onSaveQuotas: (tenant: Tenant, quotas: Quotas) => Promise<boolean>
}

export function TenantDetail({ tenant, onClose, onRotateKey, onSaveQuotas }: Props) {
  const { t } = useI18n()
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {tenant.name} <StatusBadge suspended={tenant.suspended} />
        </CardTitle>
        <CardDescription>
          <span className="font-mono text-xs">{tenant.id}</span> · {t.createdOn(formatDate(tenant.createdAt))}
        </CardDescription>
        <CardAction>
          <Button variant="ghost" size="icon-sm" aria-label={t.closeDetail} onClick={onClose}>
            <XIcon />
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        <Tabs defaultValue="keys">
          <TabsList>
            <TabsTrigger value="keys">{t.tabKeys}</TabsTrigger>
            <TabsTrigger value="indexes">{t.indexes} ({tenant.indexes.length})</TabsTrigger>
            <TabsTrigger value="quotas">{t.tabQuotas}</TabsTrigger>
            <TabsTrigger value="search">{t.tabSearch}</TabsTrigger>
          </TabsList>
          <TabsContent value="keys" className="grid gap-4 pt-3">
            <KeyField label={t.masterKey} hint={t.masterKeyHint} value={tenant.masterKey}>
              <Button variant="outline" onClick={() => onRotateKey(tenant)}>
                <RefreshCwIcon /> {t.rotate}
              </Button>
            </KeyField>
            <KeyField label={t.searchKey} hint={t.searchKeyHint} value={tenant.searchKey} />
          </TabsContent>
          <TabsContent value="indexes" className="pt-3">
            <IndexesTable tenant={tenant} />
          </TabsContent>
          <TabsContent value="quotas" className="pt-3">
            <QuotasForm tenant={tenant} onSave={onSaveQuotas} />
          </TabsContent>
          <TabsContent value="search" className="pt-3">
            <SearchTester tenant={tenant} />
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  )
}

function KeyField({ label, hint, value, children }: { label: string; hint: string; value: string | null; children?: React.ReactNode }) {
  const { t } = useI18n()
  const [shown, setShown] = useState(false)
  if (!value) {
    return (
      <div className="grid gap-1.5">
        <Label>{label}</Label>
        <p className="text-sm text-muted-foreground">{t.noKey}</p>
      </div>
    )
  }
  const copy = () =>
    navigator.clipboard.writeText(value).then(
      () => toast.success(t.copied(label)),
      () => toast.error(t.copyFailed),
    )
  return (
    <div className="grid gap-1.5">
      <Label>{label}</Label>
      <div className="flex flex-wrap items-center gap-2">
        <code className="min-w-0 flex-1 basis-64 truncate rounded-lg border bg-muted/50 px-2.5 py-1.5 font-mono text-xs">{shown ? value : '•'.repeat(32)}</code>
        <Button variant="outline" size="icon" aria-label={`${shown ? t.hide : t.show}: ${label}`} onClick={() => setShown(!shown)}>
          {shown ? <EyeOffIcon /> : <EyeIcon />}
        </Button>
        <Button variant="outline" onClick={copy}>
          <CopyIcon /> {t.copy}
        </Button>
        {children}
      </div>
      <p className="text-xs text-muted-foreground">{hint}</p>
    </div>
  )
}

function IndexesTable({ tenant }: { tenant: Tenant }) {
  const { t } = useI18n()
  if (tenant.indexes.length === 0) {
    return <p className="py-6 text-center text-sm text-muted-foreground">{t.noIndexes}</p>
  }
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>{t.index}</TableHead>
          <TableHead className="text-right">{t.documents}</TableHead>
          <TableHead className="text-right">{t.disk}</TableHead>
          <TableHead className="text-right">{t.segments}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {tenant.indexes.map((ix) => (
          <TableRow key={ix.uid}>
            <TableCell className="font-mono text-xs">{ix.uid}</TableCell>
            <TableCell className="text-right tabular-nums">{formatNumber(ix.documents)}</TableCell>
            <TableCell className="text-right tabular-nums">{formatBytes(ix.storageBytes)}</TableCell>
            <TableCell className="text-right tabular-nums">{formatNumber(ix.segments)}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function Usage({ label, used, limit, format }: { label: string; used: number; limit: number | null; format: (n: number) => string }) {
  const { t } = useI18n()
  const ratio = limit ? Math.min(1, used / limit) : 0
  return (
    <div className="grid gap-1">
      <div className="flex justify-between text-sm">
        <span>{label}</span>
        <span className="tabular-nums text-muted-foreground">
          {t.usageOf(format(used), limit === null ? null : format(limit))}
        </span>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-muted" role="presentation">
        <div className={ratio >= 0.9 ? 'h-full bg-destructive' : 'h-full bg-primary'} style={{ width: `${ratio * 100}%` }} />
      </div>
    </div>
  )
}

function QuotasForm({ tenant, onSave }: { tenant: Tenant; onSave: Props['onSaveQuotas'] }) {
  const { t } = useI18n()
  const q = tenant.quotas
  const [maxIndexes, setMaxIndexes] = useState(q.maxIndexes?.toString() ?? '')
  const [maxDocuments, setMaxDocuments] = useState(q.maxDocuments?.toString() ?? '')
  const [maxStorageMb, setMaxStorageMb] = useState(q.maxStorageBytes === null ? '' : String(q.maxStorageBytes / MB))
  const [saving, setSaving] = useState(false)

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    const mb = numberOrNull(maxStorageMb)
    setSaving(true)
    await onSave(tenant, { maxIndexes: numberOrNull(maxIndexes), maxDocuments: numberOrNull(maxDocuments), maxStorageBytes: mb === null ? null : Math.round(mb * MB) })
    setSaving(false)
  }

  return (
    <div className="grid gap-5">
      <div className="grid gap-3 sm:grid-cols-3">
        <Usage label={t.indexes} used={tenant.usage.indexes} limit={q.maxIndexes} format={formatNumber} />
        <Usage label={t.documents} used={tenant.usage.documents} limit={q.maxDocuments} format={formatNumber} />
        <Usage label={t.disk} used={tenant.usage.storageBytes} limit={q.maxStorageBytes} format={formatBytes} />
      </div>
      <form className="grid items-end gap-3 sm:grid-cols-[1fr_1fr_1fr_auto]" onSubmit={submit}>
        <div className="grid gap-1.5">
          <Label htmlFor="q-indexes">{t.maxIndexes}</Label>
          <Input id="q-indexes" type="number" min={0} placeholder={t.noLimit} value={maxIndexes} onChange={(e) => setMaxIndexes(e.target.value)} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="q-docs">{t.maxDocuments}</Label>
          <Input id="q-docs" type="number" min={0} placeholder={t.noLimit} value={maxDocuments} onChange={(e) => setMaxDocuments(e.target.value)} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="q-storage">{t.maxStorage}</Label>
          <Input id="q-storage" type="number" min={0} step="any" placeholder={t.noLimit} value={maxStorageMb} onChange={(e) => setMaxStorageMb(e.target.value)} />
        </div>
        <Button type="submit" disabled={saving}>
          {t.saveQuotas}
        </Button>
      </form>
    </div>
  )
}

function SearchTester({ tenant }: { tenant: Tenant }) {
  const { t } = useI18n()
  const [index, setIndex] = useState<string | null>(null)
  const [q, setQ] = useState('')
  const [result, setResult] = useState<SearchResult | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  if (tenant.indexes.length === 0) {
    return <p className="py-6 text-center text-sm text-muted-foreground">{t.noIndexesToSearch}</p>
  }
  const current = tenant.indexes.some((ix) => ix.uid === index) ? index! : tenant.indexes[0].uid

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    try {
      setResult(await search(tenant.masterKey, current, q))
      setError('')
    } catch (err) {
      setResult(null)
      setError(err instanceof Error ? err.message : String(err))
    }
    setBusy(false)
  }

  return (
    <div className="grid gap-3">
      <form className="flex flex-wrap gap-2" onSubmit={submit}>
        <Select value={current} onValueChange={(value) => setIndex(value as string)}>
          <SelectTrigger aria-label={t.index} className="min-w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {tenant.indexes.map((ix) => (
              <SelectItem key={ix.uid} value={ix.uid}>
                {ix.uid}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Input className="min-w-0 flex-1 basis-56" placeholder={t.searchPlaceholder} aria-label={t.searchPlaceholder} value={q} onChange={(e) => setQ(e.target.value)} />
        <Button type="submit" disabled={busy}>
          <SearchIcon /> {t.search}
        </Button>
      </form>
      {error && <p className="text-sm text-destructive">{error}</p>}
      {result && (
        <>
          <p className="text-sm text-muted-foreground">
            {t.searchSummary(formatNumber(result.estimatedTotalHits ?? result.totalHits), result.processingTimeMs, result.hits.length)}
          </p>
          {result.hits.map((hit, i) => (
            <pre key={i} className="max-h-56 overflow-auto rounded-lg border bg-muted/50 p-3 font-mono text-xs whitespace-pre-wrap">
              {JSON.stringify(hit, null, 2)}
            </pre>
          ))}
        </>
      )}
    </div>
  )
}
