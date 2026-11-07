// LeonMusic 音源插件：Jellyfin
//
// Jellyfin 的 API Key 既能放 header（X-Emby-Token）也能放 query（api_key），
// 沙箱只暴露 leon.http.get(url)，所以统一走 query。
// 副作用：key 会出现在 URL 里；对自建内网服务可以接受，公网请用 HTTPS。

function base() {
  var url = leon.config.get('baseUrl');
  if (!url) throw new Error('未配置 Jellyfin 服务器地址');
  return url.replace(/\/+$/, '');
}

function auth() {
  var key = leon.config.get('apiKey');
  if (!key) throw new Error('请在插件设置里填写 Jellyfin API Key');
  return 'api_key=' + encodeURIComponent(key);
}

function getJson(url) {
  var text = leon.http.get(url);
  try {
    return JSON.parse(text);
  } catch (e) {
    throw new Error('Jellyfin 返回的不是 JSON，请检查地址与 API Key');
  }
}

function artistsOf(item) {
  if (item.Artists && item.Artists.length) return item.Artists.join(' / ');
  if (item.AlbumArtist) return item.AlbumArtist;
  return '';
}

function toTrack(item) {
  // RunTimeTicks 单位是 100ns，除以 10000 得到毫秒
  var ticks = Number(item.RunTimeTicks) || 0;
  return {
    id: String(item.Id),
    title: item.Name || '',
    artist: artistsOf(item),
    album: item.Album || '',
    durationMs: Math.round(ticks / 10000),
    coverUrl: base() + '/Items/' + encodeURIComponent(item.Id) +
      '/Images/Primary?maxWidth=400&' + auth()
  };
}

function search(keyword, page, limit) {
  var size = limit || 30;
  var start = ((page || 1) - 1) * size;
  var userId = leon.config.get('userId');

  var url = base() + '/Items?' + auth() +
    '&SearchTerm=' + encodeURIComponent(keyword) +
    '&Recursive=true' +
    '&IncludeItemTypes=Audio' +
    '&Limit=' + size +
    '&StartIndex=' + start +
    '&Fields=Album,Artists,AlbumArtist,RunTimeTicks' +
    '&SortBy=SortName';
  if (userId) url += '&userId=' + encodeURIComponent(userId);

  var data = getJson(url);
  var items = data.Items || [];
  var out = [];
  for (var i = 0; i < items.length; i++) out.push(toTrack(items[i]));
  return out;
}

// static=true 直接发原始文件，不让服务端转码：
// 音乐本来就是目标格式，转码只会掉音质、加延迟
function getStreamUrl(track) {
  return base() + '/Audio/' + encodeURIComponent(track.id) +
    '/stream?static=true&' + auth();
}
