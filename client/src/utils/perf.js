/**
 * Frontend Performance Monitor
 *
 * Wraps axios to record per-request latency, groups by endpoint pattern,
 * and optionally reports to the backend /api/telemetry endpoint.
 *
 * Usage:
 *   import perfAxios from './perf'
 *   const data = await perfAxios.get('/api/game/random')
 *
 * Access stats: perf.getReport()
 */

import axios from 'axios'

// ─── State ───────────────────────────────────────────────────────────────────

const stats = {
  requests: [],          // { url, method, durationMs, status, ts }
  routeStats: new Map(), // normalized route -> { count, totalMs, errors, p95 }
  totalCount: 0,
  totalMs: 0,
  errorCount: 0,
}

// Web Vitals (LCP, FID, CLS) if PerformanceObserver available
const vitals = { lcp: null, fid: null, cls: 0.0 }

function initWebVitals() {
  if (typeof PerformanceObserver === 'undefined') return
  try {
    // LCP
    new PerformanceObserver(list => {
      const entries = list.getEntries()
      if (entries.length > 0) vitals.lcp = entries[entries.length - 1].startTime
    }).observe({ type: 'largest-contentful-paint', buffered: true })

    // FID
    new PerformanceObserver(list => {
      for (const entry of list.getEntries()) {
        if (entry.processingStart) vitals.fid = entry.processingStart - entry.startTime
      }
    }).observe({ type: 'first-input', buffered: true })

    // CLS
    new PerformanceObserver(list => {
      for (const entry of list.getEntries()) {
        if (!entry.hadRecentInput) vitals.cls += entry.value
      }
    }).observe({ type: 'layout-shift', buffered: true })
  } catch (_) {
    // PerformanceObserver not supported for these entry types
  }
}

initWebVitals()

// ─── Axios interceptors ───────────────────────────────────────────────────────

/**
 * Creates a tracked axios instance.
 * @param {import('axios').AxiosInstance} axiosInstance
 */
function installTracking(axiosInstance) {
  axiosInstance.interceptors.request.use(config => {
    config.metadata = { startTime: performance.now() }
    return config
  })

  axiosInstance.interceptors.response.use(
    response => {
      recordRequest(response.config, response.status, false)
      return response
    },
    error => {
      if (error.config) {
        recordRequest(error.config, error.response?.status ?? 0, true)
      }
      return Promise.reject(error)
    }
  )
}

function recordRequest(config, status, isError) {
  const durationMs = config.metadata
    ? performance.now() - config.metadata.startTime
    : 0

  const url = (config.url || '').replace(/\d{6,}/g, ':id').replace(/[?#].*/, '')
  const method = (config.method || 'GET').toUpperCase()

  stats.totalCount++
  stats.totalMs += durationMs
  if (isError || status >= 400) stats.errorCount++

  // Keep last 200 raw requests
  stats.requests.push({ url, method, durationMs, status, ts: Date.now() })
  if (stats.requests.length > 200) stats.requests.shift()

  // Per-route aggregation
  const key = `${method} ${url}`
  const entry = stats.routeStats.get(key) || { count: 0, totalMs: 0, errors: 0, samples: [] }
  entry.count++
  entry.totalMs += durationMs
  if (isError || status >= 400) entry.errors++
  entry.samples.push(durationMs)
  if (entry.samples.length > 100) entry.samples.shift()
  stats.routeStats.set(key, entry)

  // Slow request warning
  if (durationMs > 2000) {
    console.warn(`[PERF SLOW] ${method} ${url} ${durationMs.toFixed(0)}ms (status=${status})`)
  }
}

// ─── Tracked axios instance ───────────────────────────────────────────────────

const perfAxios = axios.create()
installTracking(perfAxios)

// ─── Report API ───────────────────────────────────────────────────────────────

/**
 * Get the current performance report.
 */
function getReport() {
  const routes = {}
  for (const [key, entry] of stats.routeStats) {
    const sorted = [...entry.samples].sort((a, b) => a - b)
    const p50 = sorted[Math.floor(sorted.length * 0.5)] ?? 0
    const p95 = sorted[Math.floor(sorted.length * 0.95)] ?? 0
    routes[key] = {
      count: entry.count,
      avgMs: +(entry.totalMs / entry.count).toFixed(1),
      p50Ms: +p50.toFixed(1),
      p95Ms: +p95.toFixed(1),
      errors: entry.errors,
    }
  }
  return {
    summary: {
      totalRequests: stats.totalCount,
      totalErrors: stats.errorCount,
      avgMs: stats.totalCount > 0 ? +(stats.totalMs / stats.totalCount).toFixed(1) : 0,
    },
    routes,
    vitals: { ...vitals },
    recentRequests: stats.requests.slice(-20),
  }
}

/**
 * Reset all stats (e.g. between benchmark runs).
 */
function reset() {
  stats.requests = []
  stats.routeStats.clear()
  stats.totalCount = 0
  stats.totalMs = 0
  stats.errorCount = 0
}

/**
 * Print a human-readable summary to console.
 */
function printReport() {
  const report = getReport()
  console.group('[PerfMonitor] Request Summary')
  console.table(Object.entries(report.routes).map(([route, r]) => ({
    Route: route,
    Count: r.count,
    'Avg (ms)': r.avgMs,
    'P50 (ms)': r.p50Ms,
    'P95 (ms)': r.p95Ms,
    Errors: r.errors,
  })))
  console.log('Web Vitals:', report.vitals)
  console.groupEnd()
}

export const perf = { getReport, reset, printReport, installTracking }
export default perfAxios
