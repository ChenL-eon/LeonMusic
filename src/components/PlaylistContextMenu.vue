<script setup lang="ts">
/**
 * 歌单右键菜单：重命名 / 删除 / 更新歌单。
 *
 * 由 utils/playlistMenu.ts 的全局单例驱动，App.vue 挂一份即可。
 * 系统歌单不允许改名/删除/更新（openPlaylistMenu 已过滤）。
 */
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";

import Icon from "@/components/Icon.vue";
import { useLibraryStore } from "@/stores/library";
import { useUiStore } from "@/stores/ui";
import { closePlaylistMenu, playlistMenu } from "@/utils/playlistMenu";

const library = useLibraryStore();
const ui = useUiStore();

const menu = computed(() => playlistMenu);
const playlist = computed(() => playlistMenu.playlist);
/** 云端歌单（有 remoteKey）才显示「更新歌单」 */
const canUpdate = computed(() => !!playlist.value?.remoteKey);

const newName = ref("");
const updating = ref(false);
const nameInput = ref<HTMLInputElement | null>(null);

watch(
  () => playlistMenu.mode,
  async (mode) => {
    if (mode === "rename" && playlist.value) {
      newName.value = playlist.value.name;
      await nextTick();
      nameInput.value?.focus();
      nameInput.value?.select();
    }
  },
);

function dismiss() {
  if (updating.value) return; // 更新中不允许关，避免用户以为取消了
  closePlaylistMenu();
}

function onKeydown(e: KeyboardEvent) {
  if (playlistMenu.mode === "none") return;
  if (e.key === "Escape") {
    e.preventDefault();
    dismiss();
  }
}

function onGlobalPointer(e: PointerEvent) {
  if (playlistMenu.mode === "none") return;
  const target = e.target as HTMLElement;
  // 点在菜单/弹窗内部不动；点在外面关闭
  if (target.closest(".pl-menu, .pl-dialog")) return;
  dismiss();
}

onMounted(() => {
  window.addEventListener("keydown", onKeydown);
  window.addEventListener("pointerdown", onGlobalPointer, true);
});
onUnmounted(() => {
  window.removeEventListener("keydown", onKeydown);
  window.removeEventListener("pointerdown", onGlobalPointer, true);
});

async function doRename() {
  const name = newName.value.trim();
  const p = playlist.value;
  if (!name || !p) return;
  if (name === p.name) {
    closePlaylistMenu();
    return;
  }
  try {
    await library.renamePlaylist(p.id, name);
    ui.toast("歌单已重命名", "success");
    closePlaylistMenu();
  } catch (e) {
    ui.toast(`重命名失败：${e}`, "error");
  }
}

async function doDelete() {
  const p = playlist.value;
  if (!p) return;
  try {
    await library.deletePlaylist(p.id);
    ui.toast(`已删除「${p.name}」`, "success");
    closePlaylistMenu();
  } catch (e) {
    ui.toast(`删除失败：${e}`, "error");
  }
}

/** 真实更新：重新拉云端曲目并差量同步本地歌单 */
async function doUpdate() {
  const p = playlist.value;
  if (!p || updating.value) return;
  updating.value = true;
  try {
    const result = await library.updatePlaylist(p.id);
    if (!result) {
      ui.toast("这个歌单没有云端来源，无法更新", "info");
      return;
    }
    const parts = [`已更新「${p.name}」共 ${result.total} 首`];
    if (result.added) parts.push(`新增 ${result.added}`);
    if (result.removed) parts.push(`移除 ${result.removed}`);
    if (!result.added && !result.removed) parts.push("内容无变化");
    ui.toast(parts.join(" · "), "success");
    closePlaylistMenu();
  } catch (e) {
    ui.toast(`更新失败：${e}`, "error");
  } finally {
    updating.value = false;
  }
}
</script>

<template>
  <Teleport to="body">
    <!-- 右键菜单 -->
    <div
      v-if="menu.mode === 'menu' && playlist"
      class="pl-menu"
      :style="{ left: menu.x + 'px', top: menu.y + 'px' }"
    >
      <button class="pl-item" @click="playlistMenu.mode = 'rename'">
        <Icon name="caption" :size="17" /> <span>重命名</span>
      </button>
      <button v-if="canUpdate" class="pl-item" :disabled="updating" @click="doUpdate">
        <Icon name="refresh" :size="17" /> <span>{{ updating ? "更新中…" : "更新歌单" }}</span>
      </button>
      <div class="sep" />
      <button class="pl-item danger" @click="playlistMenu.mode = 'delete'">
        <Icon name="trash" :size="17" /> <span>删除歌单</span>
      </button>
    </div>

    <!-- 重命名弹窗 -->
    <div v-if="menu.mode === 'rename' && playlist" class="pl-backdrop" @click.self="dismiss">
      <div class="pl-dialog">
        <div class="dlg-title">重命名歌单</div>
        <input
          ref="nameInput"
          v-model="newName"
          class="dlg-input"
          maxlength="60"
          placeholder="输入新名称"
          @keyup.enter="doRename"
        />
        <div class="dlg-actions">
          <button class="btn ghost" @click="dismiss">取消</button>
          <button class="btn primary" :disabled="!newName.trim()" @click="doRename">确定</button>
        </div>
      </div>
    </div>

    <!-- 删除确认 -->
    <div v-if="menu.mode === 'delete' && playlist" class="pl-backdrop" @click.self="dismiss">
      <div class="pl-dialog">
        <div class="dlg-title">删除歌单</div>
        <p class="dlg-desc">
          确定删除「{{ playlist.name }}」吗？<br />
          <small>只会删除歌单本身，歌曲仍保留在曲库里。</small>
        </p>
        <div class="dlg-actions">
          <button class="btn ghost" @click="dismiss">取消</button>
          <button class="btn danger-btn" @click="doDelete">删除</button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.pl-menu {
  position: fixed;
  z-index: 120;
  width: 186px;
  padding: 6px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--panel) 94%, transparent);
  border: 1px solid var(--border);
  backdrop-filter: blur(18px);
  -webkit-backdrop-filter: blur(18px);
  box-shadow: var(--shadow, 0 12px 32px rgba(0, 0, 0, 0.22));
  animation: pop 0.12s var(--ease, ease);
}
@keyframes pop {
  from {
    opacity: 0;
    transform: scale(0.96) translateY(-4px);
  }
}
.pl-item {
  width: 100%;
  height: 36px;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 10px;
  border-radius: 8px;
  font-size: 13px;
  color: var(--text);
  text-align: left;
  transition: background 0.12s;
}
.pl-item:hover:not(:disabled) {
  background: var(--panel-2);
}
.pl-item:disabled {
  opacity: 0.5;
}
.pl-item.danger {
  color: #ff6b6b;
}
.sep {
  height: 1px;
  margin: 5px 6px;
  background: var(--border);
}

.pl-backdrop {
  position: fixed;
  inset: 0;
  z-index: 130;
  display: grid;
  place-items: center;
  background: rgba(0, 0, 0, 0.38);
}
.pl-dialog {
  width: min(340px, calc(100vw - 40px));
  padding: 20px;
  border-radius: 16px;
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: var(--shadow, 0 18px 48px rgba(0, 0, 0, 0.3));
}
.dlg-title {
  font-size: 16px;
  font-weight: 700;
  margin-bottom: 14px;
}
.dlg-desc {
  font-size: 13px;
  line-height: 1.7;
  color: var(--text);
  margin: 0 0 18px;
}
.dlg-desc small {
  color: var(--text-3);
}
.dlg-input {
  width: 100%;
  height: 40px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text);
  padding: 0 12px;
  outline: none;
  font-size: 13px;
  margin-bottom: 18px;
}
.dlg-input:focus {
  border-color: var(--accent);
}
.dlg-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.btn {
  height: 36px;
  padding: 0 16px;
  border-radius: 9px;
  font-size: 13px;
  font-weight: 600;
}
.btn.ghost {
  background: var(--panel-2);
  color: var(--text-2);
}
.btn.primary {
  background: var(--accent);
  color: #fff;
}
.btn.primary:disabled {
  opacity: 0.5;
}
.btn.danger-btn {
  background: #ff6b6b;
  color: #fff;
}
</style>
