/**
 * 已导入 QQ 音乐歌单的封面补全。
 *
 * netease 插件 2.23.0 之前把外部歌单曲目的 coverUrl 留空（当时误以为宿主
 * 会退回歌单封面，实际没有曲目级回退），这些曲目已经落在本地库里，
 * 重新导入又会产生一份同名歌单。
 *
 * 这里在启动后做一次性回填：按 remoteKey 找出 QQ 歌单，重新拉一次云端详情，
 * 只 upsert 有封面的曲目（ON CONFLICT 会覆盖 cover_url），不新建歌单、
 * 不改歌单成员，也不碰任何账号。成功才写标记；离线 / 接口波动时不写，
 * 下次启动自动重试。账号页另有手动按钮可立即触发。
 */

import { library as libraryApi, plugins as pluginApi } from "@/api/ipc";
import { useLibraryStore } from "@/stores/library";

const MARK_KEY = "leonmusic.qqCoverBackfill.v1";
const QQ_REMOTE_PREFIX = "netease:qq:";

export interface BackfillResult {
  /** 实际处理的歌单数 */
  playlists: number;
  /** 写回封面的曲目数 */
  tracks: number;
  /** 之前已经补过，本次直接跳过 */
  skipped: boolean;
}

/** 自动补全与手动按钮可能撞在一起，共用同一个在途请求 */
let running: Promise<BackfillResult> | null = null;

export function backfillQqCovers(force = false): Promise<BackfillResult> {
  if (running) return running;
  running = doBackfill(force).finally(() => {
    running = null;
  });
  return running;
}

async function doBackfill(force: boolean): Promise<BackfillResult> {
  if (!force) {
    try {
      if (localStorage.getItem(MARK_KEY)) {
        return { playlists: 0, tracks: 0, skipped: true };
      }
    } catch {
      // 隐私模式 / 禁用存储：照常补一次，只是不记忆
    }
  }

  const store = useLibraryStore();
  const targets = store.playlists.filter((p) =>
    (p.remoteKey ?? "").startsWith(QQ_REMOTE_PREFIX),
  );
  if (!targets.length) return { playlists: 0, tracks: 0, skipped: false };

  let tracks = 0;
  let playlists = 0;
  let failed = 0;
  for (const p of targets) {
    // remoteKey = "netease:qq:<disstid>"，插件侧需要的是 "qq:<disstid>"
    const cloudId = p.remoteKey!.slice("netease:".length);
    try {
      const detail = await pluginApi.cloudPlaylist("netease", cloudId);
      const covered = detail.tracks.filter((t) => !!t.coverUrl);
      if (!covered.length) continue;
      await libraryApi.upsertTracks(covered);
      tracks += covered.length;
      playlists += 1;
    } catch {
      // 单个歌单失效 / 接口频控不能挡住其它歌单；整体失败就不写标记
      failed += 1;
    }
  }

  if (tracks > 0 && failed === 0) {
    try {
      localStorage.setItem(MARK_KEY, new Date().toISOString());
    } catch {
      // 存不下就每次启动重补一次，数据本身不会出错
    }
  }
  if (tracks > 0) {
    // upsert 会发 library_changed，但事件可能早于页面订阅；主动刷一次更稳
    try {
      await store.refresh();
    } catch {
      // 刷新失败不影响已写入数据库的封面
    }
  }
  if (failed > 0 && tracks === 0) {
    throw new Error("QQ 音乐歌单封面补全失败，请检查网络后重试");
  }
  return { playlists, tracks, skipped: false };
}
