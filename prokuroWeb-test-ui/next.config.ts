import path from 'path'
import type { NextConfig } from 'next'

const GATEWAY_URL = process.env.GATEWAY_URL ?? 'http://localhost:3000'

const nextConfig: NextConfig = {
  outputFileTracingRoot: path.join(__dirname, '../'),
  async rewrites() {
    return [
      {
        source: '/api/backend/:path*',
        destination: `${GATEWAY_URL}/:path*`,
      },
    ]
  },
}

export default nextConfig
