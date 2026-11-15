-- Thruput Points: award points to the badges around you. The points live in
-- a contract on the Thruput testnet (../points-contract).
--
-- THE AWARDS ARE NOT SIGNED. Solana OS gives apps no way to make a key pair
-- or a signature, so this app can't sign a Thruput transaction. Instead the
-- contract accepts transactions with no signature at all, and takes the
-- sender's word for which badge is awarding. Anyone can award points as
-- anyone. The app says so when it opens and keeps saying so.
--
-- Permissions "mesh" (who is nearby) and "http" (the Thruput RPC endpoint),
-- hosts ["rpc.thruput.network"]. Needs Solana OS 2.1.0 for crypto.sha256.

local thruput = require("lib.thruput")
local config = require("config")

local MAX_AWARD = 10              -- the contract refuses more in one award
local EXPIRY_BLOCKS = 1000        -- how long a submitted award stays valid
local BATCH = 10                  -- reads per request: keeps it under http's 2 KB
local MAX_NEARBY = 12             -- badges listed
local MAX_FRIENDS = 6             -- badge ids typed in, remembered
local WIFI_WAIT_S = 20            -- how long to wait for Wi-Fi after mesh.pause()
local REFRESH_MS = 15000          -- between reads while Wi-Fi and the mesh share a channel
local REFRESH_PAUSED_MS = 60000   -- between reads that have to pause the mesh
local RECEIPT_TRIES = 12          -- 1.5 s apart
local FLOOR_TRIES = 5             -- 1 s apart: waiting for the node to reach an award's block

ui.bg("bg")

local function notice(text)
  ui.label(10, 60, text, {w = 300, align = "center"})
  ui.hint("CANCEL exit")
end

if not crypto or not solana then
  notice("Thruput Points needs Solana OS 2.1.0 or later.")
  return
end
local me = badge.id()
if not me then
  notice("This badge has no id yet. Turn Wi-Fi on once, then open the app again.")
  return
end
local address = solana.decode(config.contract)
if not address or #address ~= 32 then
  notice("No contract address in config.lua yet. Deploy the points contract and put its address there.")
  return
end

local ENTRY = {}
for _, method in ipairs({"award", "points", "given"}) do
  ENTRY[method] = thruput.entrypoint(config.interface, config.major, method)
end
local ADDRESS_TEXT = solana.encode(address)

-- State ---------------------------------------------------------------------

local amount = 1                  -- points the next award carries
local points = {}                 -- badge id -> its points, a decimal string
local my_given                    -- points I have awarded, a decimal string
local names = {}                  -- badge id -> name, from the mesh
local nearby = {}                 -- badge ids heard on the mesh, strongest first
local friends = storage.get("friends") or {}   -- badge ids typed in
local rows = {}                   -- what the list shows: {id = ...} or {enter = true}
local message = "Reading points..."
local trouble = true              -- the message is about not having points to show: a good read clears it
local accepted = false            -- the warning has been answered
local seq = 0                     -- makes each transaction's nonce different

-- Screen --------------------------------------------------------------------

ui.label(8, 4, "Thruput Points", {size = 20})
local score = ui.label(0, 4, "", {size = 20, w = ui.W - 8, align = "right", color = "green"})
local who = ui.label(8, 30, "", {color = "dim", w = 304})
ui.label(8, 47, "UNSIGNED: this badge has no signing API", {size = 12, color = "orange", w = 304})
local status = ui.label(8, 61, "", {size = 12, color = "dim", w = 304})
local list                        -- made once the warning is answered

local function label_of(id)
  local name = names[id]
  if name and name ~= "" then return name end
  return id
end

local function show()
  score:text((points[me] or "-") .. " pts")
  who:text("You are " .. me .. (my_given and (", given " .. my_given) or ""))
  status:text(message)
  ui.hint("SELECT award " .. amount .. "   LEFT/RIGHT amount   CANCEL exit")
end

-- The list: badges nearby, then ids typed in, then the row that asks for one.
local shown = ""
local function show_list()
  if not list then return end
  local items, key = {}, {}
  rows = {}
  local seen = {[me] = true}
  local function add(id, sub)
    if seen[id] then return end
    seen[id] = true
    rows[#rows + 1] = {id = id}
    items[#items + 1] = {title = label_of(id), sub = sub, right = (points[id] or "-") .. " pts"}
    key[#key + 1] = id .. "=" .. label_of(id) .. "=" .. (points[id] or "-") .. sub
  end
  for _, id in ipairs(nearby) do add(id, id .. "  nearby") end
  for _, id in ipairs(friends) do add(id, id) end
  rows[#rows + 1] = {enter = true}
  items[#items + 1] = {title = "Enter a badge id", sub = "for someone who isn't nearby"}
  local now = table.concat(key, "|")
  if now == shown then return end          -- nothing changed: leave the selection alone
  shown = now
  local at = rows[list:selected()]
  list:items(items)
  if at and at.id then                     -- keep the same badge selected
    for i, row in ipairs(rows) do
      if row.id == at.id then list:select(i) break end
    end
  end
end

-- The network ---------------------------------------------------------------
--
-- The badge has one radio. Where Wi-Fi is joined on the mesh channel
-- (mesh.info().online) both work at once. Elsewhere opening this app took the
-- radio from Wi-Fi, so to reach the RPC endpoint the app pauses the mesh,
-- waits for Wi-Fi, does everything that is waiting, and resumes.

local jobs = {}                   -- each: function(done), calls done() once
local working = false
local backoff = 0                 -- failed tries to get online in a row

local function pump(release)
  local job = table.remove(jobs, 1)
  if not job then
    working = false
    release()
    show()
    return
  end
  job(function() pump(release) end)
end

local function enqueue(job)
  jobs[#jobs + 1] = job
  if working then return end
  working = true
  if mesh.info().online then
    backoff = 0
    pump(function() end)
    return
  end
  mesh.pause()
  local waited = 0
  local t
  t = timer.every(1000, function()
    waited = waited + 1
    if badge.online() then
      timer.cancel(t)
      backoff = 0
      pump(function() mesh.resume() end)
    elseif waited >= WIFI_WAIT_S then
      timer.cancel(t)
      mesh.resume()
      jobs = {}
      working = false
      backoff = backoff + 1
      message, trouble = "Offline: turn Wi-Fi on to read and award points", true
      show()
    end
  end)
end

-- One JSON-RPC call: cb(result) or cb(nil, why).
local function rpc(method, params, cb)
  http.post(config.rpc, {jsonrpc = "2.0", id = 1, method = method, params = params}, function(ok, res)
    if not ok then return cb(nil, res) end
    if res.truncated then return cb(nil, "answer too long") end
    local m = json.decode(res.body)
    if type(m) ~= "table" then return cb(nil, "server said " .. tostring(res.status)) end
    if m.error then return cb(nil, type(m.error) == "table" and m.error.message or "rpc error") end
    cb(m.result)
  end)
end

-- Reading points --------------------------------------------------------------

-- The decimal value a read returned, or nil.
local function returned(result)
  local output = type(result) == "table" and result.output
  if type(output) ~= "table" or output.result ~= "return" then return nil end
  local data = thruput.unhex(output.returnData)
  local value = data and thruput.uncxs(data)
  return value and thruput.decimal(value)
end

-- Reads my points and everyone's on the list. `floor` (a block height, as
-- the RPC gives it) asks for state at least that new: right after an award
-- the node may still answer from the block before it.
local function refresh(done, floor, tries)
  local reads = {{method = "points", id = me}, {method = "given", id = me}}
  for _, row in ipairs(rows) do
    if row.id then reads[#reads + 1] = {method = "points", id = row.id} end
  end
  local before = points[me]
  local at = 1
  local function step()
    if at > #reads then
      local gained = before and points[me] and (tonumber(points[me]) - tonumber(before)) or 0
      if gained > 0 then ui.toast("You got " .. gained .. (gained == 1 and " point!" or " points!")) end
      if trouble then message, trouble = "", false end
      show()
      show_list()
      return done()
    end
    local chunk, invocations = {}, {}
    for i = at, math.min(at + BATCH - 1, #reads) do
      local read = reads[i]
      chunk[#chunk + 1] = read
      invocations[#invocations + 1] = {
        address = ADDRESS_TEXT,
        entrypoint = "0x" .. thruput.hex(ENTRY[read.method]),
        calldata = "0x" .. thruput.hex(thruput.badge(read.id)),
      }
    end
    rpc("simulateInvocations", {invocations = invocations, minStateBlockHeight = floor}, function(result, why)
      local results = type(result) == "table" and result.results
      if type(results) ~= "table" then
        if floor and (tries or 0) < FLOOR_TRIES then       -- not at that block yet: ask again shortly
          timer.after(1000, function() refresh(done, floor, (tries or 0) + 1) end)
          return
        end
        if trouble or message == "" then
          message, trouble = "Can't read points: " .. tostring(why or "bad answer"), true
        end
        show()
        return done()
      end
      for i, read in ipairs(chunk) do
        local value = returned(results[i])
        if value then
          if read.method == "given" then my_given = value else points[read.id] = value end
        end
      end
      at = at + BATCH
      step()
    end)
  end
  step()
end

-- Awarding ------------------------------------------------------------------

local REFUSED = {"You can't award yourself", "The contract refused that amount", "That isn't a badge id"}

local function award(to, n)
  return function(done)
    local function finish(text)
      message, trouble = text, false
      show()
      done()
    end
    message, trouble = "Awarding " .. n .. " to " .. label_of(to) .. "...", false
    show()
    rpc("getChainTip", {}, function(tip, why)
      local height = type(tip) == "table" and thruput.u64(tip.blockHeight)
      if not height then return finish("Not sent: " .. tostring(why or "no chain tip")) end
      seq = seq + 1
      local entropy = table.concat({me, to, seq, sys.millis(), sys.time() or 0, math.random(0, 0x7FFFFFFF)}, ":")
      local body, envelope = thruput.transaction({
        address = address,
        entrypoint = ENTRY.award,
        calldata = thruput.badge(me) .. thruput.badge(to) .. thruput.cxs(string.char(n)),
        expiry = thruput.add(height, EXPIRY_BLOCKS),
        nonce = crypto.sha256(entropy):sub(1, 8),
      })
      local hash = solana.encode(crypto.sha256(body))
      rpc("sendTransaction", {signedTransaction = "0x" .. thruput.hex(envelope)}, function(sent, why2)
        if type(sent) ~= "table" then return finish("Not sent: " .. tostring(why2 or "bad answer")) end
        local tries = 0
        local function check()
          tries = tries + 1
          rpc("getTransactionReceipt", {transactionHash = hash}, function(receipt)
            local execution = type(receipt) == "table" and receipt.execution
            if type(execution) ~= "table" then
              if tries >= RECEIPT_TRIES then return finish("Sent, but not confirmed yet") end
              timer.after(1500, check)
              return
            end
            if execution.result ~= "return" then
              local data = thruput.unhex(execution.abortData) or ""
              return finish(REFUSED[data:byte(-1) or 0] or "The contract refused the award")
            end
            ui.toast("+" .. n .. " to " .. label_of(to))
            message = "Awarded " .. n .. " to " .. label_of(to) .. " (unsigned)"
            refresh(done, receipt.blockHeight)
          end)
        end
        timer.after(1000, check)
      end)
    end)
  end
end

local function ask_award(to)
  if to == me then
    ui.toast("You can't award yourself")
    return
  end
  local n = amount
  local text = "Award " .. n .. (n == 1 and " point to " or " points to ") .. label_of(to)
  if label_of(to) ~= to then text = text .. " (" .. to .. ")" end
  ui.confirm(text .. "? It is sent unsigned.", function(yes)
    if yes then enqueue(award(to, n)) end
  end)
end

local function enter_id()
  ui.input("Badge id", 8, function(text)
    if not text then return end
    local id = text:upper():gsub("%s", "")
    if not id:match("^[%w]+$") or #id > 8 then
      ui.toast("That isn't a badge id")
      return
    end
    if id == me then
      ui.toast("You can't award yourself")
      return
    end
    local kept = {id}
    for _, f in ipairs(friends) do
      if f ~= id and #kept < MAX_FRIENDS then kept[#kept + 1] = f end
    end
    friends = kept
    storage.set("friends", friends)
    show_list()
    ask_award(id)
  end)
end

-- Who is nearby -------------------------------------------------------------

local function listen()
  if mesh.info().paused then return end      -- the mesh hears nothing while Wi-Fi has the radio
  local heard = {}
  for _, p in ipairs(mesh.nearby()) do
    if type(p.id) == "string" and p.id ~= me and (p.kind == nil or p.kind == "badge")
        and p.id:match("^%w+$") and #p.id <= 8 and #heard < MAX_NEARBY then
      heard[#heard + 1] = p.id
      if type(p.name) == "string" then names[p.id] = p.name end
    end
  end
  nearby = heard
  show_list()
end

-- Keys ----------------------------------------------------------------------

function on_key(key)
  if not accepted then return key ~= "back" end
  if key == "left" or key == "right" then
    amount = math.max(1, math.min(MAX_AWARD, amount + (key == "right" and 1 or -1)))
    show()
    return true
  end
  return false
end

-- Start ---------------------------------------------------------------------

local last_refresh = 0
local function start()
  accepted = true
  list = ui.list(0, 76, ui.W, ui.H - 16 - 76, {}, {on_select = function(i)
    local row = rows[i]
    if not row then return end
    if row.enter then enter_id() else ask_award(row.id) end
  end})
  listen()
  show()
  enqueue(refresh)
  last_refresh = sys.millis()
  timer.every(2000, listen)
  timer.every(5000, function()
    if working then return end
    local sharing = mesh.info().online
    local wait = sharing and REFRESH_MS or (REFRESH_PAUSED_MS << math.min(backoff, 3))
    if sys.millis() - last_refresh < wait then return end
    last_refresh = sys.millis()
    enqueue(refresh)
  end)
end

show()
ui.confirm("Solana OS has no signing API for apps, so your awards go to Thruput UNSIGNED. "
  .. "Anyone can award points as anyone. Continue?", function(yes)
  if yes then start() else sys.exit() end
end)
