import { type NextRequest, NextResponse } from 'next/server'

const GATEWAY = process.env.GATEWAY_URL ?? 'http://localhost:3000'

export async function POST(req: NextRequest) {
  const form = await req.formData()

  try {
    const res = await fetch(`${GATEWAY}/v1/analyze`, {
      method: 'POST',
      body: form,
    })
    const body = await res.text()
    return new NextResponse(body, {
      status: res.status,
      headers: { 'Content-Type': 'application/json' },
    })
  } catch {
    return NextResponse.json({ error: 'Could not reach gateway service' }, { status: 502 })
  }
}
