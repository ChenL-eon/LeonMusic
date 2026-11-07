// LeonMusic 音源插件：Navidrome / Subsonic API
//
// 认证用 Subsonic 的 token 方式：t = md5(password + salt)，不传明文密码。
// salt 固定成 "leonmusic" 有两个原因：
//   1. 每次请求换随机 salt 会让封面/流地址每次都不同，缓存全部失效
//   2. Subsonic 服务端只校验 md5(pwd+salt)，不要求 salt 随机
// 代价是同一个用户 token 恒定。若你的服务器暴露在公网，请务必用 HTTPS。

var API_VERSION = '1.16.1';
var CLIENT = 'LeonMusic';
var SALT = 'leonmusic';

function base() {
  var url = leon.config.get('baseUrl');
  if (!url) throw new Error('未配置 Navidrome 服务器地址');
  return url.replace(/\/+$/, '');
}

function auth() {
  var user = leon.config.get('username');
  var pwd = leon.config.get('password');
  if (!user || !pwd) throw new Error('请在插件设置里填写用户名与密码');
  return (
    'u=' + encodeURIComponent(user) +
    '&t=' + leon.md5(pwd + SALT) +
    '&s=' + SALT +
    '&v=' + API_VERSION +
    '&c=' + CLIENT +
    '&f=json'
  );
}

function endpoint(path, params) {
  var url = base() + '/rest/' + path + '?' + auth();
  if (params) url += '&' + params;
  return url;
}

function call(path, params) {
  var text = leon.http.get(endpoint(path, params));
  var data;
  try {
    data = JSON.parse(text);
  } catch (e) {
    throw new Error('服务器返回的不是 JSON，请检查地址是否指向 Navidrome');
  }
  var res = data['subsonic-response'];
  if (!res) throw new Error('不是 Subsonic 格式的响应');
  if (res.status && res.status !== 'ok') {
    var msg = res.error && res.error.message ? res.error.message : res.status;
    throw new Error('Navidrome 错误: ' + msg);
  }
  return res;
}

function joinArtists(song) {
  if (song.artists && song.artists.length) {
    var out = [];
    for (var i = 0; i < song.artists.length; i++) out.push(song.artists[i].name || '');
    return out.join(' / ');
  }
  return song.artist || '';
}

function toTrack(song) {
  // duration 是秒；coverArt 是封面 ID，不是 URL
  var coverId = song.coverArt || song.albumId || '';
  return {
    id: String(song.id),
    title: song.title || song.name || '',
    artist: joinArtists(song),
    album: song.album || '',
    durationMs: (Number(song.duration) || 0) * 1000,
    coverUrl: coverId ? endpoint('getCoverArt.view', 'id=' + encodeURIComponent(coverId)) : ''
  };
}

function search(keyword, page, limit) {
  var size = limit || 30;
  var offset = ((page || 1) - 1) * size;
  var res = call(
    'search3.view',
    'query=' + encodeURIComponent(keyword) +
      '&songCount=' + size +
      '&songOffset=' + offset +
      '&artistCount=0&albumCount=0'
  );
  var result = res.searchResult3 || res.searchResult2 || {};
  var songs = result.song || [];
  var out = [];
  for (var i = 0; i < songs.length; i++) out.push(toTrack(songs[i]));
  return out;
}

function getStreamUrl(track) {
  return endpoint('stream.view', 'id=' + encodeURIComponent(track.id));
}

// OpenSubsonic 的 getLyricsBySongId 只有较新版本支持，
// 老服务器会直接 404 —— 歌词是可选项，失败就返回空，不能让播放报错。
function getLyric(track) {
  try {
    var res = call('getLyricsBySongId.view', 'id=' + encodeURIComponent(track.id));
    var lists = (res.lyricsList && res.lyricsList.structuredLyrics) || [];
    if (!lists.length) return '';
    var lines = lists[0].line || [];
    var out = [];
    for (var i = 0; i < lines.length; i++) {
      var line = lines[i];
      out.push('[' + toLrcTime(line.start || 0) + ']' + (line.value || ''));
    }
    return out.join('\n');
  } catch (e) {
    return '';
  }
}

function toLrcTime(ms) {
  var total = Math.floor(ms / 10); // 百分秒
  var cs = total % 100;
  var sec = Math.floor(total / 100) % 60;
  var min = Math.floor(total / 6000);
  return pad(min) + ':' + pad(sec) + '.' + pad(cs);
}

function pad(n) {
  return n < 10 ? '0' + n : String(n);
}
