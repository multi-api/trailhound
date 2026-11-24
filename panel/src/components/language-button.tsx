import { LanguagesIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useI18n } from '@/lib/i18n'

/** Switches between English and Spanish; the label names the other language. */
export function LanguageButton({ className }: { className?: string }) {
  const { t, toggleLanguage } = useI18n()
  return (
    <Button variant="ghost" className={className} onClick={toggleLanguage} lang={t.locale.startsWith('en') ? 'es' : 'en'}>
      <LanguagesIcon /> {t.otherLanguage}
    </Button>
  )
}
