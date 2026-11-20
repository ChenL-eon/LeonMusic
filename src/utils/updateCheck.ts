/**
 * 更新检测：纯查询逻辑 + 启动时自动检测的结果缓存。
 *
 * 之所以从 Settings.vue 抽出来：启动时的自动检测发生在 App.vue，而检测结果
 * 又要能被「更新与关于」页直接复用（否则用户看到提示后进去还得再点一次
 * 「检测更新」）。两边共用同一份 CHANNELS / 版本比较 / 附件筛选，避免漂移。
 *
 * 本模块不碰任何 UI 状态：失败抛 Error，文案由调用方决定。
 */
import { ref } from "vue";

export const CHANNELS = {
  cn: {
    name: "国内",
    host: "Gitee",
    api: "https://gitee.com/api/v5/repos/byging/leon-music-releases/releases?per_page=20",
    page: "https://gitee.com/byging/leon-music-releases/releases",
    // Gitee 的 /releases/latest 不排除预览版，用列表接口自己挑正式版
    list: true,
  },
  global: {
    name: "国外",
    host: "GitHub",
    api: "https://api.github.com/repos/ChenL-eon/LeonMusic/releases/latest",
    page: "https://github.com/ChenL-eon/LeonMusic/releases",
    list: false,
  },
} as const;

export type Channel = keyof typeof CHANNELS;

export interface ReleaseAsset {
  name: string;
  url: string;
}

export interface ReleaseInfo {
  tag: string;
  notes: string;
  /** 发行版网页地址：应用内升级不可用时作为兜底 */
  page: string;
  /** 当前平台能用的安装包；为 null 时只能去下载页 */
  asset: ReleaseAsset | null;
}

export type CheckOutcome =
  | { kind: "latest" }
  | { kind: "available"; release: ReleaseInfo };

/**
 * 启动时自动检测的结果。Settings.vue 挂载时（或结果稍后到达时）直接展示，
 * 不再重复发一次请求；失败不写入，用户可以手动重试。
 */
export const autoCheckResult = ref<{
  at: number;
  channel: Channel;
  outcome: CheckOutcome;
} | null>(null);

/** 三段数字版本比较（v 前缀容忍）；返回 <0 / 0 / >0。非法段按 0。 */
export function cmpVersion(a: string, b: string): number {
  const pa = a.replace(/^v/i, "").split(".").map((n) => parseInt(n, 10) || 0);
  const pb = b.replace(/^v/i, "").split(".").map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    const d = (pa[i] || 0) - (pb[i] || 0);
    if (d) return d;
  }
  return 0;
}

/** release notes 的轻量净化：剥 markdown 标记、截断到一屏内。 */
export function cleanNotes(body: string): string {
  return body
    .replace(/^#+\s/gm, "")
    .replace(/\*\*/g, "")
    .replace(/\[(.*?)\]\(.*?\)/g, "$1")
    .trim()
    .slice(0, 800);
}

/** 从发行版附件里挑当前平台要的那一个。
 *
 *  Windows 用便携包 zip（解出 exe 直接替换，不写注册表、不需要管理员）；
 *  Android 用 arm64 APK。Gitee 还会把源码 zip 一起放进 assets，
 *  所以必须按文件名后缀精确匹配，不能取第一个。 */
export function pickAsset(assets: unknown, kind: string): ReleaseAsset | null {
  const list = Array.isArray(assets) ? assets : [];
  const match = (re: RegExp): ReleaseAsset | null => {
    for (const raw of list) {
      const a = raw as { name?: unknown; browser_download_url?: unknown };
      const name = typeof a?.name === "string" ? a.name : "";
      const url = typeof a?.browser_download_url === "string" ? a.browser_download_url : "";
      if (url && re.test(name)) return { name, url };
    }
    return null;
  };
  if (kind === "android") return match(/android.*\.apk$/i);
  if (kind === "windows") return match(/windows.*-portable\.zip$/i);
  return null;
}

/** 把两种接口的响应归一成一个发行版对象。
 *
 *  GitHub：`/releases/latest` 直接给对象，且已排除草稿与预览版；
 *  Gitee：只有列表接口可用，顺序不保证、也不排除预览版 —— 这里按创建时间
 *  倒序挑最新的**正式版**，避免把 v1.4.0-beta 推给所有稳定版用户。
 *
 *  两家对「还没有发行版」的形态也不一样（404 / 200+null / 空数组），
 *  所以调用方不能只看 `resp.ok`。 */
export function pickRelease(payload: unknown, list: boolean): Record<string, unknown> | null {
  const isObj = (v: unknown): v is Record<string, unknown> =>
    !!v && typeof v === "object" && !Array.isArray(v);
  if (!list) return isObj(payload) ? payload : null;

  const ts = (v: unknown) => {
    const n = Date.parse(String(v ?? ""));
    return Number.isFinite(n) ? n : 0;
  };
  const stable = (Array.isArray(payload) ? payload : [])
    .filter(isObj)
    .filter((r) => !r.prerelease && typeof r.tag_name === "string")
    .sort((a, b) => ts(b.created_at) - ts(a.created_at));
  return stable[0] ?? null;
}

/**
 * 单次检测。成功返回 latest / available；网络、接口或解析异常一律抛 Error，
 * 消息沿用设置页原来的中文文案，启动检测则直接吞掉异常（静默）。
 */
export async function checkRelease(opts: {
  channel: Channel;
  platformKind: string;
  appVersion: string;
}): Promise<CheckOutcome> {
  // 没有可信的本地版本就不能比较：旧实现用 "0.0.0" 兜底，
  // 空版本会把任意 release 判成「有新版」，启动时误报。宁可不报。
  if (!opts.appVersion.trim()) throw new Error("无法确定当前版本，无法检测更新");
  const ch = CHANNELS[opts.channel];
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 15000);
  let resp: Response;
  try {
    resp = await fetch(ch.api, {
      signal: ctrl.signal,
      cache: "no-store",
      headers: { Accept: "application/json" },
    });
  } finally {
    clearTimeout(timer);
  }

  if (resp.status === 404) throw new Error("该渠道还没有发布过版本，可稍后再试");
  if (resp.status === 403) throw new Error(`${ch.host} 接口限流，请稍后再试`);
  if (!resp.ok) throw new Error(`接口返回 HTTP ${resp.status}`);

  const payload = (await resp.json()) as unknown;
  const data = pickRelease(payload, ch.list);
  if (!data) throw new Error("该渠道还没有发布过正式版本，可稍后再试");

  const tag = typeof data.tag_name === "string" ? data.tag_name.trim() : "";
  if (!tag) throw new Error("接口响应里没有版本号");
  if (cmpVersion(tag, opts.appVersion || "0.0.0") <= 0) return { kind: "latest" };

  const notes = typeof data.body === "string" ? data.body : "";
  const page = typeof data.html_url === "string" ? data.html_url : ch.page;
  return {
    kind: "available",
    release: {
      tag,
      notes: cleanNotes(notes),
      page,
      asset: pickAsset(data.assets, opts.platformKind),
    },
  };
}
