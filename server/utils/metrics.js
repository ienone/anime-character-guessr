/**
 * Request metrics middleware for the Node.js server.
 *
 * Usage: app.use(metricsMiddleware);
 * Exposes: GET /metrics  (Prometheus text format)
 * Also: GET /metrics/json  (JSON snapshot for easier scripting)
 *
 * Tracks:
 *  - Per-request: method, path, statusCode, durationMs
 *  - Slow request log (>500ms) at WARN level
 *  - Memory + CPU sampled every 30s
 *  - Latency histogram buckets: 10, 50, 100, 500, 1000, Inf ms
 */

import process from 'process'

// ─── State ───────────────────────────────────────────────────────────────────

const state = {
  requestsTotal: 0,
  requestsSlow: 0,
  requestsError: 0,
  latencySumMs: 0,
  histogram: { 10: 0, 50: 0, 100: 0, 500: 0, 1000: 0, Inf: 0 },
  // Per-route breakdown (top-level path, not full URL with IDs)
  routes: new Map(), // path -> { count, totalMs, errors }
  // Memory samples (ring buffer, last 60)
  memorySamples: [],
  cpuSamples: [],
  // Uptime
  startTime: Date.now(),
}

let prevCpuUsage = process.cpuUsage()
let prevSampleTime = Date.now()

// ─── Middleware ───────────────────────────────────────────────────────────────

export function metricsMiddleware(req, res, next) {
  const start = process.hrtime.bigint()

  res.on('finish', () => {
    const durationMs = Number(process.hrtime.bigint() - start) / 1_000_000

    state.requestsTotal++
    state.latencySumMs += durationMs

    // Histogram
    if      (durationMs <= 10)   state.histogram[10]++
    else if (durationMs <= 50)   state.histogram[50]++
    else if (durationMs <= 100)  state.histogram[100]++
    else if (durationMs <= 500)  state.histogram[500]++
    else if (durationMs <= 1000) state.histogram[1000]++
    else                         state.histogram['Inf']++

    if (res.statusCode >= 500) state.requestsError++

    // Slow request warning
    if (durationMs > 500) {
      state.requestsSlow++
      console.warn(`[SLOW] ${req.method} ${req.path} ${res.statusCode} ${durationMs.toFixed(1)}ms`)
    } else {
      console.log(`[HTTP] ${req.method} ${req.path} ${res.statusCode} ${durationMs.toFixed(1)}ms`)
    }

    // Per-route aggregation: normalize path (replace IDs with :id)
    const normalizedPath = normalizePath(req.path)
    const routeKey = `${req.method} ${normalizedPath}`
    const entry = state.routes.get(routeKey) || { count: 0, totalMs: 0, errors: 0 }
    entry.count++
    entry.totalMs += durationMs
    if (res.statusCode >= 500) entry.errors++
    state.routes.set(routeKey, entry)
  })

  next()
}

// ─── Background sampler ───────────────────────────────────────────────────────

function sampleResources() {
  const mem = process.memoryUsage()
  const now = Date.now()
  const elapsed = (now - prevSampleTime) / 1000

  const cpuUsage = process.cpuUsage(prevCpuUsage)
  prevCpuUsage = process.cpuUsage()
  prevSampleTime = now

  const cpuPercent = elapsed > 0
    ? ((cpuUsage.user + cpuUsage.system) / 1e6 / elapsed * 100)
    : 0

  const sample = {
    ts: now,
    rssKb: Math.round(mem.rss / 1024),
    heapUsedKb: Math.round(mem.heapUsed / 1024),
    heapTotalKb: Math.round(mem.heapTotal / 1024),
    externalKb: Math.round(mem.external / 1024),
    cpuPercent: +cpuPercent.toFixed(2),
  }

  state.memorySamples.push(sample)
  state.cpuSamples.push({ ts: now, cpu: sample.cpuPercent })

  // Keep last 60 samples (~30 minutes with 30s interval)
  if (state.memorySamples.length > 60) state.memorySamples.shift()
  if (state.cpuSamples.length > 60) state.cpuSamples.shift()
}

setInterval(sampleResources, 30_000)
sampleResources() // initial sample

// ─── /metrics Prometheus endpoint ────────────────────────────────────────────

export function metricsPrometheusHandler(req, res) {
  const mem = state.memorySamples[state.memorySamples.length - 1] || {}
  const cpu = state.cpuSamples[state.cpuSamples.length - 1] || {}
  const total = state.requestsTotal || 1
  const avgMs = state.latencySumMs / total

  // Cumulative buckets (Prometheus convention)
  const h = state.histogram
  const b10   = h[10]
  const b50   = b10 + h[50]
  const b100  = b50 + h[100]
  const b500  = b100 + h[500]
  const b1000 = b500 + h[1000]
  const binf  = b1000 + h['Inf']

  let body = `# HELP http_requests_total Total HTTP requests
# TYPE http_requests_total counter
http_requests_total ${state.requestsTotal}

# HELP http_requests_slow Requests >500ms
# TYPE http_requests_slow counter
http_requests_slow ${state.requestsSlow}

# HELP http_requests_errors 5xx responses
# TYPE http_requests_errors counter
http_requests_errors ${state.requestsError}

# HELP http_latency_avg_ms Average response latency (ms)
# TYPE http_latency_avg_ms gauge
http_latency_avg_ms ${avgMs.toFixed(2)}

# HELP http_latency_sum_ms Cumulative latency sum (ms)
# TYPE http_latency_sum_ms counter
http_latency_sum_ms ${state.latencySumMs.toFixed(2)}

# HELP http_request_duration_ms_bucket Latency histogram buckets
# TYPE http_request_duration_ms_bucket histogram
http_request_duration_ms_bucket{le="10"} ${b10}
http_request_duration_ms_bucket{le="50"} ${b50}
http_request_duration_ms_bucket{le="100"} ${b100}
http_request_duration_ms_bucket{le="500"} ${b500}
http_request_duration_ms_bucket{le="1000"} ${b1000}
http_request_duration_ms_bucket{le="+Inf"} ${binf}

# HELP process_rss_kb Resident set size (KB)
# TYPE process_rss_kb gauge
process_rss_kb ${mem.rssKb || 0}

# HELP process_heap_used_kb Heap used (KB)
# TYPE process_heap_used_kb gauge
process_heap_used_kb ${mem.heapUsedKb || 0}

# HELP process_cpu_percent CPU usage percent (30s sample)
# TYPE process_cpu_percent gauge
process_cpu_percent ${cpu.cpu || 0}

# HELP process_uptime_seconds Process uptime
# TYPE process_uptime_seconds counter
process_uptime_seconds ${Math.round((Date.now() - state.startTime) / 1000)}
`

  // Per-route metrics
  for (const [route, entry] of state.routes) {
    const safe = route.replace(/[^a-zA-Z0-9_\s]/g, '_')
    body += `http_route_requests_total{route="${route}"} ${entry.count}\n`
    body += `http_route_avg_ms{route="${route}"} ${(entry.totalMs / entry.count).toFixed(2)}\n`
  }

  res.set('Content-Type', 'text/plain; version=0.0.4; charset=utf-8')
  res.send(body)
}

// ─── /metrics/json endpoint ───────────────────────────────────────────────────

export function metricsJsonHandler(req, res) {
  const total = state.requestsTotal || 1
  const routes = {}
  for (const [route, entry] of state.routes) {
    routes[route] = {
      count: entry.count,
      avgMs: +(entry.totalMs / entry.count).toFixed(2),
      errors: entry.errors,
    }
  }
  res.json({
    uptime_s: Math.round((Date.now() - state.startTime) / 1000),
    requests: {
      total: state.requestsTotal,
      slow: state.requestsSlow,
      errors: state.requestsError,
    },
    latency: {
      avg_ms: +(state.latencySumMs / total).toFixed(2),
      sum_ms: +state.latencySumMs.toFixed(2),
      histogram: state.histogram,
    },
    routes,
    memory: state.memorySamples.slice(-5),
    cpu: state.cpuSamples.slice(-5),
  })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/** Replace numeric path segments with :id for route aggregation */
function normalizePath(path) {
  return path.replace(/\/\d+/g, '/:id').replace(/\/[0-9a-f]{24}/gi, '/:id')
}

export default metricsMiddleware
