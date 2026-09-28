#!/usr/bin/env node
/**
 * Socket.IO multiplayer stress probe.
 *
 * Uses the client package's installed socket.io-client dependency so the
 * benchmark package stays dependency-free.
 */

import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { randomUUID } from 'node:crypto'
import { performance } from 'node:perf_hooks'
import { parseArgs } from 'node:util'
import { io } from '../client/node_modules/socket.io-client/build/esm/index.js'

const __dirname = dirname(fileURLToPath(import.meta.url))

const { values: args } = parseArgs({
  options: {
    url: { type: 'string', default: 'http://localhost:3001' },
    rooms: { type: 'string', default: '10' },
    players: { type: 'string', default: '4' },
    guesses: { type: 'string', default: '40' },
    output: { type: 'string', default: resolve(__dirname, 'results') },
    timeoutMs: { type: 'string', default: '10000' },
  },
  strict: false,
})

const BASE_URL = args.url
const ROOM_COUNT = Math.max(1, parseInt(args.rooms, 10))
const PLAYERS_PER_ROOM = Math.max(2, parseInt(args.players, 10))
const GUESSES_PER_PLAYER = Math.max(1, parseInt(args.guesses, 10))
const TIMEOUT_MS = Math.max(1000, parseInt(args.timeoutMs, 10))

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
  maxAttempts: 9999,
  syncMode: false,
  nonstopMode: false,
}

const GUESS_IDS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]

function roomPayload(roomId, username, suffix) {
  return {
    roomId,
    username,
    playerSessionId: `stress-${suffix}-${randomUUID()}`,
  }
}

function connectSocket(label) {
  const socket = io(BASE_URL, {
    transports: ['websocket'],
    reconnection: false,
    timeout: TIMEOUT_MS,
    forceNew: true,
  })
  socket._stressLabel = label
  socket._stressErrors = []
  socket.on('error', payload => {
    socket._stressErrors.push(payload)
  })
  return waitFor(socket, 'connect', TIMEOUT_MS).then(() => socket)
}

function waitFor(socket, event, timeoutMs) {
  return new Promise((resolvePromise, reject) => {
    const timer = setTimeout(() => {
      cleanup()
      reject(new Error(`${socket._stressLabel || 'socket'}:${event}:timeout`))
    }, timeoutMs)
    const onEvent = payload => {
      cleanup()
      resolvePromise(payload)
    }
    const onConnectError = err => {
      cleanup()
      reject(err)
    }
    function cleanup() {
      clearTimeout(timer)
      socket.off(event, onEvent)
      socket.off('connect_error', onConnectError)
    }
    socket.once(event, onEvent)
    socket.once('connect_error', onConnectError)
  })
}

function sleep(ms) {
  return new Promise(resolvePromise => setTimeout(resolvePromise, ms))
}

async function setupRoom(roomIndex) {
  const roomId = `stress-${Date.now()}-${roomIndex}`
  const sockets = []
  const host = await connectSocket(`r${roomIndex}-host`)
  await sleep(20)
  sockets.push(host)
  const hostJoined = waitFor(host, 'updatePlayers', TIMEOUT_MS)
  host.emit('createRoom', roomPayload(roomId, `host-${roomIndex}`, `${roomIndex}-host`))
  await hostJoined

  for (let i = 1; i < PLAYERS_PER_ROOM; i++) {
    const socket = await connectSocket(`r${roomIndex}-p${i}`)
    await sleep(20)
    sockets.push(socket)
    const joined = waitFor(socket, 'updatePlayers', TIMEOUT_MS)
    socket.emit('joinRoom', roomPayload(roomId, `p${roomIndex}-${i}`, `${roomIndex}-${i}`))
    await joined
    socket.emit('toggleReady', { roomId })
  }

  await sleep(150)
  const started = Promise.all(sockets.map(socket => waitFor(socket, 'gameStart', TIMEOUT_MS)))
  host.emit('gameStart', { roomId, settings: GAME_SETTINGS })
  await started
  return { roomId, sockets }
}

async function runGuessLoop(room, socket, offset) {
  const latencies = []
  let failures = 0

  for (let i = 0; i < GUESSES_PER_PLAYER; i++) {
    const characterId = GUESS_IDS[(i + offset) % GUESS_IDS.length]
    const t0 = performance.now()
    const result = waitFor(socket, 'guessResult', TIMEOUT_MS)
    socket.emit('playerGuess', { roomId: room.roomId, characterId })
    try {
      await result
      latencies.push(performance.now() - t0)
    } catch {
      failures++
    }
  }

  return { latencies, failures, errors: socket._stressErrors.length }
}

async function run() {
  const started = Date.now()
  const rooms = []
  try {
    for (let i = 0; i < ROOM_COUNT; i++) {
      rooms.push(await setupRoom(i))
    }

    const workers = []
    for (const room of rooms) {
      room.sockets.forEach((socket, idx) => {
        if (idx === 0) return
        workers.push(runGuessLoop(room, socket, idx))
      })
    }

    const results = await Promise.all(workers)
    const latencies = results.flatMap(r => r.latencies).sort((a, b) => a - b)
    const failures = results.reduce((sum, r) => sum + r.failures, 0)
    const socketErrors = results.reduce((sum, r) => sum + r.errors, 0)
    const elapsedMs = Date.now() - started
    const summary = {
      baseUrl: BASE_URL,
      rooms: ROOM_COUNT,
      playersPerRoom: PLAYERS_PER_ROOM,
      activeGuessers: ROOM_COUNT * (PLAYERS_PER_ROOM - 1),
      requestedGuesses: ROOM_COUNT * (PLAYERS_PER_ROOM - 1) * GUESSES_PER_PLAYER,
      completedGuesses: latencies.length,
      failures,
      socketErrors,
      elapsedMs,
      rps: +(latencies.length / (elapsedMs / 1000)).toFixed(1),
      avgMs: +avg(latencies).toFixed(1),
      p50Ms: +percentile(latencies, 0.50).toFixed(1),
      p95Ms: +percentile(latencies, 0.95).toFixed(1),
      p99Ms: +percentile(latencies, 0.99).toFixed(1),
      maxMs: +(latencies.at(-1) || 0).toFixed(1),
      slowOver500ms: latencies.filter(ms => ms > 500).length,
      slowOver2000ms: latencies.filter(ms => ms > 2000).length,
    }

    const outPath = resolve(args.output, `socket_stress_${started}.json`)
    console.log(JSON.stringify(summary, null, 2))
    try {
      mkdirSync(args.output, { recursive: true })
      writeFileSync(outPath, JSON.stringify({ args, summary }, null, 2))
      console.log(`wrote ${outPath}`)
    } catch (e) {
      console.warn(`failed to write ${outPath}: ${e?.message || e}`)
    }
  } finally {
    for (const room of rooms) {
      for (const socket of room.sockets) {
        socket.disconnect()
      }
    }
  }
}

function avg(values) {
  if (!values.length) return 0
  return values.reduce((sum, value) => sum + value, 0) / values.length
}

function percentile(sorted, p) {
  if (!sorted.length) return 0
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))]
}

run().catch(e => {
  console.error(e)
  process.exit(1)
})
