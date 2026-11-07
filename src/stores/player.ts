/**
 * 播放器 store。
 *
 * **这里不持有播放状态**——`snap` 是 Rust 推过来的只读镜像。
 * 唯一「本地计算」的是 `displayPosition`：为了让进度条和歌词在两次
 * 状态推送（120ms）之间平滑，用 rAF 做插值。它只用于渲染，
 * 任何逻辑判断（比如「这首播完了吗」）都必须看 `snap.status`。
 */

import { defineStore } from "pinia";
import { computed, onScopeDispose, ref, watch } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { EVT, on, player, plugins as pluginApi, settings as settingsApi } from "@/api/ipc";
import { useUiStore } from "@/stores/ui";
import {
  emptyLyrics,
  emptySnapshot,
  qualityLabel,
  qualityName,
  type Lyrics,
  type PlatformInfo,
  type PlayerSnapshot,
  type QueueSnapshot,
  type Track,
} from "@/types";

/** 打卡累计中的一首歌：换歌或播到曲尾时结算上报。 */
interface ScrobbleState {
  trackId: string;
  pluginId: string;
  listenedMs: number;
  durationMs: number;
  fired: boolean;
}

/**
 * VIP 试听提示按登录态分流。
 *
 * 网易云对 fee=1（VIP 版权）曲目的规则是权益绑账号，不绑「登录没登录」：
 * 会员账号给全曲；非会员账号无论是否登录，官方都明确下发试听片段
 * （2026-11 真机实测：freeTrialInfo {end:20}，standard 档 128k MP3）。
 * 所以提示不能再说「请登录」——用户明明已经登录了，只会更困惑。
 */
async function vipTrialHint(track: Track): Promise<string> {
  if (track.source.kind === "plugin") {
    try {
      const acct = await pluginApi.loginInfo(track.source.pluginId);
      if (acct.loggedIn && acct.vipType >= 11) {
        return "第三方解析未命中；当前账号有黑胶权益但官方仍只下发试听，可在「设置 → 播放」调整解析开关后重播，或稍后再试。";
      }
      if (acct.loggedIn && acct.vipType > 0) {
        return "当前账号是音乐包会员（非黑胶），多数 VIP 版权曲官方只给试听；升级黑胶后重播即可，或等「受限歌曲解析」恢复。";
      }
      if (acct.loggedIn) {
        return "当前登录的账号没有网易云会员权益，VIP 曲目官方只给会员账号全曲，登录了也还是试听；开通黑胶后重播即可，或等「受限歌曲解析」的第三方服务恢复。";
      }
    } catch {
      // 插件没实现 loginInfo / 网络不通：走通用文案
    }
  }
  return "第三方解析未命中：可在「账号」页用 Cookie 登录会员账号，或检查「设置 → 播放 → 受限歌曲解析」是否开启。";
}

export const usePlayerStore = defineStore("player", () => {
  const snap = ref<PlayerSnapshot>(emptySnapshot());
  const queue = ref<QueueSnapshot>({ items: [], current: null, repeat: "all", shuffle: false });
  const lyrics = ref<Lyrics>(emptyLyrics());
  const platform = ref<PlatformInfo | null>(null);
  const buffering = ref<{ trackId: string; downloaded: number; total: number } | null>(null);
  const ready = ref(false);
  /** 上一次已经提示过的错误文本，避免 ticker 每 120ms 重复弹同一条。 */
  let lastError = "";
  /** 已提示过「只有试听片段」的曲目 id。 */
  const trialWarned = new Set<string>();
  /** 已提示过「所选音质被降级」的曲目 id，避免 120ms ticker 重复弹。 */
  const qualityWarned = new Set<string>();
  /** 正在自动拉歌词的曲目 id，避免切歌抖动时对同一首重复请求。 */
  let lyricsAutoFetch = "";

  // ---- 听歌打卡（把真实播放同步到音源平台）----
  //
  // 「听歌时长 / 播放次数」只能由客户端上报播放日志产生。这里在快照
  // 推送里累计每首的播放时长，切歌或播到曲尾时交给插件（网易云走
  // clientlog eapi）。只报真实听过的（≥30 秒），失败静默、不打扰听歌。
  let scrobbleState: ScrobbleState | null = null;
  /** 打卡被拒（未登录/风控）后该插件本会话不再尝试，省掉无效请求。 */
  const scrobbleBlocked = new Set<string>();

  async function flushScrobble() {
    const s = scrobbleState;
    if (!s || s.fired) return;
    s.fired = true;
    const secs = Math.floor(Math.min(s.listenedMs, s.durationMs || s.listenedMs) / 1000);
    // 网易云对太短的播放本来就不计，也没必要为 10 秒的试听发一条日志
    if (secs < 30 || scrobbleBlocked.has(s.pluginId)) return;
    try {
      const r = await pluginApi.scrobble(s.pluginId, s.trackId, secs * 1000);
      if (!r?.ok) scrobbleBlocked.add(s.pluginId);
    } catch {
      scrobbleBlocked.add(s.pluginId);
    }
  }

  function trackScrobble(p: PlayerSnapshot) {
    const t = p.track;
    if (!t || t.source.kind !== "plugin") {
      void flushScrobble();
      scrobbleState = null;
      return;
    }
    const pos = p.positionMs ?? 0;
    if (
      !scrobbleState ||
      scrobbleState.trackId !== t.id ||
      // 同一首重新开播（单曲循环/手动重播）：先结算上一轮再重新计数
      pos + 2000 < scrobbleState.listenedMs
    ) {
      void flushScrobble();
      scrobbleState = {
        trackId: t.id,
        pluginId: t.source.pluginId,
        listenedMs: pos,
        durationMs: p.durationMs ?? 0,
        fired: false,
      };
      return;
    }
    scrobbleState.listenedMs = Math.max(scrobbleState.listenedMs, pos);
    if (p.durationMs > 0) scrobbleState.durationMs = p.durationMs;
    // 播到接近曲尾就先打卡，不等切歌——最后 1.5 秒内快照可能已经换了歌
    if (!scrobbleState.fired && scrobbleState.durationMs > 0 && pos >= scrobbleState.durationMs - 1500) {
      void flushScrobble();
    }
  }

  // ---- 渲染插值 ----
  const displayPosition = ref(0);
  let basePos = 0;
  let baseAt = performance.now();
  let raf = 0;

  watch(
    () => snap.value.positionMs,
    (p) => {
      // 推送有网络/调度延迟，补偿一点让插值更贴近真实播放位置
      basePos = p + 40;
      baseAt = performance.now();
    },
  );

  function loop() {
    const playing = snap.value.status === "playing";
    const elapsed = playing ? performance.now() - baseAt : 0;
    const next = playing ? basePos + elapsed : basePos;
    displayPosition.value = Math.max(0, Math.min(next, snap.value.durationMs || next));
    raf = requestAnimationFrame(loop);
  }

  // ---- 派生 ----
  const track = computed(() => snap.value.track);
  const isPlaying = computed(() => snap.value.status === "playing");
  const canPlay = computed(() => !!snap.value.track);
  const progress = computed(() =>
    snap.value.durationMs > 0
      ? Math.min(1, displayPosition.value / snap.value.durationMs)
      : 0,
  );
  const currentLineIndex = computed(() => {
    const pos = displayPosition.value;
    const lines = lyrics.value.lines;
    let lo = 0;
    let hi = lines.length - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (lines[mid].startMs <= pos) {
        found = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    return found;
  });

  // ---- 事件订阅 ----
  const unlisteners: UnlistenFn[] = [];

  async function init() {
    if (ready.value) return;
    const ui = useUiStore();

    const [s, q, p] = await Promise.all([
      player.snapshot(),
      player.queue(),
      settingsApi.platformInfo().catch(() => null),
    ]);
    snap.value = s;
    queue.value = q;
    platform.value = p;
    basePos = s.positionMs;

    unlisteners.push(
      await on<PlayerSnapshot>(EVT.state, async (payload) => {
        // 用 revision 丢弃乱序（理论上不会，但网络/IPC 不可假设）
        if (payload.revision < snap.value.revision && payload.track?.id === snap.value.track?.id) {
          return;
        }
        const prevId = snap.value.track?.id;
        snap.value = payload;
        if (payload.track?.id && payload.track.id !== prevId) {
          void loadLyrics();
        }
        // Rust 侧只在「出错」时写 error，且 ticker 每 120ms 推一次快照。
        // 不去重的话同一条错误会连续弹几十次；清空后要重置，
        // 否则下一次同样的错误（比如重试同一首）就不会再提示。
        const err = (payload.error ?? "").trim();
        if (err && err !== lastError) {
          lastError = err;
          ui.toast(err, "error");
        } else if (!err) {
          lastError = "";
        }

        // 官方 eapi 常「code 200 但静默忽略 level」：选了无损却只给 320k MP3。
        // 播放条常驻显示真实规格，这里只在该曲首次播放时提示一次。
        const q = payload.quality;
        if (q?.downgraded && payload.status === "playing" && payload.track?.id) {
          const id = payload.track.id;
          if (!qualityWarned.has(id)) {
            qualityWarned.add(id);
            const want = qualityName(q.requested);
            ui.toast(
              `《${payload.track.title}》${want ? `所选「${want}」` : "所选音质"}未生效，实际是 ${qualityLabel(q)}。播放条显示的是真实音质。`,
              "info",
            );
          }
        }

        // 在线音源的 VIP / 版权曲，官方只给试听片段（非会员账号）。
        // 不提示的话用户只会觉得「播到一半自己停了」。
        // 只在真在播、且解码出来的时长明显短于元数据时才判定，
        // 并限定 plugin 来源——本地文件的 ID3 时长偶有偏差，不该被误报。
        const want = payload.track?.durationMs ?? 0;
        const got = payload.durationMs ?? 0;
        if (
          payload.status === "playing" &&
          payload.track?.source.kind === "plugin" &&
          want > 0 &&
          got > 0 &&
          got < want * 0.6 &&
          want - got > 15_000
        ) {
          const id = payload.track.id;
          if (!trialWarned.has(id)) {
            trialWarned.add(id);
            const hint = await vipTrialHint(payload.track);
            ui.toast(
              `《${payload.track.title}》只有 ${Math.round(got / 1000)} 秒试听（VIP 版权曲）。${hint}`,
              "info",
            );
          }
        }

        // 听歌打卡：切歌/曲尾时由 flushScrobble 上报（见上方说明）
        trackScrobble(payload);
      }),
      await on<QueueSnapshot>(EVT.queue, (payload) => {
        queue.value = payload;
      }),
      await on<{ trackId: string }>(EVT.buffering, (payload) => {
        buffering.value = { trackId: payload.trackId, downloaded: 0, total: 0 };
      }),
      await on<{ trackId: string; downloaded: number; total: number }>(
        EVT.bufferProgress,
        (payload) => {
          buffering.value = payload;
        },
      ),
    );

    void loadLyrics();
    raf = requestAnimationFrame(loop);
    ready.value = true;

    onScopeDispose(() => {
      cancelAnimationFrame(raf);
      unlisteners.forEach((u) => u());
    });
  }

  async function loadLyrics() {
    // 先清空：否则切歌瞬间还会显示上一首的歌词
    lyrics.value = emptyLyrics();
    const wantId = snap.value.track?.id;
    try {
      lyrics.value = await player.lyrics();
    } catch {
      return;
    }
    // 等待期间又切歌了，这一份结果已经过期
    if (!wantId || snap.value.track?.id !== wantId) return;

    // 在线曲目第一次播放时本地没有缓存，得主动去插件拉一次。
    // 不做这一步的话，用户得自己发现「正在播放」页那个小按钮——
    // 事实上没人会去找，于是「这个播放器没歌词」。
    const t = snap.value.track;
    if (lyrics.value.lines.length || t?.source.kind !== "plugin") return;
    if (lyricsAutoFetch === t.id) return;
    lyricsAutoFetch = t.id;
    try {
      const remote = await player.fetchRemoteLyrics();
      if (snap.value.track?.id === wantId) lyrics.value = remote;
    } catch {
      /* 拉不到就保持空状态，界面上还有「获取在线歌词」按钮可重试 */
    } finally {
      lyricsAutoFetch = "";
    }
  }

  // ---- 操作 ----
  async function toggle() {
    await player.toggle();
  }
  async function next() {
    await player.next();
  }
  async function prev() {
    await player.prev();
  }
  async function seek(ms: number) {
    basePos = ms;
    baseAt = performance.now();
    displayPosition.value = ms;
    await player.seek(Math.round(ms));
  }
  async function seekRatio(r: number) {
    if (!snap.value.durationMs) return;
    await seek(Math.max(0, Math.min(1, r)) * snap.value.durationMs);
  }
  async function setVolume(v: number) {
    await player.setVolume(v);
  }
  async function setSpeed(v: number) {
    await player.setSpeed(v);
  }
  async function setPitch(v: number) {
    await player.setPitch(v);
  }
  async function setGain(v: number) {
    await player.setGain(v);
  }
  async function setRepeat(mode: PlayerSnapshot["repeat"]) {
    await player.setRepeat(mode);
  }
  async function setShuffle(on: boolean) {
    await player.setShuffle(on);
  }
  async function playTrack(t: Track) {
    await player.playTrack(t.id);
  }
  async function playList(tracks: Track[], index = 0) {
    if (!tracks.length) return;
    await player.playQueue(
      tracks.map((t) => t.id),
      index,
    );
  }
  /**
   * 用曲目对象整体替换队列。
   *
   * 搜索结果的在线曲目还没入库，`playList` 走的 id 查询会直接查不到，
   * 所以这条路必须把对象本身传下去。
   */
  async function playTracks(tracks: Track[], index = 0) {
    if (!tracks.length) return;
    await player.playTracks(tracks, index);
  }
  async function addToQueue(tracks: Track[], playNext = false) {
    const localIds = tracks.filter((t) => t.source.kind === "local").map((t) => t.id);
    const remote = tracks.filter((t) => t.source.kind !== "local");
    if (localIds.length) await player.addToQueue(localIds, playNext);
    if (remote.length) await player.addTracksToQueue(remote, playNext);
  }
  async function fetchRemoteLyrics() {
    try {
      lyrics.value = await player.fetchRemoteLyrics();
    } catch {
      /* 没有就算了 */
    }
  }

  return {
    snap,
    queue,
    lyrics,
    platform,
    buffering,
    ready,
    displayPosition,
    track,
    isPlaying,
    canPlay,
    progress,
    currentLineIndex,
    init,
    loadLyrics,
    toggle,
    next,
    prev,
    seek,
    seekRatio,
    setVolume,
    setSpeed,
    setPitch,
    setGain,
    setRepeat,
    setShuffle,
    playTrack,
    playList,
    playTracks,
    addToQueue,
    fetchRemoteLyrics,
  };
});
