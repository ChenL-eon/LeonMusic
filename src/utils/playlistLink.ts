/**
 * 音乐平台分享链接解析。
 *
 * 用户从网易云 / QQ 音乐等 App 里「分享 → 复制链接」拿到的一般是
 * 一段带文案的文本（`分享xxx的歌单《yyy》 https://...`），这里从中
 * 提取平台与歌单 id。
 *
 * 目前**能真正导入**的是网易云与 QQ 音乐：前者走 netease 插件的官方
 * 歌单接口，后者由同一个插件的「外部歌单」分支拉取（`qq:<disstid>`）。
 * 酷狗 / 酷我 / 咪咕 / B 站只做到识别平台，接口签名与风控各不相同，
 * 尚未接入——识别出来就明确告知，不假装支持。
 */

export type LinkPlatform =
  | "netease"
  | "qq"
  | "kugou"
  | "kuwo"
  | "migu"
  | "bilibili";

export interface ParsedLink {
  platform: LinkPlatform;
  platformLabel: string;
  /** 歌单 id（网易云一定有；其它平台不解析） */
  id: string | null;
  /** 短链（163cn.tv 等）：拿 id 前要先跟随重定向 */
  short: boolean;
}

interface Rule {
  platform: LinkPlatform;
  label: string;
  /** 域名匹配（对整段文本做小写包含匹配） */
  hosts: string[];
  /** 短链域名：只标记，不在此解析 id */
  shortHosts?: string[];
  /** 短链路径特征（域名与完整链同一主域时用，如 QQ 的 fcgi-bin/u） */
  shortPath?: RegExp;
  /** 歌单 id 提取（可选） */
  id?: RegExp;
  /** id 的备用写法（分享页常把 id 放在 query 里） */
  idFallback?: RegExp;
}

const RULES: Rule[] = [
  {
    platform: "netease",
    label: "网易云",
    hosts: ["music.163.com", "music.126.net"],
    shortHosts: ["163cn.tv"],
    id: /playlist[^a-z0-9]*(?:#\/)?[^a-z0-9]*[?&/]?id=(\d+)/i,
  },
  {
    platform: "qq",
    label: "QQ 音乐",
    // 分享链有三种形态：网页版 /playlist/<id>、分享页 taoge.html?id=<id>、
    // 以及 c6.y.qq.com/base/fcgi-bin/u?__=xxx 短链（要跟随跳转才拿得到 id）
    hosts: ["y.qq.com", "i.y.qq.com", "c6.y.qq.com"],
    shortPath: /fcgi-bin\/u\?/,
    id: /\/playlist\/(\d+)/,
    idFallback: /[?&]id=(\d+)/,
  },
  { platform: "kugou", label: "酷狗音乐", hosts: ["kugou.com", "kgmusic"] },
  { platform: "kuwo", label: "酷我音乐", hosts: ["kuwo.cn", "kuwo.com"] },
  { platform: "migu", label: "咪咕音乐", hosts: ["migu.cn", "miguvideo"] },
  { platform: "bilibili", label: "B 站", hosts: ["b23.tv", "bilibili.com"] },
];

/** 网易云分享文本里常见「歌单」字样，用来区分单曲/专辑链接 */
function looksLikeNeteasePlaylist(text: string): boolean {
  return /歌单|playlist/i.test(text);
}

/**
 * 从任意文本（纯链接或带文案的分享文本）解析歌单链接。
 * 返回 null 表示完全识别不出音乐平台。
 */
export function parsePlaylistLink(raw: string): ParsedLink | null {
  const text = raw.trim();
  if (!text) return null;
  const lower = text.toLowerCase();

  for (const rule of RULES) {
    const hostHit =
      rule.hosts.some((h) => lower.includes(h)) ||
      rule.shortHosts?.some((h) => lower.includes(h)) === true;
    if (!hostHit) continue;

    // 先按完整链的正则取 id；取不到再判定是不是短链——短链的 id 藏在
    // 重定向目标里，调用方先 resolve_redirect 再解析一次。
    let id: string | null = null;
    if (rule.id) {
      const m = text.match(rule.id);
      id = m ? m[1] : null;
    }
    if (!id && rule.idFallback) {
      const m = text.match(rule.idFallback);
      id = m ? m[1] : null;
    }
    // 网易云：带 id 但不是歌单链接（单曲/专辑/电台），按无 id 处理
    if (rule.platform === "netease" && id && !looksLikeNeteasePlaylist(text)) {
      if (!lower.includes("playlist")) id = null;
    }

    let short = false;
    if (!id) {
      const shortHostHit =
        rule.shortHosts?.some((h) => lower.includes(h)) === true &&
        !rule.hosts.some((h) => lower.includes(h));
      const shortPathHit = !!rule.shortPath && rule.shortPath.test(lower);
      if (shortHostHit || shortPathHit) {
        short = true;
        const urlMatch = text.match(/https?:\/\/\S+/i);
        id = urlMatch ? urlMatch[0] : null; // 暂存原始短链
      }
    }

    return { platform: rule.platform, platformLabel: rule.label, id, short };
  }
  return null;
}

/** 从已跟随重定向的最终 URL 里提取网易云歌单 id */
export function neteasePlaylistIdFromUrl(url: string): string | null {
  if (!url.toLowerCase().includes("music.163.com")) return null;
  if (!url.toLowerCase().includes("playlist")) return null;
  const m = url.match(/[?&]id=(\d+)/);
  return m ? m[1] : null;
}

/** 从已跟随重定向的最终 URL 里提取 QQ 音乐歌单 id（disstid） */
export function qqPlaylistIdFromUrl(url: string): string | null {
  if (!url.toLowerCase().includes("qq.com")) return null;
  const byPath = url.match(/\/playlist\/(\d+)/);
  if (byPath) return byPath[1];
  const byQuery = url.match(/[?&](?:id|disstid)=(\d+)/);
  return byQuery ? byQuery[1] : null;
}
