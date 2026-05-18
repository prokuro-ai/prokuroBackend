import type { ParseResult, AnalyzeResult } from './types'

async function postFile(endpoint: string, file: File): Promise<Response> {
  const form = new FormData()
  form.append('file', file)
  return fetch(endpoint, { method: 'POST', body: form })
}

function isParseResult(body: unknown): body is ParseResult {
  if (!body || typeof body !== 'object') return false
  const candidate = body as Partial<ParseResult>
  return (
    typeof candidate.mapping_confidence === 'number' &&
    Array.isArray(candidate.lines) &&
    typeof candidate.source_filename === 'string'
  )
}

export async function parseFile(file: File): Promise<ParseResult> {
  const res = await postFile('/api/parse', file)
  const body: unknown = await res.json()
  if (res.ok) return body as ParseResult

  // Parser returns 422 for low-confidence parses but still provides usable data.
  if (res.status === 422 && isParseResult(body)) return body

  const message =
    typeof body === 'object' && body && 'error' in body && typeof body.error === 'string'
      ? body.error
      : `HTTP ${res.status}`
  throw new Error(message)

}

export async function analyzeFile(file: File): Promise<AnalyzeResult> {
  const res = await postFile('/api/analyze', file)
  const body: unknown = await res.json()
  if (!res.ok) {
    const message =
      typeof body === 'object' && body && 'error' in body && typeof body.error === 'string'
        ? body.error
        : `HTTP ${res.status}`
    throw new Error(message)
  }
  return body as AnalyzeResult
}
