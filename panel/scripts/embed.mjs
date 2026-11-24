// Copies the built panel to where the server embeds it (include_str! in admin.rs).
import { copyFileSync } from 'node:fs'
copyFileSync(new URL('../dist/index.html', import.meta.url), new URL('../../crates/server/src/panel.html', import.meta.url))
console.log('panel.html updated')
