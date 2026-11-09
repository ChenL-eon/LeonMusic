// LeonMusic 音源插件：QQ 音乐（内置协议）
//
// 目的：补网易云曲库缺版权的那一部分歌。只做「搜索 + 歌词 + 开播」，
// 不涉及登录、收藏、云端歌单，也不会往任何账号写数据。
//
// 三个接口（全部匿名可用）：
//   搜索  POST u.y.qq.com/cgi-bin/musicu.fcg  DoSearchForQQMusicDesktop
//   兜底  GET  c.y.qq.com/soso/fcgi-bin/client_search_cp  （主接口频控时换端点）
//   歌词  POST u.y.qq.com/cgi-bin/musicu.fcg  GetPlayLyricInfo → base64 LRC
//
// 播放直链：QQ 侧已关（vkey 匿名返回 500003，旧 C400 链接 403），所以
// `getStreamUrl` 按「歌名 + 歌手」走第三方解析链，与 netease 插件里
// QQ 导入曲目用的是同一套：酷我 BD（bodian）领头、咪咕补位。
// 2026-11-09 实测 60 首（周杰伦 / 陈奕迅 / 林俊杰 / 米津玄師 /
// Taylor Swift 各 12 首）全部拿到 320k 及以上直链。
//
// 插件沙箱里没有 require，各插件是独立的 JS 文件，所以解析链这段与
// netease 插件是刻意保持一致的副本；改一处时记得同步另一处。

var QQ_API = 'https://u.y.qq.com/cgi-bin/musicu.fcg';
var QQ_SEARCH_GET = 'https://c.y.qq.com/soso/fcgi-bin/client_search_cp';
var QQ_HEADERS = {
  'content-type': 'application/json',
  Referer: 'https://y.qq.com/',
  'user-agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)'
};

// ---------------------------------------------------------------------------
// 配置与通用工具
// ---------------------------------------------------------------------------

/** 空配置 = 开启；只有显式写 off / 0 / false 才关闭 */
function unblockOn() {
  var v = String(leon.config.get('unblock') || '').toLowerCase();
  return v !== 'off' && v !== '0' && v !== 'false';
}

/** 只有「无损及以上」才值得向解析服务要 flac（br=999）。 */
function unblockBr() {
  var manual = String(leon.config.get('unblockQuality') || '');
  if (/^(128|192|320|999)$/.test(manual)) return manual;
  var want = leon.config.get('level') || 'exhigh';
  if (want === 'lossless' || want === 'hires' || want === 'jymaster') return '999';
  if (want === 'higher') return '192';
  if (want === 'standard') return '128';
  return '320';
}

/** 档位 → 解析请求码率。智能音质降级重试时用它覆盖 unblockQuality。 */
function brForLevel(level) {
  if (level === 'lossless' || level === 'hires' || level === 'jymaster') return '999';
  if (level === 'standard') return '128';
  if (level === 'higher') return '192';
  return '320';
}

/** br 是归一化值：900 = 无损，320 / 128 = MP3。 */
function qualityEnough(actualBr, wantBr) {
  var need = Number(wantBr) === 999 ? 900 : Number(wantBr) || 128;
  return (Number(actualBr) || 0) >= need;
}

/** 从直链扩展名推断实际容器。第三方解析不返回 type 字段。 */
function formatFromUrl(u) {
  var path = String(u || '').split('?')[0];
  var m = /\.([a-z0-9]{2,5})$/i.exec(path);
  var ext = m ? m[1].toLowerCase() : '';
  if (ext === 'flac') return 'flac';
  if (ext === 'm4a' || ext === 'aac') return 'aac';
  if (ext === 'ogg' || ext === 'opus' || ext === 'wav') return ext;
  return 'mp3';
}

function streamResult(url, q) {
  var out = { url: url };
  for (var k in q) {
    if (Object.prototype.hasOwnProperty.call(q, k)) out[k] = q[k];
  }
  return out;
}

/** 专辑封面 CDN：不需要 Referer，直出 JPEG。
 *  必须用 albummid（用 songmid 拼会 404）。取 800×800：300 在播放页 /
 *  高 DPI 屏上发糊，1000 会 404，1500 单张约 550 KB 太重。与 netease
 *  插件外部歌单分支的 qqCoverUrl 保持一致，改域名 / 尺寸时两处都要动。 */
function coverUrl(albumMid) {
  if (!albumMid) return '';
  return 'https://y.gtimg.cn/music/photo_new/T002R800x800M000' + albumMid + '.jpg';
}

function singerNames(list) {
  var out = [];
  for (var i = 0; i < (list || []).length; i++) {
    if (list[i] && list[i].name) out.push(list[i].name);
  }
  return out.join('/');
}

// ---------------------------------------------------------------------------
// QQ 官方接口
// ---------------------------------------------------------------------------

/** musicu.fcg：所有请求都走这一个 CGI，module / method 决定业务。
 *  用 postFormEx 而不是 post，是为了带上 Referer 与浏览器 UA。 */
function qqCall(moduleName, method, param, lyric) {
  // 搜索与歌词两个业务的 comm 形态不同，照实测原样发，别自作主张统一
  var comm = lyric ? { ct: 24, cv: 0 } : { ct: '19', cv: '1859', uin: '0' };
  var payload = {
    comm: comm,
    req: { module: moduleName, method: method, param: param }
  };
  var r = leon.http.postFormEx(QQ_API, JSON.stringify(payload), QQ_HEADERS);
  var text = String((r && r.body) || '');
  var d = null;
  try { d = JSON.parse(text); } catch (e) { return null; }
  return d;
}

/** 主搜索：返回 { ok, list }。ok=false 表示请求失败 / 被频控，
 *  ok=true 且 list 为空才是「真的没搜到」——这两种情况要区别对待。 */
function qqSearchMusicu(keyword, page, size) {
  var param = {
    grp: 1,
    num_per_page: size,
    page_num: page,
    query: String(keyword || ''),
    search_type: 0
  };
  var d = null;
  try {
    d = qqCall('music.search.SearchCgiService', 'DoSearchForQQMusicDesktop', param, false);
  } catch (e) {
    return { ok: false, list: [] };
  }
  // 频控 / 风控时顶层 code 或 req.code 会非 0，也可能整个 data 缺失
  if (!d || d.code !== 0) return { ok: false, list: [] };
  var req = d.req || {};
  if (req.code !== 0) return { ok: false, list: [] };
  var body = ((req.data || {}).body || {});
  var song = body.song || {};
  var items = song.list || [];
  var out = [];
  for (var i = 0; i < items.length; i++) {
    var s = items[i] || {};
    if (!s.mid) continue;
    var al = s.album || {};
    out.push({
      id: String(s.mid),
      title: s.title || s.name || '',
      artist: singerNames(s.singer),
      album: al.name || '',
      durationMs: (Number(s.interval) || 0) * 1000,
      coverUrl: coverUrl(al.mid || al.pmid)
    });
  }
  return { ok: true, list: out };
}

/** 频控兜底：换一个独立端点（client_search_cp），字段命名与 musicu 不同。
 *  实测连发 8 次主接口后开始返回空，GET 端点仍可用。 */
function qqSearchFallback(keyword, page, size) {
  var url =
    QQ_SEARCH_GET +
    '?format=json&p=' + page + '&n=' + size +
    '&w=' + encodeURIComponent(String(keyword || ''));
  var text = '';
  try {
    text = String(
      leon.http.get(url, {
        timeoutMs: 6000,
        headers: { Referer: 'https://y.qq.com/', 'user-agent': QQ_HEADERS['user-agent'] }
      }) || ''
    );
  } catch (e) {
    return [];
  }
  var d = null;
  try { d = JSON.parse(text); } catch (e) { return []; }
  var items = (((d && d.data) || {}).song || {}).list || [];
  var out = [];
  for (var i = 0; i < items.length; i++) {
    var s = items[i] || {};
    if (!s.songmid) continue;
    out.push({
      id: String(s.songmid),
      title: s.songname || '',
      artist: singerNames(s.singer),
      album: s.albumname || '',
      durationMs: (Number(s.interval) || 0) * 1000,
      coverUrl: coverUrl(s.albummid)
    });
  }
  return out;
}

/** 协议入口：搜索。宿主会把返回的 id 包成 `qq:<mid>` 存进 Track，
 *  再以 pluginTrackId（原始 mid）传回给本插件。 */
function search(keyword, page, limit) {
  var q = String(keyword || '').trim();
  if (!q) return [];
  var size = Number(limit) || 30;
  var p = Number(page) || 1;

  var main = qqSearchMusicu(q, p, size);
  if (main.ok && main.list.length) return main.list;

  // 主接口失败（频控 / 风控）：换端点重试一次
  var alt = qqSearchFallback(q, p, size);
  if (alt.length) return alt;
  // 主接口明确返回「没有结果」时就是真的没有，别把空结果伪装成故障
  if (main.ok) return [];
  throw new Error('QQ 音乐搜索暂时没有响应（可能触发了频控），请稍后再试');
}

/** 纯 JS base64 解码 → UTF-8 文本。leon.base64 是编码方向，
 *  歌词接口下发的 lyric / trans 字段需要反过来解。 */
var B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
function base64Decode(s) {
  var src = String(s || '').replace(/[^A-Za-z0-9+/=]/g, '');
  var out = [];
  var buf = 0;
  var bits = 0;
  for (var i = 0; i < src.length; i++) {
    var c = src.charAt(i);
    if (c === '=') break;
    var v = B64.indexOf(c);
    if (v < 0) continue;
    buf = (buf << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push(String.fromCharCode((buf >> bits) & 0xff));
    }
  }
  return leon.crypto.utf8Decode(out.join(''));
}

/** 协议入口：歌词。track.id 是 songmid（宿主传 pluginTrackId）。
 *  QQ 歌单导入的曲目由 netease 插件收录，id 会带 `qq:` 前缀；宿主改派时
 *  通常已剥掉，但这里再做一次兜底，避免别的入口直接传全 id 时查不到词。 */
function getLyric(track) {
  var mid = String((track && track.id) || '').replace(/^qq:/, '');
  if (!mid) return '';
  var d = null;
  try {
    d = qqCall(
      'music.musichallSong.PlayLyricInfo',
      'GetPlayLyricInfo',
      { songMID: mid },
      true
    );
  } catch (e) {
    return '';
  }
  var data = ((d && d.req) || {}).data || {};
  var lrc = '';
  try { lrc = base64Decode(data.lyric || ''); } catch (e) { lrc = ''; }
  return lrc;
}

// ---------------------------------------------------------------------------
// 名称匹配（与 netease 插件同一套规则，防止解析到 Live / 翻唱）
// ---------------------------------------------------------------------------

function normalizeName(s) {
  if (!s) return '';
  return String(s)
    .toLowerCase()
    .replace(/[（(【[].*?[)）】\]]/g, '')
    .replace(/[\s\-—_·・'"‘’“”!！?？.,，。&＆+]/g, '');
}

var VERSION_MARK_RE = new RegExp(
  [
    '\\blive\\b', '\\bconcert\\b', '\\bcover(?:ed)?\\b', '\\bremix\\b', '\\bremake\\b',
    '\\binst(?:rumental)?\\b', '\\bkaraoke\\b', '\\bacoustic\\b', '\\bmashup\\b',
    '翻唱', '翻自', '翻弹', '弹唱', '伴奏', '现场版', '演唱会', '音乐节', '现场',
    '串烧', '电音版', 'remix版', '纯音乐版', '重制版', '教学', '教程', '钢琴版'
  ].join('|')
);

function hasVersionMark(name) {
  var s = String(name || '').toLowerCase();
  return !!s && VERSION_MARK_RE.test(s);
}

function gdCandidateMatches(track, item) {
  if (!hasVersionMark(track.title) && hasVersionMark(item && item.name)) return false;
  var want = normalizeName(track.title);
  var got = normalizeName(item && item.name);
  if (!want || !got) return false;
  if (got !== want && got.indexOf(want) < 0 && want.indexOf(got) < 0) return false;

  var candidateArtist = item && item.artist;
  var names = [];
  if (Array.isArray(candidateArtist)) {
    for (var i = 0; i < candidateArtist.length; i++) {
      var a = candidateArtist[i];
      names.push(typeof a === 'string' ? a : (a && a.name) || '');
    }
  } else if (typeof candidateArtist === 'string') {
    names.push(candidateArtist);
  }
  var joined = normalizeName(names.join(' '));
  if (!joined) return true;
  var parts = String(track.artist || '').split(/[\s/、,，]+/);
  for (var j = 0; j < parts.length; j++) {
    var p = normalizeName(parts[j]);
    if (p && (joined.indexOf(p) >= 0 || p.indexOf(joined) >= 0)) return true;
  }
  return false;
}

function decodeHtmlEntities(s) {
  return String(s || '')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&nbsp;/g, ' ');
}

// ---------------------------------------------------------------------------
// 解析后端一：酷我 BD（bodian）
// ---------------------------------------------------------------------------

var KUWO_SEARCH_API =
  'http://search.kuwo.cn/r.s?&correct=1&vipver=1&stype=comprehensive&encoding=utf8' +
  '&rformat=json&mobi=1&show_copyright_off=1&searchapi=6&all=';
var BODIAN_AUDIO_API = 'http://bd-api.kuwo.cn/api/play/music/v2/audioUrl';
var BODIAN_AD_API =
  'http://bd-api.kuwo.cn/api/service/advert/watch?uid=-1&token=' +
  '&timestamp=1724306124436&sign=15a676d66285117ad714e8c8371691da';

var kuwoDeviceId = '';
var bodianUnlocked = false;
var bodianCooldownUntil = 0;

function kuwoDevice() {
  if (!kuwoDeviceId) kuwoDeviceId = String(Math.floor(Math.random() * 100000000000));
  return kuwoDeviceId;
}

function kuwoSearch(query) {
  var q = String(query || '').replace(/\s+/g, ' ').trim();
  if (!q) return [];
  var body = leon.http.get(KUWO_SEARCH_API + encodeURIComponent(q), { timeoutMs: 4000 });
  var json = null;
  try { json = JSON.parse(body); } catch (e) { return []; }
  var page = json && json.content && json.content[1] && json.content[1].musicpage;
  var list = (page && page.abslist) || [];
  var out = [];
  for (var i = 0; i < list.length && out.length < 20; i++) {
    var s = list[i];
    if (!s || !s.SONGNAME || !s.MUSICRID) continue;
    out.push({
      id: String(s.MUSICRID).split('_').pop(),
      name: decodeHtmlEntities(s.SONGNAME),
      artist: decodeHtmlEntities(s.ARTIST),
      durationMs: (Number(s.DURATION) || 0) * 1000,
      album: decodeHtmlEntities(s.ALBUM)
    });
  }
  return out;
}

function pickKuwo(track, list) {
  var want = Number(track.durationMs) || 0;
  for (var i = 0; i < list.length; i++) {
    var item = list[i];
    if (!gdCandidateMatches(track, item)) continue;
    var got = Number(item.durationMs) || 0;
    if (want > 0 && got > 0 && Math.abs(got - want) > 20000) continue;
    return item;
  }
  return null;
}

function kuwoMatch(track) {
  var list = null;
  try {
    list = kuwoSearch(String(track.title || '') + ' ' + String(track.artist || ''));
  } catch (e) {
    bodianCooldownUntil = Date.now() + 2 * 60 * 1000;
    return null;
  }
  var hit = pickKuwo(track, list);
  if (hit) return hit;
  if (track.title) {
    try { list = kuwoSearch(track.title); } catch (e2) {
      bodianCooldownUntil = Date.now() + 2 * 60 * 1000;
      return null;
    }
    return pickKuwo(track, list);
  }
  return null;
}

/** Range 0-1 探测真实文件长度，反推时长是否够整曲（拦 30/60 秒试听片段）。 */
function audioLengthOk(url, br, wantMs) {
  wantMs = Number(wantMs) || 0;
  if (!wantMs) return true;
  var res = null;
  try {
    res = leon.http.getEx(url, { Range: 'bytes=0-1' });
  } catch (e) {
    return true;
  }
  var headers = (res && res.headers) || {};
  var total = 0;
  var m = /\/(\d+)\s*$/.exec(headers['content-range'] || '');
  if (m) total = Number(m[1]);
  else if (headers['content-length']) total = Number(headers['content-length']);
  if (!total || !br) return true;
  var seconds = (total * 8) / (br * 1000);
  return seconds >= (wantMs / 1000) * 0.6;
}

function bodianSign(url) {
  var rest = url.substring(url.indexOf('//') + 2);
  var pathname = rest.substring(rest.indexOf('/')).split('?')[0];
  var signed = url + '&timestamp=' + Date.now();
  var raw = signed.substring(signed.indexOf('?') + 1).replace(/[^a-zA-Z0-9]/g, '');
  var digest = leon.md5('kuwotest' + raw.split('').sort().join('') + pathname);
  return signed + '&sign=' + digest;
}

function bodianHeaders(extra) {
  var h = {
    'user-agent': 'Dart/2.19 (dart:io)',
    plat: 'ar',
    channel: 'aliopen',
    ver: '3.9.0',
    devid: kuwoDevice(),
    'X-Forwarded-For': '1.0.1.114'
  };
  if (extra) {
    for (var k in extra) {
      if (Object.prototype.hasOwnProperty.call(extra, k)) h[k] = extra[k];
    }
  }
  return h;
}

/** 「广告免单」：不打这一步，audioUrl 一律 code 20018「没有解锁付费歌曲」 */
function bodianUnlock() {
  if (bodianUnlocked) return true;
  try {
    var r = leon.http.postFormEx(
      BODIAN_AD_API,
      '{"type":5,"subType":5,"musicId":0,"adToken":""}',
      bodianHeaders({
        'content-type': 'application/json; charset=utf-8',
        qimei36: '1e9970cbcdc20a031dee9f37100017e1840e'
      })
    );
    var json = null;
    try { json = JSON.parse(r && r.body); } catch (e) { json = null; }
    bodianUnlocked = !!(json && json.code === 200);
  } catch (e2) {
    bodianCooldownUntil = Date.now() + 5 * 60 * 1000;
    bodianUnlocked = false;
  }
  return bodianUnlocked;
}

function bodianCall(url) {
  var body = null;
  try {
    body = leon.http.get(bodianSign(url), { timeoutMs: 4500, headers: bodianHeaders() });
  } catch (e) {
    return null;
  }
  try { return JSON.parse(body); } catch (e2) { return null; }
}

function bodianAudio(id, br) {
  var quality = Number(br) >= 900 ? '2000kflac' : '320kmp3';
  var url = BODIAN_AUDIO_API + '?&br=' + quality + '&musicId=' + id;
  var json = bodianCall(url);
  if (json && json.code === 20018 && bodianUnlocked) {
    bodianUnlocked = false;
    if (bodianUnlock()) json = bodianCall(url);
  }
  var audio = (json && json.data && json.data.audioUrl) || '';
  return typeof audio === 'string' && audio.indexOf('http') === 0 ? audio : '';
}

function bodianResolve(track, br) {
  if (Date.now() < bodianCooldownUntil) return null;
  var hit = kuwoMatch(track);
  if (!hit) return null;
  if (!bodianUnlock()) return null;
  var url = bodianAudio(hit.id, br);
  if (!url) return null;
  var path = url.split('?')[0].toLowerCase();
  var declared = /\.flac$/.test(path) ? 900 : 320;
  if (!audioLengthOk(url, declared, track.durationMs)) return null;
  return { url: url, br: declared };
}

// ---------------------------------------------------------------------------
// 解析后端二：咪咕（补酷我无版权的歌）
// ---------------------------------------------------------------------------

var MIGU_SEARCH_API = 'https://app.c.nf.migu.cn/bmw/search/song/v1.0';
var MIGU_URL_API = 'https://c.musicapp.migu.cn/strategy/listen-url/h5/v2.4';
var MIGU_SECURE = 'Jk8qzuePiJ1qE3mDYhLQ3T73DtDoAhLP';
var MIGU_TONES = {
  320: ['%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_128_16_Stero', '%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_320_16_Stero', '.mp3', '.mp3'],
  999: ['%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_128_16_Stero', '%E6%AD%8C%E6%9B%B2%E4%B8%8B%E8%BD%BD/flac', '.mp3', '.flac']
};

function miguSearch(keyword, limit) {
  var body = '';
  try {
    body = String(
      leon.http.get(
        MIGU_SEARCH_API + '?pageNo=1&text=' + encodeURIComponent(String(keyword || '')),
        { timeoutMs: 3500, headers: { Referer: 'https://m.music.migu.cn/' } }
      ) || ''
    );
  } catch (e) {
    return [];
  }
  var d = null;
  try { d = JSON.parse(body); } catch (e2) { return []; }
  var items = (d && d.data && d.data.items) || [];
  var out = [];
  for (var i = 0; i < items.length && out.length < (limit || 20); i++) {
    var s = (items[i] && items[i].song) || {};
    if (!s.contentId) continue;
    var artists = [];
    var sl = s.singerList || [];
    for (var k = 0; k < sl.length; k++) {
      if (sl[k] && sl[k].name) artists.push(sl[k].name);
    }
    out.push({
      contentId: String(s.contentId),
      copyrightId: String(s.copyrightId || ''),
      name: String(s.songName || ''),
      artist: artists.join(' ')
    });
  }
  return out;
}

function miguDecrypt(body) {
  body = String(body || '');
  if (body.length < 8) return null;
  if (body.charCodeAt(0) !== 0xab || body.charCodeAt(1) !== 0xcd || body.charCodeAt(2) !== 1) {
    return null;
  }
  var r = body.charCodeAt(3);
  var out = new Array(body.length - 4);
  for (var i = 4; i < body.length; i++) {
    out[i - 4] = String.fromCharCode(
      (body.charCodeAt(i) + r - MIGU_SECURE.charCodeAt((i - 4) % MIGU_SECURE.length)) & 0xff
    );
  }
  return leon.crypto.utf8Decode(out.join(''));
}

function miguUrl(contentId, copyrightId) {
  var qs =
    'contentId=' + encodeURIComponent(contentId) +
    '&copyrightId=' + encodeURIComponent(copyrightId || contentId) +
    '&resourceType=2&netType=01&toneFlag=PQ&scene=&lowerQualityContentId=' +
    encodeURIComponent(contentId);
  var raw = null;
  try {
    raw = leon.http.getRaw(MIGU_URL_API + '?' + qs, {
      timeoutMs: 3500,
      headers: {
        birth: 'h5page',
        channel: '014X031',
        Referer: 'https://y.migu.cn/',
        'location-data': '30.6698676660,104.1229614820'
      }
    });
  } catch (e) {
    return '';
  }
  var json = null;
  try { json = JSON.parse(miguDecrypt(raw && raw.body)); } catch (e2) { return ''; }
  var url = (json && json.data && json.data.url) || '';
  return url.indexOf('http') === 0 ? url.split('?')[0] : '';
}

function miguProbe(url) {
  try {
    var r = leon.http.getRaw(url, { timeoutMs: 3500, headers: { Range: 'bytes=0-1023' } });
    return !!r && (r.status === 200 || r.status === 206);
  } catch (e) {
    return false;
  }
}

function miguResolve(track, br) {
  br = Number(br) || 0;
  var query = (track.title + ' ' + (track.artist || '')).trim();
  if (query.length < 2) return null;
  var hit = null;
  var list = miguSearch(query);
  for (var i = 0; i < list.length; i++) {
    if (gdCandidateMatches(track, list[i])) { hit = list[i]; break; }
  }
  if (!hit && track.title && track.title !== query) {
    list = miguSearch(track.title);
    for (var j = 0; j < list.length; j++) {
      if (gdCandidateMatches(track, list[j])) { hit = list[j]; break; }
    }
  }
  if (!hit) return null;
  var base = miguUrl(hit.contentId, hit.copyrightId);
  if (!base) return null;
  var finalUrl = base;
  var finalBr = 128;
  var wants = br === 999 ? ['999', '320'] : br === 320 || br === 192 ? ['320'] : [];
  for (var w = 0; w < wants.length; w++) {
    var rule = MIGU_TONES[wants[w]];
    var cand = base.replace(rule[0], rule[1]).replace(/\.mp3$/, rule[3]);
    if (cand !== base && miguProbe(cand)) {
      finalUrl = cand;
      finalBr = wants[w] === '999' ? 900 : 320;
      break;
    }
  }
  return { url: finalUrl, br: finalBr };
}

// ---------------------------------------------------------------------------
// 协议入口：开播
// ---------------------------------------------------------------------------

/** QQ 曲目没有官方直链，按「酷我 BD → 咪咕」取完整链。
 *  `qualityHint` 是宿主「智能音质」降级重试时指定的本次档位，
 *  这时要按该档位重新要码率，别沿用配置里的 999 白等一轮。 */
function getStreamUrl(track) {
  if (!unblockOn()) {
    throw new Error('QQ 音乐曲目只能靠「受限歌曲解析」取直链，请在插件设置里打开');
  }
  var want = leon.config.get('level') || 'exhigh';
  var hint = track && track.qualityHint ? String(track.qualityHint) : '';
  var attempt = hint || want;
  var br = hint ? brForLevel(attempt) : unblockBr();

  var backends = [bodianResolve, miguResolve];
  var best = null;
  for (var i = 0; i < backends.length; i++) {
    var r = null;
    try {
      r = backends[i](track, br);
    } catch (e) {
      r = null; // 单个后端异常不能挡住下一个
    }
    if (!r || !r.url) continue;
    if (qualityEnough(r.br, br)) { best = r; break; }
    if (!best || (Number(r.br) || 0) > (Number(best.br) || 0)) best = r;
  }
  if (!best) throw new Error('这首歌在酷我 / 咪咕都没有找到可用音源');

  var fmt = formatFromUrl(best.url);
  return streamResult(best.url, {
    source: 'resolved',
    bitrate: best.br ? Math.round(best.br * 1000) : null,
    format: fmt,
    trial: false,
    requested: want,
    downgraded:
      (want === 'lossless' || want === 'hires' || want === 'jymaster') && fmt !== 'flac'
  });
}
