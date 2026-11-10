<script setup lang="ts">
/** 设置：外观 / 桌面歌词 / 播放 / 音源插件 / 流媒体 / 关于。 */
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";

import BrandLogo from "@/components/BrandLogo.vue";
import Icon from "@/components/Icon.vue";
import {
  EVT,
  app as appApi,
  errorText,
  on,
  player as playerApi,
  plugins as pluginApi,
  settings as settingsApi,
} from "@/api/ipc";
import type { AudioDiag, PluginInfo } from "@/types";
import type { UpdateProgress } from "@/api/ipc";
import { STREAM_PLUGIN_IDS, applyBgOpacityVars, useSettingsStore } from "@/stores/settings";
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

/** 凭据归「账号」页、音质归「播放」页，
 *  插件配置里不再暴露这些原始输入框。
 *  保存时 draft 仍是全量，所以改其它参数不会把 Cookie / 音质冲掉。 */
function configFields(p: PluginInfo) {
  const managed = ["cookie", "level", "unblockQuality"];
  return p.manifest.config.filter((f) => !managed.includes(f.key));
}

// ---------------------------------------------------------------------------
// 在线音质：写进各公共音源插件的配置，与插件 `leon.config.get()` 读的是同一份数据
// ---------------------------------------------------------------------------

/** 对标网易云 App「音质选择」的常见档位，下拉里从低到高排列。
 *  实测（咪咕 h5/v2.4 无损直链 + Range 解 FLAC STREAMINFO）：
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

/** 在线音质是公共音源共用的：网易云决定官方请求的 level，QQ 音乐决定
 *  第三方解析要的码率。只写 netease 会让后来加的音源一直停在 manifest
 *  默认值 320 —— 用户选了无损却拿不到 FLAC。按「manifest 声明了 level」
 *  来认，不写死插件 id。
 *  只改一个键，其余插件参数与已保存的 Cookie 原样保留；改完同步
 *  pluginDraft，插件页保存别的参数时不会把旧音质写回来。 */
async function patchQualityConfig(patch: Record<string, string>, okText: string) {
  if (qualityBusy.value) return;
  const ids = store.pluginList
    .filter((p) => p.manifest.config.some((f) => f.key === "level"))
    .map((p) => p.manifest.id);
  if (!ids.length) return;
  qualityBusy.value = true;
  try {
    for (const id of ids) {
      // setPluginConfig 内部会 reloadPlugins，重新取最新 config 再写入
      const p = store.pluginList.find((x) => x.manifest.id === id);
      if (!p) continue;
      await store.setPluginConfig(id, { ...p.config, ...patch });
      const draft = pluginDraft[id];
      if (draft) Object.assign(draft, patch);
    }
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
  return patchQualityConfig(
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
  void patchQualityConfig({ unblockQuality: value }, "解析音质已更新，下一首开始生效");
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

// ---------------------------------------------------------------------------
// 自定义软件背景（桌面 / 移动共用这张卡片，移动端经 MobileSettings 复用本组件）
// ---------------------------------------------------------------------------
const bgFile = ref<HTMLInputElement | null>(null);
const bgBusy = ref(false);
const BG_MAX_BYTES = 20 * 1024 * 1024;

function pickBg() {
  bgFile.value?.click();
}

async function onBgFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = ""; // 允许连续选同一个文件
  if (!file) return;
  if (file.size > BG_MAX_BYTES) {
    ui.toast("背景图片不能超过 20 MB", "error");
    return;
  }
  bgBusy.value = true;
  try {
    await store.setBackgroundFile(file);
    ui.toast(
      s.value.bgEnabled ? "背景图片已更新" : "已保存，打开「使用自定义背景」即可生效",
      "success",
    );
  } catch (err) {
    ui.toast(errorText(err), "error");
  } finally {
    bgBusy.value = false;
  }
}

async function clearBg() {
  bgBusy.value = true;
  try {
    await store.clearBackground();
    ui.toast("已清除背景图片", "success");
  } catch (err) {
    ui.toast(errorText(err), "error");
  } finally {
    bgBusy.value = false;
  }
}

/** 滑块拖动时的本地值：**圆点和右侧数字都读它**。
 *  以前圆点绑 `s.bgOpacity`、数字绑草稿，而每次 input 都会因为数字变化触发重渲染，
 *  Vue 又把 `:value` 的旧值写回 input，于是拖动时圆点被弹回原位、松手时 `change`
 *  读到的还是旧值（整次拖动被丢弃），数字与圆点也对不上。 */
const bgOpacityDraft = ref(35);
const bgOpacityDragging = ref(false);
watch(
  () => s.value.bgOpacity,
  (v) => {
    // 拖动中以手指位置为准：此时任何设置回传都不能把圆点拽走
    if (bgOpacityDragging.value) return;
    bgOpacityDraft.value = Math.round(Number(v) || 35);
  },
  { immediate: true },
);

/** 拖动时只改 CSS 变量做实时预览，松手才落盘，避免每个像素都写一次 DB。
 *  走 applyBgOpacityVars 是为了让移动端的底衬 / 玻璃 / 二级页一起联动预览，
 *  不能只改 --bg-image-opacity —— 那层固定蒙层会把滑块后半程吃干净。 */
function onBgOpacityInput(e: Event) {
  const v = Number((e.target as HTMLInputElement).value);
  bgOpacityDragging.value = true;
  bgOpacityDraft.value = v;
  applyBgOpacityVars(document.documentElement, v);
}

/** 松手落盘。`change` 与 `pointerup` 都可能触发，用「值没变就不写」去重。 */
function commitBgOpacity(e: Event) {
  const v = Math.min(100, Math.max(5, Math.round(Number((e.target as HTMLInputElement).value))));
  bgOpacityDragging.value = false;
  bgOpacityDraft.value = v;
  if (Math.round(Number(s.value.bgOpacity)) === v) return;
  void set("bgOpacity", v);
}

function onBgOpacityChange(e: Event) {
  commitBgOpacity(e);
}

/** 指针在滑块外抬起时个别 WebView 不补 `change`，这里兜一次。 */
function onBgOpacityRelease(e: Event) {
  if (bgOpacityDragging.value) commitBgOpacity(e);
}

/** 自建流媒体服务（Emby / Jellyfin / Navidrome）与其它音源插件分开两处展示：
 *  「音源插件」页只留公共音源，这三家单独一个页签，默认收起。 */
const streamPlugins = computed(() =>
  store.pluginList.filter((p) =>
    (STREAM_PLUGIN_IDS as readonly string[]).includes(p.manifest.id),
  ),
);
const sourcePlugins = computed(() =>
  store.pluginList.filter(
    (p) => !(STREAM_PLUGIN_IDS as readonly string[]).includes(p.manifest.id),
  ),
);

/** 数字输入框：失焦 / 回车时收敛到区间内再写库。
 *  空值不写 0 —— 用户清空准备重输时，`Number("")` 会把字号变成 0。 */
function setNumber(
  key: "lyricFontSize" | "nowBgBlur",
  e: Event,
  min: number,
  max: number,
): void {
  const el = e.target as HTMLInputElement;
  const raw = el.value.trim();
  if (!raw) {
    el.value = String(s.value[key]);
    return;
  }
  const n = Number(raw);
  if (!Number.isFinite(n)) {
    el.value = String(s.value[key]);
    return;
  }
  const clamped = Math.min(max, Math.max(min, Math.round(n)));
  el.value = String(clamped);
  void set(key, clamped);
}

/** 上下箭头 / 方向键：±1 并夹在区间内，不用再走一次输入校验。 */
function bump(
  key: "lyricFontSize" | "nowBgBlur",
  delta: number,
  min: number,
  max: number,
): void {
  const next = Math.min(max, Math.max(min, (Number(s.value[key]) || 0) + delta));
  if (next !== s.value[key]) void set(key, next);
}

/** 必填项齐全 = 这个自建服务真的能用（搜索页用同一套判定给音源排序）。 */
function isConfigured(p: PluginInfo): boolean {
  return p.manifest.config
    .filter((f) => f.required)
    .every((f) => (p.config[f.key] ?? "").trim().length > 0);
}

/** 流媒体总开关：开=展开面板，关=收起并真的停掉三个插件。
 *  插件在库里默认是「启用」的，只藏 UI 不改状态，搜索页照样能选到它们。
 *  打开时只启用「必填项已填全」的服务：三个都没配就全开，
 *  用户点搜索只会收到「未配置服务器地址」。 */
async function toggleStream(on: boolean) {
  await store.save({ streamEnabled: on });
  if (!on) {
    await store.syncStreamPlugins(false);
  } else {
    for (const p of streamPlugins.value) {
      const want = isConfigured(p);
      if (p.enabled !== want) await store.setPluginEnabled(p.manifest.id, want);
    }
  }
  ui.toast(on ? "流媒体已启用" : "流媒体已关闭", "success");
}

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
    // 流媒体页：填全必填项就顺手启用，省得用户再回上面找开关。
    if (
      s.value.streamEnabled &&
      (STREAM_PLUGIN_IDS as readonly string[]).includes(id)
    ) {
      const p = store.pluginList.find((x) => x.manifest.id === id);
      if (p && !p.enabled && isConfigured(p)) await store.setPluginEnabled(id, true);
    }
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
// 更新检测与升级
//
// 两条渠道：国内走 Gitee、国外走 GitHub。发行版对象的字段一致
// （tag_name / body / assets），差别只在接口形态：GitHub 有 /latest，
// Gitee 只能拿列表自己挑最新的正式版（见 pickRelease）。
//
// 应用内升级不跳浏览器：
//   Windows —— 下载便携包里的 exe，退出后由外部小进程替换并重启；
//   Android —— 下载 APK，交给系统安装器。
// 平台 / 架构不匹配时（例如以后出别的构建）只保留「下载页」入口。
// ---------------------------------------------------------------------------

interface ReleaseAsset {
  name: string;
  url: string;
}

interface ReleaseInfo {
  tag: string;
  notes: string;
  /** 发行版网页地址：应用内升级不可用时作为兜底 */
  page: string;
  /** 当前平台能用的安装包；为 null 时只能去下载页 */
  asset: ReleaseAsset | null;
}

const CHANNELS = {
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

type Channel = keyof typeof CHANNELS;
const channelList = (Object.keys(CHANNELS) as Channel[]).map((id) => ({ id, ...CHANNELS[id] }));

const channel = computed<Channel>(() => (s.value.updateChannel === "global" ? "global" : "cn"));
const channelInfo = computed(() => CHANNELS[channel.value]);
const platformKind = computed(() => store.platform?.kind ?? "other");

const appVersion = ref("");
type UpdState = "idle" | "checking" | "latest" | "available" | "downloading" | "installing" | "error";
const updState = ref<UpdState>("idle");
const updError = ref("");
/** 不致命的提示（例如安卓还没授予「安装未知应用」） */
const updHint = ref("");
const latest = ref<ReleaseInfo | null>(null);
const got = ref(0);
const total = ref(0);

const updBusy = computed(
  () => updState.value === "checking" || updState.value === "downloading" || updState.value === "installing",
);

/** 有匹配的安装包 + 当前平台支持替换式安装，才给「立即更新」 */
const canInApp = computed(() => !!latest.value?.asset && platformKind.value !== "other");

const pct = computed(() => (total.value > 0 ? Math.min(100, Math.floor((got.value / total.value) * 100)) : 0));

const sizeText = computed(() => {
  if (!got.value) return "";
  if (!total.value) return `已下载 ${fmtBytes(got.value)}`;
  return `${fmtBytes(got.value)} / ${fmtBytes(total.value)}`;
});

function fmtBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

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

/** 从发行版附件里挑当前平台要的那一个。
 *
 *  Windows 用便携包 zip（解出 exe 直接替换，不写注册表、不需要管理员）；
 *  Android 用 arm64 APK。Gitee 还会把源码 zip 一起放进 assets，
 *  所以必须按文件名后缀精确匹配，不能取第一个。 */
function pickAsset(assets: unknown, kind: string): ReleaseAsset | null {
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
function pickRelease(payload: unknown, list: boolean): Record<string, unknown> | null {
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

/** 每次检测递增：切渠道时上一个请求可能还在路上，
 *  回来晚了就丢掉——否则「国外」的结果会覆盖刚切到的「国内」。 */
let checkSeq = 0;

async function setChannel(c: Channel) {
  if (c === channel.value || updState.value === "downloading" || updState.value === "installing") return;
  await store.save({ updateChannel: c });
  checkSeq++;
  latest.value = null;
  updError.value = "";
  updHint.value = "";
  updState.value = "idle";
  // 切完立刻查一次：用户点渠道就是为了看有没有更新
  void checkUpdate();
}

async function checkUpdate() {
  if (updBusy.value) return;
  const ch = channelInfo.value;
  const seq = ++checkSeq;
  updState.value = "checking";
  updError.value = "";
  updHint.value = "";
  latest.value = null;

  try {
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
    if (seq !== checkSeq) return;

    if (resp.status === 404) throw new Error(`该渠道还没有发布过版本，可稍后再试`);
    if (resp.status === 403) throw new Error(`${ch.host} 接口限流，请稍后再试`);
    if (!resp.ok) throw new Error(`接口返回 HTTP ${resp.status}`);

    const payload = (await resp.json()) as unknown;
    if (seq !== checkSeq) return;
    const data = pickRelease(payload, ch.list);
    if (!data) throw new Error("该渠道还没有发布过正式版本，可稍后再试");

    const tag = typeof data.tag_name === "string" ? data.tag_name.trim() : "";
    if (!tag) throw new Error("接口响应里没有版本号");

    if (cmpVersion(tag, appVersion.value || "0.0.0") <= 0) {
      updState.value = "latest";
      return;
    }

    const notes = typeof data.body === "string" ? data.body : "";
    const page = typeof data.html_url === "string" ? data.html_url : ch.page;
    latest.value = {
      tag,
      notes: cleanNotes(notes),
      page,
      asset: pickAsset(data.assets, platformKind.value),
    };
    updState.value = "available";
  } catch (e) {
    if (seq !== checkSeq) return;
    updState.value = "error";
    if (e instanceof Error && e.name === "AbortError") {
      updError.value = `${ch.host} 连接超时，请检查网络后重试`;
    } else if (e instanceof Error && /^[\x20-\x7e]+$/.test(e.message)) {
      // fetch 自己抛的英文错误（Failed to fetch 等）补一层上下文
      updError.value = `检测失败：${e.message.slice(0, 80)}`;
    } else if (e instanceof Error && e.message) {
      updError.value = e.message;
    } else {
      updError.value = "检测失败，请稍后重试";
    }
  }
}

async function startUpdate() {
  const rel = latest.value;
  if (!rel?.asset || updBusy.value) return;

  got.value = 0;
  total.value = 0;
  updError.value = "";
  updHint.value = "";
  updState.value = "downloading";

  try {
    const path = await appApi.updateDownload(rel.asset.url, rel.asset.name);
    updState.value = "installing";
    await appApi.updateApply(path);
    // Windows 会在这里之前退出进程；能走到下一行的只有 Android
    if (platformKind.value === "android") {
      ui.toast("已打开系统安装器，按提示完成安装", "success");
    }
  } catch (e) {
    const err = e as { code?: string; message?: string };
    // 用户自己取消：回到「可更新」，不是错误
    if (err?.code === "CANCELLED") {
      updState.value = "available";
      return;
    }
    // 安卓「安装未知应用」没授权：Kotlin 侧已把用户送到授权页，
    // 退回可点击状态并留一条提示，比一个死掉的错误框有用
    if (err?.code === "PERMISSION") {
      updState.value = "available";
      updHint.value = err.message ?? "请先在系统设置里允许本应用安装应用";
      return;
    }
    updState.value = "error";
    updError.value = errorText(e);
  }
}

async function cancelDownload() {
  try {
    await appApi.updateCancel();
  } catch {
    /* 已经下完了：状态会由 updateApply 那一步接管 */
  }
}

function openReleasePage() {
  const url = latest.value?.page || channelInfo.value.page;
  void appApi.openExternal(url).catch((e) => ui.toast(errorText(e), "error"));
}

let unlistenProgress: UnlistenFn | null = null;

onMounted(async () => {
  try {
    appVersion.value = await appApi.getVersion();
  } catch {
    /* 非 Tauri 环境（纯浏览器开发）保持空 */
  }
  unlistenProgress = await on<UpdateProgress>(EVT.updateProgress, (p) => {
    got.value = p.downloaded;
    total.value = p.total;
  });
});

onBeforeUnmount(() => {
  unlistenProgress?.();
});
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
        <button :class="{ on: tab === 'stream' }" @click="tab = 'stream'">流媒体</button>
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

          <!-- 主色只影响桌面端的界面强调色；安卓端跟随系统 / 固定主题，
               不提供选择（移动端设计上不要一堆彩色圆点占半屏） -->
          <div v-if="!store.isAndroid" class="row">
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
              <div class="desc">播放页歌词的基准字号，20 – 56 px</div>
            </div>
            <div class="num-field">
              <input
                type="text"
                inputmode="numeric"
                spellcheck="false"
                :value="s.lyricFontSize"
                @change="setNumber('lyricFontSize', $event, 20, 56)"
                @keydown.up.prevent="bump('lyricFontSize', 1, 20, 56)"
                @keydown.down.prevent="bump('lyricFontSize', -1, 20, 56)"
              />
              <span class="steppers">
                <button
                  type="button"
                  tabindex="-1"
                  aria-label="增大"
                  @click="bump('lyricFontSize', 1, 20, 56)"
                >
                  <svg viewBox="0 0 8 5" aria-hidden="true"><path d="M4 0L8 5H0z" fill="currentColor" /></svg>
                </button>
                <button
                  type="button"
                  tabindex="-1"
                  aria-label="减小"
                  @click="bump('lyricFontSize', -1, 20, 56)"
                >
                  <svg viewBox="0 0 8 5" aria-hidden="true"><path d="M0 0h8L4 5z" fill="currentColor" /></svg>
                </button>
              </span>
            </div>
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

          <!-- 自定义软件背景：桌面 / 移动共用同一张设置卡片 -->
          <div class="card bg-card">
            <div class="bg-card-title">自定义背景</div>

            <div class="row">
              <div class="label">
                <div>使用自定义背景</div>
                <div class="desc">用一张图片作为主界面背景。</div>
              </div>
              <label class="switch">
                <input
                  type="checkbox"
                  :checked="s.bgEnabled"
                  @change="set('bgEnabled', ($event.target as HTMLInputElement).checked)"
                />
                <span />
              </label>
            </div>

            <div class="row">
              <div class="label">
                <div>背景图片</div>
                <div class="desc">{{ s.bgImage ? "已选择图片" : "未选择图片" }}</div>
              </div>
              <div class="bg-actions">
                <button class="link-btn" :disabled="bgBusy" @click="pickBg">选择图片</button>
                <button class="link-btn" :disabled="bgBusy || !s.bgImage" @click="clearBg">
                  清除
                </button>
              </div>
            </div>

            <div class="row">
              <div class="label">
                <div>图片透明度</div>
                <div class="desc">
                  {{ s.bgEnabled ? "数值越大，背景越明显" : "开启自定义背景后可调节。" }}
                </div>
              </div>
              <div class="bg-opacity">
                <input
                  type="range"
                  min="5"
                  max="100"
                  step="1"
                  :value="String(bgOpacityDraft)"
                  :disabled="!s.bgEnabled"
                  :style="`--bg-fill:${bgOpacityDraft}%`"
                  aria-label="图片透明度"
                  @input="onBgOpacityInput"
                  @change="onBgOpacityChange"
                  @pointerup="onBgOpacityRelease"
                />
                <span class="bg-opacity-value">{{ bgOpacityDraft }}%</span>
              </div>
            </div>

            <input ref="bgFile" type="file" accept="image/*" hidden @change="onBgFile" />
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
          <!-- 智能音质：解析 / 开播失败时逐档降级重试（默认开启）。
               开关只是策略开关，真正的降级链在 Rust 侧 resolve_and_play。 -->
          <div class="row">
            <div class="label with-icon">
              <span class="row-icon"><Icon name="sparkle" :size="19" filled /></span>
              <div>
                <div>智能音质</div>
                <div class="desc">播放失败时自动降级音质重试</div>
              </div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="s.smartQuality"
                @change="set('smartQuality', ($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
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
              <div class="desc">播放页封面背景的模糊半径，0 – 100 px</div>
            </div>
            <div class="num-field">
              <input
                type="text"
                inputmode="numeric"
                spellcheck="false"
                :value="s.nowBgBlur"
                @change="setNumber('nowBgBlur', $event, 0, 100)"
                @keydown.up.prevent="bump('nowBgBlur', 1, 0, 100)"
                @keydown.down.prevent="bump('nowBgBlur', -1, 0, 100)"
              />
              <span class="steppers">
                <button
                  type="button"
                  tabindex="-1"
                  aria-label="增大"
                  @click="bump('nowBgBlur', 1, 0, 100)"
                >
                  <svg viewBox="0 0 8 5" aria-hidden="true"><path d="M4 0L8 5H0z" fill="currentColor" /></svg>
                </button>
                <button
                  type="button"
                  tabindex="-1"
                  aria-label="减小"
                  @click="bump('nowBgBlur', -1, 0, 100)"
                >
                  <svg viewBox="0 0 8 5" aria-hidden="true"><path d="M0 0h8L4 5z" fill="currentColor" /></svg>
                </button>
              </span>
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
          <!-- 记住搜索平台：只管「选过的音源要不要跨重启保留」，
               音源值本身存 localStorage，开关进 Rust 设置库以便跨窗口同步 -->
          <div class="row">
            <div class="label with-icon">
              <span class="row-icon"><Icon name="search" :size="19" /></span>
              <div>
                <div>记住搜索平台</div>
                <div class="desc">
                  重启后继续使用上次选的搜索音源；关闭后每次启动回到网易云音乐
                </div>
              </div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="s.rememberSearchSource"
                @change="set('rememberSearchSource', ($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
          </div>

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

          <div v-for="p in sourcePlugins" :key="p.manifest.id" class="plugin card">
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

          <div v-if="!sourcePlugins.length" class="empty">
            <div class="big">🔌</div>
            <div>还没有插件</div>
          </div>

          <p class="plugins-foot">
            Emby / Jellyfin / Navidrome 等自建服务已移到
            <button class="link-btn" @click="tab = 'stream'">「流媒体」页</button>，
            那里统一开关，默认关闭。
          </p>
        </section>

        <!-- 流媒体：自建 Emby / Jellyfin / Navidrome。
             默认关闭并收起配置，打开总开关才展开 —— 三个服务大多用不到，
             常驻三张空配置卡只会把音源页撑长。 -->
        <section v-else-if="tab === 'stream'" class="group">
          <div class="row">
            <div class="label">
              <div>启用流媒体</div>
              <div class="desc">
                接入你自己的媒体服务器，把自建曲库当作在线音源；请求直连服务器，不经第三方
              </div>
            </div>
            <label class="switch">
              <input
                type="checkbox"
                :checked="s.streamEnabled"
                @change="toggleStream(($event.target as HTMLInputElement).checked)"
              />
              <span />
            </label>
          </div>

          <div v-if="!s.streamEnabled" class="stream-off card">
            <div class="stream-brands">
              <span v-for="name in ['Emby', 'Jellyfin', 'Navidrome']" :key="name">{{ name }}</span>
            </div>
            <p class="desc">
              默认关闭，配置项已收起。启用后在这里填写服务器地址与凭据，三个服务互不影响，
              只开你在用的那个；填全必填项并保存即自动启用。
            </p>
          </div>

          <template v-else>
            <div v-for="p in streamPlugins" :key="p.manifest.id" class="plugin card">
              <div class="plugin-head">
                <div class="plugin-title">
                  <Icon name="plug" :size="17" />
                  <strong>{{ p.manifest.name }}</strong>
                  <span class="badge" :class="{ ok: p.enabled }">
                    {{ p.enabled ? "已启用" : isConfigured(p) ? "已配置" : "待配置" }}
                  </span>
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

              <div v-if="configFields(p).length && pluginDraft[p.manifest.id]" class="fields">
                <div v-for="f in configFields(p)" :key="f.key" class="field">
                  <label>{{ f.label }}<span v-if="f.required" class="req">*</span></label>
                  <div v-if="f.fieldType === 'boolean'" class="bool-row">
                    <label class="switch">
                      <input
                        type="checkbox"
                        :checked="pluginDraft[p.manifest.id][f.key] !== 'off'"
                        @change="
                          pluginDraft[p.manifest.id][f.key] = (
                            $event.target as HTMLInputElement
                          ).checked
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
                    spellcheck="false"
                  />
                </div>
                <button class="btn primary" @click="savePlugin(p.manifest.id)">保存配置</button>
              </div>
            </div>

            <p v-if="!streamPlugins.length" class="empty">
              没有找到内置的 Emby / Jellyfin / Navidrome 插件
            </p>
          </template>
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
                <div class="desc">
                  检查并安装新版本 · 国内渠道直连 Gitee，不用跳浏览器
                </div>
              </div>
              <button class="btn small" :disabled="updBusy" @click="checkUpdate">
                <Icon
                  name="refresh"
                  :size="14"
                  :class="{ 'upd-spin': updState === 'checking' }"
                />
                {{ updState === "checking" ? "检测中…" : "检测更新" }}
              </button>
            </div>

            <!-- 渠道切换：国内默认，国内直连 Gitee 更快 -->
            <div class="upd-chan" role="group" aria-label="更新渠道">
              <button
                v-for="c in channelList"
                :key="c.id"
                class="upd-chan-btn"
                :class="{ on: channel === c.id }"
                :disabled="updState === 'downloading' || updState === 'installing'"
                @click="setChannel(c.id)"
              >
                {{ c.name }}<span class="host">{{ c.host }}</span>
              </button>
            </div>

            <div v-if="updState === 'latest'" class="upd-result ok">
              ✓ 已是最新版本（v{{ appVersion }}）
            </div>

            <div v-else-if="updState === 'available'" class="upd-result new">
              <div class="upd-new-head">
                <span>
                  发现新版本 <b>{{ latest?.tag }}</b>
                </span>
                <span class="upd-actions">
                  <button v-if="canInApp" class="btn small primary" @click="startUpdate">
                    立即更新
                  </button>
                  <button class="btn small" @click="openReleasePage">
                    {{ canInApp ? "下载页" : "前往下载" }}
                  </button>
                </span>
              </div>
              <p v-if="updHint" class="upd-hint">{{ updHint }}</p>
              <p v-else-if="canInApp" class="upd-hint info">
                {{
                  store.isAndroid
                    ? "下载完成后交给系统安装器，全程不用打开浏览器"
                    : "下载完成后自动替换并重启，无需重新安装"
                }}
              </p>
              <pre v-if="latest?.notes" class="upd-notes">{{ latest.notes }}</pre>
            </div>

            <div v-else-if="updState === 'downloading'" class="upd-result new">
              <div class="upd-new-head">
                <span>正在下载 {{ latest?.tag }}</span>
                <span class="mono">{{ total ? pct + "%" : "" }} {{ sizeText }}</span>
              </div>
              <div class="upd-bar" :class="{ unknown: !total }">
                <i :style="{ width: total ? pct + '%' : '100%' }" />
              </div>
              <div class="upd-foot">
                <span class="desc">
                  {{ store.isAndroid ? "完成后会打开系统安装器" : "完成后会自动重启完成更新" }}
                </span>
                <button class="link" @click="cancelDownload">取消</button>
              </div>
            </div>

            <div v-else-if="updState === 'installing'" class="upd-result new">
              <div class="upd-new-head">
                <span>
                  {{
                    store.isAndroid
                      ? "正在打开系统安装器…"
                      : "更新包已就绪，正在重启应用…"
                  }}
                </span>
              </div>
            </div>

            <div v-else-if="updState === 'error'" class="upd-result err">
              <span>{{ updError }}</span>
              <span class="upd-actions">
                <button class="link" @click="checkUpdate">重试</button>
                <button class="link" @click="openReleasePage">下载页</button>
              </span>
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
@media (max-width: 860px) {
  .quality-input {
    width: 100%;
    max-width: 300px;
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
/* 带前置图标的设置行（智能音质等）：图标与标题同排，说明仍在标题下方。
   图标那一路把文字包进了内层 div，所以标题样式要补一条同款。 */
.label.with-icon {
  display: flex;
  align-items: flex-start;
  gap: 11px;
  min-width: 0;
}
.row-icon {
  flex: none;
  margin-top: 1px;
  color: var(--accent);
  line-height: 0;
}
.label.with-icon > div > div:first-child {
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

/* 数字步进输入：浅灰底、无边框，值靠右，右侧自绘上下箭头。
   替代滑条 —— 字号 / 模糊半径这类精确值，拖动很难停到想要的那个数。
   箭头不用 `input[type=number]` 的原生 spinner：Chromium 只在悬停时显示，
   离开鼠标就消失，看起来像个不能点的普通数字。 */
.num-field {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 36px;
  padding: 0 6px 0 12px;
  border-radius: 9px;
  border: 1px solid transparent;
  background: var(--panel-3);
  transition: border-color 0.15s, background 0.15s;
}
.num-field:hover {
  background: var(--panel-2);
}
.num-field:focus-within {
  border-color: var(--accent);
  background: var(--panel);
}
.num-field input {
  width: 54px;
  height: 100%;
  border: 0;
  outline: none;
  background: transparent;
  color: var(--text);
  font-size: 14px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
  text-align: right;
  padding: 0;
}
.steppers {
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.steppers button {
  width: 20px;
  height: 15px;
  display: grid;
  place-items: center;
  border: 0;
  padding: 0;
  border-radius: 4px;
  background: none;
  color: var(--text-2);
  cursor: pointer;
}
.steppers button:hover {
  color: var(--text);
  background: color-mix(in srgb, var(--accent) 16%, transparent);
}
.steppers svg {
  width: 8px;
  height: 5px;
  display: block;
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
.badge.ok {
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  color: var(--accent);
}
/* 流媒体默认关闭时的占位卡：只列服务名 + 一句说明，不渲染任何输入框 */
.stream-off {
  padding: 16px 18px;
  margin-bottom: 10px;
}
.stream-brands {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 10px;
}
.stream-brands span {
  font-size: 11px;
  letter-spacing: 0.02em;
  color: var(--text-3);
  padding: 3px 10px;
  border-radius: 999px;
  border: 1px dashed var(--border-strong);
}
.stream-off .desc {
  margin: 0;
}
.plugins-foot {
  font-size: 11.5px;
  color: var(--text-3);
  line-height: 1.7;
  padding: 6px 4px 0;
}
.link-btn {
  border: 0;
  background: none;
  padding: 0;
  font: inherit;
  color: var(--accent);
  cursor: pointer;
}
.link-btn:hover {
  text-decoration: underline;
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

/* 渠道切换：两段式分段控件。国内默认，所以不加「推荐」角标也够直观 */
.upd-chan {
  display: inline-flex;
  gap: 2px;
  margin-top: 12px;
  padding: 3px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: color-mix(in srgb, var(--panel) 70%, transparent);
}
.upd-chan-btn {
  display: inline-flex;
  align-items: baseline;
  gap: 5px;
  padding: 5px 12px;
  border: none;
  border-radius: 8px;
  background: none;
  color: var(--text-2);
  font-size: 12.5px;
  cursor: pointer;
  transition: background 0.15s ease, color 0.15s ease;
}
.upd-chan-btn .host {
  font-size: 10.5px;
  letter-spacing: 0.02em;
  opacity: 0.65;
}
.upd-chan-btn:hover:not(:disabled) {
  color: var(--text);
}
.upd-chan-btn.on {
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  color: var(--text);
}
.upd-chan-btn.on .host {
  color: var(--accent);
  opacity: 1;
}
.upd-chan-btn:disabled {
  cursor: default;
  opacity: 0.55;
}
.upd-actions {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.upd-hint {
  margin: 0;
  font-size: 11.5px;
  line-height: 1.6;
  color: #e0a94a;
}
.upd-hint.info {
  color: var(--text-3);
}
/* 下载进度：total 不明时（少数代理会吞掉 Content-Length）走呼吸条 */
.upd-bar {
  height: 6px;
  border-radius: 999px;
  overflow: hidden;
  background: color-mix(in srgb, var(--text-3) 20%, transparent);
}
.upd-bar i {
  display: block;
  height: 100%;
  border-radius: 999px;
  background: linear-gradient(90deg, var(--accent), var(--accent-2));
  transition: width 0.2s ease;
}
.upd-bar.unknown i {
  animation: upd-pulse 1.2s ease-in-out infinite;
}
@keyframes upd-pulse {
  0%,
  100% {
    opacity: 0.25;
  }
  50% {
    opacity: 0.65;
  }
}
.upd-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  font-size: 11.5px;
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
  /* 控件在标题下方时铺满整行。多带一层 .bg-card 提高特异性：
     基础样式写在文件末尾，只靠媒体查询的先后顺序会被反覆盖。 */
  .bg-card .bg-opacity {
    width: 100%;
    max-width: 320px;
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
  /* 数字步进框：手机上不能跟着 .row 拉满整行 —— 数字贴最左、箭头贴最右，
     中间几百像素空白会让它看起来像两个不相干的控件。保持紧凑、贴着标题左侧，
     只把高度和点击区放大到手指好点的尺寸。 */
  .num-field {
    width: auto;
    align-self: flex-start;
    height: 44px;
    padding: 0 8px 0 14px;
  }
  .num-field input {
    flex: none;
    width: 46px;
    text-align: right;
    font-size: 16px;
  }
  .steppers {
    flex-direction: row;
    align-items: center;
    gap: 2px;
    padding-left: 6px;
    border-left: 1px solid var(--border);
  }
  .steppers button {
    width: 32px;
    height: 34px;
  }
  .steppers svg {
    width: 10px;
    height: 6px;
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
  /* 横屏又回到「标题在左、控件在右」，步进框跟着收回紧凑样式 */
  .num-field {
    width: auto;
    align-self: center;
    height: 36px;
    padding: 0 6px 0 12px;
  }
  .num-field input {
    flex: none;
    width: 54px;
    text-align: right;
    font-size: 14px;
  }
  .steppers {
    flex-direction: column;
    gap: 1px;
    padding-left: 0;
    border-left: 0;
  }
  .steppers button {
    width: 20px;
    height: 15px;
  }
  .steppers svg {
    width: 8px;
    height: 5px;
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

/* ---------- 自定义背景卡片 ---------- */
.bg-card {
  margin-top: 16px;
  padding: 4px 8px 10px;
}
.bg-card-title {
  padding: 12px 8px 2px;
  font-size: 13px;
  font-weight: 600;
  color: var(--text);
}
.bg-actions {
  display: flex;
  align-items: center;
  gap: 16px;
  flex: none;
}
.bg-actions .link-btn:disabled {
  opacity: 0.4;
  cursor: default;
  text-decoration: none;
}
.bg-opacity {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: none;
  width: 210px;
}
.bg-opacity input[type="range"] {
  flex: 1;
  min-width: 0;
  accent-color: var(--accent);
  cursor: pointer;
  /* 自定义外观后 Chromium 不再画「已选中」那一段，只剩一个圆点很难看出位置。
     这里用同一个数值（--bg-fill，来自 bgOpacityDraft）驱动渐变，
     填充比例与右侧百分比永远同源。 */
  background: linear-gradient(
    to right,
    var(--accent) 0,
    var(--accent) var(--bg-fill, 35%),
    var(--border-strong) var(--bg-fill, 35%),
    var(--border-strong) 100%
  );
}
.bg-opacity input[type="range"]:disabled {
  opacity: 0.4;
  cursor: default;
}
/* 圆点和填充段同色，落在分界处会「消失」；描一圈卡片底色把它抠出来。 */
.bg-opacity input[type="range"]::-webkit-slider-thumb {
  box-sizing: border-box;
  border: 2px solid var(--panel);
  box-shadow: 0 1px 3px rgb(0 0 0 / 0.2);
}
.bg-opacity-value {
  width: 40px;
  text-align: right;
  font-size: 12px;
  color: var(--text-2);
  font-variant-numeric: tabular-nums;
}
</style>
