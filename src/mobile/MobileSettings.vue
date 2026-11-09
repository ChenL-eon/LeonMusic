<script setup lang="ts">
/**
 * 移动端设置：分组卡片式首页 + 复用桌面 Settings.vue 的详情页。
 *
 * ## 为什么不改 Settings.vue
 * 桌面端要求「完全不动」。这里用一个只挂在 MobileShell 下的壳：
 *  * 首页是分组卡片（参考系统设置的分类布局），图标沿用 App 的内联 SVG，
 *    配色取 --accent 邻近色系并用 color-mix 融进当前主题，明暗主题都协调
 *  * 详情页直接渲染 <SettingsView>，再用 :deep() 把桌面专属的侧栏页签、
 *    标题与两列布局抹平 —— Settings.vue 本身一行不改
 *  * 「通知与锁屏」是安卓专属面板，不塞进桌面的 SettingsTab 枚举
 *
 * 返回逻辑由 MobileShell 统一接管：系统侧滑返回先让本组件消费掉
 * 「详情 → 分类」这一级，分类首页再退出设置页。
 */
import { computed, nextTick, onActivated, onMounted, ref } from "vue";

import Icon from "@/components/Icon.vue";
import SettingsView from "@/views/Settings.vue";
import { errorText, settings as settingsApi } from "@/api/ipc";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore, type SettingsTab } from "@/stores/ui";
import type { MediaStatus } from "@/types";

const emit = defineEmits<{
  (e: "back"): void;
  (e: "open-sub", page: "account"): void;
}>();

const ui = useUiStore();
const store = useSettingsStore();

interface Entry {
  id: string;
  title: string;
  desc: string;
  icon: string;
  color: string;
  keywords: string;
  tab?: SettingsTab;
  panel?: "media";
  page?: "account";
  focusSelector?: string;
  focusText?: string;
}

interface Group {
  id: string;
  title: string;
  items: Entry[];
}

/**
 * 每组对应一个真实的功能域。两个「同名 tab」的入口（播放设置 / 音质选择、
 * 主题与外观 / 歌词与显示）用滚动定位区分，点了不会看起来毫无变化。
 */
const GROUPS: Group[] = [
  {
    id: "sound",
    title: "播放与声音",
    items: [
      {
        id: "play",
        title: "播放设置",
        desc: "播放行为、衔接与音量体验",
        icon: "play",
        color: "#7c5cff",
        keywords: "智能音质 降级 采样率 缓存 后台 音量 音频输出 自检",
        tab: "play",
      },
      {
        id: "quality",
        title: "音质选择",
        desc: "在线播放与移动网络音质",
        icon: "waveform",
        color: "#4b8cf7",
        keywords: "无损 FLAC 320k 极高 标准 解析码率 档位 bitrate",
        tab: "play",
        focusSelector: ".quality-card",
      },
      {
        id: "media",
        title: "通知与锁屏",
        desc: "通知栏、锁屏播放状态与权限",
        icon: "lock",
        color: "#3dd6d0",
        keywords: "通知栏 锁屏 媒体会话 MediaSession 蓝牙 线控 封面 切歌 权限 前台服务",
        panel: "media",
      },
    ],
  },
  {
    id: "look",
    title: "界面与歌词",
    items: [
      {
        id: "theme",
        title: "主题与外观",
        desc: "主题模式与系统状态栏",
        icon: "sun",
        color: "#ffa94d",
        keywords: "深色 浅色 跟随系统 主题 外观 夜间",
        tab: "look",
      },
      {
        id: "lyric",
        title: "歌词与显示",
        desc: "歌词字号、对齐与翻译",
        icon: "lyrics",
        color: "#f26d8b",
        keywords: "歌词 字号 对齐 翻译 显示 字体",
        tab: "look",
        focusText: "歌词字号",
      },
    ],
  },
  {
    id: "source",
    title: "音源与服务",
    items: [
      {
        id: "plugin",
        title: "音源插件",
        desc: "公共音源与插件参数",
        icon: "plug",
        color: "#69db7c",
        keywords: "网易云 QQ音乐 插件 搜索源 启用 导入 manifest 记住搜索平台 搜索音源 默认平台 记忆",
        tab: "source",
      },
      {
        id: "stream",
        title: "流媒体",
        desc: "Emby / Jellyfin / Navidrome",
        icon: "radio",
        color: "#4ab6d8",
        keywords: "自建 服务器 Emby Jellyfin Navidrome 地址 凭据",
        tab: "stream",
      },
      {
        id: "account",
        title: "账号与登录",
        desc: "扫码登录、Cookie 与歌单导入",
        icon: "user",
        color: "#a78bfa",
        keywords: "登录 扫码 Cookie 网易云 歌单 导入 账号",
        page: "account",
      },
    ],
  },
  {
    id: "about",
    title: "关于",
    items: [
      {
        id: "about",
        title: "更新与关于",
        desc: "版本检测、平台信息与说明",
        icon: "settings",
        color: "#94a3b8",
        keywords: "版本 更新 GitHub 平台 架构 缓存目录 关于",
        tab: "about",
      },
    ],
  },
];

const query = ref("");
const active = ref<Entry | null>(null);
const detailRoot = ref<HTMLElement | null>(null);

/** 搜索：按标题 / 副标题 / 关键词过滤，命中项合成一组展示。 */
const groups = computed<Group[]>(() => {
  const q = query.value.trim().toLowerCase();
  if (!q) return GROUPS;
  const hit = GROUPS.flatMap((g) => g.items).filter((e) =>
    `${e.title} ${e.desc} ${e.keywords}`.toLowerCase().includes(q),
  );
  return hit.length ? [{ id: "search", title: `搜索结果 · ${hit.length}`, items: hit }] : [];
});

async function scrollToFocus(e: Entry) {
  await nextTick();
  // 桌面组件挂载 + KeepAlive 复用后布局才稳定，留一帧再定位
  setTimeout(() => {
    const root = detailRoot.value;
    if (!root) return;
    let el: HTMLElement | null = null;
    if (e.focusSelector) el = root.querySelector<HTMLElement>(e.focusSelector);
    else if (e.focusText) {
      el =
        Array.from(root.querySelectorAll<HTMLElement>(".row")).find((n) =>
          n.textContent?.includes(e.focusText!),
        ) ?? null;
    }
    el?.scrollIntoView({ behavior: "smooth", block: "center" });
  }, 80);
}

function open(e: Entry) {
  if (e.page) {
    emit("open-sub", e.page);
    return;
  }
  active.value = e;
  if (e.tab) ui.settingsTab = e.tab;
  if (e.focusSelector || e.focusText) void scrollToFocus(e);
}

function closeDetail() {
  active.value = null;
  query.value = "";
}

/** 系统返回手势：详情 → 分类由本组件消费；分类首页交回 MobileShell。 */
function consumeBack(): boolean {
  if (!active.value) return false;
  closeDetail();
  return true;
}

defineExpose({ back: consumeBack });

// ---------------------------------------------------------------------------
// 通知与锁屏面板
// ---------------------------------------------------------------------------

const PREF_KEY = "leonmusic.mediaPrefs";
const notifyOn = ref(true);
const lockOn = ref(true);
const media = ref<MediaStatus | null>(null);

async function refreshMedia() {
  if (!store.isAndroid) return;
  try {
    media.value = await settingsApi.mediaStatus();
  } catch {
    media.value = null;
  }
}

/** 开关落在 localStorage，同时下发一份到原生 SharedPreferences。
 *  后者是权威值：WebView 被回收、前台服务重建后依然读得到。 */
async function pushPrefs() {
  if (!store.isAndroid) return;
  try {
    await settingsApi.setMediaOptions(notifyOn.value, lockOn.value);
  } catch (e) {
    ui.toast(errorText(e), "error");
  }
  await refreshMedia();
}

function togglePref(which: "notification" | "lockscreen") {
  if (which === "notification") notifyOn.value = !notifyOn.value;
  else lockOn.value = !lockOn.value;
  localStorage.setItem(
    PREF_KEY,
    JSON.stringify({ notification: notifyOn.value, lockscreen: lockOn.value }),
  );
  void pushPrefs();
}

async function openNotificationSettings() {
  try {
    await settingsApi.openNotificationSettings();
    ui.toast("在系统设置里开启通知后返回即可", "info");
  } catch (e) {
    ui.toast(errorText(e), "error");
  }
}

async function requestBackground() {
  try {
    await settingsApi.requestBackground(true);
    ui.toast("已请求后台运行权限", "success");
  } catch (e) {
    ui.toast(errorText(e), "info");
  }
}

onMounted(() => {
  try {
    const raw = localStorage.getItem(PREF_KEY);
    if (raw) {
      const p = JSON.parse(raw) as { notification?: boolean; lockscreen?: boolean };
      notifyOn.value = p.notification !== false;
      lockOn.value = p.lockscreen !== false;
    }
  } catch {
    /* 坏掉的缓存按默认全开处理 */
  }
  void pushPrefs();
  void refreshMedia();
});

/** 从系统通知设置返回时，权限可能已经变了 */
onActivated(() => void refreshMedia());

const statusRows = computed(() => [
  { label: "通知权限", ok: media.value?.notificationGranted ?? false, hint: "Android 13+ 需要授予才能显示通知" },
  { label: "媒体会话", ok: media.value?.sessionActive ?? false, hint: "锁屏媒体卡片与蓝牙线控依赖它" },
  { label: "前台服务", ok: media.value?.foreground ?? false, hint: "播放开始后自动进入前台" },
]);
</script>

<template>
  <div class="ms">
    <!-- ================= 分类首页 ================= -->
    <template v-if="!active">
      <div class="ms-top">
        <button class="ms-back" aria-label="返回" @click="emit('back')">
          <Icon name="chevron" :size="22" class="ms-back-ico" />
        </button>
        <h1 class="ms-title">设置</h1>
      </div>

      <label class="ms-search">
        <Icon name="search" :size="18" />
        <input v-model="query" type="search" placeholder="搜索设置" spellcheck="false" />
        <button v-if="query" class="ms-clear" aria-label="清空" @click="query = ''">
          <Icon name="close" :size="15" />
        </button>
      </label>

      <div v-if="!groups.length" class="ms-empty">
        <Icon name="search" :size="30" />
        <span>没有找到相关设置</span>
      </div>

      <section v-for="g in groups" :key="g.id" class="ms-group">
        <h2 class="ms-group-title">{{ g.title }}</h2>
        <div class="ms-card">
          <button
            v-for="e in g.items"
            :key="e.id"
            class="ms-item"
            :style="{ '--c': e.color }"
            @click="open(e)"
          >
            <span class="ms-ico"><Icon :name="e.icon" :size="21" /></span>
            <span class="ms-text">
              <b>{{ e.title }}</b>
              <small>{{ e.desc }}</small>
            </span>
            <Icon name="chevron" :size="17" class="ms-arrow" />
          </button>
        </div>
      </section>
    </template>

    <!-- ================= 详情页 ================= -->
    <div v-else ref="detailRoot" class="ms-detail">
      <div class="ms-detail-head">
        <button class="ms-back" aria-label="返回" @click="closeDetail">
          <Icon name="chevron" :size="22" class="ms-back-ico" />
        </button>
        <b>{{ active.title }}</b>
      </div>

      <!-- 安卓专属：通知栏 + 锁屏播放状态 -->
      <template v-if="active.panel === 'media'">
        <div v-if="!store.isAndroid" class="ms-note">
          通知栏与锁屏媒体控制仅在安卓端可用。
        </div>
        <template v-else>
          <div class="ms-media-card">
            <div class="ms-media-head">
              <span class="ms-media-ico"><Icon name="music" :size="20" /></span>
              <div>
                <b>正在播放</b>
                <small>播放时同步到通知栏与锁屏</small>
              </div>
            </div>
            <div class="ms-status">
              <div v-for="r in statusRows" :key="r.label" class="ms-status-row">
                <i class="ms-dot" :class="{ ok: r.ok }" />
                <span class="ms-status-label">{{ r.label }}</span>
                <span class="ms-status-val" :class="{ warn: !r.ok }">
                  {{ r.ok ? "已就绪" : "未就绪" }}
                </span>
                <small>{{ r.hint }}</small>
              </div>
            </div>
          </div>

          <div class="ms-card ms-switches">
            <button class="ms-item ms-toggle" @click="togglePref('notification')">
              <span class="ms-ico" style="--c: #7c5cff"><Icon name="play" :size="20" /></span>
              <span class="ms-text">
                <b>通知栏播放控制</b>
                <small>显示封面、歌名与上一首 / 暂停 / 下一首</small>
              </span>
              <span class="ms-switch" :class="{ on: notifyOn }"><i /></span>
            </button>
            <button class="ms-item ms-toggle" @click="togglePref('lockscreen')">
              <span class="ms-ico" style="--c: #3dd6d0"><Icon name="lock" :size="20" /></span>
              <span class="ms-text">
                <b>锁屏播放状态</b>
                <small>锁屏媒体卡片、进度拖动与蓝牙线控</small>
              </span>
              <span class="ms-switch" :class="{ on: lockOn }"><i /></span>
            </button>
          </div>

          <div class="ms-card ms-actions">
            <button class="ms-item" @click="openNotificationSettings">
              <span class="ms-ico" style="--c: #ffa94d"><Icon name="settings" :size="20" /></span>
              <span class="ms-text">
                <b>通知权限设置</b>
                <small>被拒绝后在这里重新开启，播放不受影响</small>
              </span>
              <Icon name="chevron" :size="17" class="ms-arrow" />
            </button>
            <button class="ms-item" @click="requestBackground">
              <span class="ms-ico" style="--c: #69db7c"><Icon name="refresh" :size="20" /></span>
              <span class="ms-text">
                <b>后台运行白名单</b>
                <small>关闭电池优化，避免锁屏后被系统杀掉</small>
              </span>
              <Icon name="chevron" :size="17" class="ms-arrow" />
            </button>
          </div>

          <p class="ms-note">
            播放由 Rust 音频引擎驱动，通知栏与锁屏只是「遥控器」：切后台、锁屏甚至
            WebView 被回收都不会中断音乐。锁屏封面、进度条与切歌按钮由系统读取
            媒体会话渲染，蓝牙耳机与线控按键同样走这条通道。
          </p>
        </template>
      </template>

      <!-- 其余分类直接复用桌面设置页的内容 -->
      <SettingsView v-else />
    </div>
  </div>
</template>

<style scoped>
.ms {
  padding: 2px 4px 8px;
}
.ms-top,
.ms-detail-head {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 4px 0 14px;
}
.ms-back {
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  flex: none;
  border-radius: 50%;
  color: var(--text);
}
.ms-back:active {
  background: var(--hover);
}
.ms-back-ico {
  transform: rotate(180deg);
}
.ms-title {
  margin: 0;
  font-size: 24px;
  font-weight: 700;
  letter-spacing: -0.02em;
}
.ms-detail-head b {
  font-size: 17px;
  font-weight: 600;
}

/* 搜索框：胶囊、无边框，与系统设置一致 */
.ms-search {
  display: flex;
  align-items: center;
  gap: 10px;
  height: 46px;
  padding: 0 16px;
  margin-bottom: 6px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--panel) 82%, var(--bg));
  border: 0.5px solid var(--border);
  color: var(--text-3);
}
.ms-search:focus-within {
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
}
.ms-search input {
  flex: 1;
  min-width: 0;
  border: 0;
  outline: 0;
  background: transparent;
  color: var(--text);
  font-size: 14px;
}
.ms-search input::-webkit-search-cancel-button {
  display: none;
}
.ms-clear {
  display: grid;
  place-items: center;
  color: var(--text-3);
}

.ms-group-title {
  margin: 22px 6px 10px;
  font-size: 13px;
  font-weight: 600;
  color: var(--text-2);
}
.ms-card {
  background: var(--panel);
  border: 0.5px solid var(--border);
  border-radius: 20px;
  box-shadow: var(--m-shadow);
  overflow: hidden;
}
.ms-item {
  position: relative;
  display: flex;
  align-items: center;
  gap: 14px;
  width: 100%;
  min-height: 72px;
  padding: 12px 16px;
  text-align: left;
  color: var(--text-2);
}
.ms-item + .ms-item::before {
  content: "";
  position: absolute;
  top: 0;
  left: 66px;
  right: 16px;
  height: 0.5px;
  background: var(--border);
}
.ms-item:active {
  background: var(--hover);
}
/* 彩色圆角方图标：底色融进当前主题，图标用分组色，明暗主题都不刺眼 */
.ms-ico {
  display: grid;
  place-items: center;
  width: 42px;
  height: 42px;
  flex: none;
  border-radius: 13px;
  background: color-mix(in srgb, var(--c) 16%, var(--panel));
  color: var(--c);
}
.ms-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
  min-width: 0;
}
.ms-text b {
  font-size: 14.5px;
  font-weight: 600;
  color: var(--text);
}
.ms-text small {
  font-size: 11.5px;
  line-height: 1.45;
  color: var(--text-3);
}
.ms-arrow {
  flex: none;
  color: var(--text-3);
}
.ms-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 64px 20px;
  color: var(--text-3);
  font-size: 13px;
}

/* ---------- 通知与锁屏面板 ---------- */
.ms-media-card {
  padding: 16px;
  margin-bottom: 12px;
  border-radius: 20px;
  background: color-mix(in srgb, var(--accent) 10%, var(--panel));
  border: 0.5px solid color-mix(in srgb, var(--accent) 24%, transparent);
}
.ms-media-head {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 14px;
}
.ms-media-ico {
  display: grid;
  place-items: center;
  width: 42px;
  height: 42px;
  flex: none;
  border-radius: 14px;
  background: var(--accent);
  color: #fff;
}
.ms-media-head div {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.ms-media-head b {
  font-size: 14px;
}
.ms-media-head small {
  font-size: 11.5px;
  color: var(--text-3);
}
.ms-status {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.ms-status-row {
  display: grid;
  grid-template-columns: 8px 1fr auto;
  align-items: center;
  gap: 8px 10px;
  font-size: 12.5px;
}
.ms-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #ffa94d;
}
.ms-dot.ok {
  background: #3fb98a;
}
.ms-status-label {
  color: var(--text);
}
.ms-status-val {
  font-size: 11.5px;
  color: var(--text-2);
}
.ms-status-val.warn {
  color: #e8933a;
}
.ms-status-row small {
  grid-column: 2 / -1;
  font-size: 10.5px;
  color: var(--text-3);
}
.ms-switches,
.ms-actions {
  margin-bottom: 12px;
}
.ms-toggle {
  min-height: 76px;
}
.ms-switch {
  position: relative;
  width: 42px;
  height: 24px;
  flex: none;
  border-radius: 999px;
  background: var(--border-strong);
  transition: background 0.2s;
}
.ms-switch i {
  position: absolute;
  left: 3px;
  top: 3px;
  width: 18px;
  height: 18px;
  border-radius: 50%;
  background: #fff;
  transition: transform 0.2s var(--m-ease);
}
.ms-switch.on {
  background: var(--accent);
}
.ms-switch.on i {
  transform: translateX(18px);
}
.ms-note {
  margin: 14px 6px 0;
  font-size: 11.5px;
  line-height: 1.75;
  color: var(--text-3);
}

/* ---------- 复用桌面 Settings.vue 时抹平桌面专属布局 ---------- */
.ms-detail :deep(.tabs) {
  display: none;
}
.ms-detail :deep(.page) {
  height: auto;
  min-height: 0;
}
.ms-detail :deep(.layout) {
  display: block;
}
.ms-detail :deep(.content) {
  min-height: 0;
  padding-right: 0;
  overflow: visible;
}
.ms-detail :deep(.row),
.ms-detail :deep(.quality-card),
.ms-detail :deep(.plugin),
.ms-detail :deep(.upd-card) {
  border-radius: 16px;
}
</style>
