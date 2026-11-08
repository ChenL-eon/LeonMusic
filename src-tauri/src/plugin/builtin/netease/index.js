// LeonMusic 音源插件：网易云音乐（内置协议）
//
// 不再依赖自建 Meting 服务，直接走网易云 web 端接口。加密方案与
// NeteaseCloudMusicApi（VutronMusic 内置的那个）完全一致：
//
//   weapi = AES-128-CBC(presetKey) → AES-128-CBC(随机 secretKey) → RSA/NoPadding
//   eapi  = AES-128-ECB(固定 key)，明文里拼 md5 摘要做校验
//
// 沙箱里没有 Node 的 crypto，算法原语由宿主 `leon.crypto.*` 提供，
// 这个脚本只负责拼协议和解析响应。
//
// 匿名即可搜索 / 取试听直链 / 取歌词；在设置里扫码登录（或填 cookie）
// 可以拿到账号对应的完整曲目。官方不给全曲时，可选地把取链交给第三方
// 解析服务（见下方「受限歌曲解析」），关掉开关就完全只走官方链路。

var API = 'https://music.163.com';
// 听歌打卡专用域名（官方客户端上报播放日志的地方），见下方「听歌打卡」
var CLIENTLOG = 'https://clientlog.music.163.com';
var IV = '0102030405060708';
var PRESET_KEY = '0CoJUm6Qyw8W8jud';
var EAPI_KEY = 'e82ckenh8dichen8';

// 网易云 web 端的固定公钥（1024 位）
var PUBLIC_KEY = [
  '-----BEGIN PUBLIC KEY-----',
  'MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDgtQn2JZ34ZC28NWYpAUd98iZ3',
  '7BUrX/aKzmFbt7clFSs6sXqHauqKWqdtLkF2KexO40H1YTX8z2lSgBBOAxLsvakl',
  'V8k4cBFK9snQXE9/DDaFt6Rr7iVZMldczhC0JNgTz+SHXT6CBHuX3e9SdB1Ua44o',
  'ncaTWz7OBGLbCiK45wIDAQAB',
  '-----END PUBLIC KEY-----'
].join('\n');

// 网易云 /api/song/enhance/player/url/v1 的 level，从高到低。
// 对齐 App 里「音质选择」的六档：标准 / 较高 / 极高 / 无损 / Hi-Res / 高清臻音。
// 沉浸环绕声(sky)、杜比全景声(dolby) 返回的是多声道/特殊封装，需要专门解码设备，
// 本项目的 symphonia 解码链路不保证能播，故不列入。
// 匿名用户拿不到高码率时服务端会自己降，但整档拿不到（fee=1 的曲目）会返回 null，
// 所以还要逐档重试。
var LEVELS = ['jymaster', 'hires', 'lossless', 'exhigh', 'higher', 'standard'];

/** 选中档位优先，其余只往「更低」的方向降级。
 *  选了标准再去试高清臻音既浪费请求，也违背用户的音质/流量意图。
 *  配置里出现旧版本或手写非法值时回落到默认 exhigh。 */
function levelFallback(want) {
  var start = LEVELS.indexOf(want);
  if (start < 0) start = LEVELS.indexOf('exhigh');
  return LEVELS.slice(start);
}

/** 只有「无损及以上」才值得向解析服务要 flac（br=999）。
 *  实测 GD `br=999` 对 VIP 曲会下发真 FLAC（16bit/44.1kHz 全曲），
 *  `br=320` 则是 320k MP3；官方 eapi 的 lossless/hires/jymaster 会静默降成 320k。
 *  前端「在线音质」选档时会同步写 unblockQuality，这里只做配置缺失时的兜底。 */
function unblockBr() {
  var manual = String(leon.config.get('unblockQuality') || '');
  if (/^(128|192|320|999)$/.test(manual)) return manual;
  var want = leon.config.get('level') || 'exhigh';
  if (want === 'lossless' || want === 'hires' || want === 'jymaster') return '999';
  if (want === 'higher') return '192';
  if (want === 'standard') return '128';
  return '320';
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

/** 把服务端真正下发的东西记下来，交给宿主透传到播放条。
 *  `used !== requested` 是逐档降级；`wantsLossless && 仍是 mp3/<=320k`
 *  是官方那次「code 200 但静默忽略 level」——旧版完全看不出来。 */
function qualityFromOfficial(item, requested, used) {
  var fmt = String((item && (item.type || item.encodeType)) || '').toLowerCase();
  if (!fmt) fmt = formatFromUrl(item && item.url);
  if (fmt === 'flac') fmt = 'flac';
  else if (fmt === 'aac' || fmt === 'm4a') fmt = 'aac';
  else fmt = 'mp3';
  var br = Number(item && item.br) || 0;
  var wantsLossless =
    requested === 'lossless' || requested === 'hires' || requested === 'jymaster';
  var isLossless = fmt === 'flac' || br > 400000;
  return {
    source: 'official',
    bitrate: br || null,
    format: fmt,
    trial: false,
    requested: requested,
    downgraded: used !== requested || (wantsLossless && !isLossless)
  };
}

function streamResult(url, q) {
  var out = { url: url };
  for (var k in q) {
    if (Object.prototype.hasOwnProperty.call(q, k)) out[k] = q[k];
  }
  return out;
}

// ---------------------------------------------------------------------------
// 基础
// ---------------------------------------------------------------------------

function formValue(s) {
  // base64 只需要转义这三个字符；比 encodeURIComponent 更可控
  return s
    .replace(/\+/g, '%2B')
    .replace(/\//g, '%2F')
    .replace(/=/g, '%3D');
}

// deviceId 必须是服务端认得的形态（32 位十六进制），写个一眼假的
// "LeonMusic" 会让登录请求更容易被判成异常设备。固定值比每次随机更像
// 一台稳定的桌面客户端。
var CLIENT_COOKIE =
  'os=pc; appver=8.9.70; osver=Microsoft-Windows-10-Professional-build-22631-64bit; ' +
  'deviceId=b3d7e1f04c9a2b68e5d0a7c31f9e84d26';

function cleanCookie(s) {
  if (!s) return '';
  var t = String(s).replace(/^\s*cookie:\s*/i, '');
  // 手动粘贴时可能带换行，逐行拼回标准 Cookie 串
  return t.replace(/\s*[\r\n]+\s*/g, '; ').replace(/^\s+|\s+$/g, '');
}

/** 多段 Cookie 按名合并，后面的覆盖前面的，避免同名项重复塞进请求头 */
function mergeCookies() {
  var out = {};
  for (var a = 0; a < arguments.length; a++) {
    var text = cleanCookie(arguments[a]);
    if (!text) continue;
    var parts = text.split(';');
    for (var i = 0; i < parts.length; i++) {
      var p = parts[i];
      var eq = p.indexOf('=');
      if (eq < 1) continue;
      var k = p.slice(0, eq).replace(/^\s+|\s+$/g, '');
      if (k) out[k] = p.slice(eq + 1).replace(/^\s+|\s+$/g, '');
    }
  }
  var list = [];
  for (var k2 in out) {
    if (Object.prototype.hasOwnProperty.call(out, k2)) list.push(k2 + '=' + out[k2]);
  }
  return list.join('; ');
}

function cookie() {
  // os=pc 让服务端按桌面版下发；不带任何 cookie 也能用，但字段会少一些
  return mergeCookies(CLIENT_COOKIE, leon.config.get('cookie'));
}

/** 扫码 / 验证码登录链路用**纯 web 指纹**：UA 已经是浏览器，cookie 也
 *  必须只有浏览器会话（browserSession 预热拿到的那份）。再混进 os=osx
 *  这类客户端身份，UA 与 os 就不自洽，风控按异常环境处理（-460 / 8810
 *  一类）。普通 API（post）仍走 cookie() 里的客户端指纹，两不相干。 */
function anonCookie() {
  return '';
}

function post(path, body) {
  return leon.http.postForm(API + path, body, {
    Referer: API,
    Origin: API,
    Cookie: cookie()
  });
}

/** 推荐/账号系接口专用：纯登录 cookie（web 指纹）请求。
 *  登录 cookie 就是纯 web 会话所得，每日推荐这类风控较严的接口
 *  再混进 os=pc 客户端指纹会被判异常环境——实测表现恰好是
 *  「匿名个性推荐正常、登录后每日推荐两个接口全空」。保持与
 *  登录环境同构的 web 指纹通过率最高（与网易云 web 端行为一致）。 */
function postWeb(path, body) {
  // postFormEx 返回 {status, headers, cookies, body} 结构——推荐流只需要 body
  var res = leon.http.postFormEx(API + path, body, {
    Referer: API,
    Origin: API,
    Cookie: mergeCookies(leon.config.get('cookie')),
    'Cache-Control': 'no-cache'
  });
  return res && res.body != null ? res.body : res;
}

/** 从 cookie 里取 `__csrf`。登录后部分 POST 接口会校验它和 body 里的值一致 */
function csrfToken() {
  var m = /(?:^|;\s*)__csrf=([^;]+)/.exec(cookie());
  return m ? m[1] : '';
}

/** weapi：返回 form body 字符串 */
function weapi(obj) {
  var text = JSON.stringify(obj);
  // 16 个 base64 字符 —— 与 NeteaseCloudMusicApi 的 secretKey 生成方式一致
  var secret = leon.crypto.randomB64(16).slice(0, 16);
  var once = leon.crypto.aesCbcB64(text, PRESET_KEY, IV);
  var params = leon.crypto.aesCbcB64(once, secret, IV);
  var reversed = secret.split('').reverse().join('');
  var encSecKey = leon.crypto.rsaNoPadHex(reversed, PUBLIC_KEY);
  return 'params=' + formValue(params) + '&encSecKey=' + encSecKey;
}

/** eapi：返回 form body 字符串 */
function eapi(path, obj) {
  var text = JSON.stringify(obj);
  var digest = leon.md5('nobody' + path + 'use' + text + 'md5forencrypt');
  var data = path + '-36cd479b6b5-' + text + '-36cd479b6b5-' + digest;
  return 'params=' + leon.crypto.aesEcbHex(data, EAPI_KEY);
}

function describe(data) {
  if (!data) return '响应为空';
  if (data.msg) return data.msg;
  if (data.message) return data.message;
  if (data.code) return 'code=' + data.code;
  return '未知错误';
}

function httpsUrl(u) {
  if (!u) return '';
  // 封面走 WebView，Android 的明文流量策略会拦 http
  return u.indexOf('http://') === 0 ? 'https://' + u.slice(7) : u;
}

function artistNames(list) {
  if (!list || !list.length) return '';
  var out = [];
  for (var i = 0; i < list.length; i++) {
    var a = list[i];
    if (typeof a === 'string') out.push(a);
    else if (a && a.name) out.push(a.name);
  }
  return out.join(' / ');
}

// ---------------------------------------------------------------------------
// Meting 兜底（可选）
// ---------------------------------------------------------------------------

function metingBase() {
  return leon.config.get('baseUrl') || '';
}

function metingUrl(params) {
  var qs = ['server=netease'];
  for (var k in params) {
    if (!Object.prototype.hasOwnProperty.call(params, k)) continue;
    var v = params[k];
    if (v === undefined || v === null || v === '') continue;
    qs.push(k + '=' + encodeURIComponent(v));
  }
  var base = metingBase();
  var sep = base.indexOf('?') >= 0 ? '&' : '?';
  return base + sep + qs.join('&');
}

function metingSearch(keyword, page, limit) {
  var data = JSON.parse(
    leon.http.get(
      metingUrl({ type: 'search', id: keyword, page: page || 1, limit: limit || 30 })
    )
  );
  if (!data || !data.length) return [];
  var out = [];
  for (var i = 0; i < data.length; i++) {
    var it = data[i];
    var id = it.url_id || it.id || it.song_id || '';
    if (!id) continue;
    var dur = Number(it.duration) || 0;
    out.push({
      id: String(id),
      title: it.name || it.title || '',
      artist: artistNames(it.artist || it.artists || it.singer),
      album: it.album || (it.al && it.al.name) || '',
      durationMs: dur > 0 && dur < 100000 ? dur * 1000 : dur,
      coverUrl: httpsUrl(it.pic || it.picUrl || '')
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// 受限歌曲解析（可选，默认开启）
// ---------------------------------------------------------------------------
//
// 官方 eapi 对 fee=1 的曲目照样返回 code:200，只是给一条 30 秒试听直链：
//   freeTrialInfo: { start: 0, end: 30 }，time: 30040，br: 128012
// 免费曲这几项分别是 null / 0 / 320000，所以判定不需要靠解码时长去猜。
//
// 这里不尝试绕过权限，而是把「取一条完整直链」交给第三方解析服务
// GD 音乐台（AlgerMusicPlayer、UnblockNeteaseMusic 的 pyncmd 都用它）：
//   https://music-api.gdstudio.xyz/api.php?types=url&source=netease&id=...
// 它返回的是 music.126.net 上带时效签名的正版全曲，不是别的平台的翻唱。
// 关掉开关、或后端链全部失败（GD 限流 + 酷我搜不到），都老实回落官方
// 试听（前端会提示只有 30 秒）。

var GD_API = 'https://music-api.gdstudio.xyz/api.php';

/** 空配置 = 开启；只有显式写 off / 0 / false 才关闭 */
function unblockOn() {
  var v = (leon.config.get('unblock') || '').toLowerCase();
  return v !== 'off' && v !== '0' && v !== 'false';
}

/**
 * 解析后端链：解析按后端列表**逐个尝试**，拿到可用直链立即返回，
 * 全部失败才回落官方试听：
 *
 *   gd               —— GD 音乐台（无损优先，但会限流 / 间歇 503）
 *   migu             —— 咪咕直链（内置实现，免登录完整曲，可升无损；自家 CDN 最稳）
 *   kw               —— 酷我直链（内置实现，128k 全曲，GD 挂了它兜底）
 *   kg               —— 酷狗直链（内置实现，UNM 同款 trackercdn 配方，
 *                       免费曲可升 320k / FLAC；酷狗独家 VIP 曲拿不到）
 *   bili             —— bilibili 视频区 DASH（内置实现，FLAC 184k / AAC 83k，
 *                       翻唱过滤 + 标题歌手严格匹配，末位兜底）
 *   meting:<地址>     —— 任意 Meting 兼容后端（自建 Meting-API 等）
 *
 * 默认链 = gd → migu → kw → kg → bili + 「Meting 地址」配置（若已填）。unblockBackends
 * 写了逗号序列时按用户给的顺序整链重置（设置 → 播放 → 解析播放源 生成）；
 * 写 off/none 表示全部关闭，直接跳过解析（受限歌曲只播官方试听）。
 */
function resolveBackends() {
  var list = [];
  var seen = {};
  function push(kind, base) {
    var key = kind + (base || '');
    if (seen[key]) return;
    seen[key] = 1;
    list.push({ kind: kind, base: base || '' });
  }
  push('gd', '');
  push('migu', '');
  push('kw', '');
  push('kg', '');
  push('bili', '');
  if (metingBase()) push('meting', metingBase());

  var manual = String(leon.config.get('unblockBackends') || '').trim();
  if (manual) {
    if (manual === 'off' || manual === 'none') return [];
    list = [];
    seen = {};
    var parts = manual.split(',');
    for (var i = 0; i < parts.length; i++) {
      var seg = parts[i].trim();
      if (!seg) continue;
      if (seg === 'gd') {
        push('gd', '');
        continue;
      }
      if (seg === 'migu' || seg === 'migumusic') {
        push('migu', '');
        continue;
      }
      if (seg === 'kw' || seg === 'kuwo') {
        push('kw', '');
        continue;
      }
      if (seg === 'kg' || seg === 'kugou') {
        push('kg', '');
        continue;
      }
      if (seg === 'bili' || seg === 'bilibili') {
        push('bili', '');
        continue;
      }
      var m = /^meting:(.+)$/.exec(seg);
      if (m && m[1].trim()) push('meting', m[1].trim());
    }
  }
  return list;
}

/** Meting 兼容后端的解析：type=url 直取 + 搜索兜底（同名同歌手校验复用
 *  gdCandidateMatches）。响应形如 [{url, br}]；魔改实例直接 302 到音频流
 *  时 JSON.parse 会失败，视为该后端不可用，交给下一个。 */
function metingResolve(base, track, br) {
  function urlOf(pid) {
    var qs = ['server=netease', 'type=url', 'id=' + encodeURIComponent(pid), 'br=' + encodeURIComponent(br)];
    var sep = base.indexOf('?') >= 0 ? '&' : '?';
    try {
      var data = JSON.parse(leon.http.get(base + sep + qs.join('&'), { timeoutMs: 3500 }));
      if (Array.isArray(data)) data = data[0] || {};
      var u = gdCleanUrl(data.url || '');
      if (!u) return null;
      var bitrate = Number(data.br) || 0;
      return { url: u, br: bitrate > 0 ? bitrate : 0 };
    } catch (e) {
      return null;
    }
  }

  var direct = urlOf(track.id);
  if (direct && direct.url) return direct;

  // 同平台兜底：直取失败但能搜到（url_id 才是取链用的 id）
  var query = (track.title + ' ' + (track.artist || '')).trim();
  if (query.length < 2) return null;
  try {
    var qs2 = ['server=netease', 'type=search', 'id=' + encodeURIComponent(query), 'limit=5'];
    var sep2 = base.indexOf('?') >= 0 ? '&' : '?';
    var list = JSON.parse(leon.http.get(base + sep2 + qs2.join('&'), { timeoutMs: 3500 }));
    if (!list || typeof list.length !== 'number') return null;
    for (var i = 0; i < list.length && i < 5; i++) {
      var it = list[i];
      var rid = (it && (it.url_id || it.id)) || '';
      if (!rid || !gdCandidateMatches(track, it)) continue;
      var one = urlOf(rid);
      if (one && one.url) return one;
    }
  } catch (e) { /* 搜索挂了不影响前面已失败的事实 */ }
  return null;
}

/** 统一解析入口：按后端链逐个尝试。 */
function resolveTrack(track) {
  var br = unblockBr();
  var backends = resolveBackends();
  for (var i = 0; i < backends.length; i++) {
    var r = null;
    try {
      r = backends[i].kind === 'gd'
        ? gdResolve(track, br)
        : backends[i].kind === 'migu'
          ? miguResolve(track, br)
          : backends[i].kind === 'kw'
            ? kwResolve(track)
            : backends[i].kind === 'kg'
              ? kgResolve(track, br)
              : backends[i].kind === 'bili'
                ? biliResolve(track)
                : metingResolve(backends[i].base, track, br);
    } catch (e) {
      r = null; // 单个后端的任何异常都不能影响其它后端
    }
    if (r && r.url) return r;
  }
  return null;
}

/** GD 限流/宕机时的熔断：连续失败到阈值后冷却一段时间直接跳过，
 *  免得每首 VIP 歌都先白等它超时（20s × 2 次请求 = 40s 纯等待）。
 *  进程内存态，重启即重置。 */
var gdFailStreak = 0;
var gdCooldownUntil = 0;
var GD_COOLDOWN_TRIP = 2;
var GD_COOLDOWN_MS = 2 * 60 * 1000;

/** id 级失败缓存：GD 对某首歌超时/拿不到（VIP 歌的常态失败形态），
 *  30 分钟内这首别再撞 GD——首次仍要等一次超时（3.5s），第二次起
 *  直接落到下一个后端，配合宿主的下一首预解析，切歌等待 ≈ 0。 */
var gdFailIds = {};
var GD_ID_FAIL_MS = 30 * 60 * 1000;

function gdGet(params) {
  var qs = [];
  for (var k in params) {
    if (!Object.prototype.hasOwnProperty.call(params, k)) continue;
    var v = params[k];
    if (v === undefined || v === null || v === '') continue;
    qs.push(encodeURIComponent(k) + '=' + encodeURIComponent(v));
  }
  try {
    return JSON.parse(leon.http.get(GD_API + '?' + qs.join('&'), { timeoutMs: 3500 }));
  } catch (e) {
    // 第三方服务会限流 / 挂掉，解析失败不能影响主链路
    return null;
  }
}

function gdCleanUrl(u) {
  if (!u || typeof u !== 'string') return '';
  // 有些响应把斜杠转义成 \/
  var s = u.replace(/\\\//g, '/');
  return s.indexOf('http') === 0 ? s : '';
}

/** 去掉括号里的 (Live) / (翻唱) 备注、空白和标点，用于同名比对 */
function normalizeName(s) {
  if (!s) return '';
  return String(s)
    .toLowerCase()
    .replace(/[（(【[].*?[)）】\]]/g, '')
    .replace(/[\s\-—_·・'"‘’“”!！?？.,，。&＆+]/g, '');
}

/**
 * 版本词标记：标题带这些词 = 现场/翻唱/伴奏等非录音室版本。
 * 2026-11-07 用户反馈：网易无版权歌被兜底链解析成了 Live 版——根因是
 * normalizeName 剥括号后「晴天 (Live)」和「晴天」完全同形，歌手也对
 * （现场版还是原唱），一路绿灯。修正策略：候选原始名（剥括号之前）
 * 含版本词而目标不含 → 直接拒绝，宁可换下一个后端也不放非正式版。
 * 目标自身带标记（网易云曲名就叫「xxx (Live)」的现场专辑曲目）则不拦。
 * 英文词用 \b 防误伤（deliver/lively 之类），中文词直接子串匹配。
 */
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

/**
 * 兜底搜索必须歌名 + 歌手都对上才采用。
 * 只比时长（老版 UnblockNeteaseMusic 的做法）会拿到翻唱 / Live；
 * AlgerMusicPlayer 2026-10 的实测记录里 kuwo 就是「6/6 命中、0/6 时长正确」。
 */
function gdCandidateMatches(track, item) {
  // 版本词拦截：目标无 live/翻唱标记而候选有 → 非正式版，直接拒绝
  //（候选若是「歌名 (Live)」，normalizeName 剥括号后与目标同形，
  //  下面的名字比对根本拦不住——必须在这里按原始名拦）
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
  if (!joined) return true; // 候选没给歌手，歌名严格相等即可

  var parts = String(track.artist || '').split(/[\s/、,，]+/);
  for (var j = 0; j < parts.length; j++) {
    var p = normalizeName(parts[j]);
    if (p && (joined.indexOf(p) >= 0 || p.indexOf(joined) >= 0)) return true;
  }
  return false;
}

/** 返回 `{ url, br }`，而不是裸字符串：播放条要显示真实规格。
 *  `br` 是解析服务回传的实际码率（kbps），flac 常是 600~1000 而非固定档位。
 *  br 由调用方（resolveTrack）统一传入，保持多后端一次计算。 */
function gdResolve(track, br) {
  br = br || unblockBr();
  // 熔断窗口内直接跳过，别再耗超时预算
  if (Date.now() < gdCooldownUntil) return null;
  // 这首歌刚失败过（VIP 歌 GD 拉不动是常态）：30 分钟内换后端，别耗预算
  var blocked = gdFailIds[track.id];
  if (blocked && Date.now() < blocked) return null;

  var result = gdResolveInner(track, br);
  if (result) {
    delete gdFailIds[track.id];
    gdFailStreak = 0;
  } else {
    gdFailIds[track.id] = Date.now() + GD_ID_FAIL_MS;
    gdFailStreak++;
    if (gdFailStreak >= GD_COOLDOWN_TRIP) {
      gdCooldownUntil = Date.now() + GD_COOLDOWN_MS;
      gdFailStreak = 0;
    }
  }
  return result;
}

function gdResolveInner(track, br) {
  // 主路径：直接用网易云 ID 取，返回的就是 126.net 正版文件。
  // gdGet 返回 null = 超时/限流（VIP 歌的失败形态就是 GD 服务端拉网易源
  // 挂起直到超时）——服务不健康时连 search 兜底也别跑了，纯浪费 3.5s。
  var direct = gdGet({ types: 'url', source: 'netease', id: track.id, br: br });
  if (direct === null) return null;
  if (direct.url && (Number(direct.br) || 0) > 0) {
    var u = gdCleanUrl(direct.url);
    if (u) return { url: u, br: Number(direct.br) || 0 };
  }

  // GD 活着但这个 ID 没有（灰色曲 ID 失效）：同平台搜索兜底（不跨平台换源）
  var query = (track.title + ' ' + (track.artist || '')).trim();
  if (query.length < 2) return null;
  var list = gdGet({ types: 'search', source: 'netease', name: query, count: 5, pages: 1 });
  if (!list || typeof list.length !== 'number') return null;
  for (var i = 0; i < list.length && i < 5; i++) {
    var it = list[i];
    if (!it || !it.id || !gdCandidateMatches(track, it)) continue;
    var one = gdGet({ types: 'url', source: it.source || 'netease', id: it.id, br: br });
    if (one && one.url) {
      var oneUrl = gdCleanUrl(one.url);
      if (oneUrl) return { url: oneUrl, br: Number(one.br) || 0 };
    }
  }
  return null;
}

/** 官方返回的这一条是不是「只有几十秒」的试听片段 */
function isTrialUrlItem(item) {
  if (item.freeTrialInfo) return true;
  var time = Number(item.time) || 0;
  if (time > 0 && time < 60000) return true;
  var br = Number(item.br) || 0;
  var size = Number(item.size) || 0;
  return br > 0 && br < 200000 && size > 0 && size < 1024 * 1024;
}

// ---------------------------------------------------------------------------
// 酷我音源（内置第二后端）
// ---------------------------------------------------------------------------
//
// GD 音乐台是免费公共服务，会限流 / 间歇 503（2026-11-06 晚实测：探针刚拿到
// 全曲 FLAC，几分钟后应用内同曲取链就失败回落试听），单点靠不住。
// 酷我有两个无签名的老接口至今存活，凑成一条不依赖任何第三方的独立解析链：
//
//   搜索  https://search.kuwo.cn/r.s?all=<关键词>&ft=music&rformat=json
//         返回「单引号 JSON」（JSON.parse 不认），按条目切分后正则取字段。
//         新版 www.kuwo.cn/api/www 搜索会校验真实 kw_token，伪造 cookie
//         直接 "The request is illegal!"，老 r.s 反而宽容。
//   取链  https://antiserver.kuwo.cn/anti.s?type=convert_url&rid=MUSIC_<id>
//         &format=mp3&response=url
//         响应是纯文本直链（kw-lv.kuwo.cn 带时效签名）。format 只认 mp3：
//         实测 aac 只给 1.7MB 片段、flac 直接拒绝；mp3 稳定全曲
//         （《淘汰》285s → 4.4MB，正好 128kbps）。
//
// 音质只有 128k，所以链路排在 GD 后面：GD 活着拿无损，GD 限流时酷我兜底。

var KW_SEARCH_API = 'https://search.kuwo.cn/r.s';
var KW_ANTI_API = 'https://antiserver.kuwo.cn/anti.s';

/** r.s 的值转普通字符串：\uXXXX、&nbsp;、\' */
function kwDecode(s) {
  return String(s || '')
    .replace(/\\u([0-9a-fA-F]{4})/g, function (_, h) {
      return String.fromCharCode(parseInt(h, 16));
    })
    .replace(/\\'/g, "'")
    .replace(/&nbsp;/g, ' ');
}

/** 酷我搜索：单引号 JSON 按条目切分 + 正则抽字段，
 *  输出 gdCandidateMatches 可用的 { id, name, artist } 形态。 */
function kwSearch(keyword, limit) {
  var qs = [
    'all=' + encodeURIComponent(keyword),
    'ft=music',
    'itemset=web_20160218',
    'client=mp',
    'rformat=json',
    'encoding=utf8',
    'pn=0',
    'rn=' + (limit || 20)
  ];
  var body = '';
  try {
    body = String(leon.http.get(KW_SEARCH_API + '?' + qs.join('&'), { timeoutMs: 3500 }) || '');
  } catch (e) {
    return [];
  }
  var start = body.indexOf("'abslist':[");
  if (start < 0) return [];
  var end = body.lastIndexOf(']');
  if (end <= start) return [];
  var chunks = body.slice(start + 11, end).split('},{');
  var out = [];
  for (var i = 0; i < chunks.length && out.length < (limit || 20); i++) {
    var rid = /'MUSICRID'\s*:\s*'?(?:MUSIC_)?(\d+)'?/.exec(chunks[i]);
    if (!rid) continue;
    var name = /'SONGNAME'\s*:\s*'((?:\\.|[^'\\])*)'/.exec(chunks[i]);
    var artist = /'ARTIST'\s*:\s*'((?:\\.|[^'\\])*)'/.exec(chunks[i]);
    var dur = /'DURATION'\s*:\s*'?(\d+)'?/.exec(chunks[i]);
    out.push({
      id: rid[1],
      name: kwDecode(name ? name[1] : ''),
      artist: kwDecode(artist ? artist[1] : ''),
      duration: dur ? Number(dur[1]) : 0
    });
  }
  return out;
}

/** antiserver 取直链：响应是纯文本 URL，不是 JSON。 */
function kwPlayUrl(rid) {
  var body = '';
  try {
    body = String(
      leon.http.get(
        KW_ANTI_API +
          '?type=convert_url&rid=MUSIC_' + encodeURIComponent(rid) +
          '&format=mp3&response=url',
        { timeoutMs: 3500 }
      ) || ''
    );
  } catch (e) {
    return '';
  }
  var u = body.trim().replace(/^"+|"+$/g, '');
  return u.indexOf('http') === 0 ? u : '';
}

/** 酷我解析：搜索 → 歌名+歌手严格匹配（同 gdCandidateMatches）→ 直链。
 *  匹配不上宁可不兜底，也不放翻唱。
 *  r.s 对「中文歌名 + 英文歌手」这类混合查询经常返回一堆不相干的英文歌
 *  （不是空列表），所以只要第一轮没匹配上就退化成纯歌名再搜一轮，
 *  结果仍走同一套严格校验。 */
function kwTry(list, track) {
  var ms = Number(track.durationMs) || 0;
  for (var i = 0; i < list.length; i++) {
    if (!gdCandidateMatches(track, list[i])) continue;
    // 时长软校验（kw 的 DURATION 是秒）：差 35% 以上视为不同作品——
    // 名字歌手都对上的 Live 版往往靠版本词拦不全，时长能兜住最后一道
    var dur = Number(list[i].duration) || 0;
    if (dur && ms && Math.abs(dur * 1000 - ms) > ms * 0.35) continue;
    var u = kwPlayUrl(list[i].id);
    if (u) return { url: u, br: 128 };
  }
  return null;
}

function kwResolve(track) {
  var query = (track.title + ' ' + (track.artist || '')).trim();
  if (query.length < 2) return null;
  var r = kwTry(kwSearch(query), track);
  if (!r && track.title && track.title !== query) {
    r = kwTry(kwSearch(track.title), track);
  }
  return r;
}

// ---------------------------------------------------------------------------
// 咪咕音源（内置后端，gd 之后 / kw 之前）
// ---------------------------------------------------------------------------
//
// 2026-11-07 全链路实测复活：此前判「咪咕已死」只对老接口 scr_search_tag
// 成立（改版返回 HTML）。新链路全部存活且**免登录**：
//   搜索  app.c.nf.migu.cn/bmw/search/song/v1.0           明文 JSON
//   直链  c.musicapp.migu.cn/strategy/listen-url/h5/v2.4  响应加密（可解）
// VIP 曲免登录下发「60 秒试听」URL，但实测指向完整文件（晴天 4317311B
// ≈ 269.8s 全曲，尾部 Range 206 可达）——「试听」只是业务层声明字段，
// CDN 不限长。路径替换「标清高清/MP3_128_16_Stero → 歌曲下载/flac」
// 可拿真无损（31.5MB 与曲库标称一致）。migu.cn 全域已加入
// is_direct_host 强制直连：走代理的海外出口会被咪咕 440001 地区限制拒绝。
//
// 响应解密（对应上游 miguMusic-api-enhanced 的 url_h5v2.4.ts）：
//   头 4 字节 = 0xAB 0xCD 0x01 + 随机偏移 r，之后每字节
//   out[i] = (data[i+4] + r - key[i % keylen]) & 0xFF，key 固定 32 字节；
//   解出的字节序列是 UTF-8 的 JSON。二进制读取靠宿主 leon.http.getRaw
//   （resp.text() 的 lossy 解码会把字节打碎）+ leon.crypto.utf8Decode。

var MIGU_SEARCH_API = 'https://app.c.nf.migu.cn/bmw/search/song/v1.0';
var MIGU_URL_API = 'https://c.musicapp.migu.cn/strategy/listen-url/h5/v2.4';
var MIGU_SECURE = 'Jk8qzuePiJ1qE3mDYhLQ3T73DtDoAhLP';

// URL 路径里的中文段是 encodeURI 过的形态，替换必须按编码后的串对齐。
// [128k 路径段, 目标路径段, 原扩展名, 目标扩展名]；192 没有独立档位，
// 与 320 同走 MP3_320。
var MIGU_TONES = {
  320: ['%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_128_16_Stero', '%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_320_16_Stero', '.mp3', '.mp3'],
  999: ['%E6%A0%87%E6%B8%85%E9%AB%98%E6%B8%85/MP3_128_16_Stero', '%E6%AD%8C%E6%9B%B2%E4%B8%8B%E8%BD%BD/flac', '.mp3', '.flac']
};

/** 咪咕搜索：返回 gdCandidateMatches 可用的
 *  { contentId, copyrightId, name, artist } 列表。明文 JSON。 */
function miguSearch(keyword, limit) {
  var body = '';
  try {
    body = String(
      leon.http.get(
        MIGU_SEARCH_API +
          '?pageNo=1&text=' + encodeURIComponent(String(keyword || '')),
        { timeoutMs: 3500, headers: { Referer: 'https://m.music.migu.cn/' } }
      ) || ''
    );
  } catch (e) {
    return [];
  }
  var d = null;
  try { d = JSON.parse(body); } catch (e) { return []; }
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

/** 解密 h5v2.4 的加密响应。入参是 getRaw 的 latin1 body；
 *  头部魔法对不上（未加密 / 改版）返回 null，交给上层放弃本后端。 */
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

/** 取播放直链（128k 完整曲基准 URL）。剥掉签名查询参数——实测裸路径
 *  直接 200，带着 channelid/Key 反而多余。失败返回 ''。 */
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
  try { json = JSON.parse(miguDecrypt(raw && raw.body)); } catch (e) { return ''; }
  var url = (json && json.data && json.data.url) || '';
  return url.indexOf('http') === 0 ? url.split('?')[0] : '';
}

/** 1KB Range 探测：路径替换是猜出来的 URL，CDN 上不一定真的有文件
 *  （曲库里不少歌只有 PQ/HQ 没有 SQ）。探测通过才升级，失败退回 128k。 */
function miguProbe(url) {
  try {
    var r = leon.http.getRaw(url, {
      timeoutMs: 3500,
      headers: { Range: 'bytes=0-1023' }
    });
    return !!r && (r.status === 200 || r.status === 206);
  } catch (e) {
    return false;
  }
}

/** 咪咕解析：搜索 → 歌名+歌手严格匹配（同 gdCandidateMatches，宁缺毋滥）
 *  → h5v2.4 取 128k 完整曲 → 按档位路径替换升级（999 无损 / 320 高品，
 *  升级 URL 逐档探测）。返回 { url, br }。 */
function miguResolve(track, br) {
  // unblockBr 返回的是字符串档位（'128'/'320'/'999'），归一化成数字再比档
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
      finalBr = wants[w] === '999' ? 900 : 320; // flac 码率不固定，展示按 ~900k
      break;
    }
  }
  return { url: finalUrl, br: finalBr };
}

// ---------------------------------------------------------------------------
// 酷狗音源（内置后端，VutronMusic / UNM 链里的主力源补位）
// ---------------------------------------------------------------------------
//
// UNM（@unblockneteasemusic/server，VutronMusic 内置解锁用的就是它）的酷狗
// provider 配方，2026-11-08 实测仍然可用：
//
//   搜索  https://mobiles.kugou.com/api/v3/search/song
//         v3 老接口免签名，返回 hash / 320hash / sqhash / album_id /
//         duration（秒）/ filename（"歌手 - 歌名【备注】"）。
//         UNM 用的 http://mobilecdn 同源，但实测偶发连接失败，mobiles 稳定。
//   取链  https://trackercdn.kugou.com/i/v2/?key=md5(hash + 'kgcloudv2')
//         &hash=<hash>&appid=1005&pid=2&cmd=25&behavior=play&album_id=...
//         免签名的老 tracker 协议：status:1 → url[0] 直链；status:2 =
//         酷狗侧 VIP / 独家付费墙（邓紫棋、周杰伦这类腾讯独家目录）。
//         免费曲可达无损：实测蔡依林《日不落》sqhash 直出 29MB FLAC。
//         11-07 的盘点把「VIP 曲 status:2」误判成整站已死——免费曲一直活着。
//
// 与 UNM 的差异：UNM 拿到 url 就返回（不报码率）；这里按 hash 档位如实
// 标 br（sq≈900 flac / 320 / 128），formatFromUrl 按扩展名识别容器。
// 音频直链是 http://（fsandroid.tx.kugou.com），安卓 cleartext 已由
// apply.py 注入 usesCleartextTraffic=true，双端都能播。
// 匹配用 filename 而不是 songname：版本词（Live/现场/电影插曲）都在
// filename 里，songname 是裸歌名，会漏过版本拦截。

var KG_SEARCH_API = 'https://mobiles.kugou.com/api/v3/search/song';
var KG_TRACK_API = 'https://trackercdn.kugou.com/i/v2/';
var KG_FAIL_MS = 30 * 60 * 1000;
var kgFailIds = {};

function kgGet(base, params) {
  var qs = [];
  for (var k in params) {
    if (!Object.prototype.hasOwnProperty.call(params, k)) continue;
    var v = params[k];
    if (v === undefined || v === null || v === '') continue;
    qs.push(encodeURIComponent(k) + '=' + encodeURIComponent(v));
  }
  try {
    return JSON.parse(
      leon.http.get(base + '?' + qs.join('&'), {
        timeoutMs: 3500,
        headers: { Referer: 'https://www.kugou.com/' }
      })
    );
  } catch (e) {
    return null; // 限流 / 挂掉不能影响主链路
  }
}

/** 搜索 → 统一成 gdCandidateMatches 的候选形状。name 用 filename（带版本词）。 */
function kgSearch(query) {
  var d = kgGet(KG_SEARCH_API, { format: 'json', keyword: query, page: 1, pagesize: 5 });
  var info = d && d.data && d.data.info;
  if (!Array.isArray(info)) return [];
  var out = [];
  for (var i = 0; i < info.length; i++) {
    var it = info[i];
    if (!it || !it.hash) continue;
    out.push({
      name: it.filename || (it.singername + ' - ' + it.songname),
      artist: it.singername || '',
      durationMs: (Number(it.duration) || 0) * 1000,
      hashes: {
        sq: it.sqhash || '',
        hq: it['320hash'] || '',
        std: it.hash
      },
      albumId: String(it.album_id || '')
    });
  }
  return out;
}

/** 单个 hash 取链：status:1 且有 url 才算成功。 */
function kgTrackFor(hash, albumId) {
  if (!hash) return null;
  var d = kgGet(KG_TRACK_API, {
    key: leon.md5(hash + 'kgcloudv2'),
    hash: hash,
    appid: 1005,
    pid: 2,
    cmd: 25,
    behavior: 'play',
    album_id: albumId
  });
  var url = d && d.status === 1 && Array.isArray(d.url) && d.url[0];
  if (!url || typeof url !== 'string') return null;
  return url.indexOf('http') === 0 ? url : null;
}

/** 酷狗解析：搜索 → 严格匹配 → 按 unblockBr 档位选 hash 逐级取链。
 *  br 999 = sq → hq → std；320/192 = hq → std；其余只用 std。 */
function kgResolve(track, br) {
  br = Number(br) || 0;
  // 这首歌刚失败过（酷狗独家/VIP 的常态）：30 分钟内别再撞，切歌等待 ≈ 0
  var blocked = kgFailIds[track.id];
  if (blocked && Date.now() < blocked) return null;

  var query = (track.title + ' ' + (track.artist || '')).trim();
  if (query.length < 2) return null;
  var hit = null;
  var list = kgSearch(query);
  for (var i = 0; i < list.length; i++) {
    if (gdCandidateMatches(track, list[i])) { hit = list[i]; break; }
  }
  if (!hit && track.title && track.title !== query) {
    list = kgSearch(track.title);
    for (var j = 0; j < list.length; j++) {
      if (gdCandidateMatches(track, list[j])) { hit = list[j]; break; }
    }
  }
  if (!hit) {
    kgFailIds[track.id] = Date.now() + KG_FAIL_MS;
    return null;
  }

  var wants = br >= 999 ? ['sq', 'hq', 'std'] : br >= 192 ? ['hq', 'std'] : ['std'];
  var brOf = { sq: 900, hq: 320, std: 128 }; // flac 码率不固定，展示按 ~900k
  for (var w = 0; w < wants.length; w++) {
    var tier = wants[w];
    var url = kgTrackFor(hit.hashes[tier], hit.albumId);
    if (url) {
      delete kgFailIds[track.id];
      return { url: url, br: brOf[tier] };
    }
  }
  kgFailIds[track.id] = Date.now() + KG_FAIL_MS;
  return null;
}

// ---------------------------------------------------------------------------
// bilibili 音源（内置后端，末位兜底）
// ---------------------------------------------------------------------------
//
// 2026-11-07 实测存活率盘点的结论（同日更新：咪咕经新接口复活，见上）：
// VutronMusic / UNM-Node 生态里的酷狗（trackercdn 老协议）当时全回
// status:2——后经复核那批是 VIP/独家曲的付费墙形态，免费曲 status:1
// 一直可用，已作为内置后端接入（见上）；波点（bd-api sign invalid）已死；
// 咪咕 scr_search_tag 老接口虽死，但 bmw/search + listen-url/h5/v2.4 新链路
// 全活、免登录且自家 CDN 最稳。酷我 mobi.s 加密通道活着但匿名同样锁 128k，
// 与 antiserver 同级无增益。B 站视频区 DASH 仍是全链路存活且取链协议公开
// 的兜底：
//   Wbi 签名搜索（20 条）→ view 拿 cid → playurl(fnval=16) 拿音轨
//   → 音轨最高 30280（FLAC 184k），30232（AAC 83k），30216（AAC 66k）
// 下载需要 Referer: https://www.bilibili.com/（net/cache.rs 按域注入），
// fMP4 m4s 容器 symphonia（features=all，含 isomp4+flac+aac）可直接解码
// （examples/probe_m4s.rs 实测两条音轨整曲解出）。

var BILI_MIXIN_TAB = [
  46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49,
  33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40,
  61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25, 54, 21, 56, 59, 6, 63, 57, 62, 11,
  36, 20, 34, 44, 52
];

var BILI_UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ' +
  'Chrome/125.0.0.0 Safari/537.36';

/** 进程内缓存：Wbi 密钥（约每天轮换）与 buvid3 游客 cookie（过 B 站风控） */
var biliWbiCache = null;
var biliCookieCache = '';

function biliMixinKey(orig) {
  var out = [];
  for (var i = 0; i < BILI_MIXIN_TAB.length; i++) {
    var c = orig.charAt(BILI_MIXIN_TAB[i]);
    if (c) out.push(c);
  }
  return out.join('').slice(0, 32);
}

/** wbi 签名：参数加 wts → 按 key 排序 → 过滤 !'()* → md5(query+mixinKey) */
function biliEncWbi(params, imgKey, subKey) {
  params.wts = String(Math.floor(Date.now() / 1000));
  var keys = Object.keys(params).sort();
  var pairs = [];
  for (var i = 0; i < keys.length; i++) {
    var k = keys[i];
    var v = String(params[k]).replace(/[!'()*]/g, '');
    pairs.push(encodeURIComponent(k) + '=' + encodeURIComponent(v));
  }
  var query = pairs.join('&');
  return query + '&w_rid=' + leon.md5(query + biliMixinKey(imgKey + subKey));
}

function biliHeaders(referer) {
  var h = { 'User-Agent': BILI_UA, Referer: referer || 'https://www.bilibili.com/' };
  if (biliCookieCache) h.Cookie = biliCookieCache;
  return h;
}

/** 惰性取 Wbi 密钥 + 首页 Set-Cookie（buvid3），任一失败都返回 null */
function biliSign(params) {
  if (!biliWbiCache) {
    try {
      var nav = JSON.parse(
        leon.http.getEx('https://api.bilibili.com/x/web-interface/nav', biliHeaders()).body
      );
      var img = ((nav.data && nav.data.wbi_img && nav.data.wbi_img.img_url) || '');
      var sub = ((nav.data && nav.data.wbi_img && nav.data.wbi_img.sub_url) || '');
      if (!img || !sub) return null;
      biliWbiCache = {
        img: img.slice(img.lastIndexOf('/') + 1, img.lastIndexOf('.')),
        sub: sub.slice(sub.lastIndexOf('/') + 1, sub.lastIndexOf('.'))
      };
    } catch (e) {
      return null;
    }
  }
  if (!biliCookieCache) {
    try {
      var home = leon.http.getEx('https://www.bilibili.com/', { 'User-Agent': BILI_UA });
      var cookies = (home && home.cookies) || {};
      var parts = [];
      for (var k in cookies) {
        if (Object.prototype.hasOwnProperty.call(cookies, k) && cookies[k]) {
          parts.push(k + '=' + cookies[k]);
        }
      }
      biliCookieCache = parts.join('; ');
    } catch (e) { /* 没 cookie 也能搜，只是更容易被风控 */ }
  }
  if (!biliWbiCache) return null;
  return biliEncWbi(params, biliWbiCache.img, biliWbiCache.sub);
}

/** B 站搜索结果标题剥高亮标签 + 实体 */
function biliCleanTitle(t) {
  return String(t || '')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&amp;/g, '&')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");
}

/** 标题必须同时含歌名与至少一个歌手 token；Live/翻唱/翻弹/remix 等版本词直接排除
 *  （与 gdCandidateMatches 同一套 hasVersionMark：目标自身带标记时不拦） */
function biliTitleMatches(track, title) {
  var t = biliCleanTitle(title);
  if (!hasVersionMark(track.title) && hasVersionMark(t)) {
    return false;
  }
  var want = normalizeName(track.title);
  var got = normalizeName(t);
  if (!want || !got || got.indexOf(want) < 0) return false;
  var parts = String(track.artist || '').split(/[\s/、,，]+/);
  for (var i = 0; i < parts.length; i++) {
    var p = normalizeName(parts[i]);
    if (p && got.indexOf(p) >= 0) return true;
  }
  return parts.length === 0;
}

/** 搜索结果 "4:29" 形态的时长，差 35% 以上视为不同作品 */
function biliDurationOk(track, durStr) {
  var ms = Number(track.durationMs) || 0;
  if (!ms || !durStr) return true;
  var segs = String(durStr).split(':');
  var sec = 0;
  for (var i = 0; i < segs.length; i++) sec = sec * 60 + (Number(segs[i]) || 0);
  if (!sec) return true;
  return Math.abs(sec * 1000 - ms) < ms * 0.35;
}

/** Wbi 搜索：返回第一个通过标题+时长校验的 bvid，找不到给 null */
function biliSearchBvid(track) {
  var query = (track.title + ' ' + (track.artist || '')).trim();
  if (query.length < 2) return null;
  var signed = biliSign({ search_type: 'video', keyword: query });
  if (!signed) return null;
  var data;
  try {
    data = JSON.parse(
      leon.http.getEx(
        'https://api.bilibili.com/x/web-interface/wbi/search/type?' + signed,
        biliHeaders('https://search.bilibili.com/')
      ).body
    );
  } catch (e) {
    return null;
  }
  var list = (data && data.data && data.data.result) || [];
  for (var i = 0; i < list.length && i < 10; i++) {
    var it = list[i];
    if (!it || !it.bvid || it.bvid.indexOf('BV') !== 0) continue;
    if (!biliTitleMatches(track, it.title)) continue;
    if (!biliDurationOk(track, it.duration)) continue;
    return it.bvid;
  }
  return null;
}

/** DASH 音轨择优：只认 symphonia 解得动的编码（flac / mp4a），bandwidth 优先。
 *  base_url 常是 mcdn PCDN 节点（P2P，高峰不稳定），backup_url 里通常有
 *  upos 主 CDN，优先取主 CDN 直链，全 mcdn 才用 PCDN。 */
function biliPickAudio(audioList) {
  var best = null;
  for (var i = 0; i < audioList.length; i++) {
    var a = audioList[i];
    if (!a || !a.base_url) continue;
    var codec = String(a.codecs || '').toLowerCase();
    if (codec.indexOf('flac') < 0 && codec.indexOf('mp4a') < 0) continue; // ec-3 等解不了
    var bw = Number(a.bandwidth) || 0;
    if (!best || bw > best.bw) best = { base: a.base_url, backup: a.backup_url || [], bw: bw };
  }
  if (!best) return null;
  var urls = [best.base].concat(best.backup);
  var picked = best.base;
  for (var j = 0; j < urls.length; j++) {
    var u = String(urls[j] || '');
    if (u && u.indexOf('http') === 0 && u.indexOf('mcdn.bilivideo.cn') < 0) {
      picked = u;
      break;
    }
  }
  return { url: picked, bw: best.bw };
}

/** Wbi view 拿 cid → playurl(fnval=16) 拿 DASH 音轨直链 */
function biliPlayUrl(bvid) {
  var signedView = biliSign({ bvid: bvid });
  if (!signedView) return null;
  var cid = 0;
  try {
    var view = JSON.parse(
      leon.http.getEx(
        'https://api.bilibili.com/x/web-interface/wbi/view?' + signedView,
        biliHeaders()
      ).body
    );
    cid = (view.data && view.data.cid) || 0;
  } catch (e) {
    return null;
  }
  if (!cid) return null;
  var signedPlay = biliSign({ bvid: bvid, cid: String(cid), fnval: '16', platform: 'pc' });
  if (!signedPlay) return null;
  try {
    var play = JSON.parse(
      leon.http.getEx(
        'https://api.bilibili.com/x/player/wbi/playurl?' + signedPlay,
        biliHeaders()
      ).body
    );
    var audio = (play.data && play.data.dash && play.data.dash.audio) || [];
    return biliPickAudio(audio);
  } catch (e) {
    return null;
  }
}

/** bilibili 解析：搜索匹配 → DASH 音轨。br 用真实带宽（kbps） */
function biliResolve(track) {
  var bvid = biliSearchBvid(track);
  if (!bvid) return null;
  var picked = biliPlayUrl(bvid);
  if (picked && picked.url) return { url: picked.url, br: Math.round(picked.bw / 1000) };
  return null;
}

// ---------------------------------------------------------------------------
// 协议入口
// ---------------------------------------------------------------------------

function search(keyword, page, limit) {
  if (metingBase()) return metingSearch(keyword, page, limit);

  var size = limit || 30;
  var offset = ((page || 1) - 1) * size;
  var body = weapi({
    s: keyword,
    type: 1,
    limit: size,
    offset: offset,
    total: true,
    csrf_token: csrfToken()
  });
  var data = JSON.parse(post('/weapi/cloudsearch/pc', body));
  if (data.code !== 200) throw new Error('网易云搜索失败：' + describe(data));

  var songs = (data.result && data.result.songs) || [];
  var out = [];
  for (var i = 0; i < songs.length; i++) {
    var s = songs[i];
    var al = s.al || s.album || {};
    out.push({
      id: String(s.id),
      title: s.name || '',
      artist: artistNames(s.ar || s.artists),
      album: al.name || '',
      durationMs: s.dt || s.duration || 0,
      coverUrl: httpsUrl(al.picUrl || '')
    });
  }
  return out;
}

function getStreamUrl(track) {
  // 自建 Meting 后备：拿不到规格，明确标成 plugin 来源
  if (metingBase()) {
    var text = leon.http.get(metingUrl({ type: 'url', id: track.id }));
    var d = JSON.parse(text);
    if (Array.isArray(d)) d = d[0] || {};
    if (!d.url) throw new Error('该曲目没有可用播放地址（可能需要 VIP 或版权受限）');
    return streamResult(d.url, {
      source: 'plugin', bitrate: null, format: null,
      trial: false, requested: '', downgraded: false
    });
  }

  var want = leon.config.get('level') || 'exhigh';
  var levels = levelFallback(want);
  var wantsLossless = want === 'lossless' || want === 'hires' || want === 'jymaster';

  var lastCode = 0;
  var trial = null;
  for (var k = 0; k < levels.length; k++) {
    var body = eapi('/api/song/enhance/player/url/v1', {
      ids: '[' + track.id + ']',
      level: levels[k],
      encodeType: 'flac',
      csrf_token: csrfToken()
    });
    var data = JSON.parse(post('/eapi/song/enhance/player/url/v1', body));
    if (data.code !== 200) {
      lastCode = data.code;
      continue;
    }
    var item = (data.data && data.data[0]) || null;
    if (item && item.url) {
      var meta = qualityFromOfficial(item, want, levels[k]);
      // 免费曲 / 会员已解锁：直接返回，不打扰第三方服务。
      // 若选了无损档但官方静默给了 320k MP3，如实标 downgraded（不另去解析全曲）。
      if (!isTrialUrlItem(item)) return streamResult(item.url, meta);
      if (!trial) trial = { url: item.url, meta: meta };
    }
    if (item && item.code) lastCode = item.code;
  }

  // 只有官方不给全曲时才解析；失败就保留原试听，让上层弹 30 秒提示
  if (unblockOn()) {
    var resolved = resolveTrack(track);
    if (resolved && resolved.url) {
      var fmt = formatFromUrl(resolved.url);
      return streamResult(resolved.url, {
        source: 'resolved',
        bitrate: resolved.br ? Math.round(resolved.br * 1000) : null,
        format: fmt,
        trial: false,
        requested: want,
        // 请求无损档却只拿到 mp3，也算降级
        downgraded: wantsLossless && fmt !== 'flac'
      });
    }
  }
  if (trial) {
    trial.meta.source = 'trial';
    trial.meta.trial = true;
    trial.meta.downgraded = true;
    return streamResult(trial.url, trial.meta);
  }

  // 404 = 没找到，403 = 无权限（VIP / 版权下架）
  if (lastCode === 404) throw new Error('该曲目已下架或版权受限');
  throw new Error('该曲目没有可用播放地址（可能需要 VIP 或已下架）');
}

function getLyric(track) {
  if (metingBase()) {
    return leon.http.get(metingUrl({ type: 'lrc', id: track.id }));
  }

  var body = weapi({ id: track.id, lv: -1, kv: -1, tv: -1, csrf_token: csrfToken() });
  var data = JSON.parse(post('/weapi/song/lyric?csrf_token=' + csrfToken(), body));
  if (data.code !== 200) throw new Error('获取歌词失败：' + describe(data));

  var lrc = (data.lrc && data.lrc.lyric) || '';
  var tr = (data.tlyric && data.tlyric.lyric) || '';
  if (!lrc) return '';
  // 翻译是独立的一份同时间轴 LRC，直接接在后面：
  // 宿主解析时会把「同时间戳的相邻两行」合成原文 + 翻译
  return tr ? lrc + '\n' + tr : lrc;
}

// ---------------------------------------------------------------------------
// 推荐流与账号歌单（参考网易云 web 端 + VutronMusic 的插件实现）
// ---------------------------------------------------------------------------
//
//   personalized/playlist    个性推荐歌单，免登录可用
//   personalized/newsong     推荐新歌（未登录时的曲目来源），免登录可用
//   v3/discovery/recommend/songs  真·每日推荐歌曲（登录可用，31 首；
//                            此前 404 的 v3/recommend-songs 少了 discovery 段）
//   user/playlist            账号歌单，第一项固定是「我喜欢的音乐」
//   v6/playlist/detail       歌单详情，tracks 最多返回 n 首（trackIds 有全量 id）
//
// 全部走 weapi POST；推荐系接口统一 postWeb（纯登录 cookie 的 web 指纹，
// 与登录环境同构）。未登录时服务端对需登录接口返回 code:301，如实降级。

function songToTrack(s) {
  if (!s) return null;
  var al = s.al || s.album || {};
  var ar = s.ar || s.artists || [];
  return {
    id: String(s.id),
    title: s.name || '',
    artist: artistNames(ar),
    album: al.name || '',
    durationMs: s.dt || s.duration || 0,
    coverUrl: httpsUrl(al.picUrl || '')
  };
}

function formatPlaylistMeta(p) {
  if (!p) return null;
  return {
    id: String(p.id),
    name: p.name || '',
    coverUrl: httpsUrl((p.picUrl || p.coverImgUrl || '') + '?param=300y300'),
    trackCount: (p.trackIds && p.trackIds.length) || p.trackCount || 0,
    playCount: p.playCount || 0,
    copywriter: p.copywriter || ''
  };
}

function accountProfile() {
  var data = JSON.parse(
    post('/weapi/nuser/account/get', weapi({ csrf_token: csrfToken() }))
  );
  return (data && data.profile) || null;
}

/**
 * 推荐聚合：曲目来源 = 推荐新歌（免登录可用）；歌单 = 登录时的每日推荐
 * 歌单（若接口仍可用）∪ 个性推荐歌单。宿主把 tracks 转成完整 Track，
 * 前端拿到即可直接播放。
 */
function getRecommendFeed(limit) {
  var size = Math.min(Math.max(Number(limit) || 12, 1), 30);
  var out = { loggedIn: false, userId: 0, nickname: '', playlists: [], tracks: [] };

  var profile = null;
  try {
    profile = accountProfile();
  } catch (e) {
    profile = null; // 匿名 Cookie 偶发 301/网络抖动，不阻塞免登录推荐
  }
  if (profile && profile.userId) {
    out.loggedIn = true;
    out.userId = profile.userId;
    out.nickname = profile.nickname || '';
  }

  var seen = {};
  function push(list) {
    for (var i = 0; i < (list || []).length; i++) {
      var meta = formatPlaylistMeta(list[i]);
      if (meta && meta.id && !seen[meta.id]) {
        seen[meta.id] = true;
        out.playlists.push(meta);
      }
    }
  }

  // 三个推荐接口统一走 postWeb（纯登录 cookie 的 web 指纹）；
  // 单接口失败不阻塞其余来源，只在宿主日志留痕便于排障。
  try {
    var data = JSON.parse(
      postWeb('/weapi/personalized/playlist', weapi({ limit: size, csrf_token: csrfToken() }))
    );
    if (data.code === 200) push(data.result);
    else leon.log('recommendFeed personalized code=' + data.code);
  } catch (e) {
    leon.log('recommendFeed personalized err: ' + e);
  }

  // 推荐歌曲流。登录态走真正的每日推荐 /v3/discovery/recommend/songs
  // （VutronMusic 同款路径；2026-11 实测登录 cookie 返回 31 首，
  // data.dailySongs 字段与 songToTrack 兼容——此前 404 的
  // v1/recommend-resource / v3/recommend-songs 是少了 discovery 段）。
  // 未登录该接口 code:301，回落推荐新歌（免登录可用）。
  var dailySongs = null;
  if (out.loggedIn) {
    try {
      var daily = JSON.parse(
        postWeb('/weapi/v3/discovery/recommend/songs', weapi({ afresh: true, csrf_token: csrfToken() }))
      );
      if (daily.code === 200 && daily.data && daily.data.dailySongs) {
        dailySongs = daily.data.dailySongs;
      } else {
        leon.log('recommendFeed discovery code=' + daily.code);
      }
    } catch (e) {
      leon.log('recommendFeed discovery err: ' + e);
    }
  }
  if (dailySongs && dailySongs.length) {
    for (var k = 0; k < dailySongs.length && out.tracks.length < size; k++) {
      var t = songToTrack(dailySongs[k]);
      if (t && t.id) out.tracks.push(t);
    }
  } else {
    try {
      var songs = JSON.parse(
        postWeb('/weapi/personalized/newsong', weapi({ type: 'recommend', limit: size, csrf_token: csrfToken() }))
      );
      if (songs.code !== 200) leon.log('recommendFeed newsong code=' + songs.code);
      var list = (songs.code === 200 && songs.result) || [];
      for (var k2 = 0; k2 < list.length && out.tracks.length < size; k2++) {
        var t2 = songToTrack(list[k2] && list[k2].song);
        if (t2 && t2.id) out.tracks.push(t2);
      }
    } catch (e2) {
      leon.log('recommendFeed newsong err: ' + e2);
    }
  }

  if (out.loggedIn) {
    // 每日推荐歌单排在前面（更像网易云首页）。v1/recommend/resource 已下线，
    // 失败只留日志，不影响歌单区其余来源。
    try {
      var daily = JSON.parse(
        postWeb('/weapi/v1/recommend/resource', weapi({ csrf_token: csrfToken() }))
      );
      if (daily.code === 200 && daily.recommend) push(daily.recommend);
    } catch (e) {
      leon.log('recommendFeed resource err: ' + e);
    }
  }
  return out;
}

/** 账号歌单：liked 是「我喜欢的音乐」（服务端固定排第一）。未登录返回 loggedIn:false。 */
function getUserPlaylists() {
  var profile = accountProfile();
  if (!profile || !profile.userId) return { loggedIn: false, userId: 0, liked: null, playlists: [] };

  var body = weapi({
    uid: profile.userId,
    limit: 200,
    offset: 0,
    includeVideoSong: true,
    csrf_token: csrfToken()
  });
  var data = JSON.parse(post('/weapi/user/playlist', body));
  if (data.code !== 200) throw new Error('获取账号歌单失败：' + describe(data));

  var all = data.playlist || [];
  var liked = all.length ? formatPlaylistMeta(all[0]) : null;
  var lists = [];
  for (var i = 1; i < all.length; i++) {
    var meta = formatPlaylistMeta(all[i]);
    if (meta) lists.push(meta);
  }
  return { loggedIn: true, userId: profile.userId, liked: liked, playlists: lists };
}

/**
 * 歌单详情（云端歌单导入用）。n 限制返回的曲目详情数，
 * 超大歌单（如上千首的「我喜欢的音乐」）trackIds 有全量 id，
 * 详情最多拿 n 首 —— 导入按 1000 首一批，超出部分提示用户分批。
 */
function getPlaylist(id) {
  var body = weapi({ id: String(id), n: 1000, s: 8, csrf_token: csrfToken() });
  var data = JSON.parse(post('/weapi/v6/playlist/detail', body));
  if (data.code !== 200) throw new Error('获取歌单详情失败：' + describe(data));
  var p = data.playlist || {};
  var tracks = [];
  var songs = p.tracks || [];
  for (var i = 0; i < songs.length; i++) {
    var t = songToTrack(songs[i]);
    if (t && t.id) tracks.push(t);
  }
  return {
    id: String(p.id || id),
    name: p.name || '',
    coverUrl: httpsUrl(p.coverImgUrl || ''),
    trackCount: p.trackCount || (p.trackIds && p.trackIds.length) || tracks.length,
    tracks: tracks
  };
}

// ---------------------------------------------------------------------------
// 红心收藏 / 官方排行榜 / 私人FM / 相似歌曲
// ---------------------------------------------------------------------------
//
// 这四个接口的路径与参数按 api-enhanced（VutronMusic 背后的网易云 API
// 库，stark81 fork 维护）逐字核对，并已用真实登录 Cookie 实测过返回形状：
//   like   -> POST /weapi/radio/like  {code:200, playlistId}，未登录 301
//   toplist -> GET  /api/toplist      {list:[65 项榜单元数据]}，免登录可用
//   fm     -> POST /weapi/v1/radio/get {data:[旧式 song]}，匿名仅 1 首
//   simiSong -> POST /weapi/v1/discovery/simiSong {songs:[旧式 song]}
// FM / 相似歌曲返回的是旧式 song（artists/album/duration），songToTrack
// 的 `s.al || s.album` 兼容写法正好覆盖，无需单独映射。

/**
 * 红心 / 取消红心。成功后这首歌进（或移出）「我喜欢的音乐」。
 * 幂等：对已红心的歌再 like=true 服务端不报错（返回原 playlistId）。
 * 返回 { ok, code, playlistId, message }；未登录 code 301。
 */
function likeTrack(id, like) {
  var want = !(like === false || like === 'false' || like === 0 || like === '0');
  var data = JSON.parse(
    post(
      '/weapi/radio/like',
      weapi({
        alg: 'itembased',
        trackId: String(id || ''),
        like: want,
        time: '3',
        csrf_token: csrfToken()
      })
    )
  );
  if (Number(data.code) !== 200) {
    return { ok: false, code: Number(data.code) || -1, message: describe(data) };
  }
  return { ok: true, code: 200, liked: want, playlistId: String(data.playlistId || '') };
}

/** 检查一首歌是否已被当前账号红心。uid 可不传（自动从账号信息取）。 */
function isLikedTrack(id, uid) {
  var userId = String(uid || '');
  if (!userId) {
    var profile = null;
    try {
      profile = accountProfile();
    } catch (e) {
      profile = null;
    }
    if (!profile || !profile.userId) return { loggedIn: false, liked: false };
    userId = String(profile.userId);
  }
  var data = JSON.parse(
    post('/weapi/song/like/get', weapi({ uid: userId, csrf_token: csrfToken() }))
  );
  if (Number(data.code) !== 200) throw new Error('获取红心列表失败：' + describe(data));
  var ids = data.ids || [];
  var sid = String(id || '');
  for (var i = 0; i < ids.length; i++) {
    if (String(ids[i]) === sid) return { loggedIn: true, liked: true };
  }
  return { loggedIn: true, liked: false };
}

/**
 * 官方排行榜列表（飙升榜 / 热歌榜 / 新歌榜 / 原创榜……共 65 项）。
 * GET /api/toplist 无加密、免登录可用。榜单本身不含曲目——榜单 id
 * 就是特殊歌单 id，宿主拿到后复用 getPlaylist(id) 拉曲目即可。
 */
function getToplists() {
  var data = JSON.parse(leon.http.get(API + '/api/toplist'));
  if (Number(data.code) !== 200) throw new Error('获取排行榜失败：' + describe(data));
  var out = [];
  var list = data.list || [];
  for (var i = 0; i < list.length; i++) {
    var t = list[i];
    if (!t || !t.id) continue;
    out.push({
      id: String(t.id),
      name: t.name || '',
      coverUrl: httpsUrl((t.coverImgUrl || '') + '?param=300y300'),
      trackCount: t.trackCount || 0,
      updateFrequency: t.updateFrequency || '',
      copywriter: t.description || ''
    });
  }
  return out;
}

/**
 * 私人FM：按账号口味滚动推歌。登录态每次给 3 首，匿名只给 1 首
 * 且播放受限（拿到的多是试听）。返回 tracks 数组，宿主转完整 Track。
 */
function getPersonalFm() {
  var data = JSON.parse(
    post('/weapi/v1/radio/get', weapi({ csrf_token: csrfToken() }))
  );
  if (Number(data.code) !== 200) throw new Error('获取私人FM失败：' + describe(data));
  var tracks = [];
  var songs = data.data || [];
  for (var i = 0; i < songs.length; i++) {
    var t = songToTrack(songs[i]);
    if (t && t.id) tracks.push(t);
  }
  return tracks;
}

/**
 * 相似歌曲：基于一首歌的推荐（播放页「听相似」用）。免登录可用。
 * limit 服务端上限 50，默认取 30。
 */
function getSimilarSongs(id, limit) {
  var size = Math.min(Math.max(Number(limit) || 30, 1), 50);
  var data = JSON.parse(
    post(
      '/weapi/v1/discovery/simiSong',
      weapi({
        songid: String(id || ''),
        limit: size,
        offset: 0,
        csrf_token: csrfToken()
      })
    )
  );
  if (Number(data.code) !== 200) throw new Error('获取相似歌曲失败：' + describe(data));
  var tracks = [];
  var songs = data.songs || [];
  for (var i = 0; i < songs.length; i++) {
    var t = songToTrack(songs[i]);
    if (t && t.id) tracks.push(t);
  }
  return tracks;
}

// ---------------------------------------------------------------------------
// 听歌打卡
// ---------------------------------------------------------------------------
//
// 「听歌时长 / 播放次数」没有独立的同步接口——网易云 App 里这两个数字
// 完全由客户端上报的播放日志算出来。老的 weapi 打卡接口
// （/weapi/1.0/scrobble、music.163.com/api/feedback/weblog）2026 年起被
// 服务端静默丢弃：返回 code 200 但不计入排行（多人实测，见
// hana-music-api#31；能计入的做法是发 clientlog.music.163.com 的 eapi
// 日志，该公司 v2.8.1 真账号验证通过：打卡后周排行次数 0 → 2）。
//
// 这里按官方桌面客户端的方式上报两条日志：
//   - startplay：让这首歌进「最近播放」
//   - play     ：累计一次播放；排行得分 / 听歌时长 / 歌曲次数由它驱动
// 打卡必须带登录 Cookie，且要带 mac 客户端指纹（os=osx；实测计入的
// 形态）。只上报真实听过的播放（前端 ≥30 秒才触发），不刷榜。

/** 两条日志的组装独立成纯函数，离线单测锁住协议形状 */
function scrobbleLogs(id, secs, sourceId, source) {
  var content = 'id=' + String(sourceId || 0);
  return [
    {
      action: 'startplay',
      json: {
        id: String(id),
        type: 'song',
        mainsite: '1',
        mainsiteWeb: '1',
        content: content
      }
    },
    {
      action: 'play',
      json: {
        download: 0,
        end: 'playend',
        id: String(id),
        sourceId: Number(sourceId) || 0,
        time: Math.max(0, Math.round(Number(secs) || 0)),
        type: 'song',
        wifi: 0,
        source: source || 'list',
        mainsite: '1',
        mainsiteWeb: '1',
        content: content
      }
    }
  ];
}

function scrobbleOn() {
  var v = String(leon.config.get('scrobble') || 'on').toLowerCase();
  return v !== 'off' && v !== '0' && v !== 'false';
}

/**
 * 听歌打卡入口。宿主传来本次播放累计毫秒，网易云要整数秒。
 * 返回 { ok, code }；ok=false 通常是未登录（301），前端收到后本会话不再尝试。
 */
function scrobble(id, listenedMs) {
  if (!scrobbleOn()) return { ok: false, code: 0, message: '打卡已关闭' };
  var sid = String(id || '');
  var secs = Math.floor(Math.max(0, Number(listenedMs) || 0) / 1000);
  var userCookie = cleanCookie(leon.config.get('cookie'));
  if (userCookie.indexOf('MUSIC_U=') < 0) {
    return { ok: false, code: 301, message: '未登录，打卡不上报' };
  }
  if (!secs) return { ok: true, code: 200, skipped: true };

  var body = eapi(
    '/api/feedback/weblog',
    { logs: JSON.stringify(scrobbleLogs(sid, secs, 0, 'list')) }
  );
  // 打卡请求换成 mac 客户端指纹（实测计入排行的形态）；只覆盖这一条请求
  var scroCookie = mergeCookies(
    cookie(),
    'os=osx; appver=3.1.10.5100; osver=15.5; channel=netease'
  );
  var res = JSON.parse(
    leon.http.postForm(CLIENTLOG + '/eapi/feedback/weblog', body, {
      Referer: API,
      Origin: API,
      Cookie: scroCookie
    })
  );
  if (res && Number(res.code) === 200) {
    return { ok: true, code: 200, data: res.data || 'success' };
  }
  return { ok: false, code: Number(res && res.code) || -1, message: describe(res) };
}

// ---------------------------------------------------------------------------
// 扫码登录
// ---------------------------------------------------------------------------
//
// 流程和网页版一致：
//   1. 先访问一次首页，收集 _ntes_nuid / NMTID 等浏览器会话 Cookie
//   2. /weapi/login/qrcode/unikey  拿 unikey
//   3. 把 https://music.163.com/login?codekey=<unikey> 渲染成二维码
//   4. 每 3 秒 POST /weapi/login/qrcode/client/login 轮询
//      801 等待扫码 / 802 已扫待确认 / 803 成功 / 800 过期 / 8821 风控拦截
//
// 真实网页从进首页到建码到确认，全程是同一个会话；第三方实现常把
// unikey 下发的 Set-Cookie 丢掉，服务端就只看到一串没有指纹的裸请求，
// 手机 App 于是弹「检测到当前设备环境异常，本次操作已拦截」。
// 这里把会话 Cookie 串起来（create 返回 session，poll 原样带回）。
//
// 803 时身份凭据在 **响应头** 的 Set-Cookie 里，body 只有一句「授权登陆成功」，
// 所以这里必须用 postFormEx（宿主会顺手把 Set-Cookie 解析成对象）。

function postEx(path, body, session) {
  return leon.http.postFormEx(API + path, body, {
    Referer: API,
    Origin: API,
    Cookie: mergeCookies(anonCookie(), session || ''),
    'Cache-Control': 'no-cache'
  });
}

function cookieStringFrom(map, skipTracking) {
  if (!map) return '';
  var skip = skipTracking ? { NMTID: 1, JSESSIONID: 1 } : {};
  var out = {};
  for (var k in map) {
    if (!Object.prototype.hasOwnProperty.call(map, k)) continue;
    if (skip[k]) continue;
    var v = map[k];
    if (v === undefined || v === null || v === '') continue;
    out[k] = v;
  }
  var parts = [];
  for (var k2 in out) {
    if (Object.prototype.hasOwnProperty.call(out, k2)) parts.push(k2 + '=' + out[k2]);
  }
  return parts.join('; ');
}

/** 只保留需要长期携带的身份 cookie，埋点类的丢了才不会越滚越多 */
function cookieFrom(map) {
  return cookieStringFrom(map, true);
}

function csrfFrom(session) {
  var m = /(?:^|;\s*)__csrf=([^;]+)/.exec(session || '');
  return m ? m[1] : csrfToken();
}

/** 预热：拿首页下发的会话 Cookie。老宿主没有 getEx、或首页超时，都不阻断建码 */
function browserSession() {
  try {
    var res = leon.http.getEx('https://music.163.com/', {
      Accept: 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
      'Accept-Language': 'zh-CN,zh;q=0.9',
      'Cache-Control': 'no-cache'
    });
    return cookieStringFrom(res.cookies, false);
  } catch (e) {
    return '';
  }
}

function loginQrCreate() {
  // type=3 与 @neteasecloudmusicapienhanced/api（VutronMusic 用的那套）一致。
  // type=1 也能建码并回到 801，但没人扫到 803 时无法验证凭据下发，
  // 所以选经过实战验证的那组参数；create 与 poll 必须同 type。
  var session = browserSession();
  var res = postEx(
    '/weapi/login/qrcode/unikey',
    weapi({ type: 3, csrf_token: csrfFrom(session) }),
    session
  );
  var data = JSON.parse(res.body);
  if (data.code !== 200 || !data.unikey) {
    throw new Error('生成二维码失败：' + describe(data));
  }
  // 建码响应也可能补发 NMTID / __csrf，合并后整条链路都用这一份
  session = mergeCookies(session, cookieStringFrom(res.cookies, false));
  return {
    key: data.unikey,
    url: 'https://music.163.com/login?codekey=' + data.unikey,
    session: session
  };
}

/**
 * 把服务端的非成功码翻译成用户看得懂的状态。
 * 旧实现把所有未知码都当 waiting，于是 8821 风控拦截时前端永远
 * 停在「打开手机 App 扫一扫」，用户根本不知道登录已经失败了。
 */
function loginPollState(code, data) {
  if (code === 801) return { state: 'waiting' };
  if (code === 802) return { state: 'scanned' };
  if (code === 800) return { state: 'expired' };
  var msg = describe(data);
  if (code === 8810) {
    return {
      state: 'blocked',
      message:
        '网易云判定当前网络环境有风险（8810）：请关闭代理/VPN 或换一个网络后重试；' +
        '也可以改用 Cookie 登录。'
    };
  }
  if (code === 8821 || /设备环境|环境异常|切换其他登录方式|升级新版本/.test(msg || '')) {
    return {
      state: 'blocked',
      message:
        '网易云风控拦截（' +
        code +
        '，' +
        msg +
        '）。这是账号/设备级限制，换网络、更新手机 App、移除越狱或解锁类插件、' +
        '24 小时后再试都可能恢复；最稳妥的是改用 Cookie 登录。'
    };
  }
  return { state: 'blocked', message: '扫码状态异常（' + code + '，' + msg + '）' };
}

function loginQrPoll(key, session) {
  var sess = session || '';
  var res = postEx(
    '/weapi/login/qrcode/client/login',
    weapi({ key: key, type: 3, csrf_token: csrfFrom(sess) }),
    sess
  );
  var data;
  try {
    data = JSON.parse(res.body);
  } catch (e) {
    throw new Error('扫码状态解析失败：HTTP ' + res.status);
  }

  var code = Number(data.code) || 0;
  if (code === 803) {
    var saved = cookieFrom(res.cookies);
    if (saved.indexOf('MUSIC_U=') < 0) {
      return { state: 'blocked', message: '服务端确认了扫码，但没有下发 MUSIC_U，请改用 Cookie 登录' };
    }
    return { state: 'success', cookie: saved };
  }
  return loginPollState(code, data);
}

// ---------------------------------------------------------------------------
// 短信验证码登录
// ---------------------------------------------------------------------------
//
// 参数形态对齐 api-enhanced（NeteaseCloudMusicApiEnhanced/api-enhanced）：
//   发送验证码  weapi POST /weapi/sms/captcha/sent
//               { ctcode, secrete: 'music_middleuser_pclogin', cellphone }
//   验证码登录  weapi POST /weapi/w/login/cellphone
//               { type:'1', https:'true', phone, countrycode, captcha,
//                 remember:'true', secureCaptcha:'' }
// 成功凭据在响应头 Set-Cookie（MUSIC_U），与扫码登录同一条落库路径。
// 手机号与验证码只发往 music.163.com 本身，不经过任何第三方服务器。
//
// session 与扫码流程同构：发码时收集一次浏览器会话指纹，登录时原样带回，
// 保证「发码 → 校验」在服务端看来是同一个设备。

/** 发送验证码请求体（协议形状锁的离线测试也用它） */
function captchaSentBody(phone, ctcode) {
  return {
    ctcode: String(ctcode || '86'),
    secrete: 'music_middleuser_pclogin',
    cellphone: String(phone || '')
  };
}

/** 验证码登录请求体 */
function loginCellphoneBody(phone, captcha, ctcode) {
  return {
    type: '1',
    https: 'true',
    phone: String(phone || ''),
    countrycode: String(ctcode || '86'),
    captcha: String(captcha || ''),
    remember: 'true',
    secureCaptcha: ''
  };
}

/**
 * 发送短信验证码。成功返回 { state:'sent', session }，session 由前端
 * 保存并在登录时带回；失败时把服务端可读信息透出（手机号格式 / 频率限制）。
 */
function captchaSend(phone, ctcode, session) {
  var sess = session || browserSession();
  var res = postEx(
    '/weapi/sms/captcha/sent',
    weapi(captchaSentBody(phone, ctcode)),
    sess
  );
  var data;
  try {
    data = JSON.parse(res.body);
  } catch (e) {
    throw new Error('验证码响应解析失败：HTTP ' + res.status);
  }
  if (data.code !== 200) {
    var code = Number(data.code) || 0;
    var msg = describe(data) || '';
    return {
      state: 'failed',
      code: code,
      message: riskText(code) || '验证码发送失败（' + code + (msg ? '，' + msg : '') + '）'
    };
  }
  // 发码响应一般不补发身份 cookie，但会话指纹要整条链路保持一致
  sess = mergeCookies(sess, cookieStringFrom(res.cookies, false));
  return { state: 'sent', session: sess };
}

/** 风控 / 频率类失败码的用户话术（发码与登录共用）。
 *  语义对齐 api-enhanced 的易盾文档：8810 = 网络环境级风险（换网络/关
 *  代理，重试基本没用）、-462 = 要滑块人机验证（本应用做不了）、
 *  406/409 = 频繁、509 = 错误次数超限锁定。返回空串表示没有专门话术。 */
function riskText(code) {
  switch (code) {
    case 8810:
      return '网易云判定当前网络环境有风险（8810）：请关闭代理/VPN，或换一个网络（如手机热点）后重试；也可以改用扫码登录或 Cookie 登录';
    case -462:
      return '网易云要求人机验证（-462），请改用扫码登录或 Cookie 登录';
    case 406:
      return '操作过于频繁（406），请等一分钟后再试';
    case 409:
      return '操作过于频繁（409），请稍后再试';
    case 509:
      return '错误次数过多（509），账号被临时限制，请 30 分钟后再试';
    default:
      return '';
  }
}

/** 把服务端的失败码翻译成用户能看懂的话（验证码场景）。
 *  实测：错验证码返回 code 503 + message「验证码错误」，describe 透出即可；
 *  502 主要见于密码登录模式，这里也兜一层防服务端偶尔换码。 */
function cellphoneFailState(data) {
  var code = Number(data.code) || 0;
  var msg = describe(data) || '';
  if (code === 502) {
    return {
      state: 'failed',
      code: code,
      message: '验证码错误或已过期，请重新发送后再试（502，' + (msg || '校验未通过') + '）'
    };
  }
  var risk = riskText(code);
  if (risk) return { state: 'failed', code: code, message: risk };
  return { state: 'failed', code: code, message: '登录失败（' + code + (msg ? '，' + msg : '') + '）' };
}

/**
 * 验证码登录。成功时身份凭据在响应头 Set-Cookie（MUSIC_U）里，
 * 过滤掉埋点类 cookie 后整串返回，宿主把它并进插件配置。
 */
function loginCellphone(phone, captcha, ctcode, session) {
  var sess = session || '';
  var res = postEx(
    '/weapi/w/login/cellphone',
    weapi(loginCellphoneBody(phone, captcha, ctcode)),
    sess
  );
  var data;
  try {
    data = JSON.parse(res.body);
  } catch (e) {
    throw new Error('登录响应解析失败：HTTP ' + res.status);
  }
  if (data.code !== 200) return cellphoneFailState(data);
  var saved = cookieFrom(res.cookies);
  if (saved.indexOf('MUSIC_U=') < 0) {
    return { state: 'failed', message: '服务端确认了登录，但没有下发 MUSIC_U，请改用 Cookie 登录' };
  }
  return {
    state: 'success',
    cookie: saved,
    nickname: (data.profile && data.profile.nickname) || ''
  };
}

function loginInfo() {
  // 与增强库的 user_account 对齐（/api/nuser/account/get 的 weapi 形态）。
  // 匿名时服务端返回 code:200 + profile:null，不是 404，可直接判未登录。
  var data = JSON.parse(
    post('/weapi/nuser/account/get', weapi({ csrf_token: csrfToken() }))
  );
  var p = data && data.profile;
  if (!p) return { loggedIn: false };
  // vipType: 0 无 / 10 音乐包 / 11 黑胶 VIP
  var vip = Number(p.vipType) || 0;
  return {
    loggedIn: true,
    userId: p.userId || 0,
    nickname: p.nickname || '',
    avatarUrl: httpsUrl(p.avatarUrl || ''),
    vipType: vip,
    vipLabel: vip >= 11 ? '黑胶VIP' : vip > 0 ? '音乐包' : ''
  };
}
