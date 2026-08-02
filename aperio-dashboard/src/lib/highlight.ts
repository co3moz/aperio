/** Syntax highlighting for captured request and response bodies.
 *
 * Hand-written rather than pulled from a library: the inspector shows JSON,
 * XML and HTML and nothing else, and a general-purpose highlighter is
 * hundreds of kilobytes of grammars for languages that never appear here.
 * What follows is a tokenizer per shape, returning spans a component renders.
 *
 * Everything degrades to plain text. A body that does not parse, or is large
 * enough that tokenizing it would be felt, is shown exactly as it was before:
 * the point is to make a body easier to read, and a highlighter that stalls
 * the dialog has made it harder.
 */

/** One piece of highlighted text. */
export interface Token {
  text: string
  /** Which colour class to use; `undefined` = the default foreground. */
  kind?: 'key' | 'string' | 'number' | 'literal' | 'punct' | 'tag' | 'attr' | 'comment'
}

/** Above this many characters the body is shown unhighlighted.
 *
 * Bodies are capped by the capture itself, so this is a second fence rather
 * than the only one: it exists so that a future, larger cap cannot silently
 * turn opening a request into a freeze. */
export const MAX_HIGHLIGHT_CHARS = 200_000

export type BodyLanguage = 'json' | 'markup' | 'text'

/** What this body looks like, from the body itself rather than from a header.
 *
 * The header is not consulted on purpose: a backend that answers JSON as
 * `text/plain` is common enough, and the inspector's job is to show what was
 * actually sent. */
export function detectLanguage(body: string): BodyLanguage {
  const trimmed = body.trimStart()
  if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
    try {
      JSON.parse(body)
      return 'json'
    } catch {
      return 'text'
    }
  }
  if (trimmed.startsWith('<')) return 'markup'
  return 'text'
}

/** Re-indents JSON so a minified body is readable. Returns the original text
 *  when it does not parse, which is what a truncated capture looks like. */
export function prettyJson(body: string): string {
  try {
    return JSON.stringify(JSON.parse(body), null, 2)
  } catch {
    return body
  }
}

/** Tokenizes JSON. Assumes the text parses; a key is a string in the position
 *  before a colon, which is what makes keys and values distinguishable at all
 *  without building a syntax tree. */
function tokenizeJson(text: string): Token[] {
  const out: Token[] = []
  let i = 0
  while (i < text.length) {
    const c = text[i]
    if (c === '"') {
      let j = i + 1
      while (j < text.length) {
        if (text[j] === '\\') {
          j += 2
          continue
        }
        if (text[j] === '"') break
        j++
      }
      const literal = text.slice(i, Math.min(j + 1, text.length))
      // A colon after optional whitespace means this string was a key.
      let k = j + 1
      while (k < text.length && (text[k] === ' ' || text[k] === '\t')) k++
      out.push({ text: literal, kind: text[k] === ':' ? 'key' : 'string' })
      i = j + 1
      continue
    }
    if (c === '-' || (c >= '0' && c <= '9')) {
      let j = i
      while (j < text.length && /[-+0-9.eE]/.test(text[j])) j++
      out.push({ text: text.slice(i, j), kind: 'number' })
      i = j
      continue
    }
    for (const word of ['true', 'false', 'null']) {
      if (text.startsWith(word, i)) {
        out.push({ text: word, kind: 'literal' })
        i += word.length
      }
    }
    if (i < text.length && text[i] === c && !'"-0123456789'.includes(c)) {
      if ('{}[],:'.includes(c)) {
        out.push({ text: c, kind: 'punct' })
      } else {
        out.push({ text: c })
      }
      i++
    }
  }
  return merge(out)
}

/** Tokenizes XML and HTML: tags, attribute names, attribute values, comments.
 *  Text between tags is left alone, which is usually the part being read. */
function tokenizeMarkup(text: string): Token[] {
  const out: Token[] = []
  let i = 0
  while (i < text.length) {
    const lt = text.indexOf('<', i)
    if (lt === -1) {
      out.push({ text: text.slice(i) })
      break
    }
    if (lt > i) out.push({ text: text.slice(i, lt) })
    if (text.startsWith('<!--', lt)) {
      const end = text.indexOf('-->', lt)
      const stop = end === -1 ? text.length : end + 3
      out.push({ text: text.slice(lt, stop), kind: 'comment' })
      i = stop
      continue
    }
    const gt = text.indexOf('>', lt)
    const stop = gt === -1 ? text.length : gt + 1
    out.push(...tokenizeTag(text.slice(lt, stop)))
    i = stop
  }
  return merge(out)
}

/** Splits one `<...>` into its tag name, attribute names and values. */
function tokenizeTag(tag: string): Token[] {
  const out: Token[] = []
  const nameMatch = /^<\/?[A-Za-z_!?][\w:.-]*/.exec(tag)
  if (!nameMatch) return [{ text: tag, kind: 'tag' }]
  out.push({ text: nameMatch[0], kind: 'tag' })
  let i = nameMatch[0].length
  while (i < tag.length) {
    const attr = /^\s*([\w:.-]+)/.exec(tag.slice(i))
    if (!attr) {
      out.push({ text: tag.slice(i), kind: 'tag' })
      break
    }
    out.push({ text: attr[0].slice(0, attr[0].length - attr[1].length) })
    out.push({ text: attr[1], kind: 'attr' })
    i += attr[0].length
    const value = /^\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)/.exec(tag.slice(i))
    if (value) {
      out.push({ text: value[0].slice(0, value[0].length - value[1].length), kind: 'punct' })
      out.push({ text: value[1], kind: 'string' })
      i += value[0].length
    }
  }
  return out
}

/** Joins neighbouring tokens of the same kind, so a body becomes a handful of
 *  spans rather than one per character. */
function merge(tokens: Token[]): Token[] {
  const out: Token[] = []
  for (const token of tokens) {
    if (token.text === '') continue
    const last = out[out.length - 1]
    if (last && last.kind === token.kind) {
      last.text += token.text
    } else {
      out.push({ ...token })
    }
  }
  return out
}

/** Highlights a body, or returns it as a single plain token when it is not a
 *  shape we tokenize or is too large to be worth it. */
export function highlight(body: string): { language: BodyLanguage; tokens: Token[] } {
  if (body.length > MAX_HIGHLIGHT_CHARS) return { language: 'text', tokens: [{ text: body }] }
  const language = detectLanguage(body)
  if (language === 'json') return { language, tokens: tokenizeJson(prettyJson(body)) }
  if (language === 'markup') return { language, tokens: tokenizeMarkup(body) }
  return { language, tokens: [{ text: body }] }
}
