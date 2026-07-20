#!/usr/bin/env node
// i18n key coverage check.
//
// English is the source language: the translation KEY is the English string
// that appears in the source, and a missing entry falls back to English. Some
// keys are translated dynamically (`t(field.label)` over a constant), so the
// reference set is every string literal in the source, not only `t('...')`
// arguments — a dict key is "used" when its English string appears anywhere in
// the source. Source is parsed with the TypeScript compiler (not regex) so a
// stray apostrophe in a comment or JSX text can never desync extraction.
//
// The script audits every dictionary in src/i18n:
//
//   - duplicate keys within a dictionary file    (FATAL — silent overwrite)
//   - stale keys whose English string is gone     (FATAL — dead translation)
//   - key-set parity across languages             (info — silent fallback risk)
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
    if (ts.isPropertyAssignment(node) && ts.isStringLiteral(node.name)) {
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
  console.log(`Reference: ${reference.size} string literals in source`)

  const parsed = LANGS.map((lang) => ({ lang, ...dictKeys(lang) }))
  // Union of every dictionary's keys: the set each language should cover so no
  // language silently falls back to English on a string the others translate.
  const union = new Set()
  for (const { keys } of parsed) for (const k of keys) union.add(k)

  let fatal = 0
  for (const { lang, keys, duplicates } of parsed) {
    const stale = [...keys].filter((k) => !reference.has(k)).sort()
    const missing = [...union].filter((k) => !keys.has(k)).sort()

    const problems = []
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
    console.log(`i18n check FAILED: ${fatal} fatal problem(s) (duplicate or stale keys)`)
    process.exit(1)
  }
  console.log('i18n check OK')
}

main()
