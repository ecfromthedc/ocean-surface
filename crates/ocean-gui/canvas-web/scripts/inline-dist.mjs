import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const distDir = resolve(scriptDir, '..', 'dist')
const indexPath = resolve(distDir, 'index.html')
const inlinePath = resolve(distDir, 'inline.html')

let html = readFileSync(indexPath, 'utf8')

html = html.replace(
  /<link rel="stylesheet" crossorigin href="\.\/([^"]+)">/,
  (_match, href) => {
    const css = readFileSync(resolve(distDir, href), 'utf8').replaceAll('</style', '<\\/style')
    return `<style>${css}</style>`
  },
)

html = html.replace(
  /<script type="module" crossorigin src="\.\/([^"]+)"><\/script>/,
  (_match, src) => {
    const js = readFileSync(resolve(distDir, src), 'utf8').replaceAll('</script', '<\\/script')
    return `<script type="module">${js}</script>`
  },
)

if (html.includes('src="./assets/') || html.includes('href="./assets/')) {
  throw new Error('inline canvas bundle still references external assets')
}

writeFileSync(inlinePath, html)
