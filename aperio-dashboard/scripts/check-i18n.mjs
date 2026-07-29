#!/usr/bin/env node
// i18n key coverage check.
//
// English is the source language: the translation KEY is the English string
// that appears in the source, and a missing entry falls back to English. Some
// keys are translated dynamically (`t(field.label)` over a constant), so the
// reference set is every string literal in the source, not only `t('...')`
// arguments, a dict key is "used" when its English string appears anywhere in
// the source. Source is parsed with the TypeScript compiler (not regex) so a
// stray apostrophe in a comment or JSX text can never desync extraction.
//
// The script audits every dictionary in src/i18n:
//
//   - untranslated `t('...')` strings             (FATAL, ships English)
//   - duplicate keys within a dictionary file      (FATAL, silent overwrite)
//   - stale keys whose English string is gone      (FATAL, dead translation)
//   - key-set parity across languages              (info, dynamic keys only)
//
// The first check is the one that catches a forgotten translation. Parity
// alone cannot: when *every* language misses a new string they agree with each
// other perfectly and the dashboard quietly ships English. So the literal
// arguments of `t(...)` are extracted separately and every language must carry
// them. Runs as part of `npm run build`, so an untranslated string fails the
// build rather than reaching a release.
//
// Exits non-zero when any FATAL problem is found, so CI catches drift.

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const SRC = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'src')
const I18N_DIR = join(SRC, 'i18n')
const LANGS = ['de', 'es', 'fr', 'tr', 'ru', 'zh', 'ja']

/** Recursively lists every .ts/.tsx file under `dir`. */
function sourceFiles(dir) {
  const out = []
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) {
      out.push(...sourceFiles(full))
    } else if (/\.tsx?$/.test(entry)) {
      out.push(full)
    }
  }
  return out
}

/** Parses a file into a TypeScript AST. */
function parse(file, text) {
  return ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  )
}

const isDictFile = (file) =>
  file.startsWith(I18N_DIR + '/') && LANGS.some((l) => file.endsWith(`/${l}.ts`))

/**
 * The reference key set: the `.text` of every string / no-substitution template
 * literal in the source (dictionaries excluded). `.text` is the decoded value,
 * so it matches a dictionary key verbatim.
 */
function referenceKeys() {
  const keys = new Set()
  for (const file of sourceFiles(SRC)) {
    if (isDictFile(file)) continue
    const sf = parse(file, readFileSync(file, 'utf8'))
    const visit = (node) => {
      if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
        keys.add(node.text)
      }
      ts.forEachChild(node, visit)
    }
    visit(sf)
  }
  return keys
}

/**
 * String literals assigned to the named properties anywhere in a file. Used
 * for tables of text that reach `t()` indirectly.
 */
function tableStrings(file, properties) {
  const out = new Set()
  const sf = parse(file, readFileSync(file, 'utf8'))
  const literal = (node) =>
    ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)
  const visit = (node) => {
    if (
      ts.isPropertyAssignment(node) &&
      ts.isIdentifier(node.name) &&
      properties.includes(node.name.text) &&
      literal(node.initializer)
    ) {
      out.add(node.initializer.text)
    }
    // A map of option → what that option does. The *values* are the prose;
    // the keys are the setting's accepted values and are never translated. A
    // list of names would otherwise be checked while the sentences explaining
    // them shipped in English, the same shape as the gap that hid the
    // settings labels themselves.
    if (
      ts.isPropertyAssignment(node) &&
      ts.isIdentifier(node.name) &&
      properties.includes(node.name.text) &&
      ts.isObjectLiteralExpression(node.initializer)
    ) {
      for (const prop of node.initializer.properties) {
        if (ts.isPropertyAssignment(prop) && literal(prop.initializer)) {
          out.add(prop.initializer.text)
        }
      }
    }
    ts.forEachChild(node, visit)
  }
  visit(sf)
  if (out.size === 0) {
    throw new Error(`no ${properties.join('/')} strings found in ${file}`)
  }
  return out
}

/**
 * Strings the source explicitly asks to translate: the first argument of every
 * `t('...')` call, when it is a plain literal. Unlike [`referenceKeys`] this is
 * a *requirement*, a key here that a dictionary lacks renders as English.
 *
 * Dynamic calls (`t(field.label)`) cannot be resolved statically and are not
 * required; they are still covered by the parity check, which reports when one
 * language has a key the others do not.
 */
function requiredKeys() {
  const keys = new Set()
  for (const file of sourceFiles(SRC)) {
    if (isDictFile(file)) continue
    const sf = parse(file, readFileSync(file, 'utf8'))
    const visit = (node) => {
      if (
        ts.isCallExpression(node) &&
        ts.isIdentifier(node.expression) &&
        node.expression.text === 't' &&
        node.arguments.length > 0
      ) {
        const first = node.arguments[0]
        if (
          ts.isStringLiteral(first) ||
          ts.isNoSubstitutionTemplateLiteral(first)
        ) {
          keys.add(first.text)
        }
      }
      ts.forEachChild(node, visit)
    }
    visit(sf)
  }
  // Strings passed to `t()` through a variable are invisible to the walk
  // above, so a table of user-facing text ends up looking fully translated
  // while rendering in English. The config builder's section titles and
  // descriptions are exactly that: `t(section.spec.title)`. They are read
  // from their own table instead.
  // Catalogues whose strings reach `t()` through a variable. Without these the
  // check reports a clean sheet over a screen that ships entirely in English:
  // the strings are "used" (they appear in the source, so they are not stale)
  // but nothing ever demanded a translation for them.
  for (const [file, properties] of [
    [join(SRC, 'lib', 'configGroups.ts'), ['title', 'description']],
    [join(SRC, 'lib', 'settingsCatalog.ts'), ['title', 'description', 'label', 'hint', 'optionHints']],
    // The sidebar's own page table. Its `label`s were translated by accident,
    // because the same words appear in a literal `t()` somewhere else; the
    // `hint`s appear nowhere else, so the line under every page title shipped
    // in English in all seven languages.
    [join(SRC, 'components', 'AppSidebar.tsx'), ['label', 'hint']],
    [join(SRC, 'components', 'SettingsDialog.tsx'), ['label']],
    [join(SRC, 'components', 'ToolsDialog.tsx'), ['label']],
  ]) {
    for (const key of tableStrings(file, properties)) keys.add(key)
  }
  return keys
}

/**
 * The keys of one language dictionary, in order, plus the set of duplicates.
 * Reads string-literal property names from every object literal in the file
 * (the dictionaries are a single exported object of `'key': 'value'` pairs).
 */
function dictKeys(lang) {
  const file = join(I18N_DIR, `${lang}.ts`)
  const sf = parse(file, readFileSync(file, 'utf8'))
  const seen = new Set()
  const duplicates = new Set()
  const visit = (node) => {
    // Keys that need no quotes are written without them (`Cancel: '…'`), so
    // reading only string literals would miss them, and then report a key the
    // file already has as untranslated, which is how a duplicate gets added.
    if (
      ts.isPropertyAssignment(node) &&
      (ts.isStringLiteral(node.name) ||
        ts.isIdentifier(node.name) ||
        ts.isNoSubstitutionTemplateLiteral(node.name))
    ) {
      const key = node.name.text
      if (seen.has(key)) duplicates.add(key)
      seen.add(key)
    }
    ts.forEachChild(node, visit)
  }
  visit(sf)
  return { keys: seen, duplicates }
}

function main() {
  const reference = referenceKeys()
  const required = requiredKeys()
  console.log(
    `Reference: ${reference.size} string literals in source, ${required.size} of them translated via t()`,
  )

  const parsed = LANGS.map((lang) => ({ lang, ...dictKeys(lang) }))
  // Union of every dictionary's keys: the set each language should cover so no
  // language silently falls back to English on a string the others translate.
  const union = new Set()
  for (const { keys } of parsed) for (const k of keys) union.add(k)

  let fatal = 0
  for (const { lang, keys, duplicates } of parsed) {
    const stale = [...keys].filter((k) => !reference.has(k)).sort()
    const untranslated = [...required].filter((k) => !keys.has(k)).sort()
    // Parity covers what `required` cannot see: keys reached dynamically.
    const missing = [...union]
      .filter((k) => !keys.has(k) && !required.has(k))
      .sort()

    const problems = []
    if (untranslated.length) {
      fatal += untranslated.length
      const shown = untranslated.slice(0, 10)
      const more =
        untranslated.length > shown.length
          ? ` … and ${untranslated.length - shown.length} more`
          : ''
      problems.push(
        `  FAIL  ${untranslated.length} untranslated string(s): ${shown
          .map((k) => JSON.stringify(k))
          .join(', ')}${more}`,
      )
    }
    if (duplicates.size) {
      fatal += duplicates.size
      problems.push(`  FAIL  ${duplicates.size} duplicate key(s): ${[...duplicates].join(' | ')}`)
    }
    if (stale.length) {
      fatal += stale.length
      problems.push(`  FAIL  ${stale.length} stale key(s) (English string gone): ${stale.join(' | ')}`)
    }
    if (missing.length) {
      // Not fatal: a language may lag on new strings and fall back to English.
      problems.push(`  info  ${missing.length} key(s) another language has but this one lacks`)
    }
    if (problems.length) {
      console.log(`\n${lang}.ts:`)
      for (const p of problems) console.log(p)
    } else {
      console.log(`${lang}.ts: ok (${keys.size} keys)`)
    }
  }

  console.log()
  if (fatal > 0) {
    console.log(
      `i18n check FAILED: ${fatal} problem(s) (untranslated, duplicate, or stale keys)`,
    )
    process.exit(1)
  }
  console.log('i18n check OK')
}

main()
