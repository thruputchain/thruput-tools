-- Tests for the badge app, run by the desktop host (host.c) on the badge's
-- own Lua:  host test.lua <app dir> <test dir>
--
-- The encoding tests need nothing. The app tests need a Thruput node with
-- the points contract deployed: POINTS_CONTRACT is its address, POINTS_RPC
-- the node's URL (see run.sh).

local app_dir, test_dir = arg[1], arg[2]
local mock = assert(host.loadfile(test_dir .. "/mock.lua"))()

local failures, checks = 0, 0
local function check(ok, what)
  checks = checks + 1
  if not ok then
    failures = failures + 1
    print("FAIL: " .. what)
  end
end
local function equal(got, want, what)
  check(got == want, what .. ": got " .. tostring(got) .. ", want " .. tostring(want))
end

-- Encoding ------------------------------------------------------------------

local run_app = mock.app(app_dir)
local thruput = require("lib.thruput")
local hex, unhex = thruput.hex, function(h) return thruput.unhex("0x" .. h) end

-- Vectors made with an independent encoder whose transactions a node accepted.
local ADDRESS = "9sKgWJXZHYudXEMMbuY43Hri4ghJnscdMBUUmmT9ppaf"
for method, id in pairs({award = "3371e6e7a9aa9787", points = "08cc1e6fdd35556e",
    given = "97ee10f30132c4ae", open = "a03c44066955d994"}) do
  equal(hex(thruput.entrypoint("Points", "0.1", method)), id, "entrypoint " .. method)
end
for decimal, want in pairs({["0"] = "00", ["1"] = "01", ["62"] = "3e", ["63"] = "3f3f", ["255"] = "3fff",
    ["256"] = "7f0001", ["65535"] = "7fffff", ["65536"] = "bf00000100", ["4294967295"] = "bfffffffff",
    ["4294967296"] = "ff0000000001000000", ["18446744073709551615"] = "ffffffffffffffffff"}) do
  local bytes = thruput.u64(decimal)
  equal(hex(thruput.cxs(bytes)), want, "cxs " .. decimal)
  local back, used = thruput.uncxs(unhex(want) .. "trailing")
  equal(thruput.decimal(back), decimal, "uncxs " .. decimal)
  equal(used, #want // 2, "uncxs length " .. decimal)
  equal(thruput.decimal(bytes), decimal, "decimal " .. decimal)
end
equal(thruput.u64("18446744073709551616"), nil, "u64 overflow")
equal(thruput.u64("12a"), nil, "u64 not a number")
equal(thruput.u64(12), nil, "u64 not a string")
equal(thruput.uncxs(""), nil, "uncxs empty")
equal(thruput.uncxs("\x7f\x01"), nil, "uncxs short")
equal(thruput.uncxs("\x40"), nil, "uncxs reserved header")
equal(thruput.unhex("0xzz"), nil, "unhex not hex")
equal(thruput.unhex("0x0"), nil, "unhex odd")
equal(thruput.unhex(nil), nil, "unhex nil")
equal(thruput.decimal(thruput.add(thruput.u64("4294967295"), 1000)), "4294968295", "add carries")
equal(thruput.decimal(thruput.add(thruput.u64("805411"), 1000)), "806411", "add")
do
  local calldata = thruput.badge("7K2Q9M") .. thruput.badge("e1") .. thruput.cxs(string.char(10))
  equal(hex(calldata), "ff4d3951324b3700007f31650a", "award calldata")
  local body, envelope = thruput.transaction({
    address = solana.decode(ADDRESS),
    entrypoint = thruput.entrypoint("Points", "0.1", "award"),
    calldata = calldata,
    expiry = thruput.add(thruput.u64("805411"), 1000),
    nonce = unhex("0102030405060708"),
  })
  equal(hex(envelope), "83c1a89f32bd0ae458f45d0dec8e7f6b4b9a84d0abba0b1502ef8e9bc42b9b54"
    .. "3371e6e7a9aa978700000000009896800000000dff4d3951324b3700007f31650a000000"
    .. "00000000000c4e0b0102030405060708000000000000000000000000000000000000000000000000", "envelope")
  equal(solana.encode(crypto.sha256(body)), "4C8U5RYGZLkUtnd3cc3Z3vVrh5tdZs7rLzUTR7rqTZV8", "transaction hash")
end

-- The app -------------------------------------------------------------------

local contract = host.getenv("POINTS_CONTRACT")
if not contract then
  -- With no contract configured the app only explains itself.
  require("config").contract = ""
  run_app()
  check(mock.screen():find("No contract address", 1, true), "notice when no contract is configured")
  print(string.format("%d checks, %d failed (app tests skipped: set POINTS_CONTRACT)", checks, failures))
  if failures > 0 then error("tests failed", 0) end
  return
end

local config = require("config")
config.contract = contract
mock.rpc_url = host.getenv("POINTS_RPC")

-- Fresh badge ids each run, so the chain's history doesn't matter.
local function fresh()
  local id = ""
  for _ = 1, 6 do
    local n = math.random(0, 31)
    id = id .. ("ABCDEFGHJKLMNPQRSTUVWXYZ23456789"):sub(n + 1, n + 1)
  end
  return id
end
local ME, ALICE, BOB, CAROL = fresh(), fresh(), fresh(), fresh()
mock.id = ME
mock.peers = {
  {id = ALICE, name = "@alice", level = 3, rssi = -40, kind = "badge", bridged = false},
  {id = "BEACON", name = "quest", rssi = -50, kind = "beacon"},
  {id = BOB, rssi = -60, kind = "badge", bridged = false},
  {id = ME, name = "@me", rssi = -1, kind = "badge"},
}

local function screen_has(text) return mock.screen():find(text, 1, true) ~= nil end
local function settle(what, cond, ms)
  local ok = mock.run(ms or 40000, cond)
  check(ok, what .. "\n--- screen ---\n" .. mock.screen() .. "\n--- toasts: " .. table.concat(mock.toasts, " / "))
  return ok
end
local function row_of(id)
  for i, row in ipairs(mock.list.rows) do
    if type(row) == "table" and row.sub and row.sub:find(id, 1, true) then return i, row end
  end
end
local function select_row(id)
  local i = assert(row_of(id), "no row for " .. id)
  mock.list.at = i
  mock.key("select")
end

-- 1. It opens on the warning and does nothing until it is answered.
run_app()
check(mock.dialog and mock.dialog.text:find("UNSIGNED", 1, true), "opens on the unsigned warning")
check(mock.dialog and mock.dialog.text:find("no signing API", 1, true), "the warning names the missing signing API")
mock.run(3000)
equal(#mock.requests, 0, "no requests before the warning is answered")
check(screen_has("UNSIGNED: this badge has no signing API"), "the unsigned banner is on screen")
mock.answer(true)
mock.pace = 1                    -- from here the badge's clock is the real one: the node needs time to commit

-- 2. It lists the badges nearby (not itself, not beacons) with their points.
settle("first read", function() return screen_has("0 pts") and row_of(BOB) and select(2, row_of(BOB)).right == "0 pts" end)
check(row_of(ALICE) and select(2, row_of(ALICE)).title == "@alice", "a nearby badge shows its name")
check(row_of(BOB) and select(2, row_of(BOB)).title == BOB, "a nameless badge shows its id")
check(not row_of("BEACON"), "beacons are not listed")
equal(#mock.list.rows, 3, "two badges and the enter-an-id row")
check(screen_has("You are " .. ME .. ", given 0"), "shows who I am and what I've given")
equal(mock.pauses, 0, "no mesh pause while Wi-Fi shares the channel")

-- 3. Award 1 point to a nearby badge.
select_row(ALICE)
check(mock.dialog and mock.dialog.text:find("Award 1 point to @alice (" .. ALICE .. ")", 1, true), "asks before awarding")
check(mock.dialog and mock.dialog.text:find("unsigned", 1, true), "the question says it is unsigned")
mock.answer(true)
settle("award confirmed", function() return screen_has("Awarded 1 to @alice") end)
settle("points shown after the award", function() return select(2, row_of(ALICE)).right == "1 pts" end)
check(screen_has("given 1"), "my given count went up")
check(mock.toasts[#mock.toasts] == "+1 to @alice", "toast after the award")

-- 4. LEFT/RIGHT change the amount, within 1..10; saying no sends nothing.
for _ = 1, 20 do mock.key("right") end
check(mock.hint:find("award 10 ", 1, true), "amount stops at 10")
for _ = 1, 20 do mock.key("left") end
check(mock.hint:find("award 1 ", 1, true), "amount stops at 1")
for _ = 1, 4 do mock.key("right") end
local before = #mock.requests
select_row(BOB)
check(mock.dialog and mock.dialog.text:find("Award 5 points to " .. BOB .. "?", 1, true), "asks with the amount")
mock.answer(false)
mock.run(1500)
equal(#mock.requests, before, "declining sends nothing")

-- 5. Where Wi-Fi is on another channel, it pauses the mesh, sends, resumes.
mock.shared = false
select_row(BOB)
mock.answer(true)
settle("award through a mesh pause", function() return screen_has("Awarded 5 to " .. BOB) end)
settle("mesh resumed", function() return not mock.paused end, 5000)
equal(mock.pauses, 1, "one pause for the whole award")
settle("points after the paused award", function() return select(2, row_of(BOB)).right == "5 pts" end)
check(row_of(ALICE) ~= nil, "nearby badges stay listed through the pause")
check(screen_has("given 6"), "given counts both awards")

-- 6. An id typed in: bad ones are refused, a good one is remembered.
mock.shared = true
for _ = 1, 4 do mock.key("left") end
mock.list.at = #mock.list.rows
mock.key("select")
check(mock.dialog and mock.dialog.kind == "input", "the last row asks for an id")
mock.answer("no way!")
mock.run(100)
equal(mock.toasts[#mock.toasts], "That isn't a badge id", "a bad id is refused")
mock.list.at = #mock.list.rows
mock.key("select")
mock.answer(ME:lower())
mock.run(100)
equal(mock.toasts[#mock.toasts], "You can't award yourself", "my own id is refused")
check(not mock.dialog, "no award question for my own id")
mock.list.at = #mock.list.rows
mock.key("select")
mock.answer(CAROL:lower())
mock.run(100)
check(mock.dialog and mock.dialog.text:find("Award 1 point to " .. CAROL .. "?", 1, true), "a typed id is upper-cased and asked about")
mock.answer(true)
settle("award to a typed id", function() return screen_has("Awarded 1 to " .. CAROL) end)
check(row_of(CAROL) ~= nil, "the typed id is listed")
equal(storage.get("friends")[1], CAROL, "the typed id is remembered")

-- 7. Points someone else awards me show up, with a toast.
do
  local body = {jsonrpc = "2.0", id = 1, method = "getChainTip", params = {}}
  local _, answer = host.post(mock.rpc_url or config.rpc, json.encode(body))
  local tip = json.decode(answer).result.blockHeight
  local _, envelope = thruput.transaction({
    address = solana.decode(contract),
    entrypoint = thruput.entrypoint("Points", "0.1", "award"),
    calldata = thruput.badge(ALICE) .. thruput.badge(ME) .. thruput.cxs(string.char(7)),
    expiry = thruput.add(thruput.u64(tip), 100),
    nonce = crypto.sha256(ME .. tip):sub(1, 8),
  })
  body.method, body.params = "sendTransaction", {signedTransaction = "0x" .. hex(envelope)}
  local _, sent = host.post(mock.rpc_url or config.rpc, json.encode(body))
  check(sent and sent:find("accepted", 1, true), "an award from another badge is accepted: " .. tostring(sent))
end
settle("points I was awarded appear", function() return screen_has("7 pts") end, 60000)
equal(mock.toasts[#mock.toasts], "You got 7 points!", "toast when I get points")

-- 8. With no Wi-Fi at all it says so, and leaves the mesh running.
mock.shared, mock.wifi = false, false
mock.pace = 0
local pauses = mock.pauses
settle("offline message", function() return screen_has("Offline: turn Wi-Fi on") end, 200000)
check(not mock.paused, "the mesh is resumed after giving up on Wi-Fi")
check(mock.pauses > pauses, "it tried pausing the mesh to get online")


-- 9. Back online, the message goes and the points are read again.
mock.shared, mock.wifi = true, true
settle("recovers when Wi-Fi is back", function() return not screen_has("Offline") end, 700000)

check(mock.max_request <= 2048, "requests fit http's 2 KB (largest " .. mock.max_request .. ")")
check(mock.max_answer <= 3500, "answers fit http's 3.5 KB (largest " .. mock.max_answer .. ")")
print(string.format("%d checks, %d failed; %d requests, largest %d bytes sent, %d received",
  checks, failures, #mock.requests, mock.max_request, mock.max_answer))
print("--- final screen ---\n" .. mock.screen())
if failures > 0 then error("tests failed", 0) end
