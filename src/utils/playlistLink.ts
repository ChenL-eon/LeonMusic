/**
 * 音乐平台分享链接解析。
 *
 * 用户从网易云 / QQ 音乐等 App 里「分享 → 复制链接」拿到的一般是
 * 一段带文案的文本（`分享xxx的歌单《yyy》 https://...`），这里从中
 * 提取平台与歌单 id。目前只有网易云装了音源插件，其它平台先做到
 * 「识别得出来、提示明确」，等有插件了再接 import。
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
  /** 歌单 id 提取（可选） */
  id?: RegExp;
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
    hosts: ["y.qq.com", "i.y.qq.com", "c6.y.qq.com"],
    id: /\/playlist\/([A-Za-z0-9%]+)/,
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
    let short = false;
    const hostHit =
      rule.hosts.some((h) => lower.includes(h)) ||
      rule.shortHosts?.some((h) => lower.includes(h)) === true;
    if (!hostHit) continue;
    if (rule.shortHosts?.some((h) => lower.includes(h)) && !rule.hosts.some((h) => lower.includes(h))) {
      short = true;
    }

    let id: string | null = null;
    if (short) {
      // 短链：id 藏在重定向目标里，调用方先 resolve_redirect 再解析一次
      const urlMatch = text.match(/https?:\/\/\S+/i);
      id = urlMatch ? urlMatch[0] : null; // 暂存原始短链
    } else if (rule.id) {
      const m = text.match(rule.id);
      id = m ? m[1] : null;
      // 网易云：带 id 但不是歌单链接（单曲/专辑/电台），按无 id 处理
      if (rule.platform === "netease" && id && !looksLikeNeteasePlaylist(text)) {
        const path = text.toLowerCase();
        if (!path.includes("playlist")) id = null;
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
