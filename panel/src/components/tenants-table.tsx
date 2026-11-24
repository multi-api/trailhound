import { EllipsisIcon, EyeIcon, PauseIcon, PlayIcon, Trash2Icon } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import type { Tenant } from '@/lib/api'
import { formatBytes, formatNumber } from '@/lib/format'
import { useI18n } from '@/lib/i18n'

type Props = {
  tenants: Tenant[]
  selectedId: string | null
  onSelect: (id: string) => void
  onToggleSuspended: (tenant: Tenant) => void
  onDelete: (tenant: Tenant) => void
}

export function StatusBadge({ suspended }: { suspended: boolean }) {
  const { t } = useI18n()
  return suspended ? (
    <Badge variant="destructive">{t.suspended}</Badge>
  ) : (
    <Badge variant="outline" className="border-success/40 text-success">
      {t.active}
    </Badge>
  )
}

export function TenantsTable({ tenants, selectedId, onSelect, onToggleSuspended, onDelete }: Props) {
  const { t } = useI18n()
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>{t.tenant}</TableHead>
          <TableHead className="text-right">{t.indexes}</TableHead>
          <TableHead className="text-right">{t.documents}</TableHead>
          <TableHead className="text-right">{t.disk}</TableHead>
          <TableHead className="text-right">{t.searches}</TableHead>
          <TableHead className="text-right">{t.pendingTasks}</TableHead>
          <TableHead>{t.status}</TableHead>
          <TableHead className="w-10">
            <span className="sr-only">{t.actions}</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {tenants.map((tenant) => (
          <TableRow key={tenant.id} data-state={tenant.id === selectedId ? 'selected' : undefined} className="cursor-pointer" onClick={() => onSelect(tenant.id)}>
            <TableCell>
              <div className="font-medium">{tenant.name}</div>
              <div className="font-mono text-xs text-muted-foreground">{tenant.id}</div>
            </TableCell>
            <TableCell className="text-right tabular-nums">{formatNumber(tenant.usage.indexes)}</TableCell>
            <TableCell className="text-right tabular-nums">{formatNumber(tenant.usage.documents)}</TableCell>
            <TableCell className="text-right tabular-nums">{formatBytes(tenant.usage.storageBytes)}</TableCell>
            <TableCell className="text-right tabular-nums">{formatNumber(tenant.usage.searches)}</TableCell>
            <TableCell className="text-right tabular-nums">{formatNumber(tenant.tasks.pending)}</TableCell>
            <TableCell>
              <StatusBadge suspended={tenant.suspended} />
            </TableCell>
            <TableCell onClick={(e) => e.stopPropagation()}>
              <DropdownMenu>
                <DropdownMenuTrigger render={<Button variant="ghost" size="icon-sm" aria-label={t.actionsFor(tenant.name)} />}>
                  <EllipsisIcon />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-40">
                  <DropdownMenuItem onClick={() => onSelect(tenant.id)}>
                    <EyeIcon /> {t.viewDetail}
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => onToggleSuspended(tenant)}>
                    {tenant.suspended ? <PlayIcon /> : <PauseIcon />} {tenant.suspended ? t.resume : t.suspend}
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem variant="destructive" onClick={() => onDelete(tenant)}>
                    <Trash2Icon /> {t.remove}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}
