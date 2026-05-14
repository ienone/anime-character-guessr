#!/usr/bin/env node
/**
 * Stress probes for backend stalls.
 *
 * The default scenarios stay on local SQLite/Tantivy paths. Run image-source
 * separately when you intentionally want to measure BGM fallback behavior.
 */

import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { performance } from 'node:perf_hooks'
import { parseArgs } from 'node:util'

const __dirname = dirname(fileURLToPath(import.meta.url))

const { values: args } = parseArgs({
  options: {
    url: { type: 'string', default: 'http://localhost:3001' },
    scenario: { type: 'string', default: 'mixed' },
    concurrency: { type: 'string', default: '50' },
    requests: { type: 'string', default: '1000' },
    timeoutMs: { type: 'string', default: '8000' },
    output: { type: 'string', default: resolve(__dirname, 'results') },
    ids: { type: 'string', default: '1,2,3,4,5,6,7,8,9,10' },
  },
  strict: false,
})

const BASE_URL = args.url.replace(/\/+$/, '')
const SCENARIO = args.scenario
const CONCURRENCY = Math.max(1, parseInt(args.concurrency, 10))
const REQUESTS = Math.max(1, parseInt(args.requests, 10))
const TIMEOUT_MS = Math.max(100, parseInt(args.timeoutMs, 10))
const IDS = args.ids
  .split(',')
  .map(s => parseInt(s.trim(), 10))
  .filter(Number.isFinite)

const GAME_SETTINGS = {
  startYear: 2010,
  endYear: 2023,
  metaTags: [''],
  topNSubjects: 500,
  commonTags: true,
  subjectTagNum: 6,
  characterTagNum: 6,
  mainCharacterOnly: true,
  characterNum: 6,
}

const SEARCH_TERMS = ['鲁路修', 'saber', '春日', '利兹', '京吹部', 'miku', 'asuka', 'rei']

function pick(list, i) {
  return list[i % list.length]
}

function jsonRequest(path, body) {
  return {
    method: 'POST',
    path,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  }
}

function requestFor(scenario, i) {
  const id = pick(IDS.length ? IDS : [1], i)
  const term = encodeURIComponent(pick(SEARCH_TERMS, i))

  if (scenario === 'db-read') {
    const choices = [
      jsonRequest('/api/game/random', GAME_SETTINGS),
      { method: 'GET', path: `/api/archive/search/characters?keyword=${term}&limit=10` },
      { method: 'GET', path: `/api/archive/search/subjects?keyword=${term}&limit=10` },
      { method: 'GET', path: '/api/leaderboard' },
    ]
    return pick(choices, i)
  }

  if (scenario === 'app-write') {
    const userId = `stress-${i % 250}`
    const choices = [
      jsonRequest('/api/leaderboard/submit', {
        user_id: userId,
        username: userId,
        score_delta: (i % 7) - 3,
      }),
      jsonRequest('/api/answer-character-count', {
        characterId: id,
        characterName: `stress-${id}`,
      }),
      jsonRequest('/api/guess-character-count', {
        characterId: id,
        characterName: `stress-${id}`,
      }),
      { method: 'GET', path: `/api/character-usage/${id}` },
    ]
    return pick(choices, i)
  }

  if (scenario === 'image-cache') {
    return { method: 'GET', path: `/api/img/resolve/${id}?cachedOnly=true&waitMs=0` }
  }

  if (scenario === 'image-source') {
    return { method: 'GET', path: `/api/img/resolve/${id}?waitMs=0` }
  }

  const choices = [
    requestFor('db-read', i),
    requestFor('db-read', i + 1),
    requestFor('app-write', i),
    requestFor('image-cache', i),
  ]
  return pick(choices, i)
}

async function timedFetch(spec, index) {
  const controller = new AbortController()
  const timeout = setTimeout(() => controller.abort(), TIMEOUT_MS)
  const start = performance.now()

  try {
    const res = await fetch(new URL(spec.path, BASE_URL), {
      method: spec.method,
      headers: spec.headers,
      body: spec.body,
      signal: controller.signal,
    })
    await res.arrayBuffer()
    return {
      index,
      path: spec.path.split('?')[0],
      status: res.status,
      ok: res.status < 500,
      ms: performance.now() - start,
    }
  } catch (e) {
    return {
      index,
      path: spec.path.split('?')[0],
      status: 0,
      ok: false,
      error: e?.name === 'AbortError' ? 'timeout' : String(e?.message || e),
      ms: performance.now() - start,
    }
  } finally {
    clearTimeout(timeout)
  }
}

async function run() {
  const started = Date.now()
  let next = 0
  const results = []

  async function worker() {
    while (true) {
      const index = next++
      if (index >= REQUESTS) return
      results.push(await timedFetch(requestFor(SCENARIO, index), index))
    }
  }

  await Promise.all(Array.from({ length: CONCURRENCY }, worker))
  const elapsedMs = Date.now() - started
  const summary = summarize(results, elapsedMs)
  const outPath = resolve(args.output, `stress_${SCENARIO}_${started}.json`)
  printSummary(summary)
  try {
    mkdirSync(args.output, { recursive: true })
    writeFileSync(outPath, JSON.stringify({ args, summary, results }, null, 2))
    console.log(`wrote ${outPath}`)
  } catch (e) {
    console.warn(`failed to write ${outPath}: ${e?.message || e}`)
  }
}

function percentile(sorted, p) {
  if (!sorted.length) return 0
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))]
}

function summarize(results, elapsedMs) {
  const latencies = results.map(r => r.ms).sort((a, b) => a - b)
  const statuses = {}
  const byPath = {}
  let errors = 0
  let timeouts = 0

  for (const r of results) {
    statuses[r.status] = (statuses[r.status] || 0) + 1
    if (!r.ok) errors++
    if (r.error === 'timeout') timeouts++
    if (!byPath[r.path]) byPath[r.path] = { count: 0, errors: 0, maxMs: 0, totalMs: 0 }
    byPath[r.path].count++
    byPath[r.path].totalMs += r.ms
    byPath[r.path].maxMs = Math.max(byPath[r.path].maxMs, r.ms)
    if (!r.ok) byPath[r.path].errors++
  }

  for (const value of Object.values(byPath)) {
    value.avgMs = +(value.totalMs / value.count).toFixed(1)
    value.maxMs = +value.maxMs.toFixed(1)
    delete value.totalMs
  }

  return {
    scenario: SCENARIO,
    baseUrl: BASE_URL,
    requests: results.length,
    concurrency: CONCURRENCY,
    elapsedMs,
    rps: +(results.length / (elapsedMs / 1000)).toFixed(1),
    errors,
    timeouts,
    statuses,
    slowOver500ms: results.filter(r => r.ms > 500).length,
    slowOver2000ms: results.filter(r => r.ms > 2000).length,
    slowOver5000ms: results.filter(r => r.ms > 5000).length,
    avgMs: +(latencies.reduce((a, b) => a + b, 0) / Math.max(1, latencies.length)).toFixed(1),
    p50Ms: +percentile(latencies, 0.50).toFixed(1),
    p95Ms: +percentile(latencies, 0.95).toFixed(1),
    p99Ms: +percentile(latencies, 0.99).toFixed(1),
    maxMs: +(latencies.at(-1) || 0).toFixed(1),
    byPath,
  }
}

function printSummary(summary) {
  console.log(JSON.stringify(summary, null, 2))
}

run().catch(e => {
  console.error(e)
  process.exit(1)
})
