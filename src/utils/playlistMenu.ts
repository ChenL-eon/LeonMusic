/**
 * 歌单右键菜单的全局单例状态。
 *
 * 侧栏歌单项、歌单网格卡片、移动端歌单列表都从同一处打开菜单，
 * 菜单本体在 App.vue 里只挂一份（Teleport 到 body），避免每处重复实现。
 */
import { reactive } from "vue";
import type { Playlist } from "@/types";

export type PlaylistMenuMode = "none" | "menu" | "rename" | "delete";

export const playlistMenu = reactive({
  mode: "none" as PlaylistMenuMode,
  x: 0,
  y: 0,
  playlist: null as Playlist | null,
});

/** 打开右键菜单。系统歌单（我喜欢的音乐）不支持改名/删除/更新，直接忽略。 */
export function openPlaylistMenu(e: MouseEvent, p: Playlist) {
  if (p.system) return;
  e.preventDefault();
  e.stopPropagation();
  playlistMenu.playlist = p;
  // 预留菜单宽度约 190、高度约 132，贴边时向内收，保证完整可见
  playlistMenu.x = Math.max(8, Math.min(e.clientX, window.innerWidth - 198));
  playlistMenu.y = Math.max(8, Math.min(e.clientY, window.innerHeight - 140));
  playlistMenu.mode = "menu";
}

export function closePlaylistMenu() {
  playlistMenu.mode = "none";
}
