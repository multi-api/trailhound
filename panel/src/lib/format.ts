import { currentLocale } from '@/lib/i18n'

export const formatNumber = (n: number | null | undefined) => Number(n ?? 0).toLocaleString(currentLocale())

export function formatBytes(value: number | null | undefined) {
  let n = Number(value ?? 0)
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024
    i++
  }
  return (i ? n.toLocaleString(currentLocale(), { maximumFractionDigits: 1 }) : n) + ' ' + units[i]
}

export function formatDate(iso: string) {
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleDateString(currentLocale(), { year: 'numeric', month: 'short', day: 'numeric' })
}

/** An empty field means "no limit". */
export const numberOrNull = (text: string) => (text.trim() === '' ? null : Number(text))
