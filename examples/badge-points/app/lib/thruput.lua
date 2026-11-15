-- The little of Thruput's wire format this app needs, for a badge's Lua:
-- 32-bit integers, so every 64-bit value is a string of 8 big-endian bytes.
--
-- A transaction envelope is classic XDR (big-endian, fixed-width integers).
-- Calldata and return data are CXS, the compact form: an unsigned integer is
-- one byte for 0..62, else a header byte (0x3f, 0x7f, 0xbf, 0xff) and then
-- 1, 2, 4 or 8 little-endian bytes, always the shortest that fits.

local M = {}

function M.hex(bytes)
  return (bytes:gsub(".", function(c) return string.format("%02x", c:byte()) end))
end

-- The bytes of "0x..." hex, or nil if it isn't hex.
function M.unhex(text)
  if type(text) ~= "string" then return nil end
  local digits = text:match("^0x(%x*)$")
  if not digits or #digits % 2 ~= 0 then return nil end
  return (digits:gsub("%x%x", function(b) return string.char(tonumber(b, 16)) end))
end

-- A decimal string ("805411") as 8 big-endian bytes, or nil if it isn't one
-- or doesn't fit.
function M.u64(decimal)
  if type(decimal) ~= "string" or not decimal:match("^%d+$") then return nil end
  local b = {0, 0, 0, 0, 0, 0, 0, 0}
  for i = 1, #decimal do
    local carry = decimal:byte(i) - 48
    for j = 8, 1, -1 do
      local v = b[j] * 10 + carry
      b[j] = v & 0xFF
      carry = v >> 8
    end
    if carry ~= 0 then return nil end
  end
  return string.char(table.unpack(b))
end

-- 8 big-endian bytes plus a small non-negative number.
function M.add(bytes, n)
  local b = {bytes:byte(1, 8)}
  local carry = n
  for j = 8, 1, -1 do
    local v = b[j] + carry
    b[j] = v & 0xFF
    carry = v >> 8
  end
  return string.char(table.unpack(b))
end

-- Big-endian bytes of any length as a decimal string.
function M.decimal(bytes)
  local b = {bytes:byte(1, -1)}
  local digits = {}
  repeat
    local rem, more = 0, false
    for i = 1, #b do
      local v = rem * 256 + b[i]
      b[i] = v // 10
      rem = v % 10
      if b[i] ~= 0 then more = true end
    end
    table.insert(digits, 1, string.char(48 + rem))
  until not more
  return table.concat(digits)
end

-- An unsigned integer, given as big-endian bytes, in CXS.
function M.cxs(bytes)
  local s = bytes:gsub("^\0+", "")
  local n = #s
  if n == 0 then return "\0" end
  if n == 1 then
    if s:byte() <= 62 then return s end
    return "\x3f" .. s
  end
  local header, size
  if n == 2 then header, size = "\x7f", 2
  elseif n <= 4 then header, size = "\xbf", 4
  elseif n <= 8 then header, size = "\xff", 8
  else error("integer too long") end
  return header .. s:reverse() .. string.rep("\0", size - n)
end

-- A CXS unsigned integer at the start of `data`: its big-endian bytes and
-- the number of bytes it took, or nil.
function M.uncxs(data)
  local h = data:byte(1)
  if not h then return nil end
  if h <= 62 then return string.char(h), 1 end
  local size = ({[0x3f] = 1, [0x7f] = 2, [0xbf] = 4, [0xff] = 8})[h]
  if not size or #data < 1 + size then return nil end
  return data:sub(2, 1 + size):reverse(), 1 + size
end

-- A badge id ("7K2Q9M") as the contract's u64: its characters, big-endian.
function M.badge(id)
  return M.cxs(id)
end

-- The 8-byte entrypoint id of a method: SHA-256("<interface>::<major>::<method>").
function M.entrypoint(interface, major, method)
  return crypto.sha256(interface .. "::" .. major .. "::" .. method):sub(1, 8)
end

-- An unsigned transaction to a contract that requires no signature.
-- o.address (32 bytes), o.entrypoint (8), o.calldata, o.expiry (8), o.nonce (8),
-- o.gas (8, optional). Returns the transaction body (what the transaction
-- hash covers) and the envelope to submit.
function M.transaction(o)
  local calldata = o.calldata
  local body = o.address
    .. o.entrypoint
    .. (o.gas or "\0\0\0\0\0\x98\x96\x80")                -- gas limit: 10,000,000
    .. string.pack(">I4", #calldata) .. calldata .. string.rep("\0", -#calldata % 4)
    .. o.expiry
    .. o.nonce
    .. string.pack(">I4", 0)                              -- no contracts to deploy
    .. string.pack(">I4I4I4", 0, 0, 0)                    -- source-address claim: no keys, weight 0
  local envelope = body
    .. string.pack(">I4", 0)                              -- no signatures: there is nothing to sign with
    .. string.pack(">I4", 0)                              -- no public key
  return body, envelope
end

return M
