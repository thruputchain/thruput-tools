-- A pretend badge: the Solana OS modules the app uses, as plain Lua tables
-- that a test can look into and drive. Not the firmware: it mimics what the
-- Lua API reference promises (docs/LUA_API.md in solana-os), including the
-- limits the app has to live inside (instructions per callback, request and
-- answer sizes, allowed hosts).

local M = {
  budget = 200000,          -- VM instructions per callback
  id = "7K2Q9M",            -- badge.id()
  peers = {},               -- mesh.nearby()
  shared = true,            -- Wi-Fi is joined on the mesh channel
  wifi = true,              -- there is a Wi-Fi network to rejoin after mesh.pause()
  rejoin_ms = 3000,         -- how long that takes
  rpc_url = nil,            -- where requests to the app's host really go
  hosts = {},               -- app.json "hosts"
  labels = {}, toasts = {}, requests = {},
  hint = "", list = nil, dialog = nil, exited = false, paused = false,
  pauses = 0, max_request = 0, max_answer = 0,
  pace = 0,                 -- real ms slept per ms of the badge's clock (0: as fast as it goes)
}

local now = 0               -- the badge's clock, ms
local online_at = 0
local queue = {}            -- callbacks waiting to run
local timers, next_timer = {}, 1

-- Runs fn the way the badge runs a callback: within the instruction budget.
local function call(fn, ...)
  local result = table.pack(host.limited(M.budget, fn, ...))
  if not result[1] then error("app error: " .. tostring(result[2]), 0) end
  return table.unpack(result, 2, result.n)
end
M.call = call

local function later(fn, ...)
  queue[#queue + 1] = table.pack(fn, ...)
end

-- json ------------------------------------------------------------------------

local function encode(v, out)
  local t = type(v)
  if t == "table" then
    local n, keys = #v, 0
    for _ in pairs(v) do keys = keys + 1 end
    if n > 0 and keys == n then           -- an array: keys are exactly 1..#t
      out[#out + 1] = "["
      for i = 1, n do
        if i > 1 then out[#out + 1] = "," end
        encode(v[i], out)
      end
      out[#out + 1] = "]"
    else                                   -- empty tables are objects
      out[#out + 1] = "{"
      local first = true
      for k, x in pairs(v) do
        if not first then out[#out + 1] = "," end
        first = false
        encode(tostring(k), out)
        out[#out + 1] = ":"
        encode(x, out)
      end
      out[#out + 1] = "}"
    end
  elseif t == "string" then
    out[#out + 1] = '"' .. v:gsub('[%c"\\]', function(c)
      return string.format("\\u%04x", c:byte())
    end) .. '"'
  elseif t == "number" or t == "boolean" then
    out[#out + 1] = tostring(v)
  else
    out[#out + 1] = "null"
  end
end

local function decode(s)
  local pos = 1
  local value
  local function space() pos = s:find("%S", pos) or #s + 1 end
  local function fail(what) error("json: " .. what .. " at " .. pos, 0) end
  local function text()
    local out = {}
    pos = pos + 1
    while true do
      local c = s:sub(pos, pos)
      if c == "" then fail("unterminated string") end
      if c == '"' then pos = pos + 1 break end
      if c == "\\" then
        local e = s:sub(pos + 1, pos + 1)
        if e == "u" then
          out[#out + 1] = utf8.char(tonumber(s:sub(pos + 2, pos + 5), 16))
          pos = pos + 6
        else
          out[#out + 1] = ({n = "\n", t = "\t", r = "\r", b = "\b", f = "\f"})[e] or e
          pos = pos + 2
        end
      else
        out[#out + 1] = c
        pos = pos + 1
      end
    end
    return table.concat(out)
  end
  function value()
    space()
    local c = s:sub(pos, pos)
    if c == "{" then
      local t = {}
      pos = pos + 1
      space()
      if s:sub(pos, pos) == "}" then pos = pos + 1 return t end
      while true do
        space()
        local k = text()
        space()
        if s:sub(pos, pos) ~= ":" then fail("expected ':'") end
        pos = pos + 1
        t[k] = value()
        space()
        local d = s:sub(pos, pos)
        pos = pos + 1
        if d == "}" then return t end
        if d ~= "," then fail("expected ','") end
      end
    elseif c == "[" then
      local t = {}
      pos = pos + 1
      space()
      if s:sub(pos, pos) == "]" then pos = pos + 1 return t end
      while true do
        t[#t + 1] = value()
        space()
        local d = s:sub(pos, pos)
        pos = pos + 1
        if d == "]" then return t end
        if d ~= "," then fail("expected ','") end
      end
    elseif c == '"' then
      return text()
    elseif s:sub(pos, pos + 3) == "true" then pos = pos + 4 return true
    elseif s:sub(pos, pos + 4) == "false" then pos = pos + 5 return false
    elseif s:sub(pos, pos + 3) == "null" then pos = pos + 4 return nil
    else
      local num = s:match("^-?%d+%.?%d*[eE]?[+-]?%d*", pos)
      if not num or num == "" then fail("unexpected character") end
      pos = pos + #num
      return tonumber(num)
    end
  end
  local ok, result = pcall(value)
  if not ok then return nil, result end
  return result
end

json = {
  encode = function(v) local out = {} encode(v, out) return table.concat(out) end,
  decode = decode,
}
M.json = json

-- ui --------------------------------------------------------------------------

local function object(fields)
  local o = fields or {}
  o.visible = true
  function o:pos() end
  function o:color() end
  function o:size() end
  function o:show(b) self.visible = b end
  function o:remove() self.removed = true end
  return o
end

ui = {W = 320, H = 218}
function ui.bg() end
function ui.label(x, y, text, opts)
  local o = object({value = text, x = x, y = y})
  function o:text(t)
    assert(type(t) == "string", "label text must be a string")
    self.value = t
  end
  M.labels[#M.labels + 1] = o
  return o
end
function ui.rect() return object() end
function ui.list(x, y, w, h, items, opts)
  assert(not M.list, "one list in this test badge")
  local o = object({rows = items, at = 1, on_select = opts and opts.on_select})
  function o:items(new) self.rows = new if self.at > #new then self.at = math.max(1, #new) end end
  function o:selected() return self.at end
  function o:select(i) self.at = i end
  M.list = o
  return o
end
function ui.hint(text) M.hint = text end
function ui.toast(text) M.toasts[#M.toasts + 1] = text end
function ui.confirm(text, cb)
  assert(#text <= 200, "confirm text over a label's 200 bytes")
  M.dialog = {kind = "confirm", text = text, cb = cb}
end
function ui.input(title, max, cb) M.dialog = {kind = "input", text = title, max = max, cb = cb} end
function ui.clear() M.labels = {} M.list = nil end

-- Everything on screen as one string, for assertions and for printing.
function M.screen()
  local lines = {}
  for _, l in ipairs(M.labels) do
    if l.visible and not l.removed and l.value ~= "" then lines[#lines + 1] = l.value end
  end
  if M.list then
    for i, row in ipairs(M.list.rows) do
      local r = type(row) == "table" and row or {title = row}
      lines[#lines + 1] = string.format("%s %-22s %-28s %s", i == M.list.at and ">" or " ",
        r.title or "", r.sub or "", r.right or "")
    end
  end
  lines[#lines + 1] = "[" .. M.hint .. "]"
  if M.dialog then lines[#lines + 1] = "DIALOG: " .. M.dialog.text end
  return table.concat(lines, "\n")
end

-- Answer the open dialog: true/false for ui.confirm, text or nil for ui.input.
function M.answer(v)
  local d = assert(M.dialog, "no dialog is open")
  M.dialog = nil
  later(d.cb, v)
end

-- Press a key. The list takes up, down and select, as on the badge.
function M.key(k)
  assert(not M.dialog, "a dialog has the keys")
  local list = M.list
  if list and (k == "up" or k == "down") then
    list.at = math.max(1, math.min(#list.rows, list.at + (k == "down" and 1 or -1)))
  elseif list and k == "select" then
    if list.on_select then call(list.on_select, list.at, list.rows[list.at]) end
  elseif on_key then
    return call(on_key, k, false)
  end
end

-- timer, sys, badge, storage ----------------------------------------------------

timer = {}
function timer.after(ms, fn)
  local id = next_timer
  next_timer = next_timer + 1
  timers[id] = {at = now + ms, fn = fn}
  return id
end
function timer.every(ms, fn)
  local id = timer.after(ms, fn)
  timers[id].every = ms
  return id
end
function timer.cancel(id) timers[id] = nil end

sys = {
  millis = function() return now end,
  time = function() return host.time() end,
  battery = function() return 80 end,
  keys = function() return {} end,
  memory = function() return {used = 0, max = 6291456} end,
  exit = function() M.exited = true end,
}

badge = {
  id = function() return M.id end,
  owner = function() return nil end,
  online = function()
    if M.shared then return true end
    return M.paused and M.wifi and now >= online_at
  end,
}

local stored = {}
storage = {
  get = function(k) return stored[k] end,
  set = function(k, v) stored[k] = v end,
  remove = function(k) stored[k] = nil end,
}

-- mesh --------------------------------------------------------------------------

mesh = {
  nearby = function()
    if M.paused then return {} end
    return M.peers
  end,
  info = function() return {online = M.shared, paused = M.paused, channel = 6, band = "2.4"} end,
  pause = function()
    if not M.paused then M.pauses = M.pauses + 1 end
    M.paused = true
    online_at = now + M.rejoin_ms
  end,
  resume = function() M.paused = false end,
  send = function() end,
}

-- crypto, solana ------------------------------------------------------------------

crypto = {sha256 = host.sha256}
solana = {
  encode = host.b58encode,
  decode = function(text)
    if type(text) ~= "string" then error("solana.decode: text expected") end
    return host.b58decode(text)
  end,
}

-- http --------------------------------------------------------------------------

http = {}
function http.post(url, body, a, b)
  local cb = b or a
  assert(type(cb) == "function", "http.post needs a callback")
  local scheme, name, path = url:match("^(https)://([^/]+)(.*)$")
  assert(scheme, "http: https:// only")
  if type(body) == "table" then body = json.encode(body) end
  M.requests[#M.requests + 1] = {url = url, body = body}
  M.max_request = math.max(M.max_request, #body)
  if not badge.online() then return later(cb, false, "offline") end
  local allowed = false
  for _, h in ipairs(M.hosts) do if h == name then allowed = true end end
  if not allowed then return later(cb, false, "host not allowed") end
  if #body > 2048 then return later(cb, false, "request too big") end
  local status, answer = host.post(M.rpc_url and (M.rpc_url .. path) or url, body)
  if not status then return later(cb, false, answer) end
  M.max_answer = math.max(M.max_answer, #answer)
  local truncated = #answer > 3500
  later(cb, true, {status = status, body = answer:sub(1, 3500), type = "application/json", truncated = truncated})
end

-- require -------------------------------------------------------------------------

local loaded = {}
function M.app(dir)
  function require(name)
    if loaded[name] == nil then
      local chunk, why = host.loadfile(dir .. "/" .. name:gsub("%.", "/") .. ".lua")
      if not chunk then error(why, 0) end
      local value = chunk()
      loaded[name] = value == nil and true or value
    end
    return loaded[name]
  end
  local manifest = assert(json.decode(assert(host.readfile(dir .. "/app.json"), "no app.json")))
  M.hosts = manifest.hosts or {}
  M.manifest = manifest
  return function()
    local chunk, why = host.loadfile(dir .. "/main.lua")
    if not chunk then error(why, 0) end
    call(chunk)
  end
end

-- The loop ------------------------------------------------------------------------

-- Lets the badge run for `ms` of its own time, or until `stop()` says so.
-- Returns true if `stop` did.
function M.run(ms, stop)
  local until_ms = now + ms
  while true do
    while #queue > 0 do
      local item = table.remove(queue, 1)
      call(table.unpack(item, 1, item.n))
    end
    if stop and stop() then return true end
    local id, soon
    for i, t in pairs(timers) do
      if not soon or t.at < soon.at or (t.at == soon.at and i < id) then id, soon = i, t end
    end
    if not soon or soon.at > until_ms then
      now = until_ms
      return false
    end
    if M.pace > 0 and soon.at > now then host.sleep((soon.at - now) * M.pace // 1) end
    now = math.max(now, soon.at)
    if soon.every then soon.at = now + soon.every else timers[id] = nil end
    call(soon.fn)
  end
end

return M
