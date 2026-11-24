import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import type { Quotas } from '@/lib/api'
import { numberOrNull } from '@/lib/format'
import { useI18n } from '@/lib/i18n'

type Props = {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Resolves to whether the tenant was created. */
  onCreate: (name: string, quotas: Partial<Quotas>) => Promise<boolean>
}

export function NewTenantDialog({ open, onOpenChange, onCreate }: Props) {
  const { t } = useI18n()
  const [name, setName] = useState('')
  const [maxIndexes, setMaxIndexes] = useState('')
  const [maxDocuments, setMaxDocuments] = useState('')
  const [saving, setSaving] = useState(false)

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setSaving(true)
    const created = await onCreate(name.trim(), { maxIndexes: numberOrNull(maxIndexes), maxDocuments: numberOrNull(maxDocuments) })
    setSaving(false)
    if (created) {
      setName('')
      setMaxIndexes('')
      setMaxDocuments('')
      onOpenChange(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form className="grid gap-4" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.newTenant}</DialogTitle>
            <DialogDescription>{t.newTenantHint}</DialogDescription>
          </DialogHeader>
          <div className="grid gap-1.5">
            <Label htmlFor="new-name">{t.name}</Label>
            <Input id="new-name" required autoFocus value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="grid gap-1.5">
              <Label htmlFor="new-max-indexes">{t.maxIndexes}</Label>
              <Input id="new-max-indexes" type="number" min={0} placeholder={t.noLimit} value={maxIndexes} onChange={(e) => setMaxIndexes(e.target.value)} />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="new-max-docs">{t.maxDocuments}</Label>
              <Input id="new-max-docs" type="number" min={0} placeholder={t.noLimit} value={maxDocuments} onChange={(e) => setMaxDocuments(e.target.value)} />
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              {t.cancel}
            </Button>
            <Button type="submit" disabled={saving || !name.trim()}>
              {t.createTenant}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
