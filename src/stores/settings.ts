/**
 * 设置 store：主题、配色、歌词样式、缓存、插件。
 *
 * 主题切换靠 `<html data-theme>` + CSS 变量，不依赖任何 UI 库。
 */

import { convertFileSrc } from "@tauri-apps/api/core";
import { defineStore } from "pinia";
import { computed, ref, watch } from "vue";

import { plugins as pluginApi, settings as settingsApi, EVT, on } from "@/api/ipc";
import type { PlatformInfo, PluginInfo, Settings } from "@/types";

const DEFAULTS: Settings = {
  theme: "system",
  accent: "#7c5cff",
  volume: 0.85,
  speed: 1,
  pitch: 0,
  gainDb: 0,
  repeat: "all",
  shuffle: false,
  libraryPaths: [],
  lyricFontSize: 34,
  lyricAlign: "center",
  lyricShowTranslation: true,
  closeToTray: true,
  nowBgSource: "cover",
  nowBgBlur: 60,
  dlLocked: false,
  dlDirection: "horizontal",
  dlAlign: "center",
  dlDualLine: true,
  dlFontSize: 34,
  dlColorMode: "theme",
  dlColorLyric: "#ffffff",
  dlColorTrans: "#ffffff",
  dlAlwaysOnTop: true,
  streamEnabled: false,
  smartQuality: true,
  rememberSearchSource: true,
  bgEnabled: false,
  bgImage: "",
  bgOpacity: 35,
  cacheLimitMb: 2048,
  crossfadeMs: 0,
  updateChannel: "cn",
};

/** 自建流媒体服务的插件 id：由设置页「流媒体」总开关统一启停。
 *  设置页按这一组 id 过滤渲染，不散落在各处写字符串。 */
export const STREAM_PLUGIN_IDS = ["emby", "jellyfin", "navidrome"] as const;

/**
 * 自定义背景在移动端的联动参数。
 *
 * 手机没有桌面那套 .sidebar / .main 玻璃面板，正文全靠 .mobile-shell 一层
 * 固定 58% 的底衬托住。以前这层不随设置变化，用户把「图片透明度」拉到
 * 100% 时照片仍被压掉 58%，实际可见度只有 0.42 —— 这就是「开到最高还是
 * 不够透」的原因（桌面没有这层壳，所以桌面正常）。现在让遮挡层联动：
 *   - ≤35%（含默认值）：底衬 58%、卡片玻璃 80%，观感与旧版完全一致；
 *   - 往上逐渐让位，100% 时底衬 10%、玻璃 70%，照片可见度 0.90。
 * --m-photo-visible 是首页照片的最终可见度，二级页 .m-sub::before 直接
 * 用它对齐同一档（二级页没有蒙层，只能靠 opacity）。
 *
 * m- 前缀变量只被 mobile.css / MobileShell.vue 消费（mobile.css 仅由
 * MobileShell 引入），桌面端只认 --bg-image-opacity，不受影响。
 */
export function bgOpacityCssVars(percent: number): Record<string, string> {
  const op = Math.min(100, Math.max(5, Math.round(Number(percent) || 35)));
  const t = Math.max(0, (op - 35) / 65); // 0 → 1
  const veil = Math.round(58 - 48 * t); // 58% → 10%
  const glass = Math.round(80 - 10 * t); // 80% → 70%
  // 照片全透后，--text-2 / --text-3 那档浅灰会被亮部吃掉（实测歌手名、
  // 心形图标在黄/青区域几乎消失）。把副文字与图标往主文字色拉，0% 时
  // color-mix 的结果正好等于原值，默认档观感零变化。
  const shield = Math.round(85 * t); // 0% → 85%
  return {
    "--bg-image-opacity": String(op / 100),
    "--m-veil": `${veil}%`,
    "--m-glass-alpha": `${glass}%`,
    "--m-shield": `${shield}%`,
    "--m-photo-visible": ((op / 100) * (1 - veil / 100)).toFixed(3),
  };
}

/** 把联动变量写到根元素。启动 / 落盘 / 拖动预览共用这一套映射，避免两处漂移。 */
export function applyBgOpacityVars(root: HTMLElement, percent: number): void {
  for (const [name, val] of Object.entries(bgOpacityCssVars(percent))) {
    root.style.setProperty(name, val);
  }
}

/** 搜索音源值沿用 1.1.1 已写入的 key：升级后用户原先选的 QQ 音乐不会丢。
 *  现在它不再只属于首页 —— 桌面首页 / 桌面搜索页 / 移动搜索页共用一份。 */
const SEARCH_SOURCE_KEY = "leonmusic.home.searchSource";
const DEFAULT_SEARCH_SOURCE = "netease";

function readSavedSource(): string | null {
  try {
    return localStorage.getItem(SEARCH_SOURCE_KEY);
  } catch {
    return null; // 隐私模式 / 禁用存储
  }
}

function writeSavedSource(id: string | null): void {
  try {
    if (id) localStorage.setItem(SEARCH_SOURCE_KEY, id);
    else localStorage.removeItem(SEARCH_SOURCE_KEY);
  } catch {
    /* 存储不可用时只在本次会话内生效 */
  }
}

/** File → 纯 base64（不含 data URL 前缀）。
 *  用 FileReader 而不是 btoa + 字符串拼接：大图分块才不会爆栈，
 *  而且 Android WebView 与 Windows WebView2 行为一致。 */
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result ?? "");
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("读取图片失败"));
    reader.readAsDataURL(file);
  });
}

export const useSettingsStore = defineStore("settings", () => {
  const value = ref<Settings>({ ...DEFAULTS });
  const platform = ref<PlatformInfo | null>(null);
  const pluginList = ref<PluginInfo[]>([]);
  const cacheBytes = ref(0);
  const loaded = ref(false);

  const isAndroid = computed(() => platform.value?.kind === "android");
  const supportsLyricWindow = computed(() => platform.value?.supportsLyricWindow ?? false);

  // ---------------------------------------------------------------------------
  // 搜索音源：全软件共享的当前值 + 「记住搜索平台」开关
  //
  // 以前只有首页内联搜索把选择记在 localStorage，独立的桌面/移动搜索页各自
  // 从插件列表第一项起步，三个入口互不相认。这里把当前值收进 store：
  // 一处切换，三个页面同步；是否落盘由 Settings.rememberSearchSource 决定。
  // 音源值本身是纯 UI 偏好（localStorage 足够），开关才进 Rust 设置库，
  // 这样才能跨窗口广播、两端共用同一份配置。
  // ---------------------------------------------------------------------------

  const searchSource = ref<string>(DEFAULT_SEARCH_SOURCE);
  let searchSourceReady = false;

  /** 设置页随时可能关掉「记住」，跨窗口 settings_changed 也走这条路径。
   *  关掉时立刻抹掉历史值；重新打开时把当前会话的选择再记上。 */
  watch(
    () => value.value.rememberSearchSource,
    (remember) => {
      if (!searchSourceReady) return;
      writeSavedSource(remember ? searchSource.value : null);
    },
  );

  /** 各搜索页 mount 后调用：第一次决定初值，之后只做有效性兜底
   *  （插件停用 / 卸载后，记住的 id 不存在就回落到默认音源）。 */
  function initSearchSource(availableIds: string[]): string {
    const fallback = availableIds.includes(DEFAULT_SEARCH_SOURCE)
      ? DEFAULT_SEARCH_SOURCE
      : (availableIds[0] ?? DEFAULT_SEARCH_SOURCE);
    const remember = value.value.rememberSearchSource;
    if (!searchSourceReady) {
      const saved = remember ? readSavedSource() : null;
      searchSource.value = saved && availableIds.includes(saved) ? saved : fallback;
      searchSourceReady = true;
    } else if (!availableIds.includes(searchSource.value)) {
      searchSource.value = fallback;
    }
    if (!remember) writeSavedSource(null);
    return searchSource.value;
  }

  function selectSearchSource(id: string): void {
    searchSource.value = id;
    writeSavedSource(value.value.rememberSearchSource ? id : null);
  }

  /** 上次同步给系统栏的明暗，避免每次 watch 触发都发一次 IPC */
  let lastDarkAppearance: boolean | null = null;

  function applyTheme() {
    const root = document.documentElement;
    const mode =
      value.value.theme === "system"
        ? window.matchMedia("(prefers-color-scheme: dark)").matches
          ? "dark"
          : "light"
        : value.value.theme;
    root.dataset.theme = mode;
    root.classList.toggle("dark", mode === "dark");
    root.style.setProperty("--accent", value.value.accent);
    // 写 `-user` 而不是直接用 `--lyric-size`：手机上要把它压到 24px 上限
    // （见 main.css 的 min(var(--lyric-size-user), 24px)）。
    // 内联样式优先级高于 :root，如果直接写 --lyric-size，媒体查询就失效了。
    root.style.setProperty("--lyric-size-user", `${value.value.lyricFontSize}px`);

    // Android 是 edge-to-edge：状态栏和手势条直接压在 WebView 上，
    // 图标颜色只能由系统决定。深色主题下不通知系统，就会得到
    // 「深色背景 + 深色图标」，什么都看不见。
    const dark = mode === "dark";
    if (dark !== lastDarkAppearance) {
      lastDarkAppearance = dark;
      void settingsApi.setDarkAppearance(dark).catch(() => {
        // 桌面端是空实现，老版本 Android 也可能没有这条命令 —— 不值得打扰用户
      });
    }

    // 自定义背景：透明度与移动端联动变量挂到根元素，main.css / mobile.css
    // 直接消费；图片本身由 bgUrl 异步加载，不在这里等 IPC。
    applyBgOpacityVars(root, value.value.bgOpacity);
    root.toggleAttribute("data-custom-bg", value.value.bgEnabled && !!value.value.bgImage);
  }

  /** 已保存背景图的 asset URL，供 App.vue 的全局背景层使用。 */
  const bgUrl = ref("");

  async function refreshBackground() {
    if (!value.value.bgEnabled || !value.value.bgImage) {
      bgUrl.value = "";
      return;
    }
    try {
      const path = await settingsApi.backgroundPath();
      bgUrl.value = path ? convertFileSrc(path) : "";
    } catch {
      bgUrl.value = "";
    }
  }

  /** 选图：落盘交给 Rust，返回的新 Settings 同步本窗并立刻刷新背景。 */
  async function setBackgroundFile(file: File) {
    const data = await fileToBase64(file);
    const updated = await settingsApi.setBackgroundImage(data);
    value.value = { ...DEFAULTS, ...updated };
    applyTheme();
    await refreshBackground();
  }

  async function clearBackground() {
    const updated = await settingsApi.clearBackgroundImage();
    bgUrl.value = "";
    value.value = { ...DEFAULTS, ...updated };
    applyTheme();
  }

  watch(
    () => [value.value.bgEnabled, value.value.bgImage],
    () => void refreshBackground(),
  );

  watch(
    () => [
      value.value.theme,
      value.value.accent,
      value.value.lyricFontSize,
      value.value.bgEnabled,
      value.value.bgImage,
      value.value.bgOpacity,
    ],
    applyTheme,
    { deep: true },
  );

  /**
   * 初始化。
   *
   * 这里必须把「插件列表」也 await 完：搜索页/设置页在 mount 时就会读
   * `pluginList` 来选中默认音源，如果列表还在异步路上，页面会拿到空数组，
   * 表现就是「点搜索没反应」。
   *
   * 同时用 in-flight promise 兜住并发：多个页面同时 init 时只跑一次，
   * 后来的调用者等同一个 Promise，而不是被 `loaded` 提前打发走。
   */
  let initPromise: Promise<void> | null = null;

  async function init() {
    if (!initPromise) initPromise = doInit();
    return initPromise;
  }

  async function doInit() {
    if (loaded.value) return;
    const [s, p] = await Promise.all([
      settingsApi.get(),
      settingsApi.platformInfo().catch(() => null),
    ]);
    value.value = { ...DEFAULTS, ...s };
    platform.value = p;
    applyTheme();
    await refreshBackground();
    await reloadPlugins();
    // 流媒体默认关闭：插件行在库里默认是「启用」，这里对齐一次。
    // 否则会出现「设置页显示关着、搜索页却还能选到自建服务」的错位。
    if (!value.value.streamEnabled) await syncStreamPlugins(false);
    loaded.value = true;
    void refreshCacheSize();

    // 跨窗口设置同步：主窗口与桌面歌词窗各有一份本 store 的实例，
    // 任何一窗保存（Rust settings_update 广播），另一窗在这里实时跟随。
    // 不需要 unlisten —— store 与窗口同生命周期。
    await on<Settings>(EVT.settingsChanged, (payload) => {
      value.value = { ...DEFAULTS, ...payload };
      applyTheme();
      // 别的窗口（或本机选图命令）换了图时同步刷新，文件名带时间戳所以会变
      void refreshBackground();
    });
  }

  async function save(patch: Partial<Settings> = {}) {
    value.value = { ...value.value, ...patch };
    applyTheme();
    await settingsApi.update(value.value);
  }

  async function reloadPlugins() {
    pluginList.value = await pluginApi.list();
  }

  async function setPluginEnabled(id: string, enabled: boolean) {
    await pluginApi.setEnabled(id, enabled);
    await reloadPlugins();
  }

  /** 把三个自建服务插件的启停对齐到「流媒体」总开关。
   *  总开关关闭时必须真的停用插件 —— 只把设置页的配置块收起来，
   *  插件照样在搜索页可选，等于假关闭。 */
  async function syncStreamPlugins(enabled: boolean) {
    let dirty = false;
    for (const id of STREAM_PLUGIN_IDS) {
      const p = pluginList.value.find((x) => x.manifest.id === id);
      if (p && p.enabled !== enabled) {
        await pluginApi.setEnabled(id, enabled);
        dirty = true;
      }
    }
    if (dirty) await reloadPlugins();
  }

  async function setPluginConfig(id: string, config: Record<string, string>) {
    await pluginApi.setConfig(id, config);
    await reloadPlugins();
  }

  async function refreshCacheSize() {
    cacheBytes.value = await settingsApi.cacheSize();
  }

  async function clearCache() {
    await settingsApi.cacheClear();
    await refreshCacheSize();
  }

  return {
    value,
    platform,
    pluginList,
    cacheBytes,
    loaded,
    isAndroid,
    supportsLyricWindow,
    bgUrl,
    setBackgroundFile,
    clearBackground,
    refreshBackground,
    searchSource,
    initSearchSource,
    selectSearchSource,
    init,
    save,
    reloadPlugins,
    setPluginEnabled,
    setPluginConfig,
    syncStreamPlugins,
    refreshCacheSize,
    clearCache,
    applyTheme,
  };
});
