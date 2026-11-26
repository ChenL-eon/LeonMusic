// 收藏与云端红心同步回归测试（LM-07）。
//
// 直接加载真实的 `src/stores/library.ts`（TypeScript 转译 + 真 Pinia），只把
// Tauri IPC 换成一个可控的内存后端：`favorites` 表 + `tracks` 表 + 插件红心接口。
//
// 覆盖审查报告的复现场景：本地 `tracks` 只含本地曲目时，在线曲目的「取消红心」
// 曾经只改云端、本地收藏仍然留着。
//
// 运行：node --test scripts/tests/library-favorite-sync.test.mjs

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "..", "..");
const projectRequire = createRequire(path.join(root, "package.json"));
const ts = projectRequire("typescript");
const { createPinia, setActivePinia } = projectRequire("pinia");

// ---------------------------------------------------------------------------
// 可控的假后端
// ---------------------------------------------------------------------------

/** 本地 `favorites` 表（收藏列表就是它的顺序） */
let favRows = [];
/** 本地 `tracks` 表：id → Track（决定能不能写收藏，外键约束） */
let trackRows = new Map();
/** 云端红心状态：id → boolean */
let cloudLiked = new Map();
/** `setFavorite` 的调用记录 */
let setCalls = [];
/** 旧的盲 toggle 调用记录：新代码不应该再走这条路 */
let toggleCalls = [];
/** `likeTrack` 的调用记录 */
let likeCalls = [];
/** 下一次 `likeTrack` 的行为（测试里逐个设置） */
let likeImpl = async () => ({ ok: true, code: 200, liked: true, playlistId: "1" });

const libraryApi = {
  setFavorite: async (trackId, favorite) => {
    setCalls.push([trackId, favorite]);
    const exists = favRows.some((t) => t.id === trackId);
    if (favorite && !exists) {
      const row = trackRows.get(trackId);
      if (!row) throw new Error("FOREIGN KEY constraint failed");
      favRows = [...favRows, row];
    } else if (!favorite && exists) {
      favRows = favRows.filter((t) => t.id !== trackId);
    }
    return favorite;
  },
  toggleFavorite: async (trackId) => {
    toggleCalls.push(trackId);
    const exists = favRows.some((t) => t.id === trackId);
    if (exists) {
      favRows = favRows.filter((t) => t.id !== trackId);
      return false;
    }
    const row = trackRows.get(trackId);
    if (!row) throw new Error("FOREIGN KEY constraint failed");
    favRows = [...favRows, row];
    return true;
  },
  favorites: async () => favRows,
  track: async (id) => trackRows.get(id) ?? null,
  list: async () => [],
  cachedTracks: async () => [],
  recent: async () => [],
};

const api = {
  EVT: {},
  on: async () => () => {},
  library: libraryApi,
  playlist: { list: async () => [] },
  plugins: {
    likeTrack: async (pluginId, trackId, like) => {
      likeCalls.push({ pluginId, trackId, like });
      return likeImpl(pluginId, trackId, like);
    },
    isLiked: async (pluginId, trackId) => ({
      loggedIn: true,
      liked: cloudLiked.get(trackId) === true,
    }),
  },
};

const cache = new Map();
function load(relative) {
  const file = path.join(root, relative);
  if (cache.has(file)) return cache.get(file);
  const code = ts.transpileModule(fs.readFileSync(file, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  cache.set(file, module.exports);
  const req = (name) => {
    if (name === "@/api/ipc") return api;
    if (name === "@/types") return load("src/types/index.ts");
    if (name.startsWith("@/")) return load(`src/${name.slice(2)}.ts`);
    return projectRequire(name);
  };
  new Function("require", "module", "exports", code)(req, module, module.exports);
  cache.set(file, module.exports);
  return module.exports;
}

/** 与后端契约一致的插件曲目（LM-01：`source.pluginId`） */
function pluginTrack(id, pluginTrackId) {
  return {
    id,
    source: { kind: "plugin", pluginId: "netease" },
    pluginTrackId,
    title: id,
    artist: "",
    album: "",
    durationMs: 1000,
    addedAt: 0,
  };
}

function localTrack(id) {
  return { id, source: { kind: "local" }, title: id, artist: "", album: "", durationMs: 1, addedAt: 0 };
}

function freshStore() {
  setActivePinia(createPinia());
  favRows = [];
  trackRows = new Map();
  cloudLiked = new Map();
  setCalls = [];
  toggleCalls = [];
  likeCalls = [];
  likeImpl = async () => ({ ok: true, code: 200, liked: true, playlistId: "1" });
  const { useLibraryStore } = load("src/stores/library.ts");
  const store = useLibraryStore();
  // 插件曲目一律不挂在 `tracks`（本地曲库只列本地文件）——这正是缺陷前提
  store.tracks = [localTrack("local:1")];
  store.favorites = [];
  return store;
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

test("复现：tracks 只含本地曲目时，取消云端红心必须同时移除本地收藏", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:42", "42");
  trackRows.set(remote.id, remote);
  favRows = [remote];
  store.favorites = [remote];
  likeImpl = async () => ({ ok: true, code: 200, liked: false, playlistId: "1" });

  assert.equal(store.isFav(remote), true, "本地收藏里有它，红心应为实心");

  const on = await store.toggleFavoriteTrack(remote);

  assert.equal(on, false, "返回值必须是最终状态");
  assert.equal(likeCalls.length, 1, "一次点击只发一次云端请求");
  assert.equal(likeCalls[0].like, false, "方向是取消红心");
  assert.equal(store.isFav(remote), false, "云端取消后本地收藏不能还留着");
  assert.equal(store.favorites.length, 0);
});

test("按目标状态写：本地没收藏 + 云端已红心，取消后不能被盲 toggle 变成已收藏", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:43", "43");
  trackRows.set(remote.id, remote);
  cloudLiked.set("43", true);
  // 曲目在库里（旧版本 `tracks` 会包含插件曲目，这里显式还原那种数据形态）
  store.tracks = [remote];
  await store.refreshCloudLiked(remote);
  assert.equal(store.isFav(remote), true);
  likeImpl = async () => ({ ok: true, code: 200, liked: false, playlistId: "1" });

  const on = await store.toggleFavoriteTrack(remote);

  assert.equal(on, false);
  assert.equal(store.favorites.length, 0, "本地本来没有收藏，取消红心不能凭空加进去");
  assert.equal(toggleCalls.length, 0, "不能再用盲 toggle 写本地收藏");
  // 即便打了本地写入，方向也只能是 false（幂等），绝不能翻成已收藏
  for (const [, favorite] of setCalls) assert.equal(favorite, false);
});

test("红心显示集合与 isFav 一致：只被云端红心过的曲目也要显示实心", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:44", "44");
  cloudLiked.set("44", true);

  await store.refreshCloudLiked(remote);

  assert.equal(store.isFav(remote), true);
  assert.equal(store.favoriteIds.has(remote.id), true, "列表组件只看 favoriteIds，必须是并集");
});

test("未登录（301）：库内曲目退化为本地收藏，返回值与状态一致", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:45", "45");
  trackRows.set(remote.id, remote);
  likeImpl = async () => ({ ok: false, code: 301, liked: false, playlistId: "" });

  const on = await store.toggleFavoriteTrack(remote);

  assert.equal(on, true);
  assert.equal(store.isFav(remote), true);
  assert.deepEqual(
    store.favorites.map((t) => t.id),
    ["netease:45"],
  );
  assert.equal(likeCalls.length, 1);
});

test("未登录且曲目没入库：如实报错，不写本地也不谎报成功", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:46", "46");
  likeImpl = async () => ({ ok: false, code: 301, liked: false, playlistId: "" });

  await assert.rejects(() => store.toggleFavoriteTrack(remote), /未登录/);
  assert.equal(store.favorites.length, 0);
  assert.equal(setCalls.length, 0);
});

test("网络失败：库内曲目走本地回退，库外曲目抛错", async () => {
  const store = freshStore();
  const known = pluginTrack("netease:47", "47");
  const unknown = pluginTrack("netease:48", "48");
  trackRows.set(known.id, known);
  likeImpl = async () => {
    throw new Error("network down");
  };

  const on = await store.toggleFavoriteTrack(known);
  assert.equal(on, true);
  assert.equal(store.favorites.length, 1, "断网时本地收藏仍要能用");

  await assert.rejects(() => store.toggleFavoriteTrack(unknown), /红心同步失败/);
  assert.equal(store.favorites.length, 1, "失败的曲目不能被写进收藏");
});

test("云端明确拒绝：不写本地，也不冒充成功", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:49", "49");
  trackRows.set(remote.id, remote);
  likeImpl = async () => ({ ok: false, code: 400, liked: false, playlistId: "", message: "无版权" });

  await assert.rejects(() => store.toggleFavoriteTrack(remote), /无版权/);
  assert.equal(store.favorites.length, 0);
  assert.equal(store.isFav(remote), false);
});

test("列表页只传 id 时也能同步云端（收藏页 / 歌单页入口）", async () => {
  const store = freshStore();
  const remote = pluginTrack("netease:50", "50");
  trackRows.set(remote.id, remote);
  favRows = [remote];
  store.favorites = [remote];
  likeImpl = async () => ({ ok: true, code: 200, liked: false, playlistId: "1" });

  const on = await store.toggleFavorite(remote.id);

  assert.equal(on, false);
  assert.equal(likeCalls.length, 1, "只传 id 也必须走云端同步");
  assert.equal(store.favorites.length, 0);
});

test("本地曲目仍然只走本地收藏，不发云端请求", async () => {
  const store = freshStore();
  const local = localTrack("local:1");
  trackRows.set(local.id, local);

  const on = await store.toggleFavorite(local.id);

  assert.equal(on, true);
  assert.equal(likeCalls.length, 0);
  assert.deepEqual(
    store.favorites.map((t) => t.id),
    ["local:1"],
  );
});
