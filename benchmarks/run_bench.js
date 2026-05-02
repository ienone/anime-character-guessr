#!/usr/bin/env node
/**
 * Benchmark: Single-Player Game API
 *
 * Simulates concurrent players opening games and making guesses.
 * Runs against Node.js and Rust backends and outputs a comparison report.
 *
 * Usage:
 *   node benchmarks/run_bench.js [--target node|rust|both] [--concurrency 10] [--requests 100]
 *
 * Prerequisites:
 *   npm install autocannon  (in this directory or globally)
 */

import { parseArgs } from 'node:util'
import { execSync, spawn } from 'node:child_process'
import http from 'node:http'
import https from 'node:https'
import { writeFileSync, mkdirSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))

// ─── CLI args ─────────────────────────────────────────────────────────────────

const { values: args } = parseArgs({
  options: {
    target:      { type: 'string', default: 'both' },    // 'node' | 'rust' | 'both'
    concurrency: { type: 'string', default: '10' },
    requests:    { type: 'string', default: '200' },
    nodeUrl:     { type: 'string', default: 'http://localhost:3000' },
    rustUrl:     { type: 'string', default: 'http://localhost:3001' },
    output:      { type: 'string', default: resolve(__dirname, 'results') },
  },
  strict: false,
})

const CONCURRENCY = parseInt(args.concurrency, 10)
const TOTAL_REQUESTS = parseInt(args.requests, 10)
const TARGETS = {
  node: args.nodeUrl,
  rust: args.rustUrl,
}

// ─── Scenarios ────────────────────────────────────────────────────────────────

const GAME_SETTINGS = JSON.stringify({
  startYear: 2010,
  endYear: 2023,
  metaTags: [''],
  topNSubjects: 500,
  commonTags: true,
  subjectTagNum: 6,
  characterTagNum: 6,
  mainCharacterOnly: true,
  characterNum: 6,
})

const SCENARIOS = [
  {
    name: 'health_check',
    method: 'GET',
    path: '/health',
    body: null,
  },
  {
    name: 'game_random_character',
    method: 'POST',
    path: '/api/game/random',
    body: GAME_SETTINGS,
    contentType: 'application/json',
  },
  {
    name: 'room_count',
    method: 'GET',
    path: '/room-count',
    body: null,
  },
  {
    name: 'leaderboard',
    method: 'GET',
    path: '/api/leaderboard',
    body: null,
  },
  {
    name: 'roulette',
    method: 'GET',
    path: '/api/roulette',
    body: null,
  },
]

// ─── HTTP benchmark runner (no external deps needed) ─────────────────────────

function benchmarkScenario(baseUrl, scenario, concurrency, totalRequests) {
  return new Promise((resolve) => {
    const url = new URL(scenario.path, baseUrl)
    const isHttps = url.protocol === 'https:'
    const lib = isHttps ? https : http
    // Use a dedicated agent per scenario run to avoid socket pool limits
    const agent = new lib.Agent({ keepAlive: true, maxSockets: concurrency + 5 })

    const options = {
      hostname: url.hostname,
      port: url.port || (isHttps ? 443 : 80),
      path: url.pathname,
      method: scenario.method,
      agent,
      headers: scenario.contentType
        ? { 'Content-Type': scenario.contentType, 'Content-Length': Buffer.byteLength(scenario.body || '') }
        : {},
    }

    let completed = 0
    let errors = 0
    let totalMs = 0
    const latencies = []
    const startTime = Date.now()
    let inFlight = 0

    function makeRequest() {
      if (completed + inFlight >= totalRequests) return
      inFlight++

      const reqStart = performance.now()
      const req = lib.request(options, (res) => {
        let data = ''
        res.on('data', chunk => data += chunk)
        res.on('end', () => {
          const ms = performance.now() - reqStart
          totalMs += ms
          latencies.push(ms)
          inFlight--
          completed++

          if (res.statusCode >= 400) errors++
          if (completed >= totalRequests) finish()
          else makeRequest()
        })
      })

      req.on('error', () => {
        inFlight--
        completed++
        errors++
        if (completed >= totalRequests) finish()
        else makeRequest()
      })

      if (scenario.body) req.write(scenario.body)
      req.end()
    }

    function finish() {
      const elapsed = Date.now() - startTime
      latencies.sort((a, b) => a - b)
      const p50 = latencies[Math.floor(latencies.length * 0.5)] ?? 0
      const p95 = latencies[Math.floor(latencies.length * 0.95)] ?? 0
      const p99 = latencies[Math.floor(latencies.length * 0.99)] ?? 0
      resolve({
        scenario: scenario.name,
        requests: completed,
        errors,
        durationMs: elapsed,
        rps: Math.round((completed / elapsed) * 1000),
        avgMs: +(totalMs / completed).toFixed(1),
        p50Ms: +p50.toFixed(1),
        p95Ms: +p95.toFixed(1),
        p99Ms: +p99.toFixed(1),
      })
    }

    // Start concurrent workers
    for (let i = 0; i < Math.min(concurrency, totalRequests); i++) {
      makeRequest()
    }
  })
}

// ─── Main ─────────────────────────────────────────────────────────────────────

async function runBenchmarks(targetName, baseUrl) {
  console.log(`\n${'='.repeat(60)}`)
  console.log(`Target: ${targetName.toUpperCase()} — ${baseUrl}`)
  console.log(`Concurrency: ${CONCURRENCY}, Requests/scenario: ${TOTAL_REQUESTS}`)
  console.log('='.repeat(60))

  // Verify target is up
  try {
    const check = await benchmarkScenario(baseUrl, SCENARIOS[0], 1, 1)
    if (check.errors > 0) {
      throw new Error(`Target returned error (Status >= 400 or Network Error)`)
    }
    console.log('✅ Target is reachable')
  } catch (e) {
    console.error(`❌ Target unreachable: ${e.message}`)
    return null
  }

  const results = []
  for (const scenario of SCENARIOS) {
    process.stdout.write(`  Running: ${scenario.name}... `)
    const result = await benchmarkScenario(baseUrl, scenario, CONCURRENCY, TOTAL_REQUESTS)
    results.push(result)
    console.log(`avg=${result.avgMs}ms p95=${result.p95Ms}ms rps=${result.rps} errors=${result.errors}`)
  }

  return results
}

function printComparisonTable(nodeResults, rustResults) {
  console.log('\n' + '='.repeat(80))
  console.log('COMPARISON REPORT: Node.js vs Rust')
  console.log('='.repeat(80))

  const header = ['Scenario', 'Node avg', 'Rust avg', 'Δ avg', 'Node p95', 'Rust p95', 'Δ p95', 'Node rps', 'Rust rps', 'Δ rps']
  const rows = []

  for (let i = 0; i < SCENARIOS.length; i++) {
    const n = nodeResults?.[i]
    const r = rustResults?.[i]
    if (!n || !r) continue
    const deltaAvg = ((n.avgMs - r.avgMs) / n.avgMs * 100).toFixed(0)
    const deltaP95 = ((n.p95Ms - r.p95Ms) / n.p95Ms * 100).toFixed(0)
    const deltaRps = ((r.rps - n.rps) / n.rps * 100).toFixed(0)
    rows.push([
      n.scenario,
      `${n.avgMs}ms`, `${r.avgMs}ms`, `${deltaAvg}%`,
      `${n.p95Ms}ms`, `${r.p95Ms}ms`, `${deltaP95}%`,
      `${n.rps}`, `${r.rps}`, `+${deltaRps}%`,
    ])
  }

  // Print as ASCII table
  const colWidths = header.map((h, i) => Math.max(h.length, ...rows.map(r => String(r[i]).length)))
  const sep = '+' + colWidths.map(w => '-'.repeat(w + 2)).join('+') + '+'
  const fmt = (row) => '|' + row.map((c, i) => ` ${String(c).padEnd(colWidths[i])} `).join('|') + '|'
  console.log(sep)
  console.log(fmt(header))
  console.log(sep)
  rows.forEach(r => console.log(fmt(r)))
  console.log(sep)
}

async function main() {
  mkdirSync(args.output, { recursive: true })

  let nodeResults = null
  let rustResults = null

  if (args.target === 'node' || args.target === 'both') {
    nodeResults = await runBenchmarks('Node.js', TARGETS.node)
    if (nodeResults) {
      writeFileSync(`${args.output}/node_${Date.now()}.json`, JSON.stringify(nodeResults, null, 2))
    }
  }

  if (args.target === 'rust' || args.target === 'both') {
    rustResults = await runBenchmarks('Rust', TARGETS.rust)
    if (rustResults) {
      writeFileSync(`${args.output}/rust_${Date.now()}.json`, JSON.stringify(rustResults, null, 2))
    }
  }

  if (nodeResults && rustResults) {
    printComparisonTable(nodeResults, rustResults)
  } else if (nodeResults) {
    console.table(nodeResults)
  } else if (rustResults) {
    console.table(rustResults)
  }

  console.log(`\nResults saved to: ${args.output}/`)
}

main().catch(console.error)
