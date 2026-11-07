// LeonMusic 音源插件：Emby
//
// Emby 与 Jellyfin 的 REST 接口同源，差别只在部署习惯：
// Emby 的 API 常挂在 /emby 前缀下（baseUrl 填写时带上即可），
// 且 Emby 对 api_key query 参数的支持一直保留。

function base() {
  var url = leon.config.get('baseUrl');
  if (!url) throw new Error('未配置 Emby 服务器地址');
  return url.replace(/\/+$/, '');
}

function auth() {
  var key = leon.config.get('apiKey');
  if (!key) throw new Error('请在插件设置里填写 Emby API Key');
  return 'api_key=' + encodeURIComponent(key);
}

function getJson(url) {
  var text = leon.http.get(url);
  try {
    return JSON.parse(text);
  } catch (e) {
    throw new Error('Emby 返回的不是 JSON，请检查地址（是否漏了 /emby）与 API Key');
  }
}

function artistsOf(item) {
  if (item.Artists && item.Artists.length) return item.Artists.join(' / ');
  if (item.AlbumArtist) return item.AlbumArtist;
  return '';
}

function toTrack(item) {
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

function getStreamUrl(track) {
  return base() + '/Audio/' + encodeURIComponent(track.id) +
    '/stream?static=true&' + auth();
}
