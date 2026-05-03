#!/usr/bin/env node
/**
 * Benchmark: Single-Player Game API
 *
 * Simulates concurrent players opening games and making guesses.
 * Runs against Node.js and Rust backends and outputs a comparison report.
 *
 * Usage:
 *   node benchmarks/run_bench.js --mode server-compare --target both --concurrency 20 --requests 500
 *   node benchmarks/run_bench.js --mode api-compare --rustUrl http://localhost:3001 --requests 50
 */

import { parseArgs } from 'node:util'
import { spawn } from 'node:child_process'
import http from 'node:http'
import https from 'node:https'
import { writeFileSync, mkdirSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { performance } from 'node:perf_hooks'

const __dirname = dirname(fileURLToPath(import.meta.url))

// ─── CLI args ─────────────────────────────────────────────────────────────────

const { values: args } = parseArgs({
  options: {
    mode:        { type: 'string', default: 'server-compare' }, // 'server-compare' | 'api-compare'
    target:      { type: 'string', default: 'both' },    // 'node' | 'rust' | 'both'
    concurrency: { type: 'string', default: '10' },
    requests:    { type: 'string', default: '200' },
    nodeUrl:     { type: 'string', default: 'http://localhost:3000' },
    rustUrl:     { type: 'string', default: 'http://localhost:3001' },
    output:      { type: 'string', default: resolve(__dirname, 'results') },
    sampleMs:    { type: 'string', default: '250' }, // metrics poll interval
    warmupMs:    { type: 'string', default: '500' }, // warmup before sampling
    spawn:       { type: 'boolean', default: false }, // spawn servers locally for startup-time measurement
    startupTimeoutMs: { type: 'string', default: '20000' }, // wait for /health
    nodeCmd:     { type: 'string', default: 'node server.js' },
    rustCmd:     { type: 'string', default: 'cargo run --release' },
  },
  strict: false,
})

const CONCURRENCY = parseInt(args.concurrency, 10)
const TOTAL_REQUESTS = parseInt(args.requests, 10)
const SAMPLE_MS = Math.max(100, parseInt(args.sampleMs, 10))
const WARMUP_MS = Math.max(0, parseInt(args.warmupMs, 10))
const TARGETS = {
  node: args.nodeUrl,
  rust: args.rustUrl,
}

const STARTUP_TIMEOUT_MS = Math.max(1000, parseInt(args.startupTimeoutMs, 10))

// ─── Optional server spawn (startup time) ────────────────────────────────────

async function waitForHealth(baseUrl, timeoutMs) {
  const healthUrl = new URL('/health', baseUrl).toString()
  const deadline = Date.now() + timeoutMs
  let lastErr = null
  while (Date.now() < deadline) {
    try {
      const res = await fetch(healthUrl, { method: 'GET' })
      if (res.ok) return true
      lastErr = new Error(`health_status_${res.status}`)
    } catch (e) {
      lastErr = e
    }
    await new Promise(r => setTimeout(r, 100))
  }
  throw lastErr || new Error('health_timeout')
}

function portFromUrl(u) {
  const url = new URL(u)
  if (url.port) return url.port
  return url.protocol === 'https:' ? '443' : '80'
}

async function spawnAndMeasure({ label, cmd, cwd, baseUrl }) {
  const env = { ...process.env, PORT: portFromUrl(baseUrl) }
  const t0 = performance.now()
  const child = spawn(cmd, {
    cwd,
    env,
    shell: true,
    stdio: 'inherit',
  })

  try {
    await waitForHealth(baseUrl, STARTUP_TIMEOUT_MS)
  } catch (e) {
    try { child.kill('SIGKILL') } catch {}
    throw new Error(`${label}_startup_failed: ${e?.message || e}`)
  }

  const startupMs = performance.now() - t0
  return { child, startupMs: +startupMs.toFixed(0) }
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

function parsePrometheusText(text) {
  const out = {}
  const lines = String(text).split('\n')
  for (const line of lines) {
    if (!line || line.startsWith('#')) continue
    const parts = line.trim().split(/\s+/)
    if (parts.length < 2) continue
    // drop label section: metric{...} -> metric
    const key = parts[0].split('{')[0]
    const val = Number(parts[1])
    if (!Number.isFinite(val)) continue
    out[key] = val
  }
  return out
}

async function fetchText(url) {
  const res = await fetch(url, { method: 'GET' })
  const text = await res.text()
  return { ok: res.ok, status: res.status, text }
}

async function sampleMetrics(baseUrl, stopSignal) {
  const samples = []
  const startedAt = Date.now()

  const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
  const warmupDeadline = Date.now() + WARMUP_MS
  let warmedUp = WARMUP_MS === 0

  while (true) {
    if (!warmedUp) {
      const now = Date.now()
      const remaining = warmupDeadline - now
      if (remaining > 0 && !stopSignal.stopped) {
        await sleep(Math.min(SAMPLE_MS, remaining))
        continue
      }
      // If the benchmark finished before warmup, still take at least one sample.
      warmedUp = true
    }

    try {
      const { ok, status, text } = await fetchText(new URL('/metrics', baseUrl).toString())
      if (ok) {
        const m = parsePrometheusText(text)
        samples.push({
          t: Date.now() - startedAt,
          process_rss_kb: m.process_rss_kb ?? null,
          cpu_usage_percent: m.cpu_usage_percent ?? m.process_cpu_percent ?? null,
        })
      } else {
        samples.push({ t: Date.now() - startedAt, error: `metrics_status_${status}` })
      }
    } catch (e) {
      samples.push({ t: Date.now() - startedAt, error: e?.message || 'metrics_error' })
    }

    if (stopSignal.stopped) break
    await sleep(SAMPLE_MS)
  }
  return samples
}

function summarizeSamples(samples, field) {
  const vals = samples
    .map(s => s[field])
    .filter(v => typeof v === 'number' && Number.isFinite(v))
  if (vals.length === 0) return { avg: null, max: null }
  const sum = vals.reduce((a, b) => a + b, 0)
  return { avg: +(sum / vals.length).toFixed(2), max: +Math.max(...vals).toFixed(2) }
}

function cleanSamples(samples) {
  return samples.filter(s => s && (typeof s.process_rss_kb === 'number' || typeof s.cpu_usage_percent === 'number'))
}

// ─── Main ─────────────────────────────────────────────────────────────────────

async function runBenchmarks(targetName, baseUrl, startupMs = null) {
  console.log(`\n${'='.repeat(60)}`)
  console.log(`Target: ${targetName.toUpperCase()} — ${baseUrl}`)
  console.log(`Concurrency: ${CONCURRENCY}, Requests/scenario: ${TOTAL_REQUESTS}`)
  if (startupMs != null) {
    console.log(`Startup time: ${startupMs}ms`)
  }
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
    const stopSignal = { stopped: false }
    const metricsTask = sampleMetrics(baseUrl, stopSignal)
    const benchTask = benchmarkScenario(baseUrl, scenario, CONCURRENCY, TOTAL_REQUESTS)
    const result = await benchTask
    stopSignal.stopped = true
    const samples = cleanSamples(await metricsTask)
    const rss = summarizeSamples(samples, 'process_rss_kb')
    const cpu = summarizeSamples(samples, 'cpu_usage_percent')
    result.metrics = {
      rssMbAvg: rss.avg == null ? null : +(rss.avg / 1024).toFixed(2),
      rssMbMax: rss.max == null ? null : +(rss.max / 1024).toFixed(2),
      cpuAvg: cpu.avg,
      cpuMax: cpu.max,
      samples,
    }
    result.startupMs = startupMs
    // backwards compat: some older JSON consumers still treat this as "MB"
    if (result.metrics.rssMbAvg != null && result.metrics.rssMbAvg > 1024) {
      result.metrics.rssKbAvg = rss.avg
      result.metrics.rssKbMax = rss.max
      result.metrics.rssMbAvg = +(result.metrics.rssMbAvg / 1024).toFixed(2)
      result.metrics.rssMbMax = +(result.metrics.rssMbMax / 1024).toFixed(2)
    }
    results.push(result)
    const cpuTxt = result.metrics.cpuAvg == null ? 'cpu=?' : `cpu(avg/max)=${result.metrics.cpuAvg}/${result.metrics.cpuMax}%`
    const memTxt = result.metrics.rssMbAvg == null ? 'rss=?' : `rss(avg/max)=${result.metrics.rssMbAvg}/${result.metrics.rssMbMax}MB`
    console.log(`avg=${result.avgMs}ms p95=${result.p95Ms}ms rps=${result.rps} errors=${result.errors} ${cpuTxt} ${memTxt}`)
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

function toMarkdownServerCompare({ nodeResults, rustResults, out }) {
  const at = out?.at || new Date().toISOString()
  const lines = []
  lines.push(`# Server Compare`)
  lines.push('')
  lines.push(`- at: \`${at}\``)
  lines.push(`- concurrency: \`${out?.args?.concurrency}\``)
  lines.push(`- requests/scenario: \`${out?.args?.requests}\``)
  lines.push(`- nodeUrl: \`${out?.args?.nodeUrl}\``)
  lines.push(`- rustUrl: \`${out?.args?.rustUrl}\``)
  if (out?.startup) {
    lines.push(`- nodeStartupMs: \`${out.startup.nodeStartupMs ?? 'n/a'}\``)
    lines.push(`- rustStartupMs: \`${out.startup.rustStartupMs ?? 'n/a'}\``)
  }
  lines.push('')

  const hasBoth = Array.isArray(nodeResults) && Array.isArray(rustResults)
  if (hasBoth) {
    lines.push(`| Scenario | Node avg | Node p95 | Node p99 | Node rps | Node rss(avg/max MB) | Node cpu(avg/max %) | Rust avg | Rust p95 | Rust p99 | Rust rps | Rust rss(avg/max MB) | Rust cpu(avg/max %) |`)
    lines.push(`|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|`)
    for (let i = 0; i < SCENARIOS.length; i++) {
      const n = nodeResults[i]
      const r = rustResults[i]
      if (!n || !r) continue
      const nm = n.metrics || {}
      const rm = r.metrics || {}
      lines.push([
        `\`${n.scenario}\``,
        `${n.avgMs}ms`,
        `${n.p95Ms}ms`,
        `${n.p99Ms}ms`,
        `${n.rps}`,
        nm.rssMbAvg == null ? '' : `${nm.rssMbAvg}/${nm.rssMbMax}`,
        nm.cpuAvg == null ? '' : `${nm.cpuAvg}/${nm.cpuMax}`,
        `${r.avgMs}ms`,
        `${r.p95Ms}ms`,
        `${r.p99Ms}ms`,
        `${r.rps}`,
        rm.rssMbAvg == null ? '' : `${rm.rssMbAvg}/${rm.rssMbMax}`,
        rm.cpuAvg == null ? '' : `${rm.cpuAvg}/${rm.cpuMax}`,
      ].join(' | ') + ' |')
    }
    lines.push('')
    lines.push(`> 备注：RSS/CPU 来自压测期间轮询 \`/metrics\` 的时间序列采样（每个场景单独采样）。`)
  } else {
    const single = Array.isArray(nodeResults) ? { label: 'Node', res: nodeResults }
      : Array.isArray(rustResults) ? { label: 'Rust', res: rustResults }
      : null
    if (single) {
      lines.push(`| Scenario | avg | p95 | p99 | rps | errors | rss(avg/max MB) | cpu(avg/max %) |`)
      lines.push(`|---|---:|---:|---:|---:|---:|---:|---:|`)
      for (const s of single.res) {
        const m = s.metrics || {}
        lines.push([
          `\`${s.scenario}\``,
          `${s.avgMs}ms`,
          `${s.p95Ms}ms`,
          `${s.p99Ms}ms`,
          `${s.rps}`,
          `${s.errors}`,
          m.rssMbAvg == null ? '' : `${m.rssMbAvg}/${m.rssMbMax}`,
          m.cpuAvg == null ? '' : `${m.cpuAvg}/${m.cpuMax}`,
        ].join(' | ') + ' |')
      }
    }
  }
  lines.push('')
  return lines.join('\n')
}

function toMarkdownApiCompare(out) {
  const at = out?.at || new Date().toISOString()
  const rows = Array.isArray(out?.results) ? out.results : []
  const lines = []
  lines.push(`# API Compare`)
  lines.push('')
  lines.push(`- at: \`${at}\``)
  lines.push(`- bgmBase: \`${out?.bgmBase}\``)
  lines.push(`- localBase: \`${out?.localBase}\``)
  lines.push('')
  lines.push(`| Mode | Runs | OK | Errors | Requests | Avg | P95 |`)
  lines.push(`|---|---:|---:|---:|---:|---:|---:|`)
  for (const r of rows) {
    lines.push([
      `\`${r.mode}\``,
      `${r.runs}`,
      `${r.ok}`,
      `${r.errors}`,
      `${r.requests}`,
      `${r.avgMs}ms`,
      `${r.p95Ms}ms`,
    ].join(' | ') + ' |')
  }
  lines.push('')
  return lines.join('\n')
}

async function runApiCompare() {
  // Dimension B: old path (direct api.bgm.tv) vs new path (POST /api/game/random)
  // We model the old path as a representative chain:
  // 1) POST /v0/search/subjects (1 request)
  // 2) GET /v0/subjects/:id/characters (1 request)
  // 3) GET /v0/characters/:id (1 request)
  // 4) GET /v0/characters/:id/subjects (1 request)
  //
  // This is not an exact replica of the full frontend flow, but it captures the key
  // issue: multiple sequential BGM calls vs one local call.
  const bgmBase = 'https://api.bgm.tv'
  const localBase = args.rustUrl

  const chainCount = Math.max(10, Math.min(200, TOTAL_REQUESTS))
  const results = []

  async function oldChainOnce() {
    const t0 = performance.now()
    let requests = 0

    const searchRes = await fetch(`${bgmBase}/v0/search/subjects?limit=10&offset=0`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        sort: 'heat',
        filter: { type: [2, 4], air_date: ['>=2010-01-01', '<2024-01-01'], meta_tags: [] },
      }),
    })
    requests++
    if (!searchRes.ok) throw new Error(`bgm_search_${searchRes.status}`)
    const searchJson = await searchRes.json()
    const subjectId = searchJson?.data?.[0]?.id
    if (!subjectId) throw new Error('bgm_search_empty')

    const charsRes = await fetch(`${bgmBase}/v0/subjects/${subjectId}/characters`, { method: 'GET' })
    requests++
    if (!charsRes.ok) throw new Error(`bgm_subject_chars_${charsRes.status}`)
    const charsJson = await charsRes.json()
    const charId = charsJson?.[0]?.id
    if (!charId) throw new Error('bgm_chars_empty')

    const charRes = await fetch(`${bgmBase}/v0/characters/${charId}`, { method: 'GET' })
    requests++
    if (!charRes.ok) throw new Error(`bgm_char_${charRes.status}`)
    await charRes.json()

    const charSubRes = await fetch(`${bgmBase}/v0/characters/${charId}/subjects`, { method: 'GET' })
    requests++
    if (!charSubRes.ok) throw new Error(`bgm_char_subjects_${charSubRes.status}`)
    await charSubRes.json()

    const ms = performance.now() - t0
    return { ms, requests }
  }

  async function newChainOnce() {
    const t0 = performance.now()
    const res = await fetch(`${localBase}/api/game/random`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: GAME_SETTINGS,
    })
    if (!res.ok) throw new Error(`local_random_${res.status}`)
    await res.json()
    const ms = performance.now() - t0
    return { ms, requests: 1 }
  }

  async function runMany(label, fn) {
    const lat = []
    let errors = 0
    let totalReq = 0
    for (let i = 0; i < chainCount; i++) {
      try {
        const r = await fn()
        lat.push(r.ms)
        totalReq += r.requests
      } catch {
        errors++
      }
    }
    lat.sort((a, b) => a - b)
    const p95 = lat[Math.floor(lat.length * 0.95)] ?? 0
    const avg = lat.length ? lat.reduce((a, b) => a + b, 0) / lat.length : 0
    return {
      mode: label,
      runs: chainCount,
      ok: lat.length,
      errors,
      requests: totalReq,
      avgMs: +avg.toFixed(1),
      p95Ms: +p95.toFixed(1),
    }
  }

  console.log(`\n${'='.repeat(80)}`)
  console.log('API COMPARE: direct api.bgm.tv chain vs local POST /api/game/random')
  console.log('='.repeat(80))
  console.log(`Runs: ${chainCount} (sequential)`)

  const oldRes = await runMany('bgm_direct_chain', oldChainOnce)
  const newRes = await runMany('local_random_onecall', newChainOnce)

  results.push(oldRes, newRes)

  console.table(results)

  const out = {
    mode: 'api-compare',
    at: new Date().toISOString(),
    bgmBase,
    localBase,
    results,
  }
  return out
}

async function main() {
  mkdirSync(args.output, { recursive: true })

  const spawned = []
  const startup = {}
  const stopSpawned = async () => {
    while (spawned.length) {
      const child = spawned.pop()
      try {
        child.kill('SIGKILL')
      } catch {}
    }
  }

  try {
    if (args.spawn) {
      const rootDir = resolve(__dirname, '..')
      if (args.target === 'node' || args.target === 'both') {
        const nodeSpawn = await spawnAndMeasure({
          label: 'node',
          cmd: args.nodeCmd,
          cwd: resolve(rootDir, 'server'),
          baseUrl: TARGETS.node,
        })
        spawned.push(nodeSpawn.child)
        startup.nodeStartupMs = nodeSpawn.startupMs
      }
      if (args.target === 'rust' || args.target === 'both' || args.mode === 'api-compare') {
        const rustSpawn = await spawnAndMeasure({
          label: 'rust',
          cmd: args.rustCmd,
          cwd: resolve(rootDir, 'server-rs'),
          baseUrl: TARGETS.rust,
        })
        spawned.push(rustSpawn.child)
        startup.rustStartupMs = rustSpawn.startupMs
      }
    }

    if (args.mode === 'api-compare') {
      const apiOut = await runApiCompare()
      if (apiOut) {
        const ts = Date.now()
        writeFileSync(`${args.output}/api_compare_${ts}.json`, JSON.stringify(apiOut, null, 2))
        writeFileSync(`${args.output}/api_compare_${ts}.md`, toMarkdownApiCompare(apiOut))
      }
      console.log(`\nResults saved to: ${args.output}/`)
      return
    }

    let nodeResults = null
    let rustResults = null

    if (args.target === 'node' || args.target === 'both') {
      nodeResults = await runBenchmarks('Node.js', TARGETS.node, startup.nodeStartupMs ?? null)
      if (nodeResults) {
        writeFileSync(`${args.output}/node_${Date.now()}.json`, JSON.stringify(nodeResults, null, 2))
      }
    }

    if (args.target === 'rust' || args.target === 'both') {
      rustResults = await runBenchmarks('Rust', TARGETS.rust, startup.rustStartupMs ?? null)
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

    const out = {
      mode: 'server-compare',
      at: new Date().toISOString(),
      args: {
        concurrency: CONCURRENCY,
        requests: TOTAL_REQUESTS,
        sampleMs: SAMPLE_MS,
        warmupMs: WARMUP_MS,
        nodeUrl: TARGETS.node,
        rustUrl: TARGETS.rust,
      },
      startup,
      nodeResults,
      rustResults,
    }
    const ts = Date.now()
    writeFileSync(`${args.output}/server_compare_${ts}.json`, JSON.stringify(out, null, 2))
    writeFileSync(`${args.output}/server_compare_${ts}.md`, toMarkdownServerCompare({ nodeResults, rustResults, out }))

    console.log(`\nResults saved to: ${args.output}/`)
  } finally {
    await stopSpawned()
  }
}

main().catch(console.error)
