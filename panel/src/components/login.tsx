import { useState } from 'react'
import { LanguageButton } from '@/components/language-button'
import { Logo } from '@/components/logo'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useI18n } from '@/lib/i18n'

export function Login({ error, onLogin }: { error: string; onLogin: (key: string) => void }) {
  const { t } = useI18n()
  const [key, setKey] = useState('')
  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-6 p-4">
      <Logo />
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>{t.admin}</CardTitle>
          <CardDescription>
            {t.loginHint} <code className="font-mono text-xs">TRAILHOUND_ADMIN_KEY</code>.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="grid gap-3"
            onSubmit={(e) => {
              e.preventDefault()
              if (key.trim()) onLogin(key.trim())
            }}
          >
            <div className="grid gap-1.5">
              <Label htmlFor="admin-key">{t.adminKey}</Label>
              <Input id="admin-key" type="password" autoComplete="off" autoFocus value={key} onChange={(e) => setKey(e.target.value)} aria-invalid={error ? true : undefined} />
              {error && <p className="text-sm text-destructive">{error}</p>}
            </div>
            <Button type="submit" disabled={!key.trim()}>
              {t.signIn}
            </Button>
          </form>
        </CardContent>
      </Card>
      <LanguageButton />
    </div>
  )
}
