<script setup lang="ts">
/** 设置：外观 / 桌面歌词 / 播放 / 音源插件 / 存储 / 关于。 */
import { computed, onMounted, reactive, ref, watch } from "vue";

import BrandLogo from "@/components/BrandLogo.vue";
import Icon from "@/components/Icon.vue";
import {
  app as appApi,
  errorText,
  player as playerApi,
  plugins as pluginApi,
  settings as settingsApi,
} from "@/api/ipc";
import type { AudioDiag, PluginInfo } from "@/types";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";

const store = useSettingsStore();
const ui = useUiStore();

const tab = computed({
  get: () => ui.settingsTab,
  set: (v) => (ui.settingsTab = v),
});
const pluginDraft = reactive<Record<string, Record<string, string>>>({});
const busy = ref(false);

// ---------------------------------------------------------------------------
// 音频输出自检
// ---------------------------------------------------------------------------

const diag = ref<AudioDiag | null>(null);
const diagBusy = ref(false);

async function refreshDiag() {
  diagBusy.value = true;
  try {
    diag.value = await playerApi.audioDiag();
  } catch {
    diag.value = null;
  } finally {
    diagBusy.value = false;
  }
}

/** 运行期间切换了系统默认输出设备时，让常驻输出流重新绑定。 */
async function resetAudio() {
  diagBusy.value = true;
  try {
    await playerApi.audioReset();
    await new Promise((r) => setTimeout(r, 700));
    await refreshDiag();
    ui.toast("已重新连接音频输出设备", "success");
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    diagBusy.value = false;
  }
}

/**
 * 草稿要带上 manifest 的默认值：数据库里没存过配置时 config 是空对象，
 * 而「空 = 默认开启」这类语义必须让 UI 也显示成开启，否则开关状态会骗人。
 */
function configDraft(p: PluginInfo): Record<string, string> {
  const defaults: Record<string, string> = {};
  for (const f of p.manifest.config) {
    if (f.default != null) defaults[f.key] = f.default;
  }
  return { ...defaults, ...p.config };
}

/** 凭据归「账号」页、音质归「播放」页、解析链归「播放 → 解析播放源」，
 *  插件配置里不再暴露这些原始输入框。
 *  保存时 draft 仍是全量，所以改其它参数不会把 Cookie / 音质冲掉。 */
function configFields(p: PluginInfo) {
  const managed = [
    "cookie",
    "level",
    "unblockQuality",
    "unblockBackends",
    "ncmBase",
    "kgapiBase",
    "kgapiCookie",
  ];
  return p.manifest.config.filter((f) => !managed.includes(f.key));
}

// ---------------------------------------------------------------------------
// 在线音质：写进 netease 插件配置，与插件 `leon.config.get()` 读的是同一份数据
// ---------------------------------------------------------------------------

/** 对标网易云 App「音质选择」的常见档位，下拉里从低到高排列。
 *  实测（`music-api.gdstudio.xyz` br=999 直取 + Range 解 FLAC STREAMINFO）：
 *  同平台解析的 VIP 曲能拿到 44.1kHz/16bit FLAC 全曲，而官方 eapi 对
 *  lossless/hires/jymaster 一律静默降成 320k MP3。所以：
 *   - `br` 是选档时同步写入解析请求的码率（单一决策链，不再各管各的）
 *   - `available: false` 的两档在当前链路不可保证，只保留说明、禁止选择
 *  沉浸环绕声 / 杜比全景声需要专门解码设备，这里不提供。 */
const QUALITY_LEVELS = [
  { level: "standard", name: "标准", desc: "128 kbps · MP3", tag: "", br: "128", available: true },
  { level: "higher", name: "较高", desc: "192 kbps · MP3", tag: "", br: "192", available: true },
  { level: "exhigh", name: "极高", desc: "320 kbps · MP3", tag: "", br: "320", available: true },
  {
    level: "lossless",
    name: "无损",
    desc: "FLAC · 16bit / 44.1kHz",
    tag: "解析可得",
    br: "999",
    available: true,
  },
  {
    level: "hires",
    name: "Hi-Res",
    desc: "96kHz / 24bit · 暂不可用",
    tag: "不确保有效",
    br: "999",
    available: false,
  },
  {
    level: "jymaster",
    name: "高清臻音",
    desc: "192kHz / 24bit · 暂不可用",
    tag: "不确保有效",
    br: "999",
    available: false,
  },
] as const;

const UNBLOCK_BR = [
  { value: "128", label: "128 kbps" },
  { value: "192", label: "192 kbps" },
  { value: "320", label: "320 kbps" },
  { value: "999", label: "999 · 无损 FLAC（取决于源站）" },
] as const;

const qualityBusy = ref(false);
const neteasePlugin = computed(
  () => store.pluginList.find((p) => p.manifest.id === "netease") ?? null,
);

function manifestDefault(p: PluginInfo, key: string, fallback: string) {
  return p.manifest.config.find((f) => f.key === key)?.default || fallback;
}

const currentLevel = computed(() => {
  const p = neteasePlugin.value;
  if (!p) return "exhigh";
  const saved = p.config.level || manifestDefault(p, "level", "exhigh");
  return QUALITY_LEVELS.some((q) => q.level === saved) ? saved : "exhigh";
});

const currentUnblockBr = computed(() => {
  const p = neteasePlugin.value;
  if (!p) return "320";
  const saved = p.config.unblockQuality || manifestDefault(p, "unblockQuality", "320");
  return UNBLOCK_BR.some((b) => b.value === saved) ? saved : "320";
});

/** 只改一个键，其余插件参数与已保存的 Cookie 原样保留。
 *  改完同步 pluginDraft：插件页保存别的参数时不会把旧音质写回来。 */
async function patchNeteaseConfig(patch: Record<string, string>, okText: string) {
  const p = neteasePlugin.value;
  if (!p || qualityBusy.value) return;
  qualityBusy.value = true;
  try {
    await store.setPluginConfig(p.manifest.id, { ...p.config, ...patch });
    const draft = pluginDraft[p.manifest.id];
    if (draft) Object.assign(draft, patch);
    ui.toast(okText, "success");
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    qualityBusy.value = false;
  }
}

/** 一个档位同时决定官方请求 level 与受限歌曲解析码率。
 *  以前两个设置各管各的：选「无损」但解析仍是 320，实际播的是 MP3。 */
function setQuality(q: (typeof QUALITY_LEVELS)[number]) {
  if (!q.available || q.level === currentLevel.value) return Promise.resolve();
  return patchNeteaseConfig(
    { level: q.level, unblockQuality: q.br },
    `已设为「${q.name}」${q.br === "999" ? "（解析请求无损）" : `（解析 ${q.br} kbps）`}，下一首开始生效`,
  );
}

/** 下拉项文案：把原先卡片上的档位名、规格和角标合成一行。 */
function qualityLabel(q: (typeof QUALITY_LEVELS)[number]) {
  const base = `${q.name} · ${q.desc}`;
  return q.available && q.tag ? `${base} · ${q.tag}` : base;
}

/** 保存失败时把控件拉回实际生效的档位，避免显示值与插件配置不一致。 */
function setQualityByLevel(e: Event) {
  const el = e.target as HTMLSelectElement;
  const level = el.value;
  const q = QUALITY_LEVELS.find((item) => item.level === level);
  if (!q) return;
  void setQuality(q).then(() => {
    if (currentLevel.value !== level) el.value = currentLevel.value;
  });
}

function setUnblockBr(e: Event) {
  const value = (e.target as HTMLSelectElement).value;
  if (value === currentUnblockBr.value) return;
  void patchNeteaseConfig({ unblockQuality: value }, "解析音质已更新，下一首开始生效");
}

// ---------------------------------------------------------------------------
// 解析播放源：手动开关 + 排序。数据落在插件配置 unblockBackends（逗号序列），
// 插件 resolveBackends() 启动解析时按这份序列逐个尝试 —— 全关存 "off"。
// 列表用本地序列做事实源（乐观更新 + 串行落库队列）：快速连点开关/排序
// 不会像「busy 直接 return」那样静默丢操作，也不会被 reload 回读覆盖中间态。
// ---------------------------------------------------------------------------

/** 与插件内置链一致的源元数据；数组顺序 = 默认链顺序。
 *  自建服务（ncm/kgapi）在链头：VutronMusic 同款架构，填了地址才进链。 */
const BACKEND_SOURCES = [
  { id: "ncm", name: "自建网易云 API", desc: "api-enhanced 服务（VutronMusic 同款），官方直链" },
  { id: "kgapi", name: "自建酷狗 API", desc: "KuGouMusicApi 服务，配酷狗 Cookie 可取账号权益内 VIP 曲" },
  { id: "gd", name: "GD 音乐台", desc: "社区解析，无损优先，可能限流" },
  { id: "migu", name: "咪咕音乐", desc: "免登录直链，国内直连最稳，可升无损" },
  { id: "kw", name: "酷我音乐", desc: "128k 直链，轻量兜底" },
  { id: "kg", name: "酷狗音乐", desc: "免费曲可升 320k/FLAC，独家 VIP 曲拿不到" },
  { id: "bili", name: "bilibili", desc: "DASH 音频，严格匹配标题并过滤翻唱" },
] as const;

/** 本地序列事实源：启用源的 id 按播放顺序；null = 尚未从插件配置初始化。 */
const backendSeq = ref<string[] | null>(null);

/** 串行落库队列 + 待写计数（须先于下方 immediate watch 声明）。 */
let backendPatchChain: Promise<unknown> = Promise.resolve();
let backendPending = 0;

function loadBackendSeq() {
  const p = neteasePlugin.value;
  if (!p) return;
  const raw = String(p.config.unblockBackends ?? "").trim();
  if (!raw) {
    // 空 = 默认全开、按内置链顺序
    backendSeq.value = BACKEND_SOURCES.map((b) => b.id);
  } else if (raw === "off" || raw === "none") {
    backendSeq.value = [];
  } else {
    backendSeq.value = [
      ...new Set(
        raw
          .split(",")
          .map((s) => s.trim())
          .filter((id) => BACKEND_SOURCES.some((b) => b.id === id)),
      ),
    ];
  }
}

/** 插件列表变化（保存后 reload）时回读权威值；有未落库的本地修改时跳过，
 *  等队列清空后的最后一次 reload 再同步（届时库里已是最终值）。 */
watch(
  neteasePlugin,
  () => {
    if (backendPending > 0) return;
    loadBackendSeq();
  },
  { immediate: true },
);

/** UI 行：启用的按本地序列在前（行下标即序列下标），禁用的按默认顺序缀后。 */
const backendRows = computed(() => {
  const seq = backendSeq.value ?? BACKEND_SOURCES.map((b) => b.id);
  const on = BACKEND_SOURCES.filter((b) => seq.includes(b.id)).sort(
    (a, b) => seq.indexOf(a.id) - seq.indexOf(b.id),
  );
  const off = BACKEND_SOURCES.filter((b) => !seq.includes(b.id));
  return [...on, ...off].map((b) => ({ ...b, on: seq.includes(b.id) }));
});

const backendOnCount = computed(() => (backendSeq.value ?? []).length);

const metingConfigured = computed(() => {
  const p = neteasePlugin.value;
  return p ? Boolean(String(configDraft(p).baseUrl ?? "").trim()) : false;
});

const ncmConfigured = computed(() => {
  const p = neteasePlugin.value;
  return p ? Boolean(String(configDraft(p).ncmBase ?? "").trim()) : false;
});

const kgapiConfigured = computed(() => {
  const p = neteasePlugin.value;
  return p ? Boolean(String(configDraft(p).kgapiBase ?? "").trim()) : false;
});

/** 自建服务地址 / 酷狗 Cookie 的保存：与解析源开关共用同一条串行落库队列，
 *  快速连续改动不会互相覆盖，也不会被 reload 回读冲掉中间态。 */
function saveSvcField(key: "ncmBase" | "kgapiBase" | "kgapiCookie", e: Event) {
  const p = neteasePlugin.value;
  if (!p) return;
  const el = e.target as HTMLInputElement;
  const v = el.value.trim();
  const cur = String(configDraft(p)[key] ?? "").trim();
  if (v === cur) return;
  backendPending++;
  const run = backendPatchChain
    .then(async () => {
      const cur2 = neteasePlugin.value;
      if (!cur2) return;
      await store.setPluginConfig(cur2.manifest.id, { ...cur2.config, [key]: v });
      const draft = pluginDraft[cur2.manifest.id];
      if (draft) draft[key] = v;
      ui.toast("已保存，下一首解析开始生效", "success");
    })
    .catch((err) => ui.toast(errorText(err), "error"))
    .finally(() => {
      backendPending--;
    });
  backendPatchChain = run;
}

function serializeBackends(): string {
  const seq = backendSeq.value ?? [];
  if (seq.length === 0) return "off";
  let v = seq.join(",");
  const p = neteasePlugin.value;
  if (p) {
    const base = String(configDraft(p).baseUrl ?? "").trim();
    if (base) v += `,meting:${base}`;
  }
  return v;
}

/** 把当前本地序列串行写进插件配置。队列执行时才取最新 config 与序列，
 *  连续快速操作也总是落库最终状态。 */
function pushBackendPatch() {
  const p = neteasePlugin.value;
  if (!p) return;
  backendPending++;
  qualityBusy.value = true;
  const run = backendPatchChain
    .then(async () => {
      const cur = neteasePlugin.value;
      if (!cur) return;
      const value = serializeBackends();
      await store.setPluginConfig(cur.manifest.id, { ...cur.config, unblockBackends: value });
      ui.toast(
        value === "off"
          ? "已关闭全部播放源：受限歌曲将只播官方试听片段"
          : "播放源已更新，下一首解析开始生效",
        "success",
      );
    })
    .catch((e) => ui.toast(errorText(e), "error"))
    .finally(() => {
      backendPending--;
      if (backendPending === 0) qualityBusy.value = false;
    });
  backendPatchChain = run;
}

function toggleBackend(id: string, on: boolean) {
  const seq = [...(backendSeq.value ?? [])];
  const i = seq.indexOf(id);
  // 新启用的源排到链头（刚开启 = 想让它优先试）；禁用再启用同语义
  if (on && i < 0) seq.unshift(id);
  if (!on && i >= 0) seq.splice(i, 1);
  backendSeq.value = seq;
  pushBackendPatch();
}

/** 在启用序列内交换相邻位置（禁用行不参与排序）。 */
function moveBackend(id: string, dir: -1 | 1) {
  const seq = [...(backendSeq.value ?? [])];
  const i = seq.indexOf(id);
  const j = i + dir;
  if (i < 0 || j < 0 || j >= seq.length) return;
  [seq[i], seq[j]] = [seq[j], seq[i]];
  backendSeq.value = seq;
  pushBackendPatch();
}

// 装了新插件 / 重新扫描后，draft 要跟上，否则输入框绑 undefined
watch(
  () => store.pluginList,
  (list) => {
    for (const p of list) {
      if (!pluginDraft[p.manifest.id]) pluginDraft[p.manifest.id] = configDraft(p);
    }
  },
);

const s = computed(() => store.value);
const cacheMb = computed(() => (store.cacheBytes / 1024 / 1024).toFixed(1));

const accents = [
  "#7c5cff",
  "#3dd6d0",
  "#ff6b6b",
  "#ffa94d",
  "#4dabf7",
  "#e599f7",
  "#69db7c",
];

// ---------------------------------------------------------------------------
// 桌面歌词（窗口行为 + 排版 + 颜色，写入 dl* 设置字段，
// 歌词窗经 settings_changed 事件实时跟随）
// ---------------------------------------------------------------------------

/** 颜色调整对象：当前色板/取色器改的是歌词还是翻译 */
const dlTarget = ref<"lyric" | "trans">("lyric");
const dlHex = ref("#ffffff");

/** 预设色板（对照参考实现的白/青/黄/橙/绿/蓝/紫/粉/灰等常用色） */
const DL_SWATCHES = [
  "#ffffff",
  "#a5f3fc",
  "#fde68a",
  "#fdba74",
  "#86efac",
  "#5eead4",
  "#93c5fd",
  "#c4b5fd",
  "#f9a8d4",
  "#fef08a",
  "#9ca3af",
  "#fecdd3",
] as const;

function dlCurrentColor(): string {
  return dlTarget.value === "lyric" ? s.value.dlColorLyric : s.value.dlColorTrans;
}

watch(dlTarget, () => {
  dlHex.value = dlCurrentColor();
});

/** 接受 #RGB / #RRGGBB / #RRGGBBAA，统一成大写 #RRGGBB[AA]；非法返回 null */
function normalizeHex(input: string): string | null {
  let h = input.trim().replace(/^#/, "");
  if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join("");
  if (h.length !== 6 && h.length !== 8) return null;
  if (!/^[0-9a-fA-F]+$/.test(h)) return null;
  return `#${h.toUpperCase()}`;
}

async function applyDlColor() {
  const v = normalizeHex(dlHex.value);
  if (!v) {
    ui.toast("颜色格式应为 #RRGGBB（可带 AA 透明度），如 #fff48fb1", "error");
    return;
  }
  dlHex.value = v;
  if (dlTarget.value === "lyric") await set("dlColorLyric", v);
  else await set("dlColorTrans", v);
  ui.toast(`桌面歌词${dlTarget.value === "lyric" ? "歌词" : "翻译"}颜色已应用`, "success");
}

async function pickDlSwatch(c: string) {
  dlHex.value = c;
  if (dlTarget.value === "lyric") await set("dlColorLyric", c);
  else await set("dlColorTrans", c);
}

function onDlColorInput(e: Event) {
  dlHex.value = (e.target as HTMLInputElement).value.toUpperCase();
}

/** 桌面歌词总开关：显示/隐藏悬浮歌词窗。
 *  状态取命令返回值（与 PlayerBar 开关、歌词窗自行关闭广播共用 ui.lyricWindowOn）。 */
async function toggleDlWindow(on: boolean) {
  try {
    ui.lyricWindowOn = on
      ? await playerApi.showLyricWindow()
      : await playerApi.hideLyricWindow();
  } catch {
    ui.lyricWindowOn = false;
    ui.toast("桌面歌词仅 Windows 支持", "error");
  }
}

onMounted(async () => {
  await store.init();
  for (const p of store.pluginList) {
    pluginDraft[p.manifest.id] = configDraft(p);
  }
  dlHex.value = dlCurrentColor();
  refreshDiag();
});


async function set<K extends keyof typeof s.value>(key: K, value: (typeof s.value)[K]) {
  await store.save({ [key]: value } as never);
}

async function savePlugin(id: string) {
  try {
    await store.setPluginConfig(id, pluginDraft[id] ?? {});
    ui.toast("插件配置已保存", "success");
  } catch (e) {
    ui.toast(errorText(e), "error");
  }
}

async function togglePlugin(id: string, enabled: boolean) {
  await store.setPluginEnabled(id, enabled);
  ui.toast(enabled ? "已启用" : "已停用", "success");
}

async function clearCache() {
  busy.value = true;
  try {
    await store.clearCache();
    ui.toast("缓存已清空", "success");
  } finally {
    busy.value = false;
  }
}

async function requestBattery() {
  try {
    await settingsApi.requestBackground(true);
    ui.toast("已请求后台运行权限", "success");
  } catch (e) {
    ui.toast(errorText(e), "info");
  }
}

function installPlugin() {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".json";
  input.onchange = async () => {
    const file = input.files?.[0];
    if (!file) return;
    const manifestJson = await file.text();
    let id = "";
    try {
      id = JSON.parse(manifestJson).id;
    } catch {
      ui.toast("manifest.json 格式错误", "error");
      return;
    }
    const jsInput = document.createElement("input");
    jsInput.type = "file";
    jsInput.accept = ".js";
    jsInput.onchange = async () => {
      const js = jsInput.files?.[0];
      if (!js) return;
      try {
        await pluginApi.install(manifestJson, await js.text());
        await store.reloadPlugins();
        ui.toast(`已安装插件 ${id}`, "success");
      } catch (e) {
        ui.toast(errorText(e), "error");
      }
    };
    jsInput.click();
  };
  input.click();
}

// ---------------------------------------------------------------------------
// 更新检测：GitHub Releases。仓库尚未建/未发版时给出友好提示，
// 发布首个 Release 后这里自动开始工作。
// ---------------------------------------------------------------------------

const RELEASE_API = "https://api.github.com/repos/ChenL-eon/LeonMusic/releases/latest";
const RELEASE_PAGE = "https://github.com/ChenL-eon/LeonMusic/releases";

const appVersion = ref("");
type UpdState = "idle" | "checking" | "latest" | "available" | "error";
const updState = ref<UpdState>("idle");
const updError = ref("");
const latestRelease = ref<{ tag: string; notes: string; url: string } | null>(null);

onMounted(async () => {
  try {
    appVersion.value = await appApi.getVersion();
  } catch {
    /* 非 Tauri 环境（纯浏览器开发）保持空 */
  }
});

/** 三段数字版本比较（v 前缀容忍）；返回 <0 / 0 / >0。非法段按 0。 */
function cmpVersion(a: string, b: string): number {
  const pa = a.replace(/^v/i, "").split(".").map((n) => parseInt(n, 10) || 0);
  const pb = b.replace(/^v/i, "").split(".").map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    const d = (pa[i] || 0) - (pb[i] || 0);
    if (d) return d;
  }
  return 0;
}

/** release notes 的轻量净化：剥 markdown 标记、截断到一屏内。 */
function cleanNotes(body: string): string {
  return body
    .replace(/^#+\s/gm, "")
    .replace(/\*\*/g, "")
    .replace(/\[(.*?)\]\(.*?\)/g, "$1")
    .trim()
    .slice(0, 800);
}

async function checkUpdate() {
  if (updState.value === "checking") return;
  updState.value = "checking";
  updError.value = "";
  try {
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 15000);
    const resp = await fetch(RELEASE_API, {
      signal: ctrl.signal,
      headers: { Accept: "application/vnd.github+json" },
    });
    clearTimeout(timer);
    if (resp.status === 404) {
      updState.value = "error";
      updError.value = "仓库还没有发布过版本，可稍后再试";
      return;
    }
    if (resp.status === 403) {
      updState.value = "error";
      updError.value = "GitHub 接口限流，请稍后再试";
      return;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();
    const tag = String(data.tag_name ?? "").trim();
    if (!tag) throw new Error("响应里没有版本号");
    if (cmpVersion(tag, appVersion.value || "0.0.0") > 0) {
      latestRelease.value = {
        tag,
        notes: typeof data.body === "string" ? cleanNotes(data.body) : "",
        url: String(data.html_url || RELEASE_PAGE),
      };
      updState.value = "available";
    } else {
      updState.value = "latest";
    }
  } catch (e) {
    updState.value = "error";
    updError.value =
      e instanceof Error && e.name === "AbortError"
        ? "网络超时，请检查网络或代理后重试"
        : `检测失败：${String(e).slice(0, 80)}`;
  }
}

function goDownload() {
  if (!latestRelease.value) return;
  void appApi
    .openExternal(latestRelease.value.url)
    .catch((e) => ui.toast(errorText(e), "error"));
}
</script>

<template>
  <div class="page">
    <header class="head">
      <h1 class="page-title">设置</h1>
      <p class="page-sub">所有播放参数都会写回 Rust 引擎并持久化</p>
    </header>

    <div class="layout">
      <nav class="tabs">
        <button :class="{ on: tab === 'look' }" @click="tab = 'look'">外观</button>
        <!-- 桌面歌词窗只有桌面端有，安卓端整个 tab 隐藏 -->
        <button v-if="!store.isAndroid" :class="{ on: tab === 'lyric' }" @click="tab = 'lyric'">桌面歌词</button>
        <button :class="{ on: tab === 'play' }" @click="tab = 'play'">播放</button>
        <button :class="{ on: tab === 'source' }" @click="tab = 'source'">音源插件</button>
        <button :class="{ on: tab === 'about' }" @click="tab = 'about'">更新与关于</button>
      </nav>

      <div class="content scroll">
        <!-- 外观 -->
        <section v-if="tab === 'look'" class="group">
          <div class="row">
            <div class="label">
              <div>主题</div>
              <div class="desc">跟随系统或强制明暗</div>
            </div>
            <div class="seg">
              <button
                v-for="m in ['system', 'dark', 'light'] as const"
                :key="m"
                :class="{ on: s.theme === m }"
                @click="set('theme', m)"
              >
                {{ m === "system" ? "跟随系统" : m === "dark" ? "深色" : "浅色" }}
              </button>
            </div>
          </div>

          <div class="row">
            <div class="label">
              <div>主色</div>
              <div class="desc">影响按钮、歌词高亮、进度条</div>
            </div>
            <div class="swatches">
              <button
                v-for="c in accents"
                :key="c"
                class="swatch"
                :class="{ on: s.accent === c }"
                :style="{ background: c }"
                @click="set('accent', c)"
              />
            </div>
          </div>

          <div class="row">
            <div class="label">
              <div>歌词字号</div>
              <div class="desc">{{ s.lyricFontSize }} px</div>
            </div>
            <input
              type="range"
              min="20"
              max="56"
              step="1"
              :value="s.lyricFontSize"
              @change="set('lyricFontSize', Number(($event.target as HTMLInputElement).value))"
            />
          </div>

          <div class="row">
            <div class="label"><div>歌词对齐</div></div>
            <div class="seg">
              <button :class="{ on: s.lyricAlign === 'center' }" @click="set('lyricAlign', 'center')">
                居中
              </button>
              <button :class="{ on: s.lyricAlign === 'left' }" @click="set('lyricAlign', 'left')">
                左对齐
              </button>
            </div>
          </div>

          <div class="row">
            <div class="label">
              <div>显示翻译歌词</div>
              <div class="desc">存在 `.zh.lrc` 时自动合并</div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="s.lyricShowTranslation"
                @change="set('lyricShowTranslation', ($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
          </div>
        </section>

        <!-- 桌面歌词（独立板块：总开关 + 窗口 + 排版 + 颜色，
             设置写入 dl* 字段，歌词窗经 settings_changed 事件实时跟随） -->
        <section v-else-if="tab === 'lyric' && !store.isAndroid" class="group">
          <div class="row" v-if="store.supportsLyricWindow">
            <div class="label">
              <div>显示桌面歌词</div>
              <div class="desc">悬浮在桌面上的卡拉OK歌词窗，开关状态与窗口实时同步</div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="ui.lyricWindowOn"
                @change="toggleDlWindow(($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
          </div>
          <div class="row" v-else>
            <div class="label">
              <div>桌面歌词</div>
              <div class="desc">当前平台不支持悬浮歌词窗口</div>
            </div>
          </div>

          <!-- 窗口行为 -->
          <div class="sub-title">窗口</div>
          <div class="row">
            <div class="label">
              <div>桌面歌词锁定</div>
              <div class="desc">
                {{
                  s.dlLocked
                    ? "锁定中：歌词窗口不响应鼠标，点底部播放条的锁形按钮或右侧按钮解除。"
                    : "锁定后歌词窗口鼠标穿透，不挡下方窗口的点击；解锁点播放条的锁形按钮。"
                }}
              </div>
            </div>
            <div class="inline">
              <label class="switch">
                <input
                  type="checkbox"
                  :checked="s.dlLocked"
                  @change="set('dlLocked', ($event.target as HTMLInputElement).checked)"
                />
                <span />
              </label>
              <button v-if="s.dlLocked" class="btn ghost" @click="set('dlLocked', false)">
                解除锁定
              </button>
            </div>
          </div>

          <!-- 排版 -->
          <div class="sub-title">排版</div>
          <div class="row">
            <div class="label">
              <div>排版方向</div>
              <div class="desc">横向逐行滚动，竖向逐列排列</div>
            </div>
            <select
              class="quality-select"
              :value="s.dlDirection"
              @change="
                set(
                  'dlDirection',
                  ($event.target as HTMLSelectElement).value as 'horizontal' | 'vertical',
                )
              "
            >
              <option value="horizontal">横向</option>
              <option value="vertical">竖向</option>
            </select>
          </div>

          <div class="row">
            <div class="label">
              <div>对齐方式</div>
            </div>
            <select
              class="quality-select"
              :value="s.dlAlign"
              @change="
                set(
                  'dlAlign',
                  ($event.target as HTMLSelectElement).value as 'left' | 'center' | 'right',
                )
              "
            >
              <option value="center">居中</option>
              <option value="left">左对齐</option>
              <option value="right">右对齐</option>
            </select>
          </div>

          <div class="row">
            <div class="label">
              <div>双行歌词</div>
              <div class="desc">歌词上下交替显示，开启后隐藏翻译</div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="s.dlDualLine"
                @change="set('dlDualLine', ($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
          </div>

          <!-- 颜色 -->
          <div class="sub-title">颜色</div>
          <div class="row">
            <div class="label">
              <div>调整对象</div>
              <div class="desc">选择要修改颜色的歌词部分</div>
            </div>
            <select class="quality-select" v-model="dlTarget">
              <option value="lyric">歌词</option>
              <option value="trans">翻译</option>
            </select>
          </div>

          <div class="row">
            <div class="label">
              <div>颜色模式</div>
              <div class="desc">主题渐变 = 卡拉OK高亮跟随主色</div>
            </div>
            <select
              class="quality-select"
              :value="s.dlColorMode"
              @change="
                set('dlColorMode', ($event.target as HTMLSelectElement).value as 'theme' | 'custom')
              "
            >
              <option value="theme">主题渐变</option>
              <option value="custom">自定义</option>
            </select>
          </div>

          <div class="row" v-if="s.dlColorMode === 'custom'">
            <div class="label">
              <div>颜色</div>
              <div class="desc">#RRGGBB，可带 AA 透明度</div>
            </div>
            <div class="dl-color-row">
              <input
                type="color"
                class="dl-color-picker"
                :value="dlHex.length >= 7 ? dlHex.slice(0, 7) : '#ffffff'"
                @input="onDlColorInput"
              />
              <input
                type="text"
                class="num dl-hex"
                :value="dlHex"
                spellcheck="false"
                @input="dlHex = ($event.target as HTMLInputElement).value"
                @keydown.enter="applyDlColor"
              />
              <button class="btn ghost" @click="applyDlColor">应用</button>
            </div>
          </div>

          <div class="row" v-if="s.dlColorMode === 'custom'">
            <div class="label">
              <div>预设色板</div>
              <div class="desc">点按即用</div>
            </div>
            <div class="swatches">
              <button
                v-for="c in DL_SWATCHES"
                :key="c"
                class="swatch"
                :class="{ on: dlCurrentColor().toUpperCase() === c }"
                :style="{ background: c }"
                @click="pickDlSwatch(c)"
              />
            </div>
          </div>
        </section>

        <!-- 播放 -->
        <section v-else-if="tab === 'play'" class="group">
          <div class="row">
            <div class="label">
              <div>音量</div>
              <div class="desc">{{ Math.round(s.volume * 100) }}%</div>
            </div>
            <input
              type="range"
              min="0"
              max="1"
              step="0.01"
              :value="s.volume"
              @change="set('volume', Number(($event.target as HTMLInputElement).value))"
            />
          </div>

          <div class="row">
            <div class="label">
              <div>输出采样率</div>
              <div class="desc">引擎固定 48kHz，源文件自动重采样</div>
            </div>
            <span class="mono muted">48 000 Hz</span>
          </div>

          <!-- 播放页背景 / 背景模糊只作用于桌面播放页（移动端播放页是固定的封面模糊），
               安卓端隐藏，避免出现调了没反应的设置项 -->
          <div v-if="!store.isAndroid" class="row">
            <div class="label">
              <div>播放页背景</div>
              <div class="desc">打开播放页时显示的背景来源</div>
            </div>
            <select
              class="quality-select"
              :value="s.nowBgSource"
              @change="
                set('nowBgSource', ($event.target as HTMLSelectElement).value as 'cover' | 'off')
              "
            >
              <option value="cover">歌曲封面</option>
              <option value="off">关闭</option>
            </select>
          </div>

          <div v-if="!store.isAndroid" class="row">
            <div class="label">
              <div>背景模糊</div>
              <div class="desc">数值越大越柔和，0 为不模糊</div>
            </div>
            <div class="inline">
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                :value="s.nowBgBlur"
                @input="set('nowBgBlur', Number(($event.target as HTMLInputElement).value))"
              />
              <span class="mono muted">{{ s.nowBgBlur }}px</span>
            </div>
          </div>

          <div class="quality-card card">
            <div class="quality-head">
              <div>
                <div class="quality-title">在线音质</div>
                <div class="desc">对标网易云常见档位；实际规格以播放条为准</div>
              </div>
              <span class="badge">网易云音乐</span>
            </div>

            <template v-if="neteasePlugin">
              <div class="quality-pick">
                <div class="label">
                  <div>音质档位</div>
                  <div class="desc">切换后从下一首开始生效</div>
                </div>
                <select
                  class="quality-select quality-level"
                  :value="currentLevel"
                  :disabled="qualityBusy"
                  @change="setQualityByLevel"
                >
                  <option
                    v-for="q in QUALITY_LEVELS"
                    :key="q.level"
                    :value="q.level"
                    :disabled="!q.available"
                  >{{ qualityLabel(q) }}</option>
                </select>
              </div>

              <!-- 解析播放源：开关 + 排序（写插件配置 unblockBackends，逐个尝试） -->
              <div class="backend-block">
                <div class="label">
                  <div>解析播放源</div>
                  <div class="desc">
                    受限歌曲按此顺序逐个尝试，可开关与排序；全部关闭后只播官方试听
                  </div>
                </div>
                <div
                  v-for="(b, i) in backendRows"
                  :key="b.id"
                  class="backend-row"
                  :class="{ off: !b.on }"
                >
                  <div class="backend-info">
                    <div class="backend-name">
                      {{ b.name }}
                      <span class="backend-order" v-if="b.on">{{ i + 1 }}</span>
                    </div>
                    <div class="desc">{{ b.desc }}</div>
                  </div>
                  <div class="backend-ops">
                    <button
                      class="mini-btn"
                      :disabled="!b.on || i === 0"
                      title="上移"
                      @click="moveBackend(b.id, -1)"
                    >↑</button>
                    <button
                      class="mini-btn"
                      :disabled="!b.on || i >= backendOnCount - 1"
                      title="下移"
                      @click="moveBackend(b.id, 1)"
                    >↓</button>
                    <label class="switch">
                      <input
                        type="checkbox"
                        :checked="b.on"
                        @change="toggleBackend(b.id, ($event.target as HTMLInputElement).checked)"
                      />
                      <span />
                    </label>
                  </div>
                </div>
                <div class="svc-fields">
                  <div class="quality-foot-row">
                    <div class="label">
                      <div>自建网易云 API（api-enhanced）</div>
                      <div class="desc">
                        {{ ncmConfigured
                          ? "已配置：作为链头第一跳，取官方直链（试听片段自动跳过）"
                          : "VutronMusic 同款架构；填地址后自动作为链头第一跳，未登录网易也能用服务端 cookie 取全曲" }}
                      </div>
                    </div>
                    <input
                      class="quality-input"
                      type="text"
                      :value="String(configDraft(neteasePlugin!).ncmBase ?? '')"
                      placeholder="http://127.0.0.1:3010"
                      spellcheck="false"
                      @change="saveSvcField('ncmBase', $event)"
                    />
                  </div>
                  <div class="quality-foot-row">
                    <div class="label">
                      <div>自建酷狗 API（KuGouMusicApi）</div>
                      <div class="desc">
                        {{ kgapiConfigured
                          ? "已配置：链头第二跳；设备注册自动完成，免费曲免登录"
                          : "填地址后自动作为链头第二跳；免费曲免登录可取，配酷狗 Cookie 可取账号权益内 VIP 曲" }}
                      </div>
                    </div>
                    <input
                      class="quality-input"
                      type="text"
                      :value="String(configDraft(neteasePlugin!).kgapiBase ?? '')"
                      placeholder="http://127.0.0.1:3020"
                      spellcheck="false"
                      @change="saveSvcField('kgapiBase', $event)"
                    />
                  </div>
                  <div class="quality-foot-row">
                    <div class="label">
                      <div>酷狗 Cookie（可选）</div>
                      <div class="desc">
                        在自建服务里登录酷狗（验证码 / 扫码接口都有），把返回的
                        cookie 粘到这里；留空 = 只取免费曲
                      </div>
                    </div>
                    <input
                      class="quality-input"
                      type="password"
                      :value="String(configDraft(neteasePlugin!).kgapiCookie ?? '')"
                      placeholder="token=...; userid=..."
                      @change="saveSvcField('kgapiCookie', $event)"
                    />
                  </div>
                </div>
                <p class="quality-note backend-note">
                  {{
                    metingConfigured
                      ? "已配置 Meting 地址：始终作为链尾兜底追加。"
                      : "「Meting 地址」填好后会自动作为链尾兜底追加，无需手动排序。"
                  }}
                </p>
              </div>

              <div class="quality-foot">
                <p class="quality-note">
                  <strong>不确保有效</strong>：官方接口按账号权限与版权静默降级，选了「无损」也可能只回
                  320k MP3；此时播放条显示真实规格并提示一次。官方只给 30 秒试听时才走同平台解析，
                  按所选档位请求码率（「无损」→ 999，实测对 VIP 曲可拿到 FLAC 16bit / 44.1kHz 全曲，
                  不保证每首都有）。
                </p>
                <p class="quality-note">
                  Hi-Res / 高清臻音暂不可选：解析源最高 44.1kHz / 16bit，播放器输出统一重采样到 48 kHz，
                  高采样率与 24bit 位深无法原样呈现。沉浸环绕声 / 杜比全景声需专门解码设备，不提供。
                </p>

                <details class="quality-advanced">
                  <summary>
                    高级：手动指定解析码率（当前
                    {{ currentUnblockBr === "999" ? "999 · 无损" : currentUnblockBr + " kbps" }}）
                  </summary>
                  <div class="quality-foot-row">
                    <div class="label">
                      <div>受限歌曲解析音质</div>
                      <div class="desc">
                        仅当官方只给 30 秒试听时生效；换音质档位会重新自动同步
                      </div>
                    </div>
                    <select
                      class="quality-select"
                      :value="currentUnblockBr"
                      :disabled="qualityBusy"
                      @change="setUnblockBr"
                    >
                      <option v-for="b in UNBLOCK_BR" :key="b.value" :value="b.value">
                        {{ b.label }}
                      </option>
                    </select>
                  </div>
                </details>
              </div>
            </template>
            <div v-else class="quality-empty muted">
              未找到网易云音源插件：请在「音源插件」页启用后再设置音质。
            </div>
          </div>

          <div class="audio-diag">
            <div class="diag-head">
              <span class="diag-title">音频输出自检</span>
              <div class="inline">
                <button class="btn ghost" :disabled="diagBusy" @click="refreshDiag">刷新</button>
                <button class="btn ghost" :disabled="diagBusy" @click="resetAudio">
                  重新连接设备
                </button>
              </div>
            </div>
            <div v-if="diag" class="diag-body">
              <div class="diag-grid">
                <span>输出设备</span>
                <b class="mono">{{ diag.device || "未打开" }}</b>
                <span>格式</span>
                <b class="mono">{{ diag.sampleRate || "—" }} Hz / {{ diag.channels || "—" }} 声道</b>
                <span>应用音量</span>
                <b class="mono" :class="{ warn: diag.volume === 0 }">
                  {{ Math.round(diag.volume * 100) }}%{{ diag.volume === 0 ? "（已静音）" : "" }}
                </b>
                <span>已输出帧</span>
                <b class="mono">{{ diag.playedFrames }}</b>
                <span>缓冲 / 欠载</span>
                <b class="mono">{{ diag.bufferedSamples }} / {{ diag.underruns }}</b>
              </div>
              <p class="diag-hint" :class="{ warn: diag.volume === 0 || diag.deviceError }">
                {{ diag.hint }}
              </p>
            </div>
            <div v-else class="muted">点「刷新」读取当前输出状态。</div>
          </div>

          <div class="row">
            <div class="label">
              <div>缓存上限</div>
              <div class="desc">当前占用 {{ cacheMb }} MB</div>
            </div>
            <div class="inline">
              <input
                type="number"
                min="256"
                step="256"
                :value="s.cacheLimitMb"
                class="num"
                @change="set('cacheLimitMb', Number(($event.target as HTMLInputElement).value))"
              />
              <span class="muted">MB</span>
              <button class="btn ghost" :disabled="busy" @click="clearCache">清空缓存</button>
            </div>
          </div>

          <!-- Windows 专属：安卓没有窗口 / 托盘概念 -->
          <div v-if="!store.isAndroid" class="row">
            <div class="label">
              <div>关闭窗口时最小化到托盘</div>
              <div class="desc">Windows 专属行为</div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="s.closeToTray"
                @change="set('closeToTray', ($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
          </div>

          <div v-if="store.isAndroid" class="row">
            <div class="label">
              <div>后台运行白名单</div>
              <div class="desc">关闭电池优化，避免锁屏后被杀</div>
            </div>
            <button class="btn" @click="requestBattery">去设置</button>
          </div>
        </section>

        <!-- 插件 -->
        <section v-else-if="tab === 'source'" class="group">
          <div class="account-link card">
            <Icon name="user" :size="18" />
            <div class="link-text">
              <div class="name">账号与登录</div>
              <div class="desc">扫码 / Cookie 登录已移到独立页面，这里只保留插件参数配置</div>
            </div>
            <button class="btn primary small" @click="ui.go('account')">前往账号页</button>
          </div>
          <div class="plugins-head">
            <div class="desc">
              插件在 JS 沙箱里运行，只能通过宿主提供的 HTTP 访问网络；账号在「账号」页、
              音质在「播放」页设置
            </div>
            <div class="inline">
              <button class="btn ghost" @click="store.reloadPlugins()">
                <Icon name="refresh" :size="15" /> 重新扫描
              </button>
              <button class="btn" @click="installPlugin">
                <Icon name="plus" :size="15" /> 导入插件
              </button>
            </div>
          </div>

          <div v-for="p in store.pluginList" :key="p.manifest.id" class="plugin card">
            <div class="plugin-head">
              <div class="plugin-title">
                <Icon name="plug" :size="17" />
                <strong>{{ p.manifest.name }}</strong>
                <span class="ver">v{{ p.manifest.version }}</span>
                <span v-if="p.builtin" class="badge">内置</span>
              </div>
              <label class="switch">
                <input
                  type="checkbox"
                  :checked="p.enabled"
                  @change="togglePlugin(p.manifest.id, ($event.target as HTMLInputElement).checked)"
                />
                <span />
              </label>
            </div>
            <p class="plugin-desc">{{ p.manifest.description }}</p>

            <div v-if="configFields(p).length" class="fields">
              <div v-for="f in configFields(p)" :key="f.key" class="field">
                <label>{{ f.label }}<span v-if="f.required" class="req">*</span></label>
                <div v-if="f.fieldType === 'boolean'" class="bool-row">
                  <label class="switch">
                    <input
                      type="checkbox"
                      :checked="pluginDraft[p.manifest.id][f.key] !== 'off'"
                      @change="
                        pluginDraft[p.manifest.id][f.key] = ($event.target as HTMLInputElement).checked
                          ? 'on'
                          : 'off'
                      "
                    />
                    <span />
                  </label>
                  <span class="bool-hint">{{ f.placeholder }}</span>
                </div>
                <input
                  v-else
                  v-model="pluginDraft[p.manifest.id][f.key]"
                  :placeholder="f.placeholder ?? ''"
                  :type="f.fieldType === 'password' ? 'password' : 'text'"
                />
              </div>
              <button class="btn primary" @click="savePlugin(p.manifest.id)">保存配置</button>
            </div>
          </div>

          <div v-if="!store.pluginList.length" class="empty">
            <div class="big">🔌</div>
            <div>还没有插件</div>
          </div>
        </section>

        <!-- 更新与关于 -->
        <section v-else class="group">
          <div class="about">
            <div class="logo-big">
              <BrandLogo :size="58" />
            </div>
            <div>
              <div class="about-title">LeonMusic <span class="ver">{{ appVersion || "0.1.0" }}</span></div>
              <div class="desc">Tauri v2 · Rust 音频内核 · Vue 3</div>
            </div>
          </div>

          <!-- 检测更新 -->
          <div class="upd-card">
            <div class="upd-row">
              <div class="label">
                <div>版本更新</div>
                <div class="desc">对照 GitHub Releases 检查是否有新版本</div>
              </div>
              <button class="btn small" :disabled="updState === 'checking'" @click="checkUpdate">
                <Icon
                  name="refresh"
                  :size="14"
                  :class="{ 'upd-spin': updState === 'checking' }"
                />
                {{ updState === "checking" ? "检测中…" : "检测更新" }}
              </button>
            </div>

            <div v-if="updState === 'latest'" class="upd-result ok">
              ✓ 已是最新版本（v{{ appVersion }}）
            </div>
            <div v-else-if="updState === 'available'" class="upd-result new">
              <div class="upd-new-head">
                <span>
                  发现新版本 <b>{{ latestRelease?.tag }}</b>
                </span>
                <button class="btn small" @click="goDownload">前往下载</button>
              </div>
              <pre v-if="latestRelease?.notes" class="upd-notes">{{ latestRelease.notes }}</pre>
            </div>
            <div v-else-if="updState === 'error'" class="upd-result err">
              <span>{{ updError }}</span>
              <button class="link" @click="checkUpdate">重试</button>
            </div>
          </div>

          <div class="kv">
            <div><span>平台</span><span>{{ store.platform?.kind }}</span></div>
            <div><span>系统</span><span>{{ store.platform?.osVersion }}</span></div>
            <div><span>架构</span><span>{{ store.platform?.arch }}</span></div>
            <div><span>媒体会话</span><span>{{ store.platform?.supportsMediaSession ? "支持" : "不支持" }}</span></div>
            <div><span>桌面歌词</span><span>{{ store.platform?.supportsLyricWindow ? "支持" : "不支持" }}</span></div>
            <div><span>缓存目录</span><span class="mono small">{{ store.platform?.cacheDir }}</span></div>
          </div>

          <p class="legal">
            LeonMusic 是一个技术演示项目：音频解码、播放状态与插件沙箱全部在 Rust 侧。
            在线音源由第三方插件提供，本项目不内置任何内容源，请自行确保使用方式符合当地法律与服务条款。
          </p>
        </section>
      </div>
    </div>

  </div>
</template>

<style scoped>
/* ---------- 在线音质 ---------- */
.quality-card {
  padding: 16px 18px;
  margin: 10px 0 12px;
}
.quality-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 14px;
}
.quality-title {
  font-size: 14px;
  font-weight: 600;
}
.quality-pick {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
}
/* 解析播放源：可开关、可排序的源列表 */
.backend-block {
  margin-top: 14px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.backend-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 12px;
  border-radius: 10px;
  background: color-mix(in srgb, var(--hover) 55%, transparent);
  transition: opacity 0.15s var(--ease);
}
.backend-row.off {
  opacity: 0.55;
}
.backend-name {
  font-size: 13px;
  font-weight: 600;
}
.backend-order {
  display: inline-grid;
  place-items: center;
  min-width: 17px;
  height: 17px;
  margin-left: 6px;
  padding: 0 4px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--accent) 16%, transparent);
  color: var(--accent);
  font-size: 10.5px;
  font-weight: 700;
  vertical-align: 1px;
}
.backend-ops {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
}
.mini-btn {
  width: 26px;
  height: 26px;
  border-radius: 8px;
  font-size: 14px;
  line-height: 1;
  color: var(--text-2);
  display: grid;
  place-items: center;
}
.mini-btn:hover:not(:disabled) {
  background: var(--hover);
  color: var(--text);
}
.mini-btn:disabled {
  opacity: 0.3;
  cursor: default;
}
.backend-note {
  margin-top: 0;
}
.quality-level {
  min-width: 260px;
  font-weight: 500;
}
.quality-level:disabled {
  opacity: 0.6;
}
.quality-foot {
  margin-top: 14px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
}
.quality-foot-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
}
.quality-select {
  height: 34px;
  min-width: 170px;
  padding: 0 10px;
  border: 1px solid var(--border);
  border-radius: 9px;
  background: var(--panel);
  color: var(--text);
  font-size: 12.5px;
  outline: none;
  flex: none;
}
.quality-input {
  height: 34px;
  width: 230px;
  padding: 0 10px;
  border: 1px solid var(--border);
  border-radius: 9px;
  background: var(--panel);
  color: var(--text);
  font-size: 12.5px;
  outline: none;
  flex: none;
}
.quality-input:focus {
  border-color: var(--accent);
}
.svc-fields {
  margin-top: 10px;
}
.svc-fields .quality-foot-row + .quality-foot-row {
  margin-top: 8px;
}
@media (max-width: 860px) {
  .quality-input {
    width: 100%;
    max-width: 300px;
  }
  .svc-fields .quality-foot-row {
    flex-direction: column;
    align-items: stretch;
    gap: 6px;
  }
}
.quality-note {
  margin: 10px 0 0;
  font-size: 11px;
  color: var(--text-3);
  line-height: 1.6;
}
.quality-note strong {
  color: var(--accent);
}
.quality-advanced {
  margin-top: 10px;
  font-size: 11.5px;
}
.quality-advanced summary {
  cursor: pointer;
  color: var(--text-3);
  user-select: none;
}
.quality-advanced[open] summary {
  color: var(--accent);
}
.quality-advanced .quality-foot-row {
  margin-top: 10px;
}
.quality-empty {
  padding: 10px 0;
  font-size: 12px;
}

.account-link {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px 16px;
  margin-bottom: 14px;
}
.account-link .link-text {
  flex: 1;
  min-width: 0;
}
.account-link .name {
  font-size: 13.5px;
  font-weight: 600;
}
.account-link .desc {
  font-size: 11.5px;
  color: var(--text-3);
  margin-top: 3px;
}
.page {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.layout {
  display: grid;
  grid-template-columns: 150px minmax(0, 1fr);
  gap: 18px;
  min-height: 0;
  flex: 1;
}
.tabs {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.tabs button {
  text-align: left;
  padding: 9px 14px;
  border-radius: 10px;
  color: var(--text-2);
  font-size: 13px;
  transition: background 0.15s, color 0.15s;
}
.tabs button:hover {
  background: var(--hover);
}
.tabs button.on {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
  font-weight: 600;
}
.content {
  min-height: 0;
  padding-right: 6px;
}
.group {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding-bottom: 30px;
}
/* 桌面歌词等分组小标题：上留白 + 分隔线，视觉上把一行行设置切开 */
.sub-title {
  margin: 22px 0 6px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.04em;
  color: var(--text-2);
}
.row:first-of-type .sub-title,
.sub-title:first-child {
  margin-top: 0;
  padding-top: 0;
  border-top: none;
}
/* 自定义颜色行：取色器 + hex 输入 + 应用 */
.dl-color-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.dl-color-picker {
  width: 34px;
  height: 30px;
  padding: 0;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: transparent;
  cursor: pointer;
}
.dl-color-picker::-webkit-color-swatch-wrapper {
  padding: 2px;
}
.dl-color-picker::-webkit-color-swatch {
  border: none;
  border-radius: 5px;
}
.dl-hex {
  width: 110px;
  text-transform: uppercase;
}
.row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  padding: 14px 16px;
  border-radius: var(--radius-m);
  transition: background 0.15s;
}
.row:hover {
  background: var(--hover);
}
.label > div:first-child {
  font-size: 13.5px;
  font-weight: 500;
}
.desc {
  font-size: 11.5px;
  color: var(--text-3);
  margin-top: 3px;
  line-height: 1.5;
}
.seg {
  display: flex;
  background: var(--panel-3);
  border-radius: 999px;
  padding: 3px;
  gap: 2px;
}
.seg button {
  padding: 6px 14px;
  border-radius: 999px;
  font-size: 12px;
  color: var(--text-2);
}
.seg button.on {
  background: var(--panel);
  color: var(--text);
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.12);
}
.swatches {
  display: flex;
  gap: 8px;
}
.swatch {
  width: 22px;
  height: 22px;
  border-radius: 50%;
  border: 2px solid transparent;
  transition: transform 0.15s, border-color 0.15s;
}
.swatch:hover {
  transform: scale(1.12);
}
.swatch.on {
  border-color: var(--text);
  transform: scale(1.12);
}
input[type="range"] {
  -webkit-appearance: none;
  appearance: none;
  width: 180px;
  height: 4px;
  border-radius: 999px;
  background: var(--border-strong);
  outline: none;
}
input[type="range"]::-webkit-slider-thumb {
  -webkit-appearance: none;
  width: 13px;
  height: 13px;
  border-radius: 50%;
  background: var(--accent);
  cursor: pointer;
}
.switch {
  position: relative;
  width: 42px;
  height: 24px;
  flex: none;
}
.switch input {
  opacity: 0;
  width: 0;
  height: 0;
}
.switch span {
  position: absolute;
  inset: 0;
  border-radius: 999px;
  background: var(--border-strong);
  transition: background 0.2s;
  cursor: pointer;
}
.switch span::after {
  content: "";
  position: absolute;
  left: 3px;
  top: 3px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: #fff;
  transition: transform 0.2s var(--ease);
}
.switch input:checked + span {
  background: var(--accent);
}
.switch input:checked + span::after {
  transform: translateX(18px);
}

.inline {
  display: flex;
  align-items: center;
  gap: 8px;
}
.num {
  width: 92px;
  height: 34px;
  border-radius: 9px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text);
  padding: 0 10px;
  outline: none;
  font-size: 13px;
}

.plugins-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  padding: 4px 4px 14px;
}
.plugin {
  padding: 16px 18px;
  margin-bottom: 10px;
}
.plugin-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.plugin-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 14px;
}
.ver {
  font-size: 10.5px;
  color: var(--text-3);
  font-weight: 400;
}
.badge {
  font-size: 10px;
  padding: 1px 7px;
  border-radius: 999px;
  background: var(--panel-3);
  color: var(--text-2);
  font-weight: 400;
}
.plugin-desc {
  font-size: 12px;
  color: var(--text-3);
  line-height: 1.6;
  margin: 8px 0 12px;
}
.fields {
  display: flex;
  flex-direction: column;
  gap: 10px;
  align-items: flex-start;
}
.field {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.field label {
  font-size: 12px;
  color: var(--text-2);
}
.req {
  color: #ff6b6b;
  margin-left: 3px;
}
.field input {
  height: 36px;
  border-radius: 9px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text);
  padding: 0 12px;
  font-size: 13px;
  outline: none;
}
.field input:focus {
  border-color: var(--accent);
}
.field .switch input {
  height: 0;
  padding: 0;
  border: 0;
}
.bool-row {
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 36px;
}
.bool-hint {
  font-size: 12px;
  color: var(--text-3);
  line-height: 1.5;
}

.btn.small {
  height: 32px;
  padding: 0 14px;
  font-size: 12px;
}

.about {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 10px 16px 22px;
}
.logo-big {
  width: 58px;
  height: 58px;
  filter: drop-shadow(0 10px 18px rgba(119, 69, 250, 0.4));
}
.about-title {
  font-size: 19px;
  font-weight: 700;
}
.kv {
  display: flex;
  flex-direction: column;
  border-top: 1px solid var(--border);
}
.kv > div {
  display: flex;
  justify-content: space-between;
  gap: 20px;
  padding: 11px 16px;
  font-size: 12.5px;
  border-bottom: 1px solid var(--border);
}
.kv span:first-child {
  color: var(--text-3);
}
.kv .small {
  font-size: 11px;
  max-width: 60%;
  text-align: right;
  word-break: break-all;
}
.legal {
  font-size: 11.5px;
  color: var(--text-3);
  line-height: 1.7;
  padding: 18px 16px;
}

/* ---------- 更新检测 ---------- */
.upd-card {
  margin: 0 0 6px;
  padding: 14px 16px;
  border: 1px solid var(--border);
  border-radius: var(--radius-m);
  background: color-mix(in srgb, var(--panel) 55%, transparent);
}
.upd-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.upd-result {
  margin-top: 10px;
  padding: 10px 12px;
  border-radius: 10px;
  font-size: 12.5px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.upd-result.ok {
  color: var(--accent-2);
  background: color-mix(in srgb, var(--accent-2) 10%, transparent);
}
.upd-result.new {
  background: color-mix(in srgb, var(--accent) 9%, transparent);
  border: 1px solid color-mix(in srgb, var(--accent) 26%, transparent);
}
.upd-result.err {
  color: var(--text-2);
  background: color-mix(in srgb, #ff6b6b 8%, transparent);
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
}
.upd-new-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  font-size: 13px;
}
.upd-notes {
  margin: 0;
  font-family: inherit;
  font-size: 12px;
  line-height: 1.65;
  color: var(--text-2);
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 220px;
  overflow-y: auto;
}
.upd-spin {
  animation: upd-rotate 0.9s linear infinite;
}
@keyframes upd-rotate {
  to {
    transform: rotate(360deg);
  }
}
.link {
  background: none;
  border: none;
  color: var(--accent);
  cursor: pointer;
  font-size: 12.5px;
  padding: 0 4px;
}
.link:hover {
  text-decoration: underline;
}

@media (max-width: 860px) {
  .layout {
    grid-template-columns: 1fr;
    grid-template-rows: auto minmax(0, 1fr);
    gap: 8px;
  }

  /* tab 改成横向滚动的 chip 行。左右负边距让它能贴到屏幕边缘，
     滚动时视觉上是「整条滑过去」而不是被裁在容器里 */
  .tabs {
    flex-direction: row;
    gap: 6px;
    overflow-x: auto;
    overscroll-behavior-x: contain;
    scrollbar-width: none;
    margin: 0 -14px;
    padding: 2px 14px 8px;
  }
  .tabs button {
    flex: none;
    white-space: nowrap;
    padding: 9px 15px;
    border-radius: 999px;
    background: var(--panel);
    border: 1px solid var(--border);
    font-size: 13px;
    text-align: center;
  }
  .tabs button.on {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
    color: var(--accent);
  }

  .content {
    padding-right: 0;
    /* 最后一项默认会贴着播放条，滚到底再留一点呼吸 */
    padding-bottom: 18px;
  }
  .group {
    padding: 2px 0 12px;
  }

  .quality-card {
    padding: 14px;
  }
  .quality-pick {
    flex-direction: column;
    align-items: stretch;
    gap: 10px;
  }
  .quality-foot-row {
    flex-direction: column;
    align-items: stretch;
    gap: 10px;
  }
  .quality-select {
    width: 100%;
    min-width: 0;
  }

  /* 每行标题在上、控件在下：384px 宽度横排会把手柄/分段控件压成一团 */
  .row {
    flex-direction: column;
    align-items: stretch;
    gap: 10px;
    padding: 13px 14px;
  }
  .desc {
    font-size: 11px;
  }
  .seg {
    align-self: flex-start;
    width: 100%;
  }
  .seg button {
    flex: 1 1 0;
    padding: 9px 10px;
    text-align: center;
  }
  .swatches {
    gap: 12px;
  }
  .swatch {
    width: 30px;
    height: 30px;
  }
  input[type="range"] {
    width: 100%;
    height: 5px;
  }
  input[type="range"]::-webkit-slider-thumb {
    width: 20px;
    height: 20px;
  }
  /* 开关不该拉满整行 */
  .switch {
    align-self: flex-start;
  }
}

/* 横屏：每行改回「标题在左、控件在右」，比竖排省一半高度 */
@media (max-width: 900px) and (max-height: 520px) {
  .tabs {
    padding: 0 14px 6px;
  }
  .tabs button {
    padding: 6px 12px;
    font-size: 12px;
  }
  .row {
    flex-direction: row;
    align-items: center;
    gap: 14px;
    padding: 9px 14px;
  }
  .row .label {
    flex: 1;
    min-width: 0;
  }
  .seg {
    width: auto;
    flex: 0 0 auto;
  }
  .seg button {
    padding: 7px 12px;
  }
  input[type="range"] {
    flex: 0 0 150px;
    width: 150px;
  }
  .switch {
    align-self: center;
  }
}

/* ---------- 音频输出自检 ---------- */
.audio-diag {
  margin: 4px 0 14px;
  padding: 12px 14px;
  border: 1px solid rgba(128, 128, 128, 0.22);
  border-radius: 12px;
  background: rgba(128, 128, 128, 0.06);
}
.diag-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 10px;
}
.diag-title {
  font-size: 13px;
  font-weight: 600;
}
.diag-grid {
  display: grid;
  grid-template-columns: 92px 1fr;
  gap: 5px 12px;
  font-size: 12px;
  color: var(--text-3);
}
.diag-grid b {
  color: var(--text, inherit);
  font-weight: 500;
  overflow-wrap: anywhere;
}
.diag-grid b.warn,
.diag-hint.warn {
  color: #e05555;
}
.diag-hint {
  margin: 10px 0 0;
  font-size: 12px;
  line-height: 1.55;
  color: var(--text-3);
}
</style>
