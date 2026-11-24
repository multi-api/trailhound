import { createContext, useContext, useEffect, useState } from 'react'

const en = {
  locale: 'en-US',
  otherLanguage: 'Español',
  title: 'Trailhound · Administration',
  admin: 'Administration',
  logout: 'Sign out',
  cancel: 'Cancel',

  loginHint: 'The server prints the key the first time it starts, or you can set it with',
  adminKey: 'Admin key',
  signIn: 'Sign in',
  invalidKey: 'That admin key is not valid.',

  tenants: 'Tenants',
  tenantsHint: 'A tenant is one customer or project of yours. Each gets its own indexes, API keys and limits, and cannot see the others.',
  newTenant: 'New tenant',
  newTenantHint: 'Create one for each customer or project that will use the search engine. Empty limits mean no limit.',
  noTenants: 'There are no tenants yet. Create one to get the API keys your application will use.',
  createFirst: 'Create the first one',
  createTenant: 'Create tenant',
  name: 'Name',
  noLimit: 'No limit',
  maxIndexes: 'Max. indexes',
  maxDocuments: 'Max. documents',
  maxStorage: 'Max. disk (MB)',

  tenant: 'Tenant',
  indexes: 'Indexes',
  documents: 'Documents',
  disk: 'Disk',
  onDisk: 'On disk',
  searches: 'Searches',
  pendingTasks: 'Pending tasks',
  status: 'Status',
  actions: 'Actions',
  actionsFor: (name: string) => `Actions for ${name}`,
  active: 'Active',
  suspended: 'Suspended',
  viewDetail: 'View details',
  suspend: 'Suspend',
  resume: 'Resume',
  remove: 'Delete',

  closeDetail: 'Close details',
  createdOn: (date: string) => `created on ${date}`,
  tabKeys: 'Keys',
  tabQuotas: 'Limits',
  tabSearch: 'Try a search',
  masterKey: 'Master key',
  masterKeyHint: "Full access to this tenant's indexes. Keep it on your server.",
  searchKey: 'Search key',
  searchKeyHint: 'Can only search; this is the one to use in the browser.',
  noKey: 'This tenant does not have one.',
  show: 'Show',
  hide: 'Hide',
  copy: 'Copy',
  copied: (label: string) => `${label} copied`,
  copyFailed: 'The browser did not allow copying',
  rotate: 'Rotate',
  index: 'Index',
  segments: 'Segments',
  noIndexes: 'This tenant has no indexes yet.',
  usageOf: (used: string, limit: string | null) => `${used} of ${limit ?? 'no limit'}`,
  saveQuotas: 'Save limits',
  noIndexesToSearch: 'This tenant has no indexes to search yet.',
  searchPlaceholder: 'Text to search for',
  search: 'Search',
  searchSummary: (total: string, ms: number, shown: number) => `${total} results · ${ms} ms · showing the first ${shown}`,

  confirmDeleteTitle: (name: string) => `Delete ${name}?`,
  confirmDeleteText: 'All of its indexes and documents are deleted. This cannot be undone.',
  confirmDeleteAction: 'Delete tenant',
  confirmRotateTitle: 'Rotate the master key?',
  confirmRotateText: (name: string) => `The current key of ${name} stops working immediately.`,
  confirmRotateAction: 'Rotate key',

  created: (name: string) => `${name} created`,
  deleted: (name: string) => `${name} deleted`,
  suspendedDone: (name: string) => `${name} suspended`,
  resumedDone: (name: string) => `${name} resumed`,
  rotated: 'Master key rotated',
  quotasSaved: 'Limits saved',
}

export type Messages = typeof en

const es: Messages = {
  locale: 'es-CL',
  otherLanguage: 'English',
  title: 'Trailhound · Administración',
  admin: 'Administración',
  logout: 'Salir',
  cancel: 'Cancelar',

  loginHint: 'El servidor muestra la clave la primera vez que arranca, o se define con',
  adminKey: 'Clave de administración',
  signIn: 'Entrar',
  invalidKey: 'La clave de administración no es válida.',

  tenants: 'Clientes',
  tenantsHint: 'Un cliente es cada empresa o proyecto tuyo que usa el buscador. Cada uno tiene sus propios índices, claves de API y límites, y no ve a los demás.',
  newTenant: 'Nuevo cliente',
  newTenantHint: 'Crea uno por cada empresa o proyecto que vaya a usar el buscador. Los límites vacíos significan sin límite.',
  noTenants: 'Todavía no hay clientes. Crea uno para obtener las claves de API que usará tu aplicación.',
  createFirst: 'Crear el primero',
  createTenant: 'Crear cliente',
  name: 'Nombre',
  noLimit: 'Sin límite',
  maxIndexes: 'Máx. índices',
  maxDocuments: 'Máx. documentos',
  maxStorage: 'Máx. disco (MB)',

  tenant: 'Cliente',
  indexes: 'Índices',
  documents: 'Documentos',
  disk: 'Disco',
  onDisk: 'En disco',
  searches: 'Búsquedas',
  pendingTasks: 'Tareas pend.',
  status: 'Estado',
  actions: 'Acciones',
  actionsFor: (name) => `Acciones de ${name}`,
  active: 'Activo',
  suspended: 'Suspendido',
  viewDetail: 'Ver detalle',
  suspend: 'Suspender',
  resume: 'Reactivar',
  remove: 'Eliminar',

  closeDetail: 'Cerrar detalle',
  createdOn: (date) => `creado el ${date}`,
  tabKeys: 'Claves',
  tabQuotas: 'Límites',
  tabSearch: 'Probar búsqueda',
  masterKey: 'Clave maestra',
  masterKeyHint: 'Acceso completo a los índices de este cliente. Guárdala en tu servidor.',
  searchKey: 'Clave de búsqueda',
  searchKeyHint: 'Solo permite buscar; es la que va en el navegador.',
  noKey: 'Este cliente no tiene una.',
  show: 'Mostrar',
  hide: 'Ocultar',
  copy: 'Copiar',
  copied: (label) => `${label} copiada`,
  copyFailed: 'El navegador no permitió copiar',
  rotate: 'Rotar',
  index: 'Índice',
  segments: 'Segmentos',
  noIndexes: 'Este cliente todavía no tiene índices.',
  usageOf: (used, limit) => `${used} de ${limit ?? 'sin límite'}`,
  saveQuotas: 'Guardar límites',
  noIndexesToSearch: 'Este cliente todavía no tiene índices donde buscar.',
  searchPlaceholder: 'Texto a buscar',
  search: 'Buscar',
  searchSummary: (total, ms, shown) => `${total} resultados · ${ms} ms · se muestran los primeros ${shown}`,

  confirmDeleteTitle: (name) => `¿Eliminar a ${name}?`,
  confirmDeleteText: 'Se borran todos sus índices y documentos. No se puede deshacer.',
  confirmDeleteAction: 'Eliminar cliente',
  confirmRotateTitle: '¿Rotar la clave maestra?',
  confirmRotateText: (name) => `La clave actual de ${name} deja de funcionar de inmediato.`,
  confirmRotateAction: 'Rotar clave',

  created: (name) => `${name} creado`,
  deleted: (name) => `${name} eliminado`,
  suspendedDone: (name) => `${name} suspendido`,
  resumedDone: (name) => `${name} reactivado`,
  rotated: 'Clave maestra rotada',
  quotasSaved: 'Límites guardados',
}

export type Language = 'en' | 'es'
const messages: Record<Language, Messages> = { en, es }
const STORAGE_KEY = 'trailhound-language'

/** English unless the person chose Spanish before. */
function storedLanguage(): Language {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'es' ? 'es' : 'en'
  } catch {
    return 'en'
  }
}

// Read by the formatters in format.ts, which are called outside React too.
let locale = en.locale
export const currentLocale = () => locale

const I18nContext = createContext<{ t: Messages; toggleLanguage: () => void }>({ t: en, toggleLanguage: () => {} })

export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [language, setLanguage] = useState(storedLanguage)
  const t = messages[language]
  locale = t.locale

  useEffect(() => {
    document.documentElement.lang = language
    document.title = t.title
  }, [language, t])

  function toggleLanguage() {
    const next = language === 'en' ? 'es' : 'en'
    try {
      localStorage.setItem(STORAGE_KEY, next)
    } catch {
      // Without storage the choice lasts until the page is reloaded.
    }
    setLanguage(next)
  }

  return <I18nContext value={{ t, toggleLanguage }}>{children}</I18nContext>
}

export const useI18n = () => useContext(I18nContext)
