// 歌词异步请求竞争回归测试（LM-06）。
//
// 直接加载真实的 `src/stores/player.ts`（TypeScript 转译 + 真 Pinia），
// 只把 Tauri IPC 换成可控 Promise，用来复现「A 的响应晚于切歌返回」。
//
// 运行：node --test scripts/tests/player-lyrics-race.test.mjs

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

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/** 每次 `player.lyrics()` 调用压入一个可控 deferred */
let localCalls = [];
/** 每次 `player.fetchRemoteLyrics()` 调用压入一个可控 deferred */
let remoteCalls = [];

const api = {
  EVT: {},
  on: async () => () => {},
  player: {
    lyrics: () => {
      const d = deferred();
      localCalls.push(d);
      return d.promise;
    },
    fetchRemoteLyrics: () => {
      const d = deferred();
      remoteCalls.push(d);
      return d.promise;
    },
    state: async () => null,
    setVolume: async () => {},
  },
  settings: { get: async () => ({}) },
  plugins: { loginInfo: async () => ({ loggedIn: false }) },
};

const cache = new Map();
function load(relative, mocks = {}) {
  const file = path.join(root, relative);
  if (cache.has(file)) return cache.get(file);
  const code = ts.transpileModule(fs.readFileSync(file, "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const module = { exports: {} };
  cache.set(file, module.exports);
  const req = (name) => {
    if (name in mocks) return mocks[name];
    if (name === "@/api/ipc") return api;
    if (name === "@/stores/ui") return { useUiStore: () => ({ toast() {} }) };
    if (name === "@/types") return load("src/types/index.ts");
    // 相对路径的 @/ 别名（types 内部若再引用）
    if (name.startsWith("@/")) return load(`src/${name.slice(2)}.ts`);
    return projectRequire(name);
  };
  new Function("require", "module", "exports", code)(req, module, module.exports);
  cache.set(file, module.exports);
  return module.exports;
}

function freshStore() {
  setActivePinia(createPinia());
  // 缓存里已有模块，但 defineStore 的实例由 pinia 决定，换 pinia 就是新实例
  const { usePlayerStore } = load("src/stores/player.ts");
  localCalls = [];
  remoteCalls = [];
  return usePlayerStore();
}

function track(id, kind = "local") {
  return {
    id,
    source: kind === "plugin" ? { kind: "plugin", pluginId: "netease" } : { kind },
    title: id,
    artist: "",
    album: "",
    durationMs: 1000,
    addedAt: 0,
  };
}

function lyricsWith(text) {
  return { kind: "plain", offsetMs: 0, lines: [{ startMs: 0, text }] };
}

function firstText(store) {
  return store.lyrics.lines[0]?.text ?? null;
}

test("首次加载：A 的响应晚于切到 B 时不能覆盖 B", async () => {
  const store = freshStore();
  store.snap.track = track("A");
  const pending = store.loadLyrics();
  assert.equal(localCalls.length, 1);

  // 请求还没回来就切到 B
  store.snap.track = track("B");
  localCalls[0].resolve(lyricsWith("LOCAL_A"));
  await pending;

  assert.equal(store.snap.track.id, "B");
  assert.notEqual(firstText(store), "LOCAL_A", "过期的本地歌词不能提交");
});

test("手动获取：A 的响应晚于切到 B 时不能覆盖 B", async () => {
  const store = freshStore();
  store.snap.track = track("A");
  const pending = store.fetchRemoteLyrics();
  assert.equal(remoteCalls.length, 1);

  store.snap.track = track("B");
  remoteCalls[0].resolve(lyricsWith("LYRICS_A"));
  await pending;

  assert.equal(store.snap.track.id, "B");
  assert.notEqual(firstText(store), "LYRICS_A");
});

test("A→B→A：第一次 A 的响应不能覆盖第二次 A 的结果", async () => {
  const store = freshStore();

  store.snap.track = track("A");
  const p1 = store.loadLyrics(); // 第 0 个请求（旧 A）
  store.snap.track = track("B");
  const p2 = store.loadLyrics(); // 第 1 个请求（B）
  store.snap.track = track("A");
  const p3 = store.loadLyrics(); // 第 2 个请求（新 A，最新代次）
  assert.equal(localCalls.length, 3);

  // 最新的 A 先返回并提交
  localCalls[2].resolve(lyricsWith("A_NEW"));
  await p3;
  assert.equal(firstText(store), "A_NEW");

  // 旧 A 晚到：曲目 ID 与当前相同（都是 A），只有代次能拦住它
  localCalls[0].resolve(lyricsWith("A_OLD"));
  await p1;
  assert.equal(firstText(store), "A_NEW", "旧代次响应不能覆盖较新结果");

  // B 的响应也晚到，同样不能提交
  localCalls[1].resolve(lyricsWith("B_LATE"));
  await p2;
  assert.equal(firstText(store), "A_NEW");
});

test("过期请求抛错不清空较新的歌词状态", async () => {
  const store = freshStore();

  store.snap.track = track("A");
  const p1 = store.loadLyrics(); // 旧 A
  store.snap.track = track("B");
  const p2 = store.loadLyrics(); // B（最新）
  localCalls[1].resolve(lyricsWith("B_OK"));
  await p2;
  assert.equal(firstText(store), "B_OK");

  localCalls[0].reject(new Error("network down"));
  await p1; // 内部 catch，不应抛出
  assert.equal(firstText(store), "B_OK", "过期请求的异常不能清空较新歌词");
});

test("连续手动请求：只有最后一次的结果生效", async () => {
  const store = freshStore();
  store.snap.track = track("A");

  const p1 = store.fetchRemoteLyrics();
  const p2 = store.fetchRemoteLyrics();
  assert.equal(remoteCalls.length, 2);

  remoteCalls[1].resolve(lyricsWith("SECOND"));
  await p2;
  remoteCalls[0].resolve(lyricsWith("FIRST"));
  await p1;

  assert.equal(firstText(store), "SECOND");
});
