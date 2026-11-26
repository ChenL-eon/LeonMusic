// TrackSource 跨语言字段契约回归测试。
//
// 背景（LM-01）：Rust 的 `TrackSource::Plugin { plugin_id }` 曾直接序列化成
// `{"kind":"plugin","plugin_id":"..."}`，而 TypeScript 声明与所有调用点都读
// `source.pluginId`，导致云端红心 / 打卡 / 相似歌曲拿不到插件 ID。
//
// 本测试与 Rust 单测 `track_source_plugin_matches_shared_frontend_fixture`
// 共用同一份 fixture：Rust 断言序列化输出等于它，这里断言按前端方式能读出值。
// 任一侧把字段改回 snake_case 都会让两边之一失败。
//
// 运行：node --test scripts/tests/track-source-contract.test.mjs
// 或：npm run test:contract

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");

const fixturePath = join(here, "fixtures", "track-source-plugin.json");
const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));

test("Rust fixture 输出 camelCase 的 pluginId", () => {
  assert.equal(fixture.kind, "plugin");
  assert.equal(fixture.pluginId, "netease");
  // 旧字段名不得出现在 Rust 输出里，否则前端读到 undefined
  assert.equal(
    Object.prototype.hasOwnProperty.call(fixture, "plugin_id"),
    false,
    `Rust 输出仍带 plugin_id: ${JSON.stringify(fixture)}`
  );
});

test("前端按 source.pluginId 能取到插件 ID", () => {
  // 模拟真实消费：库/播放器里都是 `t.source.pluginId`
  const source = fixture;
  if (source.kind !== "plugin") throw new Error("fixture kind 不是 plugin");
  assert.equal(source.pluginId, "netease");
  // 与 undefined 比较能抓出字段名不一致时调用方拿到的真实值
  assert.notEqual(source.pluginId, undefined);
});

test("TypeScript 声明仍是 pluginId（防止类型定义漂移）", () => {
  const dts = readFileSync(join(root, "src", "types", "index.ts"), "utf8");
  assert.match(
    dts,
    /kind:\s*"plugin";\s*pluginId:\s*string/,
    "src/types/index.ts 的 TrackSource plugin 变体应声明 pluginId: string"
  );
  assert.doesNotMatch(
    dts,
    /kind:\s*"plugin";\s*plugin_id:/,
    "src/types/index.ts 不应声明 plugin_id"
  );
});

test("前端源码不读取 snake_case 的 plugin_id", () => {
  const files = [
    "src/stores/library.ts",
    "src/stores/player.ts",
    "src/components/VipBadge.vue",
    "src/components/TrackList.vue",
    "src/views/NowPlaying.vue",
  ];
  for (const rel of files) {
    const text = readFileSync(join(root, rel), "utf8");
    assert.equal(
      /\.plugin_id\b/.test(text),
      false,
      `${rel} 仍在读 source.plugin_id`
    );
  }
});

test("旧持久化数据 plugin_id 仍可被 Rust 接受（fixture 记录兼容样本）", () => {
  // 这条不断言 Rust（由 Rust 单测覆盖），只固定住需要兼容的旧格式样本，
  // 防止有人「清理」时误以为旧数据不存在。
  const legacy = JSON.parse('{"kind":"plugin","plugin_id":"netease"}');
  assert.equal(legacy.plugin_id, "netease");
  assert.equal(legacy.pluginId, undefined);
});
