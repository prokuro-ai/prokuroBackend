import { type NextRequest, NextResponse } from 'next/server'

const PARSER = process.env.PARSER_URL ?? 'http://localhost:3001'

export async function POST(req: NextRequest) {
  const form = await req.formData()

  try {
    const res = await fetch(`${PARSER}/v1/parse`, {
      method: 'POST',
      body: form,
    })
    const body = await res.text()
    return new NextResponse(body, {
      status: res.status,
      headers: { 'Content-Type': 'application/json' },
    })
  } catch {
    return NextResponse.json({ error: 'Could not reach parser service' }, { status: 502 })
  }
}
