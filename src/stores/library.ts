/**
 * 媒体库 store：本地曲目、收藏、歌单、最近播放。
 *
 * 扫描是 Rust 侧的后台任务，这里只负责发起与展示进度。
 */

import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { EVT, library, on, playlist as playlistApi } from "@/api/ipc";
import type { Playlist, Track } from "@/types";

export const useLibraryStore = defineStore("library", () => {
  const tracks = ref<Track[]>([]);
  const favorites = ref<Track[]>([]);
  const recent = ref<Track[]>([]);
  const playlists = ref<Playlist[]>([]);
  const activePlaylist = ref<Playlist | null>(null);
  const playlistTracks = ref<Track[]>([]);
  const loading = ref(false);
  const scanning = ref(false);
  const scanProgress = ref({ done: 0, total: 0 });
  const query = ref("");

  const filtered = computed(() => {
    const q = query.value.trim().toLowerCase();
    if (!q) return tracks.value;
    return tracks.value.filter(
      (t) =>
        t.title.toLowerCase().includes(q) ||
        t.artist.toLowerCase().includes(q) ||
        t.album.toLowerCase().includes(q),
    );
  });

  const favoriteIds = computed(() => new Set(favorites.value.map((t) => t.id)));

  const unlisteners: UnlistenFn[] = [];
  let initialized = false;

  async function init() {
    if (initialized) return;
    initialized = true;
    await refresh();
    unlisteners.push(
      await on(EVT.libraryChanged, () => void refresh()),
      await on<{ done: number; total: number }>(EVT.scanProgress, (p) => {
        scanProgress.value = p;
      }),
    );
  }

  async function refresh() {
    loading.value = true;
    try {
      const [list, favs, recents, lists] = await Promise.all([
        library.list(undefined, "local"),
        library.favorites(),
        library.recent(80),
        playlistApi.list(),
      ]);
      tracks.value = list;
      favorites.value = favs;
      recent.value = recents;
      playlists.value = lists;
      if (activePlaylist.value) {
        await openPlaylist(activePlaylist.value.id);
      }
    } finally {
      loading.value = false;
    }
  }

  async function scan(paths: string[]) {
    if (!paths.length) return null;
    scanning.value = true;
    scanProgress.value = { done: 0, total: 0 };
    try {
      const summary = await library.scan(paths);
      await refresh();
      return summary;
    } finally {
      scanning.value = false;
    }
  }

  async function toggleFavorite(trackId: string): Promise<boolean> {
    const nowFavorite = await library.toggleFavorite(trackId);
    favorites.value = await library.favorites();
    return nowFavorite;
  }

  async function openPlaylist(id: string) {
    const p = playlists.value.find((x) => x.id === id) ?? null;
    activePlaylist.value = p;
    playlistTracks.value = await playlistApi.tracks(id);
  }

  async function createPlaylist(name: string) {
    const p = await playlistApi.create(name);
    playlists.value = await playlistApi.list();
    return p;
  }

  async function deletePlaylist(id: string) {
    await playlistApi.remove(id);
    if (activePlaylist.value?.id === id) {
      activePlaylist.value = null;
      playlistTracks.value = [];
    }
    playlists.value = await playlistApi.list();
  }

  async function addToPlaylist(id: string, trackIds: string[]) {
    await playlistApi.add(id, trackIds);
    playlists.value = await playlistApi.list();
    if (activePlaylist.value?.id === id) {
      playlistTracks.value = await playlistApi.tracks(id);
    }
  }

  return {
    tracks,
    favorites,
    recent,
    playlists,
    activePlaylist,
    playlistTracks,
    loading,
    scanning,
    scanProgress,
    query,
    filtered,
    favoriteIds,
    init,
    refresh,
    scan,
    toggleFavorite,
    openPlaylist,
    createPlaylist,
    deletePlaylist,
    addToPlaylist,
  };
});
