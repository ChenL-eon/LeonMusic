// 插件解析逻辑单测（2.21.0：酷我 BD → 咪咕 → GD 三级链 + 低规格不收工 + 智能音质降级）：mock leon 宿主对象后直接跑 index.js。
// 用 node 执行：node test_resolve_chain.cjs
//
// 覆盖：
//   1. 主链顺序：酷我领头，命中即收工；低规格时不收工，继续问咪咕
//   2. 咪咕严格匹配 + 解密取链 + 档位升级探测
//   3. 酷我 BD：md5 签名校验、20018 重解锁、片段长度校验、搜索失败冷却
//   4. GD 补位（含 30 秒片段拦截、503 冷却）
//   5. unblock=off 时零请求
//   6. 同名同歌手校验：不同歌手 / Live 翻唱拒绝（gdCandidateMatches/hasVersionMark）
//   7. 咪咕响应解密与搜索解析细节
//   8. 智能音质 qualityHint：按指定档位请求，不为 FLAC 追问第三方

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const script = fs.readFileSync(path.join(__dirname, '../src-tauri/src/plugin/builtin/netease/index.js'), 'utf8');

// ---- mock 宿主 ----
const httpCalls = [];
const httpGetTimeouts = [];
let httpHandlers = []; // { test: (url) => bool, respond: (url) => string }
let config = {};

function respondFor(url) {
  const h = httpHandlers.find((h) => h.test(url));
  // 未声明的后端（bodian/GD 兜底）默认返回空 JSON：测试只关心当前声明的链路
  if (!h) return { body: '{}' };
  const r = h.respond(url);
  if (typeof r === 'object' && r.throw) throw new Error(r.throw);
  return r;
}

const leon = {
  config: {
    get: (k) => (k in config ? config[k] : ''),
  },
  // 与宿主 host_md5 一致：小写 hex
  md5: (s) => crypto.createHash('md5').update(String(s)).digest('hex'),
  http: {
    get: (url, opts) => {
      httpCalls.push(url);
      if (opts && opts.timeoutMs) httpGetTimeouts.push(opts.timeoutMs);
      return respondFor(url).body;
    },
    // 咪咕 h5v2.4 用的二进制版本；mock 返回 latin1 形态 body（与宿主一致）
    getRaw: (url, opts) => {
      httpCalls.push(url);
      if (opts && opts.timeoutMs) httpGetTimeouts.push(opts.timeoutMs);
      const r = respondFor(url);
      return { status: (r && r.status) || 200, body: (r && r.body) || '' };
    },
    getEx: (url) => {
      httpCalls.push(url);
      const r = respondFor(url);
      return {
        status: (r && r.status) || 200,
        headers: (r && r.headers) || {},
        cookies: {},
        body: (r && r.body) || '',
      };
    },
    // eapi / weapi 都走 postForm（官方直链查询用它）
    postForm: (url) => {
      httpCalls.push(url);
      return respondFor(url).body;
    },
    postFormEx: (url) => {
      httpCalls.push(url);
      const r = respondFor(url);
      return { status: (r && r.status) || 200, headers: {}, cookies: {}, body: (r && r.body) || '' };
    },
  },
  crypto: {
    // 与宿主实现同语义：latin1 字符串（每 charCode 一字节）按 UTF-8 还原
    utf8Decode: (s) => Buffer.from([...String(s)].map((c) => c.charCodeAt(0) & 0xff), 'latin1').toString('utf8'),
    // eapi 的加密结果测试不关心内容，只要不抛错（postForm 已被 mock 掉）
    aesEcbHex: () => crypto.randomBytes(16).toString('hex'),
    rsaNoPadHex: () => '00'.repeat(128),
  },
};

// 隔离执行：index.js 顶层是 var 声明 + 函数，用 new Function 注入 leon/GLOBAL
// 顶层只在 load 时跑一遍（无副作用），函数体在调用时才触网。
const factory = new Function('leon', script + `
  return {
    resolveTrack, unblockBr, miguResolve, miguSearch, miguDecrypt, miguUrl, miguProbe,
    gdCandidateMatches, normalizeName, hasVersionMark, kuwoSearch, kuwoMatch,
    bodianSign, bodianResolve, gdResolve, qualityEnough, getStreamUrl,
    getPlaylist, qqStream,
    // 冷却与解锁位是模块级状态，用例之间要能清干净
    resetCooldown: function () {
      bodianCooldownUntil = 0;
      gdCooldownUntil = 0;
      bodianUnlocked = false;
    }
  };
`);
const api = factory(leon);

let pass = 0, fail = 0;
function ok(cond, name) {
  if (cond) { pass++; console.log('  ok  ' + name); }
  else { fail++; console.log('  FAIL ' + name); }
}

const track = { id: '65528', title: '淘汰', artist: '陈奕迅', durationMs: 285000 };

// 与宿主 leon.http.getRaw 同语义的加密构造：0xAB 0xCD 0x01 + 偏移 r，
// enc[i] = (plain[i] - r + key[i % keylen]) & 0xFF（解密式反推）
function miguEncrypt(plainStr) {
  const key = Buffer.from('Jk8qzuePiJ1qE3mDYhLQ3T73DtDoAhLP', 'utf8');
  const r = 0x0b;
  const data = Buffer.from(plainStr, 'utf8');
  const out = Buffer.alloc(data.length + 4);
  out[0] = 0xab; out[1] = 0xcd; out[2] = 0x01; out[3] = r;
  for (let i = 0; i < data.length; i++) {
    out[i + 4] = (data[i] - r + key[i % key.length]) & 0xff;
  }
  return out.toString('latin1');
}

const MIGU_PLAIN = JSON.stringify({
  code: '000000',
  data: { url: 'https://freetyst.nf.migu.cn/public/x/%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_128_16_Stero/6005.mp3?channelid=02&Key=abc' },
});
const MIGU_SEARCH_BODY = JSON.stringify({
  code: '000000',
  data: { items: [
    { song: { contentId: '1', copyrightId: 'c1', songName: '淘汰', singerList: [{ name: '周杰伦' }] } },
    { song: { contentId: '2', copyrightId: 'c2', songName: '淘汰 (Live)', singerList: [{ name: '陈奕迅' }, { name: 'Duo Band' }] } },
    { song: { contentId: '3', copyrightId: 'c3', songName: '淘汰', singerList: [{ name: '陈奕迅' }] } },
  ] },
});
const miguHandlers = (probeBody, probeStatus) => [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_SEARCH_BODY }) },
  { test: (u) => u.indexOf('listen-url/h5/v2.4') >= 0, respond: () => ({ body: miguEncrypt(MIGU_PLAIN) }) },
  { test: () => true, respond: () => ({ body: probeBody, status: probeStatus }) },
];

// ---- 1. 默认解析 = 咪咕（唯一后端）----
config = { unblockQuality: '320' };
httpHandlers = miguHandlers('bin', 206);
const r1 = api.resolveTrack(track);
ok(r1 && r1.url === 'https://freetyst.nf.migu.cn/public/x/%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_320_16_Stero/6005.mp3' && r1.br === 320,
  'resolveTrack 直走咪咕：Live 版被拒、命中陈奕迅原曲 → 解密取链剥签名参数 → 320 替换探测 206 通过');

// ---- 2. unblock=off：零请求 ----
config = { unblock: 'off', unblockQuality: '320' };
httpCalls.length = 0;
ok(api.resolveTrack(track) === null && httpCalls.length === 0,
  'unblock=off 时 resolveTrack 不发任何第三方请求，回落 null（官方试听）');
config = { unblockQuality: '320' };

// ---- 3. 名字相似度：不同歌手拒绝 ----
ok(!api.gdCandidateMatches(
     { title: 'Walk On', artist: '陶喆' },
     { name: 'Walk On', artist: '周杰伦' }),
  '搜索兜底拒绝不同歌手的同名曲');

// ---- 3b. 版本词拦截：Live/翻唱/伴奏候选直接拒绝 ----
// 根因：normalizeName 剥括号后「晴天 (Live)」与「晴天」同形，歌手也对（现场版
// 还是原唱），一路绿灯播出了 Live 版。现在候选原始名带版本词且目标不带 → 拒绝。
ok(api.hasVersionMark('晴天 (Live)') && api.hasVersionMark('晴天 Live') &&
   api.hasVersionMark('晴天 翻唱版') && api.hasVersionMark('晴天 (伴奏)') &&
   !api.hasVersionMark('晴天') && !api.hasVersionMark('Deliver') && !api.hasVersionMark('Lively'),
  'hasVersionMark 识别 live/翻唱/伴奏等版本词，不误伤含 live 子串的普通词');
ok(!api.gdCandidateMatches(
     { title: '晴天', artist: '周杰伦', durationMs: 269000 },
     { name: '晴天 (Live)', artist: '周杰伦' }),
  '版本词拦截：目标「晴天」拒绝候选「晴天 (Live)」（剥括号同形也拦）');
ok(api.gdCandidateMatches(
     { title: '突然的自我 (Live)', artist: '伍佰' },
     { name: '突然的自我 (Live)', artist: '伍佰' }),
  '目标自身是 Live 版（现场专辑曲目）时不拦截 live 候选');

// ---- 4. 咪咕响应解密（加密往返 + 魔法校验）----
ok(api.miguDecrypt(miguEncrypt(MIGU_PLAIN)) === MIGU_PLAIN,
  '咪咕响应解密：0xAB 0xCD 0x01 头 + 随机偏移字节运算往返一致（中文 UTF-8 不碎）');
ok(api.miguDecrypt('GIF89a-not-encrypted') === null,
  '非加密响应（魔法不符）返回 null，放弃本响应');
ok(api.miguDecrypt('') === null, '空响应返回 null');

// ---- 5. 咪咕搜索解析 + 严格匹配 ----
httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_SEARCH_BODY }) },
];
const miguList = api.miguSearch('淘汰 陈奕迅');
ok(miguList.length === 3 && miguList[0].contentId === '1' && miguList[1].artist === '陈奕迅 Duo Band',
  'bmw 搜索解析出 contentId/copyrightId/歌名/歌手列表');

// ---- 6. 无损档升级与逐级回落 ----
config = { unblockQuality: '999' };
httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_SEARCH_BODY }) },
  { test: (u) => u.indexOf('listen-url/h5/v2.4') >= 0, respond: () => ({ body: miguEncrypt(MIGU_PLAIN) }) },
  { test: (u) => u.endsWith('.flac'), respond: () => ({ body: '', status: 404 }) },
  { test: (u) => u.indexOf('MP3_320_16_Stero') >= 0, respond: () => ({ body: 'bin', status: 200 }) },
];
const r6 = api.resolveTrack(track);
ok(r6 && r6.url === 'https://freetyst.nf.migu.cn/public/x/%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_320_16_Stero/6005.mp3' && r6.br === 320,
  '无损档 flac 探测 404 时逐级退到 320（探测通过的档位才采用）');

httpHandlers[2].respond = () => ({ body: 'bin', status: 206 });
const r6b = api.resolveTrack(track);
ok(r6b && r6b.url.endsWith('.flac') && r6b.br === 900,
  '无损档 flac 探测通过：URL 升级为歌曲下载/flac、br 展示 ~900k');

// 全部替换档位探测失败 → 回落 128k 完整曲基准（不空手而归）
httpHandlers[2].respond = () => ({ body: '', status: 404 });
httpHandlers[3].respond = () => ({ body: '', status: 404 });
const r6c = api.resolveTrack(track);
ok(r6c && r6c.url === 'https://freetyst.nf.migu.cn/public/x/%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_128_16_Stero/6005.mp3' && r6c.br === 128,
  '所有升档探测失败时回落 128k 基准直链');

// ---- 7. 咪咕拿不到 → null（官方试听回落）----
// VIP 曲（接口 url 为空）与歌手不符都算失败
const MIGU_EMPTY = JSON.stringify({ code: '000000', data: { url: '', cannotCode: '440001' } });
httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_SEARCH_BODY }) },
  { test: (u) => u.indexOf('listen-url/h5/v2.4') >= 0, respond: () => ({ body: miguEncrypt(MIGU_EMPTY) }) },
];
ok(api.resolveTrack(track) === null, '咪咕接口 url 为空（下架/地区限制）时解析失败，不硬塞');

httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: JSON.stringify({ code: '000000', data: { items: [ { song: { contentId: '9', copyrightId: 'c9', songName: '淘汰', singerList: [{ name: '周杰伦' }] } } ] } }) }) },
];
ok(api.resolveTrack(track) === null, '咪咕搜索只有其他歌手的同名曲时拒绝（宁缺毋滥）');

// 服务挂（抛错）→ resolveTrack 兜底 null，不影响官方试听回落
httpHandlers = [{ test: () => true, respond: () => ({ throw: 'connect ECONNREFUSED' }) }];
ok(api.resolveTrack(track) === null, '咪咕服务挂/超时安静降级为 null');

// ---- 8. 探测带短超时 ----
config = { unblockQuality: '320' };
httpGetTimeouts.length = 0;
httpHandlers = miguHandlers('bin', 206);
api.resolveTrack(track);
ok(httpGetTimeouts.length >= 2 && httpGetTimeouts.every((t) => t === 3500),
  '咪咕搜索/取链/探测全部带 3.5s 短超时');

// ---- 9. bodian 领头（2.19.0 起）：版本词拦截 / 签名 / 解锁 / FLAC ----
// 上一条「服务挂」用例让所有请求抛错，按新逻辑会触发 bodian 的 2 分钟冷却，
// 这里先把冷却与解锁位清干净。
config = { unblockQuality: '999' };
api.resetCooldown();
const KUWO_LIST_BODY = JSON.stringify({
  content: [null, { musicpage: { abslist: [
    { MUSICRID: 'MUSIC_228909', SONGNAME: '晴天 (Live)', ARTIST: '周杰伦', DURATION: '249' },
    { MUSICRID: 'MUSIC_228908', SONGNAME: '晴天', ARTIST: '周杰伦', DURATION: '269', ALBUM: '叶惠美' },
  ] } }],
});
const MIGU_NO_HIT = JSON.stringify({ code: '000000', data: { items: [] } });
const BODIAN_FLAC = 'http://bd-er.kuwo.cn/x/1/resource/trackmedia/F000000bYDlc2XxKLs.flac?from=bodian';
let bodianAudioUrl = '';

httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_NO_HIT }) },
  { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: KUWO_LIST_BODY }) },
  { test: (u) => u.indexOf('advert/watch') >= 0, respond: () => ({ body: '{"code":200,"msg":"success"}' }) },
  { test: (u) => u.indexOf('audioUrl') >= 0, respond: (u) => {
      bodianAudioUrl = u;
      return { body: JSON.stringify({ code: 200, data: { audioUrl: BODIAN_FLAC } }) };
    } },
  { test: (u) => u.indexOf('bd-er.kuwo.cn') >= 0, respond: () => ({
      status: 206,
      headers: { 'content-range': 'bytes 0-1/29865596' }, // 29.8MB FLAC ≈ 265s，够整曲
      body: '',
    }) },
];
const sunny = { id: '186016', title: '晴天', artist: '周杰伦', durationMs: 269000 };
httpCalls.length = 0;
const rb = api.resolveTrack(sunny);
ok(rb && rb.url === BODIAN_FLAC && rb.br === 900,
  'bodian 领头命中：Live 候选被版本词拦下，命中原曲并返回 FLAC（br 900）');

ok(httpCalls.some((u) => u.indexOf('search.kuwo.cn') >= 0) &&
   !httpCalls.some((u) => u.indexOf('bmw/search/song') >= 0),
  'bodian 一次命中即收工，无损档下不再惊动咪咕（顺序：酷我在前）');

const tsMatch = /&timestamp=(\d+)&sign=([0-9a-f]{32})$/.exec(bodianAudioUrl);
const beforeTs = bodianAudioUrl.slice(0, bodianAudioUrl.indexOf('&timestamp='));
const wantSign = crypto.createHash('md5').update(
  'kuwotest' +
  (beforeTs + '&timestamp=' + (tsMatch ? tsMatch[1] : '')).split('?')[1].replace(/[^a-zA-Z0-9]/g, '').split('').sort().join('') +
  '/api/play/music/v2/audioUrl'
).digest('hex');
ok(!!tsMatch && tsMatch[2] === wantSign,
  'bodian 签名 = md5(kuwotest + 排序后的查询串 + path)，与 Node 重算完全一致');

ok(httpCalls.findIndex((u) => u.indexOf('advert/watch') >= 0) >= 0 &&
   httpCalls.findIndex((u) => u.indexOf('advert/watch') >= 0) <
   httpCalls.findIndex((u) => u.indexOf('audioUrl') >= 0),
  'bodian 先打「广告免单」再取链（不然一律 20018）');

// ---- 9b. 低规格不提前收工（best-of）----
// 复现 2026-11-08 实测的《郭源潮》/《Dear God》：酷我只有 320k，咪咕反而有 FLAC。
// 旧「先命中先返回」会把 320k 直接交出去，现在要继续问下去。
{
  api.resetCooldown();
  config = { unblockQuality: '999' };
  const MIGU_SUNNY_HIT = JSON.stringify({
    code: '000000',
    data: { items: [{ song: { contentId: '7', copyrightId: 'c7', songName: '晴天', singerList: [{ name: '周杰伦' }] } }] },
  });
  httpCalls.length = 0;
  httpHandlers = [
    { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: KUWO_LIST_BODY }) },
    { test: (u) => u.indexOf('advert/watch') >= 0, respond: () => ({ body: '{"code":200}' }) },
    { test: (u) => u.indexOf('audioUrl') >= 0, respond: () => ({
        body: JSON.stringify({ code: 200, data: { audioUrl: 'http://bd-er.kuwo.cn/x/resource/trackmedia/F2.mp3' } }) }) },
    { test: (u) => u.indexOf('bd-er.kuwo.cn') >= 0, respond: () => ({
        status: 206, headers: { 'content-range': 'bytes 0-1/11000000' }, body: '' }) },
    { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_SUNNY_HIT }) },
    { test: (u) => u.indexOf('listen-url/h5/v2.4') >= 0, respond: () => ({ body: miguEncrypt(MIGU_PLAIN) }) },
    { test: (u) => u.endsWith('.flac'), respond: () => ({ status: 206, body: 'bin' }) },
  ];
  const r9b = api.resolveTrack(sunny);
  ok(httpCalls.some((u) => u.indexOf('bmw/search/song') >= 0),
    '酷我只给到 320k 时不收工，继续去问咪咕');
  ok(r9b && r9b.br === 900 && /\.flac$/.test(r9b.url.split('?')[0]),
    '低规格结果只当兜底：最终采用咪咕的 FLAC，而不是酷我的 320k');
}

// ---- 9c. 酷我搜索网络失败 → 冷却 2 分钟 ----
// 搜索走的是 bd-api 同域，整体不可达时每首歌都会白等一次 4s 超时。
{
  api.resetCooldown();
  config = { unblockQuality: '999' };
  let kuwoSearchCalls = 0;
  httpHandlers = [
    { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => { kuwoSearchCalls++; throw new Error('connect timeout'); } },
    { test: () => true, respond: () => ({ body: '{}' }) },
  ];
  api.resolveTrack(sunny);
  ok(kuwoSearchCalls === 1, '酷我搜索失败一次后立刻进入冷却，不在同一首歌里重试');
  api.resolveTrack(sunny);
  ok(kuwoSearchCalls === 1, '冷却期内后续歌曲直接跳过酷我，不再白等超时');
}
api.resetCooldown();

// ---- 10. bodian 解锁位失效（20018）→ 重新免单并重试一次 ----
config = { unblockQuality: '320' };
let audioCalls = 0;
let adCalls = 0;
httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_NO_HIT }) },
  { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: KUWO_LIST_BODY }) },
  { test: (u) => u.indexOf('advert/watch') >= 0, respond: () => { adCalls++; return { body: '{"code":200}' }; } },
  { test: (u) => u.indexOf('audioUrl') >= 0, respond: () => {
      audioCalls++;
      return { body: audioCalls === 1 ? '{"code":20018,"msg":"没有解锁付费歌曲"}' : JSON.stringify({ code: 200, data: { audioUrl: 'http://bd-bj.kuwo.cn/x/2/resource/trackmedia/M000.mp3' } }) };
    } },
];
const r10 = api.resolveTrack(sunny);
ok(r10 && r10.br === 320 && audioCalls === 2 && adCalls >= 1,
  'bodian 返回 20018 时视为解锁位失效：重新免单后重试一次（第二次命中 mp3/320）');

// ---- 10b. 时长优先匹配 + 片段长度校验 ----
// 酷我同一首歌常同时有「15 秒片段」和整曲条目；先按时长挑对条目，
// 再用 Range 反推文件长度，拦住 241KB 这种「假成功」。
config = { unblockQuality: '320' };
let pickAudioUrl = '';
httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_NO_HIT }) },
  { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: JSON.stringify({ content: [null, { musicpage: { abslist: [
      { MUSICRID: 'MUSIC_1', SONGNAME: '晴天', ARTIST: '周杰伦', DURATION: '15' },
      { MUSICRID: 'MUSIC_2', SONGNAME: '晴天', ARTIST: '周杰伦', DURATION: '269' },
    ] } }] }) }) },
  { test: (u) => u.indexOf('advert/watch') >= 0, respond: () => ({ body: '{"code":200}' }) },
  { test: (u) => u.indexOf('audioUrl') >= 0, respond: (u) => {
      pickAudioUrl = u;
      return { body: JSON.stringify({ code: 200, data: { audioUrl: 'http://bd-er.kuwo.cn/x/resource/trackmedia/F1.mp3' } }) };
    } },
  { test: (u) => u.indexOf('bd-er.kuwo.cn') >= 0, respond: () => ({
      status: 206,
      headers: { 'content-range': 'bytes 0-1/241289' }, // 241KB ≈ 6s，明显是片段
      body: '',
    }) },
];
const r10b = api.resolveTrack(sunny);
ok(pickAudioUrl.indexOf('musicId=2') >= 0,
  '时长优先：候选里 15 秒片段排在前面，仍按 269 秒挑中整曲条目');
ok(r10b === null,
  'bodian 只返回 241KB 片段时长度校验拒绝（不再把 6 秒音频当整曲播）');

// ---- 11. GD 补位：两个国内源全失手时用网易 ID 换直链 ----
config = { unblockQuality: '320' };
let gdCalls = 0;
httpHandlers = [
  { test: (u) => u.indexOf('bmw/search/song') >= 0, respond: () => ({ body: MIGU_NO_HIT }) },
  { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: JSON.stringify({ content: [null, { musicpage: { abslist: [] } }] }) }) },
  { test: (u) => u.indexOf('gdstudio.xyz') >= 0, respond: () => {
      gdCalls++;
      return { body: JSON.stringify({ url: 'https://m801.music.126.net/2026/x/y.mp3', br: 320, size: 10000000 }) };
    } },
];
const r11 = api.resolveTrack(sunny);
ok(r11 && r11.url.indexOf('m801.music.126.net') >= 0 && r11.br === 320,
  '咪咕/bodian 都失手时 GD 用网易曲库 ID 兜底');

// ---- 12. GD 只给 30 秒片段时拒绝（size/br 反推时长 < 目标一半）----
httpHandlers[2].respond = () => ({ body: JSON.stringify({ url: 'https://m801.music.126.net/2026/x/y.mp3', br: 320, size: 1200000 }) });
ok(api.resolveTrack(sunny) === null,
  'GD 返回的 size/br 反推只有 30 秒时判定为试听片段，拒绝采用');

// ---- 13. GD 503 后进入冷却，后续歌曲不再重复请求 ----
httpHandlers[2].respond = () => ({ body: '<html><head><title>503 Service Temporarily Unavailable</title></head></html>' });
api.resolveTrack(sunny); // 触发 503 → 冷却
const gdBefore = gdCalls;
api.resolveTrack(sunny);
ok(gdCalls === gdBefore,
  'GD 返回 503/维护页后冷却 10 分钟，后续歌曲不再白等一次超时');

// ---- 14. 官方只给 320k 全曲时，继续问解析要 FLAC ----
// 2026-11-08 用户实测《水之湄》：官方 eapi 返回 320k 非试听，旧逻辑
// 「非试听直接 return」导致第三方源从未被问过，提示永远是「无损未生效」。
{
  api.resetCooldown();
  config = { unblockQuality: '999', level: 'lossless' };
  httpCalls.length = 0;
  const OFFICIAL_MP3 = JSON.stringify({ code: 200, data: [{
    url: 'https://m701.music.126.net/x/official.mp3', br: 320000, type: 'mp3',
    size: 10000000, time: 269000, code: 200,
  }] });
  httpHandlers = [
    { test: (u) => u.indexOf('/eapi/song/enhance/player/url/v1') >= 0, respond: () => ({ body: OFFICIAL_MP3 }) },
    { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: KUWO_LIST_BODY }) },
    { test: (u) => u.indexOf('advert/watch') >= 0, respond: () => ({ body: '{"code":200}' }) },
    { test: (u) => u.indexOf('audioUrl') >= 0, respond: () => ({
        body: JSON.stringify({ code: 200, data: { audioUrl: BODIAN_FLAC } }) }) },
    { test: (u) => u.indexOf('bd-er.kuwo.cn') >= 0, respond: () => ({
        status: 206, headers: { 'content-range': 'bytes 0-1/50000000' }, body: '' }) },
  ];
  const r14 = api.getStreamUrl(sunny);
  ok(httpCalls.some((u) => u.indexOf('audioUrl') >= 0),
    '官方只给 320k 全曲时不收工，继续问第三方解析');
  ok(r14 && r14.format === 'flac' && r14.downgraded === false && r14.source === 'resolved',
    '解析拿到 FLAC 时替换官方 320k：downgraded 归 false');
}

// ---- 15. 官方 320k 全曲 + 解析全失败 → 保留官方，不越换越差 ----
{
  api.resetCooldown();
  config = { unblockQuality: '999', level: 'lossless' };
  httpCalls.length = 0;
  const OFFICIAL_MP3 = JSON.stringify({ code: 200, data: [{
    url: 'https://m701.music.126.net/x/official.mp3', br: 320000, type: 'mp3',
    size: 10000000, time: 269000, code: 200,
  }] });
  httpHandlers = [
    { test: (u) => u.indexOf('/eapi/song/enhance/player/url/v1') >= 0, respond: () => ({ body: OFFICIAL_MP3 }) },
    { test: (u) => u.indexOf('search.kuwo.cn') >= 0, respond: () => ({ body: JSON.stringify({ content: [null, { musicpage: { abslist: [] } }] }) }) },
    { test: () => true, respond: () => ({ body: '{}' }) },
  ];
  const r15 = api.getStreamUrl(sunny);
  ok(r15 && r15.url.indexOf('official.mp3') >= 0 && r15.bitrate === 320000,
    '解析失败时退回官方 320k 全曲，而不是试听或空');
  ok(r15 && r15.downgraded === true && r15.trial === false,
    '官方 320k 仍如实标记 downgraded=true、trial=false');
}

// ---- 16. 官方直接给 FLAC → 零解析请求 ----
{
  api.resetCooldown();
  config = { unblockQuality: '999', level: 'lossless' };
  httpCalls.length = 0;
  const OFFICIAL_FLAC = JSON.stringify({ code: 200, data: [{
    url: 'https://m701.music.126.net/x/official.flac', br: 900000, type: 'flac',
    size: 40000000, time: 269000, code: 200,
  }] });
  httpHandlers = [
    { test: (u) => u.indexOf('/eapi/song/enhance/player/url/v1') >= 0, respond: () => ({ body: OFFICIAL_FLAC }) },
    { test: () => true, respond: () => ({ body: '{}' }) },
  ];
  const r16 = api.getStreamUrl(sunny);
  ok(r16 && r16.format === 'flac' && r16.downgraded === false,
    '官方给 FLAC 时直接用，标 downgraded=false');
  ok(!httpCalls.some((u) => u.indexOf('search.kuwo.cn') >= 0),
    '官方 FLAC 已达标：不打扰第三方，零多余请求');
}

// ---- 17. 智能音质降级：qualityHint 指定本次实际档位 ----
// 宿主重试时带 qualityHint=exhigh（用户设置仍是 lossless）：这一次只按 320k
// 档去要，不再为 FLAC 去问第三方；但 reported 仍按用户选择如实标记，
// 界面不会因为「悄悄降级」而显示成功。
{
  api.resetCooldown();
  config = { unblockQuality: '999', level: 'lossless' };
  httpCalls.length = 0;
  const OFFICIAL_MP3 = JSON.stringify({ code: 200, data: [{
    url: 'https://m701.music.126.net/x/official.mp3', br: 320000, type: 'mp3',
    size: 10000000, time: 269000, code: 200,
  }] });
  httpHandlers = [
    { test: (u) => u.indexOf('/eapi/song/enhance/player/url/v1') >= 0, respond: () => ({ body: OFFICIAL_MP3 }) },
    { test: () => true, respond: () => ({ body: '{}' }) },
  ];
  const r17 = api.getStreamUrl(Object.assign({}, sunny, { qualityHint: 'exhigh' }));
  ok(r17 && r17.format === 'mp3' && r17.bitrate === 320000,
    'qualityHint=exhigh 时直接采用官方 320k 起播');
  ok(!httpCalls.some((u) => u.indexOf('audioUrl') >= 0 || u.indexOf('search.kuwo.cn') >= 0),
    '降级到 320k 档后不为 FLAC 再问第三方（少一次无谓等待）');
  ok(r17 && r17.downgraded === true && r17.requested === 'lossless',
    '仍按用户选的 lossless 标记 requested / downgraded');
}

// ---- 18. 外部歌单：QQ 音乐歌单解析（qq: 前缀分支）----
// 匿名接口返回歌单名 / 封面 / 全量曲目；曲目 id 加 qq: 前缀，
// 与网易云曲目区分开，开播时才知道该走哪条路。
{
  const QQ_DISS_BODY = JSON.stringify({
    code: 0,
    cdlist: [{
      disstid: 7729596131,
      dissname: '耳机里的秘密 | 宝藏女声集合站',
      logo: 'http://p.qpic.cn/music_cover/abc/300',
      songnum: 2,
      songlist: [
        { songmid: '003Et45Q22uBOv', songname: '寻不回', singer: [{ name: '小羊阿旸' }], albumname: '寻不回', albummid: '002JHJdH2LqxOi', interval: 217 },
        { songmid: '001wldka157dWs', songname: '永远在身边', singer: [{ name: '大嘴巴' }, { name: '嘉宾' }], albumname: '王元 口力口', albummid: '003TdIgo0sPm0U', interval: 280 },
      ],
    }],
  });
  httpHandlers = [
    { test: (u) => u.indexOf('fcg_ucc_getcdinfo_byids_cp') >= 0, respond: () => ({ body: QQ_DISS_BODY }) },
    { test: () => true, respond: () => ({ body: '{}' }) },
  ];
  httpCalls.length = 0;
  const pl = api.getPlaylist('qq:7729596131');
  ok(httpCalls.length === 1 && httpCalls[0].indexOf('disstid=7729596131') >= 0,
    'getPlaylist("qq:…") 只打一次 QQ 歌单接口，不再问网易云');
  ok(pl && pl.name === '耳机里的秘密 | 宝藏女声集合站' && pl.trackCount === 2,
    '歌单名与总数按接口返回，不靠曲目数组长度猜');
  ok(pl && pl.coverUrl === 'https://p.qpic.cn/music_cover/abc/300',
    '封面 http 升级成 https（Android 明文策略会拦）');
  ok(pl && pl.tracks.length === 2 && pl.tracks[0].id === 'qq:003Et45Q22uBOv',
    '曲目 id = qq:<songmid>，与网易云数字 id 天然不撞');
  ok(pl && pl.tracks[0].durationMs === 217000,
    'interval 秒换成毫秒');
  ok(pl && pl.tracks[1].artist === '大嘴巴 / 嘉宾',
    '多歌手用「 / 」拼接，与网易云的展示一致');
  ok(pl && pl.tracks[0].coverUrl ===
    'https://y.gtimg.cn/music/photo_new/T002R300x300M000002JHJdH2LqxOi.jpg',
    '曲目封面按 albummid 拼 CDN 地址，不是留空');
  ok(pl && pl.tracks[1].coverUrl.indexOf('003TdIgo0sPm0U') > 0 &&
    pl.tracks[1].coverUrl !== pl.tracks[0].coverUrl,
    '每首用各自的专辑封面，不是共用歌单封面');
}

// ---- 18b. 外部歌单：缺 albummid 的老歌退回歌单封面 ----
{
  const QQ_NO_ALBUM = JSON.stringify({
    code: 0,
    cdlist: [{
      dissname: '老歌合集',
      logo: 'http://p.qpic.cn/music_cover/abc/300',
      songnum: 1,
      songlist: [
        { songmid: '00oldSong', songname: '老歌', singer: [{ name: '某人' }], albumname: '老专辑', interval: 200 },
      ],
    }],
  });
  httpHandlers = [
    { test: (u) => u.indexOf('fcg_ucc_getcdinfo_byids_cp') >= 0, respond: () => ({ body: QQ_NO_ALBUM }) },
    { test: () => true, respond: () => ({ body: '{}' }) },
  ];
  const pl = api.getPlaylist('qq:999');
  ok(pl && pl.tracks[0].coverUrl === 'https://p.qpic.cn/music_cover/abc/300',
    '缺 albummid 时退回歌单封面（http 升级成 https），不留空占位');
}

// ---- 19. 外部歌单曲目开播：走酷我 / 咪咕解析链 ----
// QQ 侧没有直链（匿名 vkey 已封），必须由解析链接管；这里只有咪咕能命中。
{
  api.resetCooldown();
  config = { unblockQuality: '320' };
  httpHandlers = miguHandlers('bin', 206);
  httpCalls.length = 0;
  const qqTrack = { id: 'qq:003Et45Q22uBOv', title: '淘汰', artist: '陈奕迅', durationMs: 285000 };
  const s = api.getStreamUrl(qqTrack);
  ok(s && s.url.indexOf('migu.cn') >= 0 && s.source === 'resolved',
    'qq: 曲目跳过官方查询，直接由解析链拿到直链');
  ok(!httpCalls.some((u) => u.indexOf('/eapi/song/enhance/player/url') >= 0),
    '不向网易云查 qq: 曲目的播放地址（那边根本没有这个 id）');
}

// ---- 20. 解析开关关闭时的 QQ 曲目：明确报错，不放 30 秒试听 ----
{
  config = { unblock: 'off' };
  let msg = '';
  try {
    api.qqStream({ id: 'qq:003Et45Q22uBOv', title: '淘汰', artist: '陈奕迅', durationMs: 285000 });
  } catch (e) {
    msg = String(e && e.message);
  }
  ok(msg.indexOf('受限歌曲解析') >= 0,
    '关掉解析后 QQ 曲目报「需要受限歌曲解析」，而不是静默失败');
}

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail > 0 ? 1 : 0);
