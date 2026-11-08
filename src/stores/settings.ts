/**
 * 设置 store：主题、配色、歌词样式、缓存、插件。
 *
 * 主题切换靠 `<html data-theme>` + CSS 变量，不依赖任何 UI 库。
 */

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
  cacheLimitMb: 2048,
  crossfadeMs: 0,
};

/** 自建流媒体服务的插件 id：由设置页「流媒体」总开关统一启停。
 *  设置页按这一组 id 过滤渲染，不散落在各处写字符串。 */
export const STREAM_PLUGIN_IDS = ["emby", "jellyfin", "navidrome"] as const;

export const useSettingsStore = defineStore("settings", () => {
  const value = ref<Settings>({ ...DEFAULTS });
  const platform = ref<PlatformInfo | null>(null);
  const pluginList = ref<PluginInfo[]>([]);
  const cacheBytes = ref(0);
  const loaded = ref(false);

  const isAndroid = computed(() => platform.value?.kind === "android");
  const supportsLyricWindow = computed(() => platform.value?.supportsLyricWindow ?? false);

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
  }

  watch(() => [value.value.theme, value.value.accent, value.value.lyricFontSize], applyTheme, {
    deep: true,
  });

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
