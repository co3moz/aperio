#!/usr/bin/env node
/**
 * Exports the brand lockup, the mark plus the APERIO wordmark, as a PNG for
 * anything that cannot render the SVG and the webfont itself. Today that is
 * the LaTeX book's title page.
 *
 * The wordmark is Michroma, the same file the dashboard ships, so the book
 * and the product spell the name identically instead of approximating it
 * with whatever the TeX installation happens to have.
 *
 *   node scripts/export-brand.mjs
 *
 * Writes `docs/images/aperio-lockup.png` (transparent, 4x, for print).
 */
import { chromium } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = fileURLToPath(new URL('.', import.meta.url))
const REPO = resolve(HERE, '../..')
const OUT = join(REPO, 'docs/images/aperio-lockup.png')

// Print, not screen: the book scales this down to a few centimetres, and a
// title page is the one place a soft edge is obvious.
const SCALE = 4

const mark = readFileSync(join(REPO, 'docs/assets/aperio-mark.svg'), 'utf8')
const michroma = readFileSync(
  join(REPO, 'aperio-dashboard/node_modules/@fontsource/michroma/files/michroma-latin-400-normal.woff2'),
).toString('base64')

const page = `<!doctype html>
<style>
  @font-face {
    font-family: 'Michroma';
    src: url(data:font/woff2;base64,${michroma}) format('woff2');
    font-weight: 400;
  }
  html, body { margin: 0; background: transparent; }
  #lockup {
    display: inline-flex;
    align-items: center;
    gap: 26px;
    padding: 8px 10px;
  }
  #lockup svg { width: 132px; height: 132px; display: block; }
  #word {
    font-family: 'Michroma', sans-serif;
    font-size: 92px;
    letter-spacing: 0.04em;
    color: #1B1B1B;
    line-height: 1;
    /* The wordmark is always uppercase; the dashboard pins it the same way
       so a translation cannot lowercase the brand. */
    text-transform: uppercase;
  }
</style>
<div id="lockup">${mark}<div id="word">Aperio</div></div>`

const browser = await chromium.launch()
const context = await browser.newContext({ deviceScaleFactor: SCALE })
const tab = await context.newPage()
await tab.setContent(page)
await tab.evaluate(() => document.fonts.ready)
await tab.locator('#lockup').screenshot({ path: OUT, omitBackground: true })
await browser.close()
console.log(`wrote docs/images/${OUT.split('/').pop()}`)
