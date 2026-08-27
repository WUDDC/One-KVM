// Mock One-KVM backend for browser testing of the IR remote UI.
// Plain Node (no deps). Serves the /api endpoints the web UI touches and
// simulates the IR learn flow over WebSocket. Data is in-memory only.
//
//   node web/mock/mock-server.mjs        # listens on http://localhost:8080
//
// Learn flow simulation: POST /api/ir/learn -> WS "waiting", then ~2.5s later
// WS "saved" with a random NEC scancode (button inserted). Cancel works too.

import http from 'node:http'
import crypto from 'node:crypto'

const PORT = 8080

// ------------------------------------------------------------- in-memory DB

let nextRemoteId = 3
let nextButtonId = 7

const remotes = [
  {
    id: 1,
    name: '客厅电视',
    buttons: [
      { id: 1, remote_id: 1, name: '电源', proto: 'nec', scancode: 40186, has_raw: false, carrier: 38000, slot: 1 },
      { id: 2, remote_id: 1, name: '音量+', proto: 'nec', scancode: 40218, has_raw: false, carrier: 38000, slot: null },
      { id: 3, remote_id: 1, name: '音量-', proto: 'nec', scancode: 40219, has_raw: false, carrier: 38000, slot: null },
    ],
  },
  {
    id: 2,
    name: '机顶盒',
    buttons: [
      { id: 4, remote_id: 2, name: '电源', proto: 'nec', scancode: 12345, has_raw: false, carrier: 38000, slot: 2 },
      { id: 5, remote_id: 2, name: 'OK', proto: 'nec', scancode: 12390, has_raw: false, carrier: 38000, slot: null },
    ],
  },
]

const irConfig = {
  enabled: true,
  rx_device: 'auto',
  tx_mode: 'auto',
  tx_gpio_chip: '/dev/gpiochip0',
  tx_gpio_line: 23,
  tx_mmap_base: 0xff634440,
  tx_mmap_oen_offset: 28,
  tx_mmap_out_offset: 32,
  tx_bit: 23,
  carrier: 38000,
  learn_timeout_ms: 10000,
  led_enabled: true,
  led_mmap_base: 0xff800024,
  led_oen_offset: 0,
  led_out_offset: 16,
  led_bit: 8,
  led_brightness: 40,
}

let learnTimer = null

// Per-domain config stores served via GET/PATCH /api/config/<domain>.
const configs = {
  auth: { session_timeout_secs: 86400, single_user_allow_multiple_sessions: true },
  video: { width: 1920, height: 1080, fps: 30, quality: 80 },
  hid: {
    backend: 'none',
    ch9329_port: '',
    ch9329_baudrate: 9600,
    mouse_absolute: true,
  },
  otg_network: {
    enabled: false,
    driver_mode: 'ncm',
    bridge_interface: '',
    host_mac: '',
    device_mac: '',
  },
  msd: { enabled: false, msd_dir: '', flash_inquiry_string: '', cdrom_inquiry_string: '' },
  atx: { enabled: false, driver: 'none', device: '', baud_rate: 9600, wol_interface: '' },
  audio: { enabled: false, device: '', quality: 'balanced' },
  stream: { mode: 'mjpeg', encoder: 'software', bitrate_preset: 'balanced' },
  web: {
    http_port: 8080,
    https_port: 8443,
    bind_addresses: ['0.0.0.0'],
    bind_address: '0.0.0.0',
    https_enabled: false,
  },
  computer_use: { enabled: false, base_url: '', model: '' },
  extensions: { ttyd: {}, gostc: {}, easytier: {}, frpc: {} },
  rustdesk: { enabled: false, codec: 'vp8', rendezvous_server: '', device_id: '' },
  vnc: { enabled: false, bind: '0.0.0.0', port: 5900, encoding: 'tight_jpeg', allow_one_client: true },
  rtsp: {
    enabled: false,
    bind: '0.0.0.0',
    port: 8554,
    path: '/',
    allow_one_client: true,
    codec: 'h264',
  },
  redfish: { enabled: false },
  watchdog: { enabled: false },
  uac: { enabled: false },
}

// ------------------------------------------------------------------- helpers

const json = (res, code, body, headers = []) => {
  const payload = typeof body === 'string' ? body : JSON.stringify(body)
  res.writeHead(code, {
    'Content-Type': 'application/json',
    ...Object.fromEntries(headers),
  })
  res.end(payload)
}

const readBody = (req) =>
  new Promise((resolve) => {
    let data = ''
    req.on('data', (chunk) => (data += chunk))
    req.on('end', () => {
      try {
        resolve(data ? JSON.parse(data) : {})
      } catch {
        resolve({})
      }
    })
  })

// ------------------------------------------------------------------- ws hub

const wsClients = new Set()

function wsAccept(key) {
  return crypto
    .createHash('sha1')
    .update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
    .digest('base64')
}

function wsSend(socket, text) {
  const payload = Buffer.from(text)
  const len = payload.length
  let header
  if (len < 126) {
    header = Buffer.from([0x81, len])
  } else if (len < 65536) {
    header = Buffer.alloc(4)
    header[0] = 0x81
    header[1] = 126
    header.writeUInt16BE(len, 2)
  } else {
    header = Buffer.alloc(10)
    header[0] = 0x81
    header[1] = 127
    header.writeBigUInt64BE(BigInt(len), 2)
  }
  socket.write(Buffer.concat([header, payload]))
}

function broadcast(event, data) {
  const text = JSON.stringify({ event, data })
  for (const socket of wsClients) {
    try {
      wsSend(socket, text)
    } catch {
      wsClients.delete(socket)
    }
  }
}

function irEvent(state, extra = {}) {
  broadcast('ir.learn', { state, ...extra })
}

// ------------------------------------------------------------------- routes

async function handle(req, res) {
  const url = new URL(req.url, `http://localhost:${PORT}`)
  const path = url.pathname
  const method = req.method
  let m

  // ---- auth / bootstrap ----
  if (path === '/api/health') return json(res, 200, { status: 'ok', version: '0.2.6-mock' })

  if (path === '/api/setup')
    return json(res, 200, {
      initialized: true,
      needs_setup: false,
      platform: { mode: 'linux', is_windows: false, is_linux: true },
    })

  if (path === '/api/setup/init') return json(res, 200, { success: true })

  if (path === '/api/auth/login')
    return json(res, 200, { next: 'authenticated' }, [
      ['Set-Cookie', 'one_kvm_session=mock; Path=/; SameSite=Lax'],
    ])

  if (path === '/api/auth/check') return json(res, 200, { authenticated: true, user: 'admin' })

  if (path === '/api/auth/logout') return json(res, 200, { success: true })

  if (path === '/api/auth/totp')
    return json(res, 200, { enabled: false, server_time_unix_ms: Date.now() })

  if (path === '/api/info')
    return json(res, 200, {
      version: '0.2.6-mock',
      initialized: true,
      platform: { mode: 'linux', is_windows: false, is_linux: true },
      capabilities: {
        video: { available: false, reason: 'mock server (no capture device)' },
        hid: { available: false, reason: 'mock server' },
        msd: { available: false, reason: 'mock server' },
        atx: { available: false, reason: 'mock server' },
        audio: { available: false, reason: 'mock server' },
        rustdesk: { available: false, reason: 'mock server' },
        vnc: { available: false, reason: 'mock server' },
      },
      disk_space: { total: 8e9, available: 4e9, used: 4e9 },
    })

  // ---- config ----
  if (path === '/api/config' && method === 'GET') {
    return json(res, 200, {
      initialized: true,
      ...configs,
    })
  }

  m = path.match(/^\/api\/config\/([a-z-]+)$/)
  if (m) {
    const key = m[1].replace(/-/g, '_')
    if (!(key in configs) && key !== 'otg_network') return json(res, 404, { message: 'unknown config' })
    const store = configs[key] ?? configs.otg_network
    if (method === 'GET') return json(res, 200, store)
    if (method === 'PATCH') {
      const body = await readBody(req)
      Object.assign(store, body)
      return json(res, 200, store)
    }
  }

  if (path === '/api/config/ir') {
    if (method === 'GET') return json(res, 200, irConfig)
    if (method === 'PATCH') {
      const body = await readBody(req)
      Object.assign(irConfig, body)
      return json(res, 200, irConfig)
    }
  }

  // ---- IR remotes ----
  if (path === '/api/ir/remotes' && method === 'GET')
    return json(res, 200, { remotes })

  if (path === '/api/ir/remotes' && method === 'POST') {
    const body = await readBody(req)
    const name = (body.name || '').trim()
    if (!name) return json(res, 400, { message: 'name required' })
    const existing = remotes.find((r) => r.name === name)
    if (existing) return json(res, 409, { message: `remote '${name}' already exists` })
    const remote = { id: nextRemoteId++, name, buttons: [] }
    remotes.push(remote)
    return json(res, 200, { id: remote.id, name })
  }

  m = path.match(/^\/api\/ir\/remotes\/(\d+)$/)
  if (m) {
    const id = Number(m[1])
    const remote = remotes.find((r) => r.id === id)
    if (method === 'PATCH') {
      const body = await readBody(req)
      if (!remote) return json(res, 404, { message: 'not found' })
      remote.name = (body.name || remote.name).trim()
      return json(res, 200, { success: true })
    }
    if (method === 'DELETE') {
      const index = remotes.findIndex((r) => r.id === id)
      if (index >= 0) remotes.splice(index, 1)
      return json(res, 200, { success: true })
    }
  }

  m = path.match(/^\/api\/ir\/remotes\/(\d+)\/export$/)
  if (m && method === 'GET') {
    const remote = remotes.find((r) => r.id === Number(m[1]))
    if (!remote) return json(res, 404, { message: 'not found' })
    const pack = {
      format: 'one-kvm-ir-pack',
      version: 1,
      remotes: [
        {
          name: remote.name,
          buttons: remote.buttons.map((b) => ({
            name: b.name,
            protocol: b.proto,
            scancode: b.scancode,
            raw: null,
            carrier: b.carrier,
          })),
        },
      ],
    }
    const safe = remote.name.replace(/[^\w-]/g, '_')
    return json(res, 200, pack, [
      ['Content-Disposition', `attachment; filename="${safe}.onekvm-ir.json"`],
    ])
  }

  // ---- IR learn ----
  if (path === '/api/ir/learn' && method === 'POST') {
    const body = await readBody(req)
    const name = (body.name || '').trim()
    const remote = remotes.find((r) => r.id === body.remote_id)
    if (!name || !remote) return json(res, 400, { message: 'remote_id and name required' })
    if (learnTimer) clearTimeout(learnTimer)

    irEvent('waiting', { remote_id: remote.id })
    // Simulate pressing the physical remote after a moment.
    learnTimer = setTimeout(() => {
      learnTimer = null
      const scancode = 0x8000 + Math.floor(Math.random() * 0x7fff)
      const button = {
        id: nextButtonId++,
        remote_id: remote.id,
        name,
        proto: 'nec',
        scancode,
        has_raw: false,
        carrier: 38000,
        slot: null,
      }
      remote.buttons.push(button)
      irEvent('saved', {
        remote_id: remote.id,
        button_id: button.id,
        proto: 'nec',
        scancode,
      })
    }, 2500)
    return json(res, 200, { success: true, message: 'learning started' })
  }

  if (path === '/api/ir/learn/cancel' && method === 'POST') {
    if (learnTimer) {
      clearTimeout(learnTimer)
      learnTimer = null
    }
    irEvent('cancelled')
    return json(res, 200, { success: true })
  }

  // ---- IR buttons ----
  m = path.match(/^\/api\/ir\/buttons\/(\d+)\/send$/)
  if (m && method === 'POST') {
    const id = Number(m[1])
    const button = remotes.flatMap((r) => r.buttons).find((b) => b.id === id)
    if (!button) return json(res, 404, { message: 'button not found' })
    console.log(`[mock] transmit ${button.proto} 0x${button.scancode.toString(16)} (${button.name})`)
    irEvent('sent', {
      remote_id: button.remote_id,
      button_id: button.id,
      proto: button.proto,
      scancode: button.scancode,
    })
    return json(res, 200, { success: true, message: 'IR signal sent' })
  }

  m = path.match(/^\/api\/ir\/buttons\/(\d+)$/)
  if (m) {
    const id = Number(m[1])
    const button = remotes.flatMap((r) => r.buttons).find((b) => b.id === id)
    if (!button) return json(res, 404, { message: 'button not found' })
    if (method === 'PATCH') {
      const body = await readBody(req)
      if (typeof body.name === 'string' && body.name.trim()) button.name = body.name.trim()
      if (body.slot !== undefined) button.slot = body.slot
      return json(res, 200, { success: true })
    }
    if (method === 'DELETE') {
      for (const r of remotes) {
        const index = r.buttons.findIndex((b) => b.id === id)
        if (index >= 0) r.buttons.splice(index, 1)
      }
      return json(res, 200, { success: true })
    }
  }

  if (path === '/api/ir/import' && method === 'POST') {
    const body = await readBody(req)
    if (body.format !== 'one-kvm-ir-pack')
      return json(res, 400, { message: "unsupported pack format (expected 'one-kvm-ir-pack')" })
    const result = { remotes_imported: 0, remotes_merged: 0, buttons_imported: 0, buttons_skipped: 0 }
    for (const packRemote of body.remotes || []) {
      let remote = remotes.find((r) => r.name === packRemote.name)
      if (remote) result.remotes_merged++
      else {
        remote = { id: nextRemoteId++, name: packRemote.name, buttons: [] }
        remotes.push(remote)
        result.remotes_imported++
      }
      for (const packButton of packRemote.buttons || []) {
        if (remote.buttons.some((b) => b.name === packButton.name)) {
          result.buttons_skipped++
          continue
        }
        remote.buttons.push({
          id: nextButtonId++,
          remote_id: remote.id,
          name: packButton.name,
          proto: packButton.protocol,
          scancode: packButton.scancode ?? null,
          has_raw: !!packButton.raw,
          carrier: packButton.carrier ?? 38000,
          slot: null,
        })
        result.buttons_imported++
      }
    }
    return json(res, 200, result)
  }

  if (path === '/api/ir/hardware')
    return json(res, 200, {
      rx_available: true,
      rx_device: '/dev/lirc0',
      tx_available: true,
      tx_mode: 'lirc',
      tx_device: '/dev/lirc1',
      led_ready: true,
      learn_active: learnTimer !== null,
    })

  // ---- misc endpoints the console view probes ----
  if (path === '/api/stream/mode') return json(res, 200, { mode: 'mjpeg' })
  if (path.startsWith('/api/stream/') && method === 'POST') return json(res, 200, {})
  if (path.startsWith('/api/audio/') && method === 'POST') return json(res, 200, {})
  if (path === '/api/audio/status')
    return json(res, 200, {
      available: false,
      streaming: false,
      device: null,
      quality: 'balanced',
      error: 'mock server',
    })
  if (path === '/api/video/input-status') return json(res, 200, { online: false })
  if (path === '/api/stream/status')
    return json(res, 200, {
      mode: 'mjpeg',
      state: 'stopped',
      device: null,
      resolution: null,
      fps: 0,
      online: false,
      config_changing: false,
      error: 'mock server (no capture device)',
    })

  if (path === '/api/atx/status')
    return json(res, 200, {
      available: false,
      backend: 'none',
      initialized: false,
      power_status: 'unknown',
      led_supported: false,
      hdd_status: 'unknown',
      hdd_supported: false,
    })

  if (path === '/api/devices') return json(res, 200, [])
  if (path === '/api/devices/atx')
    return json(res, 200, { gpio_chips: [], usb_relays: [], serial_devices: [] })
  if (path === '/api/audio/devices') return json(res, 200, [])
  if (path === '/api/extensions') return json(res, 200, {})
  if (path === '/api/hid/status')
    return json(res, 200, {
      available: false,
      backend: 'none',
      initialized: false,
      online: false,
      supports_absolute_mouse: false,
      keyboard_leds_enabled: false,
      led_state: { num_lock: false, caps_lock: false, scroll_lock: false, compose: false, kana: false },
      device: null,
      error: 'mock server',
      error_code: null,
    })

  return json(res, 404, { message: `mock: no handler for ${method} ${path}` })
}

// ------------------------------------------------------------------- server

const server = http.createServer((req, res) => {
  handle(req, res).catch((error) => {
    console.error('[mock] handler error:', error)
    json(res, 500, { message: String(error) })
  })
})

server.on('upgrade', (req, socket) => {
  const key = req.headers['sec-websocket-key']
  if (!key || !req.url?.startsWith('/api/ws')) {
    socket.destroy()
    return
  }
  socket.write(
    'HTTP/1.1 101 Switching Protocols\r\n' +
      'Upgrade: websocket\r\n' +
      'Connection: Upgrade\r\n' +
      `Sec-WebSocket-Accept: ${wsAccept(key)}\r\n` +
      '\r\n',
  )
  wsClients.add(socket)
  console.log(`[mock] ws client connected (${wsClients.size} total)`)
  socket.on('data', () => {
    // Ignore client frames (subscriptions, pings) — nothing to answer.
  })
  socket.on('close', () => wsClients.delete(socket))
  socket.on('error', () => wsClients.delete(socket))
})

server.listen(PORT, () => {
  console.log(`[mock] One-KVM mock backend on http://localhost:${PORT}`)
})
