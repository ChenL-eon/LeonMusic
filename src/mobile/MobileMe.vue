<script setup lang="ts">
/** 移动端「我的」：2×2 彩色快捷卡 + 可折叠歌单 + 账号/设置入口。
 *  账号卡显示真实登录账号（取第一个带 login 能力的插件）：
 *  已登录 = 真实头像 + 昵称；未登录 = 占位图标 + 「未登录」，点击进账号页。 */
import { computed, onActivated, onMounted, ref } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import { plugins as pluginApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";
import type { Page } from "@/stores/ui";
import type { PluginAccount } from "@/types";

const emit = defineEmits<{
  (e: "navigate", page: "home" | "search" | "favorites" | "me"): void;
  (e: "open-sub", page: Page): void;
  (e: "open-list", kind: "recent" | "cached"): void;
}>();

const library = useLibraryStore();
const store = useSettingsStore();
const ui = useUiStore();

const BLANK_ACCOUNT: PluginAccount = {
  loggedIn: false,
  userId: 0,
  nickname: "",
  avatarUrl: "",
  vipType: 0,
  vipLabel: "",
};
const account = ref<PluginAccount | null>(null);

/** 与账号页同口径：插件没配 cookie 必然未登录，不浪费一次 IPC */
async function loadAccount() {
  const p = store.pluginList.find((x) => x.manifest.capabilities.includes("login"));
  if (!p) {
    account.value = null;
    return;
  }
  if (!p.config.cookie) {
    account.value = { ...BLANK_ACCOUNT };
    return;
  }
  try {
    account.value = await pluginApi.loginInfo(p.manifest.id);
  } catch {
    account.value = { ...BLANK_ACCOUNT };
  }
}

onMounted(() => {
  if (!store.loaded) void store.init().then(loadAccount);
  else void loadAccount();
});
/* 从账号页登录 / 退出后切回「我的」要刷新头像昵称 */
onActivated(() => void loadAccount());

const expanded = ref(true);
const creating = ref(false);
const newName = ref("");

const playlists = computed(() => library.playlists.filter((p) => !p.system));
const cachedCount = computed(
  () => library.tracks.filter((t) => t.source.kind === "plugin").length,
);

interface Shortcut {
  id: string;
  label: string;
  hint: string;
  icon: string;
  color: string;
  page?: "favorites" | "library";
  kind?: "recent" | "cached";
}

const shortcuts = computed<Shortcut[]>(() => [
  { id: "favorites", label: "收藏", hint: `${library.favorites.length} 首`, icon: "heart", color: "#f26d8b", page: "favorites" },
  { id: "recent", label: "最近", hint: "播放历史", icon: "clock", color: "#4b8cf7", kind: "recent" },
  { id: "local", label: "本地", hint: `${library.tracks.length} 首`, icon: "library", color: "#3fb98a", page: "library" },
  { id: "cached", label: "已缓存", hint: `${cachedCount.value} 首`, icon: "download", color: "#4ab6d8", kind: "cached" },
]);

function onShortcut(s: Shortcut) {
  if (s.kind) emit("open-list", s.kind);
  else if (s.page === "favorites") emit("navigate", "favorites");
  else emit("open-sub", s.page ?? "library");
}

async function createPlaylist() {
  const name = newName.value.trim();
  if (!name) return;
  await library.createPlaylist(name);
  newName.value = "";
  creating.value = false;
  expanded.value = true;
  ui.toast(`已创建「${name}」`, "success");
}

function openPlaylist(id: string) {
  void library.openPlaylist(id);
  emit("open-sub", "playlist");
}
</script>

<template>
  <div class="m-page me">
    <div class="m-top">
      <h1 class="m-title">我的</h1>
      <div class="top-actions">
        <button class="m-icon" aria-label="新建歌单" @click="creating = !creating">
          <Icon name="plus" :size="22" />
        </button>
        <button class="m-icon" aria-label="设置" @click="emit('open-sub', 'settings')">
          <Icon name="settings" :size="21" />
        </button>
      </div>
    </div>

    <button class="profile m-card" @click="emit('open-sub', 'account')">
      <img
        v-if="account?.loggedIn && account.avatarUrl"
        class="avatar-img"
        :src="account.avatarUrl"
        alt=""
        referrerpolicy="no-referrer"
      />
      <span v-else class="avatar"><Icon name="music" :size="24" /></span>
      <span class="who">
        <b>{{ account?.loggedIn ? account.nickname || "已登录" : "未登录" }}</b>
        <small><i class="dot" /> 本地已入库 {{ library.tracks.length }} 首</small>
      </span>
      <Icon name="chevron" :size="18" />
    </button>

    <div v-if="creating" class="create m-card">
      <input
        v-model="newName"
        placeholder="新歌单名称"
        autofocus
        @keyup.enter="createPlaylist"
      />
      <button class="m-chip on" @click="createPlaylist">创建</button>
    </div>

    <div class="shortcuts">
      <button v-for="s in shortcuts" :key="s.id" class="shortcut m-card" @click="onShortcut(s)">
        <span class="badge" :style="{ background: s.color }">
          <Icon :name="s.icon" :size="22" />
        </span>
        <span class="sc-text">
          <b>{{ s.label }}</b>
          <small>{{ s.hint }}</small>
        </span>
      </button>
    </div>

    <div class="m-section pl-head">
      <button class="fold" @click="expanded = !expanded">
        <Icon name="chevron-down" :size="17" :class="{ up: !expanded }" />
        <span>我创建的歌单 <b>{{ playlists.length }}</b></span>
      </button>
      <div class="inline">
        <button class="m-icon" aria-label="排序" @click="ui.toast('歌单按创建时间排列')">
          <Icon name="queue" :size="19" />
        </button>
        <button class="m-icon" aria-label="新建歌单" @click="creating = !creating">
          <Icon name="plus" :size="20" />
        </button>
      </div>
    </div>

    <div v-if="expanded" class="pl-list m-card">
      <button v-for="p in playlists" :key="p.id" class="pl" @click="openPlaylist(p.id)">
        <CoverArt :cover-path="p.coverPath" :cover-url="p.coverUrl" size="48px" radius="12px" class="pl-art" />
        <span class="pl-text">
          <b>{{ p.name }}</b>
          <small>{{ p.trackCount }} 首</small>
        </span>
        <Icon name="chevron" :size="17" />
      </button>
      <div v-if="!playlists.length" class="pl-empty">还没有自建歌单，点右上角 + 新建一个</div>
    </div>

    <div class="links m-card">
      <button @click="emit('open-sub', 'account')">
        <Icon name="user" :size="20" />
        <span>账号与登录</span>
        <Icon name="chevron" :size="17" />
      </button>
      <button @click="emit('open-sub', 'settings')">
        <Icon name="settings" :size="20" />
        <span>设置</span>
        <Icon name="chevron" :size="17" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.me {
  padding-left: 20px;
  padding-right: 20px;
}
.top-actions,
.inline {
  display: flex;
  align-items: center;
  gap: 4px;
}
.profile {
  display: flex;
  align-items: center;
  gap: 14px;
  width: 100%;
  padding: 14px 16px;
  text-align: left;
  color: var(--text-2);
}
.avatar {
  display: grid;
  place-items: center;
  width: 54px;
  height: 54px;
  border-radius: 50%;
  background: color-mix(in srgb, var(--accent) 16%, var(--panel));
  color: var(--accent);
  flex: none;
}
.avatar-img {
  width: 54px;
  height: 54px;
  border-radius: 50%;
  object-fit: cover;
  flex: none;
}
.who {
  display: flex;
  flex-direction: column;
  gap: 5px;
  flex: 1;
  min-width: 0;
}
.who b {
  font-size: 16px;
  color: var(--text);
}
.who small {
  font-size: 12px;
  color: var(--text-3);
  display: flex;
  align-items: center;
  gap: 6px;
}
.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #3fb98a;
}
.create {
  display: flex;
  gap: 8px;
  padding: 10px;
  margin-top: 12px;
}
.create input {
  flex: 1;
  min-width: 0;
  height: 38px;
  padding: 0 12px;
  border: 0.5px solid var(--border);
  border-radius: 999px;
  background: var(--panel-2);
  color: var(--text);
  font-size: 16px !important;
  outline: none;
}
.shortcuts {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
  margin-top: 16px;
}
.shortcut {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 14px;
  text-align: left;
}
.shortcut:active {
  transform: scale(0.98);
}
.badge {
  display: grid;
  place-items: center;
  width: 42px;
  height: 42px;
  border-radius: var(--m-r-sm);
  color: #fff;
  flex: none;
}
.sc-text {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}
.sc-text b {
  font-size: 14px;
}
.sc-text small {
  font-size: 11px;
  color: var(--text-3);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pl-head {
  margin-top: 24px;
}
.fold {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 15px;
  font-weight: 600;
}
.fold b {
  color: var(--text-3);
  font-size: 12px;
  margin-left: 2px;
}
.up {
  transform: rotate(180deg);
}
.pl-list {
  padding: 6px 8px;
  overflow: hidden;
}
.pl {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  padding: 8px;
  text-align: left;
  color: var(--text-3);
}
.pl-art :deep(.cover) {
  box-shadow: none;
}
.pl-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
  min-width: 0;
}
.pl-text b {
  font-size: 14px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pl-text small {
  font-size: 11.5px;
}
.pl-empty {
  padding: 18px 10px;
  font-size: 12px;
  color: var(--text-3);
  text-align: center;
}
.links {
  margin-top: 18px;
  overflow: hidden;
}
.links button {
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  height: 52px;
  padding: 0 16px;
  color: var(--text-2);
  font-size: 14px;
  text-align: left;
}
.links button + button {
  border-top: 0.5px solid var(--border);
}
.links button span {
  flex: 1;
}
</style>
