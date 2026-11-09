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

/** 降级重试专用的档位顺序：从这一档开始**往低**问。
 *  普通探测用 levelFallback 是逐档往上（服务端偶尔会给更高一档的完整曲），
 *  但重试的意义是把码率压下来救回播放，再回头问更高档只会重蹈覆辙。 */
function levelFallbackDown(want) {
  var start = LEVELS.indexOf(want);
  if (start < 0) start = LEVELS.indexOf('exhigh');
  var out = [];
  for (var k = start; k >= 0; k--) out.push(LEVELS[k]);
  return out;
}

/** 只有「无损及以上」才值得向解析服务要 flac（br=999）。
 *  实测咪咕 h5v2.4 `br=999` 对 VIP 曲会下发真 FLAC（16bit/44.1kHz 全曲），
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

/** 档位 → 解析请求码率。智能音质降级重试时用它覆盖 `unblockQuality`：
 *  用户设置了 999，但这次是 320k 档在重试，就不该再要 FLAC 白等一轮。 */
function brForLevel(level) {
  if (level === 'lossless' || level === 'hires' || level === 'jymaster') return '999';
  if (level === 'standard') return '128';
  if (level === 'higher') return '192';
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
// 受限歌曲解析（可选，默认开启）
// ---------------------------------------------------------------------------
//
// 官方 eapi 对 fee=1 的曲目照样返回 code:200，只是给一条 30 秒试听直链：
//   freeTrialInfo: { start: 0, end: 30 }，time: 30040，br: 128012
// 免费曲这几项分别是 null / 0 / 320000，所以判定不需要靠解码时长去猜。
//
// 这里不尝试绕过权限，而是把「取一条完整直链」交给三个第三方源，按顺序
// 轮换（见下方「咪咕音源」「补位音源」区块）：酷我 BD 命中率与规格最高，
// 排第一；咪咕免登录、国内直连最稳，补酷我无版权的歌；GD 音乐台用网易
// 曲库兜底。低规格结果不提前收工，会继续问下一个源（见 resolveTrack）。
// 三个源全部失败，就老实回落官方试听（前端会提示只有 30 秒）。
/** 空配置 = 开启；只有显式写 off / 0 / false 才关闭 */
function unblockOn() {
  var v = (leon.config.get('unblock') || '').toLowerCase();
  return v !== 'off' && v !== '0' && v !== 'false';
}
/** 各源返回的规格是否已达到用户档位。
 *  br 是归一化值：900 = 无损（实际 800–2000k），320 / 128 = MP3。
 *  咪咕对部分曲目只给 128k/320k，这时不该挡住后面的无损源。 */
function qualityEnough(actualBr, wantBr) {
  var need = Number(wantBr) === 999 ? 900 : Number(wantBr) || 128;
  return (Number(actualBr) || 0) >= need;
}

/** 统一解析入口。
 *  2026-11-08 实测 12 首真实曲库（无损档）：酷我命中 10/12 且全为无损、
 *  平均 1.00s；咪咕命中 9/12、只有 7 首无损、平均 1.06s；GD 5/12、
 *  平均 1.91s。故酷我排第一，咪咕补它无版权的歌（如《海阔天空》），
 *  GD 兜底（独有 24bit 曲目，但常 503）。
 *  命中低规格时不收工：咪咕给 128k/320k 的《Dear God》《郭源潮》在旧
 *  「先命中先返回」下会挡住酷我的 FLAC，这里记下最好的继续问。 */
function resolveTrack(track, brOverride) {
  if (!unblockOn()) return null;
  var br = brOverride || unblockBr();
  var backends = [bodianResolve, miguResolve, gdResolve];
  var best = null;
  for (var i = 0; i < backends.length; i++) {
    var r = null;
    try {
      r = backends[i](track, br);
    } catch (e) {
      r = null; // 单个后端的任何异常都不能影响其它后端
    }
    if (!r || !r.url) continue;
    if (qualityEnough(r.br, br)) return r;
    if (!best || (Number(r.br) || 0) > (Number(best.br) || 0)) best = r;
  }
  return best;
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
 * 解析候选必须歌名 + 歌手都对上才采用（名字沿用历史命名）。
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
// 咪咕音源（主解析后端，链头）
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
// 补位音源：酷我 BD（bodian）与 GD 音乐台
// ---------------------------------------------------------------------------
//
// 2026-11-08 用 12 首真实曲库曲目（无损档）逐个单源实测：
//   bodian 命中 10/12、全部无损、平均 1.00s；
//   migu   命中  9/12、其中 7 首无损、平均 1.06s；
//   gd     命中  5/12、平均 1.91s（最长 4.0s，常 503）。
// 所以 bodian 领头、migu 补缺、gd 兜底（见 resolveTrack）。细节：
//   bodian —— 酷我 BD 接口。先打一次「广告免单」拿解锁位，再用
//             md5(kuwotest + 排序后的查询串 + path) 签名换直链。
//             br=2000kflac 能拿真 FLAC（晴天 55MB / 稻香 26MB / We Are The
//             World 91MB 的 24bit），命中率最高。直链实测 5 分钟后仍 206，
//             取到即播即可，但不要长期缓存。
//   gd     —— GD 音乐台（与 VutronMusic 的 pyncmd 同源），用**网易曲库
//             ID** 直接换直链，补 bodian 拿不到的网易独家（《迷恋》24bit
//             80MB 只有它有）；限流 503 频繁，失败后冷却 10 分钟。
// 酷我 antiserver 故意没接：与 bodian 同曲库，且 VIP 曲只下发固定
// 181521 字节（30 秒试听），接进来属于「假成功」，比没有更糟。

var KUWO_SEARCH_API =
  'http://search.kuwo.cn/r.s?&correct=1&vipver=1&stype=comprehensive&encoding=utf8' +
  '&rformat=json&mobi=1&show_copyright_off=1&searchapi=6&all=';
var BODIAN_AUDIO_API = 'http://bd-api.kuwo.cn/api/play/music/v2/audioUrl';
var BODIAN_AD_API =
  'http://bd-api.kuwo.cn/api/service/advert/watch?uid=-1&token=' +
  '&timestamp=1724306124436&sign=15a676d66285117ad714e8c8371691da';
var GD_API = 'https://music-api.gdstudio.xyz/api.php';

var kuwoDeviceId = '';
var bodianUnlocked = false;
var bodianCooldownUntil = 0;
var gdCooldownUntil = 0;

/** 设备号：BD 接口按 devid 记解锁位，同一次会话复用即可 */
function kuwoDevice() {
  if (!kuwoDeviceId) {
    kuwoDeviceId = String(Math.floor(Math.random() * 100000000000));
  }
  return kuwoDeviceId;
}

/** 搜索接口偶尔带 HTML 实体，解码后再参与匹配 */
function decodeHtmlEntities(s) {
  return String(s || '')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&nbsp;/g, ' ');
}

/** 酷我搜索（searchapi=6 免签名）：返回 [{id,name,artist,durationMs}] */
function kuwoSearch(query) {
  var q = String(query || '').replace(/\s+/g, ' ').trim();
  if (!q) return [];
  var body = leon.http.get(KUWO_SEARCH_API + encodeURIComponent(q), { timeoutMs: 4000 });
  var json = null;
  try {
    json = JSON.parse(body);
  } catch (e) {
    return [];
  }
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

/** 歌名+歌手严格匹配（复用咪咕那套 gdCandidateMatches），并优先选时长对得上的候选：
 *  酷我同一首歌常同时有「15 秒片段」「Live」「未定义」等条目，只按名字取会挑中片段。
 *  搜索走的是 bd-api 同域，服务挂了每首都会白等一次 4s 超时——这里冷却 2 分钟。 */
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
    try {
      list = kuwoSearch(track.title);
    } catch (e) {
      bodianCooldownUntil = Date.now() + 2 * 60 * 1000;
      return null;
    }
    return pickKuwo(track, list);
  }
  return null;
}

function pickKuwo(track, list) {
  var want = Number(track.durationMs) || 0;
  for (var i = 0; i < list.length; i++) {
    var item = list[i];
    if (!gdCandidateMatches(track, item)) continue;
    var got = Number(item.durationMs) || 0;
    // 时长信息缺失时只能信名字；有信息但差超过 20 秒 → 不是同一版本，宁缺毋滥
    if (want > 0 && got > 0 && Math.abs(got - want) > 20000) continue;
    return item;
  }
  return null;
}

/** Range 0-1 探测真实文件长度，反推时长是否够整曲（拦 30/60 秒试听片段）。
 *  声明码率只是估算：FLAC 按 900k 估会比真实时长更长，不会误杀整曲。 */
function audioLengthOk(url, br, wantMs) {
  wantMs = Number(wantMs) || 0;
  if (!wantMs) return true;
  var res = null;
  try {
    res = leon.http.getEx(url, { Range: 'bytes=0-1' });
  } catch (e) {
    return true; // 探测失败不拦，宁可放行也别误杀整曲
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

/** bodian 签名：md5('kuwotest' + 排序后的查询字符 + path)，追加 timestamp */
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
    try {
      json = JSON.parse(r && r.body);
    } catch (e) {
      json = null;
    }
    bodianUnlocked = !!(json && json.code === 200);
  } catch (e) {
    // 服务不可达：冷却 5 分钟，别让每首歌都白等一次 20s 默认超时
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
  try {
    return JSON.parse(body);
  } catch (e) {
    return null;
  }
}

/** bodian 取链：2000kflac / 320kmp3 两档，解锁位失效时重打一次免单 */
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
  // 独家曲也可能只下发片段（实测 241KB ≈ 6s），长度不够就交给下一个后端
  if (!audioLengthOk(url, declared, track.durationMs)) return null;
  return { url: url, br: declared };
}

/** GD 音乐台：直接用网易 ID 换直链，限流时冷却 10 分钟 */
function gdResolve(track, br) {
  if (Date.now() < gdCooldownUntil) return null;
  var id = String(track.id || '');
  if (!/^\d+$/.test(id)) return null;
  var quality = Number(br) >= 900 ? '999' : '320';
  var url = GD_API + '?types=url&source=netease&id=' + id + '&br=' + quality;
  var body = null;
  try {
    body = leon.http.get(url, { timeoutMs: 4000 });
  } catch (e) {
    body = null;
  }
  var json = null;
  try {
    json = JSON.parse(body);
  } catch (e) {
    // 503 / 维护页 → 冷却，链尾后端不该反复拖慢首播
    gdCooldownUntil = Date.now() + 10 * 60 * 1000;
    return null;
  }
  if (!json || !json.url || !json.br) return null;

  var realBr = Number(json.br) || 0;
  var size = Number(json.size) || 0;
  var wantMs = Number(track.durationMs) || 0;
  if (size > 0 && realBr > 0 && wantMs > 90000) {
    var seconds = (size * 8) / (realBr * 1000);
    if (seconds < (wantMs / 1000) * 0.5) return null; // 30 秒试听片段
  }
  return { url: json.url, br: realBr >= 900 ? 900 : realBr };
}

// ---------------------------------------------------------------------------
// 协议入口
// ---------------------------------------------------------------------------

function search(keyword, page, limit) {

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

/** QQ 音乐导入曲目的开播：QQ 侧拿不到直链，按歌名 + 歌手走第三方解析链。
 *  与网易云受限曲目同一条链（酷我 BD 领头、咪咕补位），区别有两个：
 *  GD 用不了——它按网易云 id 取链，`qq:` 曲目没有这个 id；
 *  没有官方试听可回落，两源都空就直接报错，让 UI 明确提示而不是放 30 秒。 */
function qqStream(track) {
  if (!unblockOn()) {
    throw new Error('QQ 音乐曲目要靠「受限歌曲解析」取直链，请先在插件设置里打开');
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
    if (qualityEnough(r.br, br)) {
      best = r;
      break;
    }
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

function getStreamUrl(track) {
  // 外部歌单曲目（QQ 音乐导入）：没有官方直链，直接走第三方解析
  if (String((track && track.id) || '').indexOf('qq:') === 0) return qqStream(track);
  var want = leon.config.get('level') || 'exhigh';
  // 智能音质降级：宿主重试时用 `qualityHint` 指定这一次实际请求的档位。
  // `want` 仍是用户选的档位，只用于 requested / downgraded —— 选的是无损，
  // 界面就该一直如实显示「无损未生效」，而不是悄悄变成 320k 却不解释。
  var hint = track && track.qualityHint ? String(track.qualityHint) : '';
  var attempt = hint || want;
  var levels = hint ? levelFallbackDown(attempt) : levelFallback(attempt);
  var attemptLossless = attempt === 'lossless' || attempt === 'hires' || attempt === 'jymaster';
  var userWantsLossless = want === 'lossless' || want === 'hires' || want === 'jymaster';

  var lastCode = 0;
  var trial = null;
  // 选了无损、官方却只给 MP3：先记下来，循环结束后去第三方要 FLAC。
  // 2026-11-08 用户实测《水之湄》即此类：官方给 320k 非试听，旧逻辑
  // 「非试听直接 return」把第三方源彻底跳过，于是永远显示「无损未生效」。
  var official = null;
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
      if (!isTrialUrlItem(item)) {
        // 免费曲 / 会员已解锁：直接返回，不打扰第三方服务。
        if (!attemptLossless || meta.format === 'flac') return streamResult(item.url, meta);
        // 官方静默降级成 MP3：记下这条合法全曲，再去问第三方有没有 FLAC。
        // 已是「非试听」，继续降档问不会变好，直接跳出循环。
        if (!official) official = { url: item.url, meta: meta };
        break;
      }
      if (!trial) trial = { url: item.url, meta: meta };
    }
    if (item && item.code) lastCode = item.code;
  }

  // 官方不给全曲、或只给 MP3 时解析；失败再退回官方全曲 / 试听
  if (unblockOn()) {
    // 降级重试时按这一次的档位要码率，不再沿用设置页里的 unblockQuality
    var resolved = resolveTrack(track, hint ? brForLevel(attempt) : null);
    if (resolved && resolved.url) {
      var fmt = formatFromUrl(resolved.url);
      var resolvedBr = Number(resolved.br) || 0;
      // 官方已有合法全曲（只是不到无损）时，只有解析确实拿到 FLAC 才替换：
      // 解析也可能只给 128k/320k，官方链更稳，不该越换越差。
      if (!official || (resolvedBr >= 900 && fmt === 'flac')) {
        return streamResult(resolved.url, {
          source: 'resolved',
          bitrate: resolved.br ? Math.round(resolved.br * 1000) : null,
          format: fmt,
          trial: false,
          requested: want,
          // 用户选的是无损、这一档却只拿到 mp3，仍算降级
          downgraded: userWantsLossless && fmt !== 'flac'
        });
      }
    }
  }
  // 官方 320k 全曲优先于试听：至少是完整的一首
  if (official) return streamResult(official.url, official.meta);
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
  var sid = String(id);
  // 外部歌单：`qq:<disstid>` 由本插件的 QQ 分支拉取，曲目 id 用前缀区分音源
  if (sid.indexOf('qq:') === 0) return qqPlaylist(sid.slice(3));
  var body = weapi({ id: sid, n: 1000, s: 8, csrf_token: csrfToken() });
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
// 外部歌单导入（QQ 音乐）
// ---------------------------------------------------------------------------
//
// 2026-11-09 实测：`fcg_ucc_getcdinfo_byids_cp.fcg` 匿名可读公开歌单，
// 带 `Referer: https://y.qq.com/portal/playlist.html` 就能拿到歌单名、
// 封面与全量曲目（songname / singer / albumname / interval / songmid /
// albummid），不需要登录，也不会往任何账号写东西——导入只落本地库。
// 曲目不给现成图片 URL，但每首都带 albummid，可拼出专辑封面。
//
// 播放侧拿不到 QQ 的 vkey（匿名 uin=0 被服务端判 500003/860100001），
// 所以曲目 id 记成 `qq:<songmid>`，开播时按「歌名 + 歌手」去酷我 BD /
// 咪咕找直链，与网易云受限曲目走的是同一条解析链。

var QQ_DISS_API =
  'https://c.y.qq.com/qzone/fcg-bin/fcg_ucc_getcdinfo_byids_cp.fcg';

/** 专辑封面 CDN。歌单曲目只有 albummid，没有现成图片地址；
 *  用 songmid 拼会 404（2026-11-09 实测），必须用专辑 mid。
 *  y.gtimg.cn 不需要 Referer，直出 JPEG；1292 首样本 albummid 覆盖率 100%。
 *  300×300 在播放页 / 高 DPI 屏上会糊，改取 800×800（12/12 抽样实测均为
 *  真 800×800；1000 会 404、1500 单张约 550 KB 过重）。
 *  与独立 qq 插件的 coverUrl 保持一致，改域名 / 尺寸时两处都要动。 */
function qqCoverUrl(song) {
  var mid = song.albummid ||
    (song.album && (song.album.mid || song.album.pmid)) || '';
  if (!mid) return '';
  return 'https://y.gtimg.cn/music/photo_new/T002R800x800M000' + mid + '.jpg';
}

function qqPlaylist(disstid) {
  var url =
    QQ_DISS_API +
    '?type=1&json=1&onlysong=0&disstid=' + encodeURIComponent(disstid) +
    '&format=json&g_tk=5381&loginUin=0&hostUin=0&inCharset=GB2312' +
    '&outCharset=utf-8&notice=0&platform=yqq&needNewCode=0';
  var res = leon.http.getEx(url, {
    Referer: 'https://y.qq.com/portal/playlist.html',
    'User-Agent':
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36'
  });
  var data = JSON.parse((res && res.body) || '{}');
  if (Number(data.code) !== 0) {
    throw new Error('QQ 音乐歌单接口返回异常：code ' + data.code);
  }
  var cd = (data.cdlist || [])[0];
  if (!cd) {
    throw new Error('没拿到 QQ 音乐歌单：链接可能指向私密歌单，或该歌单已删除');
  }

  var songs = cd.songlist || [];
  // 极少数老歌可能缺 albummid：至少退回歌单封面，别让列表里全是无图占位符
  var fallbackCover = httpsUrl(cd.logo || '');
  var tracks = [];
  for (var i = 0; i < songs.length; i++) {
    var s = songs[i] || {};
    var mid = s.songmid || s.mid || '';
    if (!mid) continue;
    tracks.push({
      id: 'qq:' + mid,
      title: s.songname || s.name || '',
      artist: artistNames(s.singer),
      album: s.albumname || (s.album && s.album.name) || '',
      durationMs: (Number(s.interval) || 0) * 1000,
      coverUrl: qqCoverUrl(s) || fallbackCover
    });
  }
  if (!tracks.length) throw new Error('QQ 音乐歌单里没有可导入的曲目');
  return {
    id: 'qq:' + disstid,
    name: cd.dissname || ('QQ 音乐歌单 ' + disstid),
    coverUrl: httpsUrl(cd.logo || ''),
    trackCount: Number(cd.songnum) || tracks.length,
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
