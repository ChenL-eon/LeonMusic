// QQ 音乐音源插件单测（1.0.0）：mock leon 宿主后直接跑 builtin/qq/index.js。
// 用 node 执行：node test_qq_plugin.cjs
//
// 覆盖：
//   1. search：musicu 主接口解析、封面拼接、分页参数
//   2. search：主接口频控（code!=0）时退到 client_search_cp GET 端点
//   3. search：真的没结果返回 []；两端都失败才抛可操作的错误
//   4. getLyric：base64 → UTF-8 LRC
//   5. getStreamUrl：unblock=off 报错、酷我命中即收工、低规格继续问咪咕
//   6. getStreamUrl：qualityHint 覆盖 999、两源全空明确报错
//   7. 名称匹配：Live / 翻唱版本拒绝

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const script = fs.readFileSync(
  path.join(__dirname, '../src-tauri/src/plugin/builtin/qq/index.js'),
  'utf8',
);

// ---- mock 宿主 ----
let config = {};
let handlers = []; // { test: (url, body) => bool, respond: (url, body) => { body, status? } }
const calls = [];

function dispatch(url, body) {
  calls.push({ url, body });
  const h = handlers.find((x) => x.test(url, body));
  return h ? h.respond(url, body) : { body: '{}', status: 200 };
}

const utf8Decode = (s) =>
  Buffer.from([...String(s)].map((c) => c.charCodeAt(0) & 0xff), 'latin1').toString('utf8');

const leon = {
  config: { get: (k) => (k in config ? config[k] : '') },
  md5: (s) => crypto.createHash('md5').update(String(s)).digest('hex'),
  http: {
    get: (url, opts) => dispatch(url, '').body,
    getRaw: (url, opts) => {
      const r = dispatch(url, '');
      return { status: r.status || 200, headers: {}, body: r.body };
    },
    getEx: (url, opts) => ({ status: 200, headers: {}, cookies: {}, body: '' }),
    postForm: (url, body) => dispatch(url, body).body,
    postFormEx: (url, body, headers) => {
      const r = dispatch(url, body);
      return { status: r.status || 200, headers: {}, cookies: {}, body: r.body };
    },
  },
  crypto: {
    utf8Decode,
    aesEcbHex: () => crypto.randomBytes(16).toString('hex'),
    rsaNoPadHex: () => '00'.repeat(128),
  },
};

const factory = new Function('leon', script + `
  return {
    search, getLyric, getStreamUrl, base64Decode,
    normalizeName, hasVersionMark, gdCandidateMatches,
    resetCooldown: function () { bodianCooldownUntil = 0; bodianUnlocked = false; }
  };
`);
const api = factory(leon);

let pass = 0;
let fail = 0;
function ok(cond, name) {
  if (cond) { pass++; console.log('  ok  ' + name); }
  else { fail++; console.log('  FAIL ' + name); }
}

function reset(extraConfig) {
  config = Object.assign({ unblock: 'on', level: 'exhigh', unblockQuality: '320' }, extraConfig || {});
  handlers = [];
  calls.length = 0;
  api.resetCooldown();
}

function song(over) {
  return Object.assign({
    mid: '0039MnYb0qxYhV',
    title: '晴天',
    singer: [{ name: '周杰伦' }],
    album: { name: '叶惠美', mid: '000MkMni19ClKG' },
    interval: 269,
  }, over || {});
}
function musicuResp(list, opts) {
  const o = opts || {};
  return {
    code: o.topCode === undefined ? 0 : o.topCode,
    req: {
      code: o.reqCode === undefined ? 0 : o.reqCode,
      data: { body: { song: { list } } },
    },
  };
}

// ---------------------------------------------------------------------------
console.log('\n[1] search：主接口解析');

reset();
handlers.push({
  test: (url, body) => url.indexOf('u.y.qq.com/cgi-bin/musicu.fcg') >= 0,
  respond: (url, body) => {
    const p = JSON.parse(body);
    ok(p.req.module === 'music.search.SearchCgiService', '  模块名正确');
    ok(p.req.param.query === '晴天 周杰伦' && p.req.param.page_num === 2 &&
       p.req.param.num_per_page === 40, '  关键词 / 分页 / 条数透传');
    return { body: JSON.stringify(musicuResp([song()])) };
  },
});
let r = api.search('晴天 周杰伦', 2, 40);
ok(r.length === 1, '返回 1 首');
ok(r[0].id === '0039MnYb0qxYhV' && r[0].title === '晴天' && r[0].artist === '周杰伦', 'id / 歌名 / 歌手');
ok(r[0].album === '叶惠美' && r[0].durationMs === 269000, '专辑 / 时长（秒→毫秒）');
ok(r[0].coverUrl === 'https://y.gtimg.cn/music/photo_new/T002R800x800M000000MkMni19ClKG.jpg', '封面 URL 拼接（800×800，CDN 域名，无需 Referer）');
ok(api.search('', 1, 30).length === 0, '空关键词不发请求');

// ---------------------------------------------------------------------------
console.log('\n[2] search：频控 → GET 兜底');

reset();
handlers.push({
  test: (url) => url.indexOf('musicu.fcg') >= 0,
  respond: () => ({ body: JSON.stringify({ code: 1000001 }) }),
});
handlers.push({
  test: (url) => url.indexOf('client_search_cp') >= 0,
  respond: (url) => {
    ok(url.indexOf('w=%E6%99%B4%E5%A4%A9') >= 0, '  兜底端点关键词已编码');
    return { body: JSON.stringify({ code: 0, data: { song: { list: [{
      songmid: '0039MnYb0qxYhV', songname: '晴天',
      singer: [{ name: '周杰伦' }], albumname: '叶惠美',
      albummid: '000MkMni19ClKG', interval: 269,
    }] } } }) };
  },
});
r = api.search('晴天', 1, 30);
ok(r.length === 1 && r[0].title === '晴天' && r[0].coverUrl.indexOf('000MkMni19ClKG') > 0,
   '主接口频控时兜底端点生效');

// ---------------------------------------------------------------------------
console.log('\n[3] search：空结果 / 双端失败');

reset();
handlers.push({
  test: (url) => url.indexOf('musicu.fcg') >= 0,
  respond: () => ({ body: JSON.stringify(musicuResp([])) }),
});
handlers.push({
  test: (url) => url.indexOf('client_search_cp') >= 0,
  respond: () => ({ body: JSON.stringify({ code: 0, data: { song: { list: [] } } }) }),
});
ok(api.search('不存在的歌xyzzy', 1, 30).length === 0, '真的没结果：返回空数组，不报错');

reset();
handlers.push({
  test: (url) => url.indexOf('musicu.fcg') >= 0,
  respond: () => ({ body: 'not json' }),
});
handlers.push({
  test: (url) => url.indexOf('client_search_cp') >= 0,
  respond: () => ({ body: '<html>503</html>' }),
});
let threw = '';
try { api.search('晴天', 1, 30); } catch (e) { threw = e.message; }
ok(threw.indexOf('频控') >= 0, '两端都失败：抛出可操作的错误而不是静默空结果');

// ---------------------------------------------------------------------------
console.log('\n[4] getLyric：base64 → LRC');

reset();
const lrc = '[ti:晴天]\n[ar:周杰伦]\n[00:06.75]故事的小黄花';
handlers.push({
  test: (url, body) => url.indexOf('musicu.fcg') >= 0,
  respond: (url, body) => {
    const p = JSON.parse(body);
    ok(p.req.module === 'music.musichallSong.PlayLyricInfo', '  歌词模块名正确');
    ok(p.req.param.songMID === '0039MnYb0qxYhV', '  只传 songMID 即可取词');
    return { body: JSON.stringify({ code: 0, req: { code: 0, data: {
      lyric: Buffer.from(lrc, 'utf8').toString('base64'),
    } } }) };
  },
});
ok(api.getLyric({ id: '0039MnYb0qxYhV' }) === lrc, '中文 LRC 无损解码');
ok(api.getLyric({ id: 'qq:0039MnYb0qxYhV' }) === lrc, 'QQ 歌单导入曲目带前缀也能取词');
ok(api.getLyric({ id: '' }) === '', '空 id 不发请求');
ok(api.base64Decode('') === '', 'base64 空串安全');

// ---------------------------------------------------------------------------
console.log('\n[5] 名称匹配：拒绝非正式版本');

reset();
ok(api.hasVersionMark('晴天 (Live)') && !api.hasVersionMark('晴天'), 'Live 版本词识别');
ok(api.normalizeName('晴天 (Live)') === api.normalizeName('晴天'), '剥括号后同形（因此需要版本词拦截）');
ok(!api.gdCandidateMatches(
  { title: '晴天', artist: '周杰伦' },
  { name: '晴天 (Live)', artist: '周杰伦' }
), '目标非 Live 时拒绝现场版');
ok(api.gdCandidateMatches(
  { title: '晴天', artist: '周杰伦' },
  { name: '晴天', artist: '周杰伦' }
), '正式版同名同歌手通过');
ok(!api.gdCandidateMatches(
  { title: '晴天', artist: '周杰伦' },
  { name: '晴天', artist: '某翻唱者' }
), '歌手不符拒绝');

// ---------------------------------------------------------------------------
console.log('\n[6] getStreamUrl：解析链分派');

function mockKuwo(audioUrl, list) {
  handlers.push({
    test: (url) => url.indexOf('search.kuwo.cn/r.s') >= 0,
    respond: () => ({ body: JSON.stringify({ content: [{}, { musicpage: { abslist: list } }] }) }),
  });
  handlers.push({
    test: (url) => url.indexOf('service/advert/watch') >= 0,
    respond: () => ({ body: JSON.stringify({ code: 200 }) }),
  });
  handlers.push({
    test: (url) => url.indexOf('audioUrl') >= 0,
    respond: (url) => {
      calls.push({ url, body: '' });
      return { body: JSON.stringify({ code: 200, data: { audioUrl } }) };
    },
  });
}
const kuwoHit = [{ SONGNAME: '晴天', ARTIST: '周杰伦', MUSICRID: '228908', DURATION: '269', ALBUM: '叶惠美' }];
const track = { id: '0039MnYb0qxYhV', title: '晴天', artist: '周杰伦', durationMs: 269000 };

reset();
config.unblock = 'off';
threw = '';
try { api.getStreamUrl(track); } catch (e) { threw = e.message; }
ok(threw.indexOf('受限歌曲解析') >= 0, 'unblock=off 时明确报错，不静默失败');

reset();
mockKuwo('https://example.com/a.mp3', kuwoHit);
let out = api.getStreamUrl(track);
ok(out.url === 'https://example.com/a.mp3' && out.format === 'mp3' && out.bitrate === 320000,
   '酷我命中：返回 320k 与正确规格');
ok(out.source === 'resolved' && out.trial === false && out.requested === 'exhigh',
   '规格字段如实标注（不是 official / 不是试听）');
ok(!calls.some((c) => c.url.indexOf('migu.cn') >= 0), '命中即收工，不打扰咪咕');

reset({ level: 'lossless', unblockQuality: '999' });
mockKuwo('https://example.com/a.mp3', kuwoHit);
out = api.getStreamUrl(track);
ok(out.downgraded === true, '选无损却只拿到 MP3：如实标记 downgraded');

reset();
mockKuwo('https://example.com/a.mp3', []);
handlers.push({
  test: (url) => url.indexOf('migu.cn') >= 0,
  respond: () => ({ body: '{}' }),
});
threw = '';
try { api.getStreamUrl(track); } catch (e) { threw = e.message; }
ok(threw.indexOf('酷我 / 咪咕都没有找到') >= 0, '两源全空：明确报错而不是放试听');

reset({ level: 'lossless', unblockQuality: '999' });
mockKuwo('https://example.com/a.mp3', kuwoHit);
out = api.getStreamUrl(Object.assign({}, track, { qualityHint: 'exhigh' }));
ok(out.requested === 'lossless', '降级重试仍显示用户原本选的档位');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
