/* A desktop stand-in for the badge, for testing the app's Lua without one.
 *
 * Links the badge firmware's own Lua (32-bit integers and floats) and gives
 * a test script the few things Lua can't do for itself here: SHA-256, base58,
 * an HTTPS POST (through curl), files and a clock. Everything the badge's
 * modules do on top of those is mocked in Lua (mock.lua).
 *
 *   host <script.lua> [args...]
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "lauxlib.h"
#include "lua.h"
#include "lualib.h"

/* SHA-256 ------------------------------------------------------------------ */

static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};

#define ROR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))

static void sha256_block(uint32_t h[8], const unsigned char *p)
{
    uint32_t w[64], a[8];
    for (int i = 0; i < 16; i++)
        w[i] = (uint32_t)p[i * 4] << 24 | (uint32_t)p[i * 4 + 1] << 16 | (uint32_t)p[i * 4 + 2] << 8 | p[i * 4 + 3];
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = ROR(w[i - 15], 7) ^ ROR(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = ROR(w[i - 2], 17) ^ ROR(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    memcpy(a, h, sizeof a);
    for (int i = 0; i < 64; i++) {
        uint32_t s1 = ROR(a[4], 6) ^ ROR(a[4], 11) ^ ROR(a[4], 25);
        uint32_t ch = (a[4] & a[5]) ^ (~a[4] & a[6]);
        uint32_t t1 = a[7] + s1 + ch + K[i] + w[i];
        uint32_t s0 = ROR(a[0], 2) ^ ROR(a[0], 13) ^ ROR(a[0], 22);
        uint32_t maj = (a[0] & a[1]) ^ (a[0] & a[2]) ^ (a[1] & a[2]);
        uint32_t t2 = s0 + maj;
        a[7] = a[6], a[6] = a[5], a[5] = a[4], a[4] = a[3] + t1;
        a[3] = a[2], a[2] = a[1], a[1] = a[0], a[0] = t1 + t2;
    }
    for (int i = 0; i < 8; i++) h[i] += a[i];
}

static int host_sha256(lua_State *L)
{
    size_t n;
    const unsigned char *data = (const unsigned char *)luaL_checklstring(L, 1, &n);
    uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    size_t i = 0;
    for (; i + 64 <= n; i += 64) sha256_block(h, data + i);
    unsigned char tail[128] = {0};
    size_t rest = n - i;
    memcpy(tail, data + i, rest);
    tail[rest] = 0x80;
    size_t total = rest + 9 <= 64 ? 64 : 128;
    uint64_t bits = (uint64_t)n * 8;
    for (int j = 0; j < 8; j++) tail[total - 1 - j] = (unsigned char)(bits >> (8 * j));
    for (size_t j = 0; j < total; j += 64) sha256_block(h, tail + j);
    unsigned char out[32];
    for (int j = 0; j < 8; j++) {
        out[j * 4] = h[j] >> 24, out[j * 4 + 1] = h[j] >> 16, out[j * 4 + 2] = h[j] >> 8, out[j * 4 + 3] = h[j];
    }
    lua_pushlstring(L, (const char *)out, 32);
    return 1;
}

/* Base58 ------------------------------------------------------------------- */

static const char B58[] = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

static int host_b58encode(lua_State *L)
{
    size_t n;
    const unsigned char *in = (const unsigned char *)luaL_checklstring(L, 1, &n);
    luaL_argcheck(L, n <= 128, 1, "too long");
    unsigned char digits[256] = {0};
    size_t len = 0;
    for (size_t i = 0; i < n; i++) {
        unsigned carry = in[i];
        for (size_t j = 0; j < len; j++) {
            carry += (unsigned)digits[j] << 8;
            digits[j] = carry % 58;
            carry /= 58;
        }
        while (carry) digits[len++] = carry % 58, carry /= 58;
    }
    char out[300];
    size_t o = 0;
    for (size_t i = 0; i < n && in[i] == 0; i++) out[o++] = '1';
    for (size_t j = len; j > 0; j--) out[o++] = B58[digits[j - 1]];
    lua_pushlstring(L, out, o);
    return 1;
}

static int host_b58decode(lua_State *L)
{
    size_t n;
    const char *in = luaL_checklstring(L, 1, &n);
    unsigned char bytes[256] = {0};
    size_t len = 0;
    for (size_t i = 0; i < n; i++) {
        const char *at = in[i] ? strchr(B58, in[i]) : NULL;
        if (!at) {
            lua_pushnil(L);
            lua_pushliteral(L, "not base58");
            return 2;
        }
        unsigned carry = (unsigned)(at - B58);
        for (size_t j = 0; j < len; j++) {
            carry += (unsigned)bytes[j] * 58;
            bytes[j] = carry & 0xff;
            carry >>= 8;
        }
        while (carry) {
            if (len >= 128) {
                lua_pushnil(L);
                lua_pushliteral(L, "too long");
                return 2;
            }
            bytes[len++] = carry & 0xff, carry >>= 8;
        }
    }
    char out[256];
    size_t o = 0;
    for (size_t i = 0; i < n && in[i] == '1'; i++) out[o++] = 0;
    for (size_t j = len; j > 0; j--) out[o++] = (char)bytes[j - 1];
    lua_pushlstring(L, out, o);
    return 1;
}

/* Files, HTTP, time -------------------------------------------------------- */

static int host_readfile(lua_State *L)
{
    FILE *f = fopen(luaL_checkstring(L, 1), "rb");
    if (!f) return 0;
    luaL_Buffer b;
    luaL_buffinit(L, &b);
    char chunk[4096];
    size_t got;
    while ((got = fread(chunk, 1, sizeof chunk, f)) > 0) luaL_addlstring(&b, chunk, got);
    fclose(f);
    luaL_pushresult(&b);
    return 1;
}

static int host_loadfile(lua_State *L)
{
    if (luaL_loadfile(L, luaL_checkstring(L, 1)) != LUA_OK) {
        lua_pushnil(L);
        lua_insert(L, -2);
        return 2;
    }
    return 1;
}

/* host.post(url, body) -> status, answer   (or nil, why) */
static int host_post(lua_State *L)
{
    const char *url = luaL_checkstring(L, 1);
    size_t n;
    const char *body = luaL_checklstring(L, 2, &n);
    for (const char *c = url; *c; c++)
        luaL_argcheck(L, *c > ' ' && *c != '\'' && *c != '\\' && *c < 127, 1, "bad url");
    char path[] = "/tmp/badge-host-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return luaL_error(L, "mkstemp failed");
    if (write(fd, body, n) != (ssize_t)n) return luaL_error(L, "write failed");
    close(fd);
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
             "curl -sS -m 6 -o - -w '\\n%%{http_code}' -H 'content-type: application/json' --data-binary @%s '%s' 2>/dev/null",
             path, url);
    FILE *p = popen(cmd, "r");
    luaL_Buffer b;
    luaL_buffinit(L, &b);
    char chunk[4096];
    size_t got;
    while (p && (got = fread(chunk, 1, sizeof chunk, p)) > 0) luaL_addlstring(&b, chunk, got);
    int rc = p ? pclose(p) : -1;
    unlink(path);
    luaL_pushresult(&b);
    if (rc != 0) {
        lua_pushnil(L);
        lua_pushliteral(L, "unreachable");
        return 2;
    }
    size_t len;
    const char *all = lua_tolstring(L, -1, &len);
    const char *nl = all + len;
    while (nl > all && nl[-1] != '\n') nl--;
    lua_pushinteger(L, atoi(nl));
    lua_pushlstring(L, all, nl > all ? (size_t)(nl - all - 1) : 0);
    return 2;
}

static int host_millis(lua_State *L)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    lua_pushinteger(L, (lua_Integer)((ts.tv_sec * 1000 + ts.tv_nsec / 1000000) & 0x3fffffff));
    return 1;
}

static int host_time(lua_State *L)
{
    lua_pushinteger(L, (lua_Integer)time(NULL));
    return 1;
}

static int host_sleep(lua_State *L)
{
    usleep((useconds_t)luaL_checkinteger(L, 1) * 1000);
    return 0;
}

static int host_getenv(lua_State *L)
{
    const char *v = getenv(luaL_checkstring(L, 1));
    if (!v) return 0;
    lua_pushstring(L, v);
    return 1;
}

/* host.limited(budget, fn, ...) -> true, results... | false, error
 * Runs fn like a badge callback: at most `budget` VM instructions, pattern
 * matching included. The three lh_* functions are the hooks the badge's Lua
 * build (luaconf.h) expects from its host. */
static long steps_left;
static int limiting;

static void step_hook(lua_State *L, lua_Debug *ar)
{
    (void)ar;
    steps_left -= 100;
    if (limiting && steps_left < 0) luaL_error(L, "Script took too long");
}

void lh_charge_steps(lua_State *L, unsigned int n)
{
    steps_left -= (long)n;
    if (limiting && steps_left < 0) luaL_error(L, "Script took too long");
}

void lh_thread_new(lua_State *L1)
{
    lua_sethook(L1, step_hook, LUA_MASKCOUNT, 100);
}

int lh_cstack_check(void)
{
    return 0;
}

static int host_limited(lua_State *L)
{
    steps_left = (long)luaL_checkinteger(L, 1);
    limiting = 1;
    lua_sethook(L, step_hook, LUA_MASKCOUNT, 100);
    int status = lua_pcall(L, lua_gettop(L) - 2, LUA_MULTRET, 0);
    lua_sethook(L, NULL, 0, 0);
    limiting = 0;
    lua_pushboolean(L, status == LUA_OK);
    lua_replace(L, 1);
    return lua_gettop(L);
}

static const luaL_Reg host[] = {
    {"sha256", host_sha256},   {"b58encode", host_b58encode}, {"b58decode", host_b58decode},
    {"readfile", host_readfile}, {"loadfile", host_loadfile}, {"post", host_post},
    {"millis", host_millis},   {"time", host_time},           {"sleep", host_sleep},
    {"getenv", host_getenv},   {"limited", host_limited},     {NULL, NULL}};

int main(int argc, char **argv)
{
    if (argc < 2) {
        fprintf(stderr, "usage: host <script.lua> [args...]\n");
        return 2;
    }
    lua_State *L = luaL_newstate();
    luaL_requiref(L, "_G", luaopen_base, 1);
    luaL_requiref(L, "string", luaopen_string, 1);
    luaL_requiref(L, "table", luaopen_table, 1);
    luaL_requiref(L, "math", luaopen_math, 1);
    luaL_requiref(L, "utf8", luaopen_utf8, 1);
    luaL_requiref(L, "coroutine", luaopen_coroutine, 1);
    lua_settop(L, 0);
    luaL_newlib(L, host);
    lua_setglobal(L, "host");
    lua_newtable(L);
    for (int i = 2; i < argc; i++) {
        lua_pushstring(L, argv[i]);
        lua_rawseti(L, -2, i - 1);
    }
    lua_setglobal(L, "arg");
    if (luaL_loadfile(L, argv[1]) != LUA_OK || lua_pcall(L, 0, 0, 0) != LUA_OK) {
        fprintf(stderr, "%s\n", lua_tostring(L, -1));
        return 1;
    }
    return 0;
}
