<script setup lang="ts">
/**
 * 账号与登录（独立页面）。
 *
 * 登录态按插件 id 管理：只要有插件声明 `login` 能力，这里就会出现一张卡片。
 * 设置页只负责插件参数，不再塞登录入口。
 * 登录后自动拉取云端歌单（网易云 2.6.0）：账号歌单列表 + 「我喜欢的音乐」，
 * 支持一键同步收藏、把云端歌单导入为本地歌单（导入记录持久存在 remoteKey 上，
 * 重复导入会先弹确认）；也支持直接粘贴网易云歌单分享链接导入（免登录）。
 */
import { computed, onActivated, onBeforeUnmount, onDeactivated, onMounted, reactive, ref } from "vue";

import CoverArt from "@/components/CoverArt.vue";
import Icon from "@/components/Icon.vue";
import { errorText, library as libraryApi, net as netApi, playlist as playlistApi, plugins as pluginApi } from "@/api/ipc";
import type { PluginAccount, PluginInfo, RemotePlaylistMeta } from "@/types";
import { neteasePlaylistIdFromUrl, parsePlaylistLink, qqPlaylistIdFromUrl } from "@/utils/playlistLink";
import { useLibraryStore } from "@/stores/library";
import { usePlayerStore } from "@/stores/player";
import { useSettingsStore } from "@/stores/settings";
import { useUiStore } from "@/stores/ui";

const store = useSettingsStore();
const ui = useUiStore();
const library = useLibraryStore();
const player = usePlayerStore();

const BLANK_ACCOUNT: PluginAccount = {
  loggedIn: false,
  userId: 0,
  nickname: "",
  avatarUrl: "",
  vipType: 0,
  vipLabel: "",
};

const accounts = reactive<Record<string, PluginAccount>>({});
const loginPlugins = computed(() =>
  store.pluginList.filter((p) => p.manifest.capabilities.includes("login")),
);

type LoginStatus =
  | "waiting"
  | "scanned"
  | "success"
  | "expired"
  | "error"
  | "blocked";
const login = reactive({
  id: "",
  name: "",
  key: "",
  session: "",
  qrSvg: "",
  status: "waiting" as LoginStatus,
  message: "",
});

/**
 * 二维码有效期约 5 分钟。反复开关弹层就重新建码会加剧网易云的
 * 「不要频繁操作」风控，所以 4 分钟内直接复用同一张码。
 */
const QR_TTL_MS = 4 * 60 * 1000;
const qrCache: Record<
  string,
  { key: string; qrSvg: string; session: string; at: number }
> = {};

/** 扫码被风控时唯一的可靠路径：真实浏览器登录一次，把 Cookie 粘进来 */
const cookieLogin = reactive({ id: "", name: "", text: "", busy: false });

/** 3 秒一轮；`polling` 防止上一轮还没回来就叠第二次 */
let pollTimer: number | null = null;
let polling = false;

function acct(id: string): PluginAccount {
  return accounts[id] ?? BLANK_ACCOUNT;
}

async function refreshAccount(id: string) {
  try {
    accounts[id] = await pluginApi.loginInfo(id);
  } catch {
    // 插件没实现或网络不通时按「未登录」处理，不要把页面卡住
    accounts[id] = { ...BLANK_ACCOUNT };
  }
  // 登录成功（扫码 / Cookie）都会走到这里：自动拉云端歌单
  await loadCloudIfLoggedIn(id);
}

/** 没 cookie 就不用发请求了：那一定是未登录 */
async function loadAccounts() {
  for (const p of loginPlugins.value) {
    if (!p.config.cookie) accounts[p.manifest.id] = { ...BLANK_ACCOUNT };
    else await refreshAccount(p.manifest.id);
  }
}

async function openLogin(p: PluginInfo) {
  login.id = p.manifest.id;
  login.name = p.manifest.name;
  login.key = "";
  login.session = "";
  login.qrSvg = "";
  login.status = "waiting";
  login.message = "正在生成二维码…";

  const cached = qrCache[p.manifest.id];
  if (cached && Date.now() - cached.at < QR_TTL_MS) {
    login.key = cached.key;
    login.session = cached.session;
    login.qrSvg = cached.qrSvg;
    login.message = "打开手机 App 扫一扫（二维码仍在有效期内）";
    startPolling();
    return;
  }

  try {
    const qr = await pluginApi.loginQr(p.manifest.id);
    if (login.id !== p.manifest.id) return; // 等请求回来时弹层已经关了
    login.key = qr.key;
    login.session = qr.session ?? "";
    login.qrSvg = qr.qrSvg;
    qrCache[p.manifest.id] = {
      key: qr.key,
      session: login.session,
      qrSvg: qr.qrSvg,
      at: Date.now(),
    };
    login.message = "打开手机 App 扫一扫";
    startPolling();
  } catch (e) {
    login.status = "error";
    login.message = errorText(e);
  }
}

async function refreshQr() {
  delete qrCache[login.id];
  const p = loginPlugins.value.find((x) => x.manifest.id === login.id);
  if (p) await openLogin(p);
}

function startPolling() {
  stopPolling();
  pollTimer = window.setInterval(poll, 3000);
}

function stopPolling() {
  if (pollTimer !== null) {
    window.clearInterval(pollTimer);
    pollTimer = null;
  }
}

async function poll() {
  if (polling || !login.id || !login.key) return;
  polling = true;
  const id = login.id;
  try {
    const st = await pluginApi.loginPoll(id, login.key, login.session);
    if (login.id !== id) return;
    if (st.state === "scanned") {
      login.status = "scanned";
      login.message = "已扫码，请在手机上确认";
    } else if (st.state === "blocked") {
      // 8821 / 设备环境异常：再轮下去也不会变，必须把原因摆出来
      login.status = "blocked";
      login.message = st.message || "网易云拒绝了本次登录，请改用 Cookie 登录";
      stopPolling();
      delete qrCache[id];
    } else if (st.state === "expired") {
      login.status = "expired";
      login.message = "二维码已过期";
      stopPolling();
      delete qrCache[id];
    } else if (st.state === "success") {
      login.status = "success";
      login.message = "登录成功";
      stopPolling();
      delete qrCache[id];
      await store.reloadPlugins();
      await refreshAccount(id);
      const nick = acct(id).nickname;
      ui.toast(nick ? `已登录 ${nick}` : "登录成功", "success");
      window.setTimeout(() => {
        if (login.id === id) closeLogin();
      }, 900);
    }
  } catch {
    // 网络抖一下就等下一轮，不打断扫码
  } finally {
    polling = false;
  }
}

function closeLogin() {
  stopPolling();
  login.id = "";
  login.key = "";
  login.session = "";
  login.qrSvg = "";
}

// ---------------------------------------------------------------------------
// Cookie 登录：绕开扫码风控的可靠路径
// ---------------------------------------------------------------------------

function openCookieLogin(p: PluginInfo) {
  closeLogin();
  cookieLogin.id = p.manifest.id;
  cookieLogin.name = p.manifest.name;
  cookieLogin.text = "";
  cookieLogin.busy = false;
}

function closeCookieLogin() {
  cookieLogin.id = "";
  cookieLogin.text = "";
}

function switchToCookie() {
  const p = loginPlugins.value.find((x) => x.manifest.id === login.id);
  if (p) openCookieLogin(p);
}

async function submitCookieLogin() {
  const id = cookieLogin.id;
  const raw = cookieLogin.text
    .trim()
    .replace(/^\s*cookie:\s*/i, "")
    .replace(/\s*[\r\n]+\s*/g, "; ")
    .trim();
  if (!/(?:^|;\s*)MUSIC_U=/i.test(raw)) {
    ui.toast(
      "Cookie 里没有 MUSIC_U：请从 F12 → Network → 请求头复制整段（document.cookie 读不到它）",
      "error",
    );
    return;
  }
  cookieLogin.busy = true;
  try {
    const p = store.pluginList.find((x) => x.manifest.id === id);
    if (!p) return;
    // 只覆盖 cookie 一项，其余插件参数原样保留
    await store.setPluginConfig(id, { ...(p.config ?? {}), cookie: raw });
    await refreshAccount(id);
    if (acct(id).loggedIn) {
      ui.toast(`已登录 ${acct(id).nickname || "网易云账号"}`, "success");
      closeCookieLogin();
    } else {
      ui.toast("Cookie 已保存，但账号验证没通过：确认含 MUSIC_U 且未被吊销", "error");
    }
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    cookieLogin.busy = false;
  }
}

// ---------------------------------------------------------------------------
// 验证码登录：手机号 + 短信验证码，直连网易官方（参考 api-enhanced 实现）
// ---------------------------------------------------------------------------

const captchaLogin = reactive({
  id: "",
  name: "",
  phone: "",
  captcha: "",
  /** 发码时的浏览器会话指纹，登录时原样带回（与扫码 session 同构） */
  session: "",
  sending: false,
  countdown: 0,
  busy: false,
  message: "",
  error: false,
});
let captchaTimer: number | undefined;

function openCaptchaLogin(p: PluginInfo) {
  closeLogin();
  closeCookieLogin();
  captchaLogin.id = p.manifest.id;
  captchaLogin.name = p.manifest.name;
  captchaLogin.phone = "";
  captchaLogin.captcha = "";
  captchaLogin.session = "";
  captchaLogin.sending = false;
  captchaLogin.countdown = 0;
  captchaLogin.busy = false;
  captchaLogin.message = "";
  captchaLogin.error = false;
}

function stopCaptchaTimer() {
  if (captchaTimer !== undefined) {
    window.clearInterval(captchaTimer);
    captchaTimer = undefined;
  }
}

function closeCaptchaLogin() {
  stopCaptchaTimer();
  captchaLogin.id = "";
}

function captchaPhoneOk(): boolean {
  return /^\d{11}$/.test(captchaLogin.phone.trim());
}

function captchaHint(msg: string, err: boolean) {
  captchaLogin.message = msg;
  captchaLogin.error = err;
}

async function sendCaptcha() {
  const id = captchaLogin.id;
  if (captchaLogin.sending || captchaLogin.countdown > 0) return;
  if (!captchaPhoneOk()) {
    captchaHint("请输入 11 位手机号", true);
    return;
  }
  captchaLogin.sending = true;
  try {
    const st = await pluginApi.captchaSend(id, captchaLogin.phone.trim(), captchaLogin.session);
    if (captchaLogin.id !== id) return; // 弹层已经关了
    if (st.state === "sent") {
      captchaLogin.session = st.session || captchaLogin.session;
      captchaHint("验证码已发送，10 分钟内有效", false);
      captchaLogin.countdown = 60;
      stopCaptchaTimer();
      captchaTimer = window.setInterval(() => {
        if (captchaLogin.countdown <= 1) {
          stopCaptchaTimer();
          captchaLogin.countdown = 0;
        } else {
          captchaLogin.countdown -= 1;
        }
      }, 1000);
    } else {
      captchaHint(st.message || "验证码发送失败，请稍后重试", true);
    }
  } catch (e) {
    if (captchaLogin.id === id) captchaHint(errorText(e), true);
  } finally {
    captchaLogin.sending = false;
  }
}

async function submitCaptchaLogin() {
  const id = captchaLogin.id;
  const phone = captchaLogin.phone.trim();
  const captcha = captchaLogin.captcha.trim();
  if (captchaLogin.busy || !captchaPhoneOk() || !captcha) return;
  captchaLogin.busy = true;
  try {
    const st = await pluginApi.loginCellphone(id, phone, captcha, captchaLogin.session);
    if (captchaLogin.id !== id) return;
    if (st.state === "success") {
      captchaHint("登录成功", false);
      await store.reloadPlugins();
      await refreshAccount(id);
      const nick = acct(id).nickname;
      ui.toast(nick ? `已登录 ${nick}` : "登录成功", "success");
      window.setTimeout(() => {
        if (captchaLogin.id === id) closeCaptchaLogin();
      }, 900);
    } else {
      captchaHint(st.message || "登录失败，请确认验证码后重试", true);
    }
  } catch (e) {
    if (captchaLogin.id === id) captchaHint(errorText(e), true);
  } finally {
    captchaLogin.busy = false;
  }
}

async function logout(id: string) {
  try {
    await pluginApi.logout(id);
    accounts[id] = { ...BLANK_ACCOUNT };
    resetCloud(id);
    await store.reloadPlugins();
    ui.toast("已退出登录", "success");
  } catch (e) {
    ui.toast(errorText(e), "error");
  }
}

// ---------------------------------------------------------------------------
// 云端歌单：登录后自动拉取账号歌单 + 「我喜欢的音乐」
// ---------------------------------------------------------------------------
// 登录动作（扫码成功 / Cookie 保存）都会走 refreshAccount，这里跟着拉取。
// 导入 = upsertTracks 入库 → 挂到收藏或新建本地歌单；完成后 library_changed
// 事件会让各 store 自动刷新，不需要手动维护列表。

interface CloudState {
  loggedIn: boolean;
  loading: boolean;
  error: string;
  liked: RemotePlaylistMeta | null;
  playlists: RemotePlaylistMeta[];
}

const cloud = reactive<Record<string, CloudState>>({});
const importing = ref("");
/** 已导入的云端歌单（`pluginId:cloudId`）：来自本地歌单的 remoteKey，重启后仍在 */
const importedKeys = computed(() => {
  const s = new Set<string>();
  for (const p of library.playlists) {
    if (p.remoteKey) s.add(p.remoteKey);
  }
  return s;
});
/** 重复导入确认弹层：点了已导入过的歌单才出现 */
const pendingImport = ref<{ pluginId: string; cloudId: string; label: string } | null>(null);

/** 歌单链接导入（网易云歌单详情接口免登录可用，未登录也能导入公开歌单） */
const linkImport = reactive({ text: "", busy: false });

function cloudOf(id: string): CloudState {
  return (
    cloud[id] ?? { loggedIn: false, loading: false, error: "", liked: null, playlists: [] }
  );
}

function resetCloud(id: string) {
  cloud[id] = { loggedIn: false, loading: false, error: "", liked: null, playlists: [] };
}

async function loadCloud(id: string) {
  cloud[id] = { loggedIn: false, loading: true, error: "", liked: null, playlists: [] };
  try {
    const data = await pluginApi.userPlaylists(id);
    cloud[id] = {
      loggedIn: data.loggedIn,
      loading: false,
      error: "",
      liked: data.liked,
      playlists: data.playlists,
    };
  } catch (e) {
    cloud[id] = { loggedIn: false, loading: false, error: errorText(e), liked: null, playlists: [] };
  }
}

/** 登录态确定后自动拉取（扫码成功与 Cookie 登录都走这里） */
async function loadCloudIfLoggedIn(id: string) {
  if (acct(id).loggedIn) await loadCloud(id);
}

/** 把云端曲目写进本地库，返回 Track 列表 */
async function fetchCloudTracks(pluginId: string, cloudId: string, hint: string) {
  const detail = await pluginApi.cloudPlaylist(pluginId, cloudId);
  if (!detail.tracks.length) {
    ui.toast(`${hint} 里没有拿到曲目（可能内容为空或接口分页限制）`, "info");
    return null;
  }
  await libraryApi.upsertTracks(detail.tracks);
  return detail;
}

/** 「我喜欢的音乐」→ 本地收藏：只补差集，已收藏的不动 */
async function syncLiked(pluginId: string, liked: RemotePlaylistMeta) {
  if (importing.value) return;
  importing.value = liked.id;
  try {
    const detail = await fetchCloudTracks(pluginId, liked.id, `「${liked.name}」`);
    if (!detail) return;
    const favIds = new Set(await libraryApi.favorites().then((fs) => fs.map((f) => f.id)));
    const fresh = detail.tracks.filter((t) => !favIds.has(t.id)).map((t) => t.id);
    if (!fresh.length) {
      ui.toast("云端「我喜欢的音乐」已经全部同步过了", "info");
      return;
    }
    // favorites 是系统歌单，playlist_add 即收藏（INSERT OR IGNORE，重复安全）
    await playlistApi.add("favorites", fresh);
    ui.toast(`已同步 ${fresh.length} 首到「我喜欢的音乐」`, "success");
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    importing.value = "";
  }
}

/** 云端歌单 → 本地歌单（新建，允许同名）。
 *  先弹确认的场景：本地已经有这个云端歌单的导入记录（remoteKey 命中）。 */
function requestImport(pluginId: string, meta: RemotePlaylistMeta) {
  if (importing.value) return;
  if (importedKeys.value.has(`${pluginId}:${meta.id}`)) {
    pendingImport.value = { pluginId, cloudId: meta.id, label: `「${meta.name}」` };
    return;
  }
  void importCore(pluginId, meta.id, `「${meta.name}」`);
}

function confirmImport() {
  const p = pendingImport.value;
  pendingImport.value = null;
  if (p) void importCore(p.pluginId, p.cloudId, p.label);
}

/** 真正的导入：拉详情 → 入库 → 新建本地歌单（记封面 + remoteKey）→ 挂曲目 */
async function importCore(pluginId: string, cloudId: string, label: string) {
  if (importing.value) return;
  importing.value = cloudId;
  try {
    const detail = await fetchCloudTracks(pluginId, cloudId, label);
    if (!detail) return;
    const local = await playlistApi.create(detail.name);
    // 云端封面记到本地歌单上（侧栏缩略图 /「我的」页有图可看）；
    // remoteKey 是「已导入过」的持久标记，重启后仍能识别
    if (detail.coverUrl) await playlistApi.setCover(local.id, detail.coverUrl);
    await playlistApi.setRemoteKey(local.id, `${pluginId}:${cloudId}`);
    await playlistApi.add(local.id, detail.tracks.map((t) => t.id));
    // setCover / setRemoteKey 不触发 libraryChanged，主动刷一次
    await library.refresh();
    ui.toast(`已导入「${detail.name}」${detail.tracks.length} 首到本地歌单`, "success");
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    importing.value = "";
  }
}

/** 通过分享链接导入歌单。
 *  能真正导入的：网易云（完整链 / 163cn.tv 短链 / 分享文本）与 QQ 音乐
 *  （网页链 / 分享页 / c6.y.qq.com 短链）。QQ 歌单由网易云插件的
 *  「外部歌单」分支拉取（`qq:<disstid>`），曲目只落本地库、不写账号，
 *  播放时按歌名 + 歌手走第三方解析链。
 *  酷狗 / 酷我 / 咪咕 / B 站只识别平台：接口签名与风控各不相同，尚未适配。 */
async function importFromLink(pluginId: string) {
  const raw = linkImport.text.trim();
  if (!raw || importing.value) return;
  let parsed = parsePlaylistLink(raw);
  if (!parsed) {
    ui.toast("没有识别出音乐平台链接：请粘贴 App「分享」里复制的歌单链接", "error");
    return;
  }
  linkImport.busy = true;
  try {
    // 短链：先跟随重定向拿到带 id 的完整地址，再解析一次
    if (parsed.short && parsed.id) {
      const finalUrl = await netApi.resolveRedirect(parsed.id);
      const cloudId =
        parsed.platform === "netease"
          ? neteasePlaylistIdFromUrl(finalUrl)
          : qqPlaylistIdFromUrl(finalUrl);
      if (!cloudId) {
        ui.toast("短链解析失败：跳转目标里没有歌单 ID（可能是单曲或非歌单页）", "error");
        return;
      }
      parsed = { ...parsed, short: false, id: cloudId };
    }
    if (!parsed.id) {
      ui.toast(
        parsed.platform === "netease"
          ? "链接里没找到歌单 ID：请从网易云 App 或网页端复制完整的歌单分享链接"
          : `链接里没找到${parsed.platformLabel}歌单 ID，请复制完整的歌单分享链接`,
        "error",
      );
      return;
    }

    // 平台 → 真正取歌单的插件 + 插件侧的歌单标识
    let targetPlugin = pluginId;
    let cloudId: string;
    let label: string;
    if (parsed.platform === "netease") {
      cloudId = parsed.id;
      label = `网易云歌单 ${parsed.id}`;
    } else if (parsed.platform === "qq") {
      // QQ 歌单由网易云插件的外部歌单分支接管（解析链也在那边）
      targetPlugin = "netease";
      cloudId = `qq:${parsed.id}`;
      label = `QQ 音乐歌单 ${parsed.id}`;
    } else {
      ui.toast(
        `识别到${parsed.platformLabel}歌单链接：目前支持网易云与 QQ 音乐导入，这个平台还没有接通`,
        "info",
      );
      return;
    }

    if (importedKeys.value.has(`${targetPlugin}:${cloudId}`)) {
      pendingImport.value = { pluginId: targetPlugin, cloudId, label };
      return;
    }
    await importCore(targetPlugin, cloudId, label);
    linkImport.text = "";
  } catch (e) {
    ui.toast(errorText(e), "error");
  } finally {
    linkImport.busy = false;
  }
}

/** 直接播放云端歌单（不落库） */
async function playCloudPlaylist(pluginId: string, meta: RemotePlaylistMeta) {
  try {
    const detail = await pluginApi.cloudPlaylist(pluginId, meta.id);
    if (!detail.tracks.length) {
      ui.toast("这个歌单还没有歌曲", "info");
      return;
    }
    await player.playTracks(detail.tracks, 0);
    ui.toast(`正在播放「${detail.name}」`);
  } catch (e) {
    ui.toast(errorText(e), "error");
  }
}

onMounted(async () => {
  if (!store.loaded) await store.init();
  await loadAccounts();
});

onBeforeUnmount(stopPolling);

/* 页面被 KeepAlive 缓存后切走：轮询先停、弹层收起（Teleport 的 DOM
   在 deactivated 后仍挂在 body 上，不收会一直盖在别的页面上面）；
   切回来若扫码弹层还开着，恢复轮询。 */
onDeactivated(() => {
  stopPolling();
  closeLogin();
  closeCookieLogin();
  closeCaptchaLogin();
});
onActivated(() => {
  if (
    login.id &&
    login.key &&
    (login.status === "waiting" || login.status === "scanned")
  ) {
    startPolling();
  }
});
</script>

<template>
  <div class="page">
    <header class="head">
      <div class="badge"><Icon name="user" :size="28" /></div>
      <div>
        <h1 class="page-title">账号与登录</h1>
        <p class="page-sub">登录在线音源账号，播放账号权限内的完整曲目</p>
      </div>
    </header>

    <div class="body scroll">
      <div v-if="!loginPlugins.length" class="empty">
        <div class="big">👤</div>
        <div>当前没有支持账号登录的音源插件</div>
      </div>

      <div v-for="p in loginPlugins" :key="p.manifest.id" class="account-card card">
        <div class="card-head">
          <div class="plugin-name">
            <Icon name="plug" :size="16" />
            <strong>{{ p.manifest.name }}</strong>
            <span class="ver">v{{ p.manifest.version }}</span>
          </div>
          <span class="state" :class="{ on: acct(p.manifest.id).loggedIn }">
            {{ acct(p.manifest.id).loggedIn ? "已登录" : "未登录" }}
          </span>
        </div>

        <div class="account-row">
          <img
            v-if="acct(p.manifest.id).avatarUrl"
            class="avatar"
            :src="acct(p.manifest.id).avatarUrl"
            alt=""
            referrerpolicy="no-referrer"
          />
          <div v-else class="avatar fallback"><Icon name="user" :size="24" /></div>
          <div class="who">
            <div class="name">
              {{ acct(p.manifest.id).loggedIn ? acct(p.manifest.id).nickname || "已登录" : "未登录" }}
            </div>
            <div class="sub">
              <template v-if="acct(p.manifest.id).loggedIn">
                <span v-if="acct(p.manifest.id).vipLabel" class="vip">
                  {{ acct(p.manifest.id).vipLabel }}
                </span>
                <span v-else>未开通会员 · VIP 曲目仍为试听</span>
              </template>
              <span v-else>扫码或用 Cookie 登录后，使用账号权限与音质</span>
            </div>
          </div>
          <div class="actions">
            <template v-if="acct(p.manifest.id).loggedIn">
              <button class="btn ghost small" @click="logout(p.manifest.id)">退出登录</button>
            </template>
            <template v-else>
              <button class="btn primary small" @click="openLogin(p)">扫码登录</button>
              <button class="btn ghost small" @click="openCaptchaLogin(p)">验证码登录</button>
              <button class="btn ghost small" @click="openCookieLogin(p)">用 Cookie 登录</button>
            </template>
          </div>
        </div>

        <!-- 通过链接导入歌单：网易云歌单详情接口免登录可用，未登录也能导公开歌单 -->
        <div v-if="p.manifest.capabilities.includes('playlist')" class="link-import">
          <div class="li-head">通过链接导入歌单</div>
          <div class="li-row">
            <input
              v-model="linkImport.text"
              class="li-input"
              placeholder="粘贴网易云 / QQ 音乐歌单分享链接"
              spellcheck="false"
              @keyup.enter="importFromLink(p.manifest.id)"
            />
            <button
              class="btn primary small"
              :disabled="linkImport.busy || !linkImport.text.trim()"
              @click="importFromLink(p.manifest.id)"
            >
              {{ linkImport.busy ? "解析中…" : "导入" }}
            </button>
          </div>
          <div class="li-hint">
            支持网易云（完整链 / 163cn.tv 短链）与 QQ 音乐（网页链 / 分享页 / 短链）；只导入到本地歌单，不同步到任何账号。酷狗 / 酷我 / 咪咕 / B 站暂未适配。
          </div>
        </div>

        <!-- 云端歌单：登录后自动拉取 -->
        <div v-if="acct(p.manifest.id).loggedIn" class="cloud">
          <div class="cloud-head">
            <h3>云端歌单</h3>
            <button class="btn ghost small" :disabled="cloudOf(p.manifest.id).loading" @click="loadCloud(p.manifest.id)">
              {{ cloudOf(p.manifest.id).loading ? "获取中…" : "刷新" }}
            </button>
          </div>
          <div v-if="cloudOf(p.manifest.id).loading" class="cloud-dim">正在获取账号歌单与「我喜欢的音乐」…</div>
          <div v-else-if="cloudOf(p.manifest.id).error" class="cloud-dim">
            获取失败：{{ cloudOf(p.manifest.id).error }}
          </div>
          <template v-else>
            <button
              v-if="cloudOf(p.manifest.id).liked"
              class="cloud-item"
              :disabled="importing === cloudOf(p.manifest.id).liked!.id"
              @click="syncLiked(p.manifest.id, cloudOf(p.manifest.id).liked!)"
            >
              <CoverArt
                v-if="cloudOf(p.manifest.id).liked!.coverUrl"
                class="ci-art"
                :cover-url="cloudOf(p.manifest.id).liked!.coverUrl"
                size="38px"
                radius="12px"
              />
              <span v-else class="ci-icon liked"><Icon name="heart" :size="18" filled /></span>
              <span class="ci-text">
                <b>{{ cloudOf(p.manifest.id).liked!.name }}</b>
                <small>{{ cloudOf(p.manifest.id).liked!.trackCount }} 首 · 同步到本地「我喜欢的」（只补新歌）</small>
              </span>
              <span class="ci-go">{{ importing === cloudOf(p.manifest.id).liked!.id ? "同步中…" : "同步" }}</span>
            </button>
            <div
              v-for="pl in cloudOf(p.manifest.id).playlists"
              :key="pl.id"
              class="cloud-item"
            >
              <button class="ci-main" :title="`播放「${pl.name}」`" @click="playCloudPlaylist(p.manifest.id, pl)">
                <CoverArt v-if="pl.coverUrl" class="ci-art" :cover-url="pl.coverUrl" size="38px" radius="12px" />
                <span v-else class="ci-icon"><Icon name="library" :size="18" /></span>
                <span class="ci-text">
                  <b>{{ pl.name }}</b>
                  <small>{{ pl.trackCount }} 首{{ importedKeys.has(`${p.manifest.id}:${pl.id}`) ? " · 已导入过" : "" }}</small>
                </span>
              </button>
              <button
                class="ci-import"
                :disabled="importing === pl.id"
                @click="requestImport(p.manifest.id, pl)"
              >
                {{ importing === pl.id ? "导入中…" : importedKeys.has(`${p.manifest.id}:${pl.id}`) ? "再导入" : "导入" }}
              </button>
            </div>
            <div v-if="!cloudOf(p.manifest.id).playlists.length && !cloudOf(p.manifest.id).liked" class="cloud-dim">
              账号里没有歌单
            </div>
          </template>
        </div>
      </div>

      <div class="info card">
        <h2>扫码被拦截怎么办</h2>
        <p>
          手机 App 提示「检测到当前设备环境异常，本次操作已拦截」，或服务端返回
          <code>8821</code>，是网易云对第三方扫码登录的账号级风控，不是二维码失效或播放器故障。
          可以先试官方给的四条：切换网络或更换设备、移除手机上的越狱 / 解锁类插件、
          把网易云 App 升到最新版、不要频繁操作并等待 24 小时。
        </p>
        <p><strong>最可靠的是 Cookie 登录</strong>：</p>
        <ol>
          <li>浏览器打开 <code>music.163.com</code> 并登录账号</li>
          <li>F12 → Network → 刷新 → 点任一发往 <code>music.163.com</code> 的请求</li>
          <li>
            复制 Request Headers 里的整段 Cookie（必须含 <code>MUSIC_U</code>；
            它是 HttpOnly，<code>document.cookie</code> 读不到）
          </li>
          <li>回到本页点「用 Cookie 登录」粘贴并保存，会自动校验账号</li>
        </ol>
        <div class="note">
          只有会员账号、且曲目在权限内才会下发完整时长；普通账号登录仍是 30 秒试听。
          Cookie 等同账号凭据，只保存在本机，点「退出登录」即可清除，请勿外发。
        </div>
      </div>
    </div>

    <!-- 扫码登录弹层。二维码是宿主渲染的 SVG，直接 v-html 即可 -->
    <Teleport to="body">
      <div v-if="login.id" class="login-mask" @click.self="closeLogin">
        <div class="login-box">
          <div class="login-title">扫码登录 · {{ login.name }}</div>
          <div class="qr-frame">
            <div v-if="login.qrSvg" class="qr" v-html="login.qrSvg" />
            <div v-else class="qr loading">···</div>
          </div>
          <div class="login-status" :class="login.status">{{ login.message }}</div>
          <div v-if="login.status === 'blocked'" class="login-blocked">
            手机提示「设备环境异常」属于网易云账号级风控，换二维码或改参数都绕不过。
            在真实浏览器里登录一次、把 Cookie 粘过来即可。
          </div>
          <div class="login-hint">手机 App →「扫一扫」→ 确认登录</div>
          <div class="login-actions">
            <button
              v-if="
                login.status === 'expired' ||
                login.status === 'error' ||
                login.status === 'blocked'
              "
              class="btn primary"
              @click="refreshQr"
            >
              刷新二维码
            </button>
            <button class="btn ghost" @click="switchToCookie">用 Cookie 登录</button>
            <button class="btn ghost" @click="closeLogin">关闭</button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- 扫码被风控时的兜底：从真实浏览器复制 Cookie -->
    <Teleport to="body">
      <div v-if="cookieLogin.id" class="login-mask" @click.self="closeCookieLogin">
        <div class="login-box cookie-box">
          <div class="login-title">用 Cookie 登录 · {{ cookieLogin.name }}</div>
          <ol class="cookie-steps">
            <li>浏览器打开 music.163.com 并登录账号</li>
            <li>F12 → Network → 刷新 → 点任一 music.163.com 请求</li>
            <li>Request Headers 里复制整段 Cookie（必须含 MUSIC_U）</li>
          </ol>
          <textarea
            v-model="cookieLogin.text"
            class="cookie-input"
            rows="4"
            spellcheck="false"
            placeholder="MUSIC_U=...; __csrf=...; NMTID=..."
          />
          <div class="cookie-note">
            MUSIC_U 是 HttpOnly，document.cookie 读不到，必须从请求头复制。
            凭据只保存在本机，可以随时「退出登录」清除。
          </div>
          <div class="login-actions">
            <button class="btn primary" :disabled="cookieLogin.busy" @click="submitCookieLogin">
              {{ cookieLogin.busy ? "验证中…" : "保存并验证" }}
            </button>
            <button class="btn ghost" @click="closeCookieLogin">取消</button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- 验证码登录：手机号只发往网易官方服务器，本软件不经手不存储 -->
    <Teleport to="body">
      <div v-if="captchaLogin.id" class="login-mask" @click.self="closeCaptchaLogin">
        <div class="login-box captcha-box">
          <div class="login-title">验证码登录 · {{ captchaLogin.name }}</div>
          <div class="cap-row">
            <span class="cap-cc mono">+86</span>
            <input
              v-model="captchaLogin.phone"
              class="cap-input"
              inputmode="numeric"
              maxlength="11"
              placeholder="手机号"
              spellcheck="false"
            />
          </div>
          <div class="cap-row">
            <input
              v-model="captchaLogin.captcha"
              class="cap-input"
              inputmode="numeric"
              maxlength="6"
              placeholder="短信验证码"
              spellcheck="false"
              @keyup.enter="submitCaptchaLogin"
            />
            <button
              class="btn ghost small cap-send"
              :disabled="captchaLogin.countdown > 0 || captchaLogin.sending || !captchaPhoneOk()"
              @click="sendCaptcha"
            >
              {{
                captchaLogin.sending
                  ? "发送中…"
                  : captchaLogin.countdown > 0
                    ? `${captchaLogin.countdown}s 后重发`
                    : "发送验证码"
              }}
            </button>
          </div>
          <div v-if="captchaLogin.message" class="cap-msg" :class="captchaLogin.error ? 'err' : 'ok'">
            {{ captchaLogin.message }}
          </div>
          <div class="cookie-note">
            验证码 10 分钟内有效。手机号与验证码只发往网易官方服务器，
            本软件不经手、不存储；登录成功后凭据只保存在本机。
          </div>
          <div class="login-actions">
            <button
              class="btn primary"
              :disabled="captchaLogin.busy || !captchaLogin.captcha.trim() || !captchaPhoneOk()"
              @click="submitCaptchaLogin"
            >
              {{ captchaLogin.busy ? "登录中…" : "登录" }}
            </button>
            <button class="btn ghost" @click="closeCaptchaLogin">取消</button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- 重复导入确认：本地已有该云端歌单的导入记录，再导会新建一份重复的 -->
    <Teleport to="body">
      <div v-if="pendingImport" class="login-mask" @click.self="pendingImport = null">
        <div class="login-box confirm-box">
          <div class="login-title">再导入一次？</div>
          <p class="confirm-text">
            {{ pendingImport.label }} 已经导入过本地歌单了。
            再次导入会<strong>新建一个重复的歌单</strong>，确定要继续吗？
          </p>
          <div class="login-actions">
            <button class="btn ghost" @click="pendingImport = null">取消</button>
            <button class="btn primary" @click="confirmImport">仍然导入</button>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.page {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.head {
  display: flex;
  gap: 18px;
  align-items: center;
  padding-bottom: 18px;
}
.badge {
  width: 76px;
  height: 76px;
  border-radius: 24px;
  display: grid;
  place-items: center;
  background: linear-gradient(135deg, var(--accent), var(--accent-2));
  color: #fff;
  box-shadow: 0 16px 40px -16px var(--accent);
  flex: none;
}
.body {
  flex: 1;
  min-height: 0;
  padding-bottom: 40px;
}
.account-card {
  padding: 18px 20px;
  margin-bottom: 14px;
}
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}
.plugin-name {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13.5px;
  color: var(--text);
}
.ver {
  font-size: 11px;
  color: var(--text-3);
}
.state {
  font-size: 11px;
  padding: 3px 10px;
  border-radius: 999px;
  background: var(--panel-3);
  color: var(--text-3);
}
.state.on {
  background: color-mix(in srgb, #32c5a0 18%, transparent);
  color: #32c5a0;
}
.account-row {
  display: flex;
  align-items: center;
  gap: 14px;
  margin-top: 16px;
}
.avatar {
  width: 56px;
  height: 56px;
  border-radius: 50%;
  object-fit: cover;
  flex: none;
}
.avatar.fallback {
  display: grid;
  place-items: center;
  background: var(--panel-3);
  color: var(--text-3);
}
.who {
  flex: 1;
  min-width: 0;
}
.who .name {
  font-size: 15px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.who .sub {
  font-size: 12px;
  color: var(--text-3);
  margin-top: 5px;
}
.vip {
  font-size: 10.5px;
  padding: 2px 8px;
  border-radius: 999px;
  background: linear-gradient(135deg, #ffd166, #ff9f43);
  color: #43290a;
  font-weight: 600;
}
.actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.btn.small {
  height: 32px;
  padding: 0 14px;
  font-size: 12px;
}

/* ---------- 歌单链接导入 ---------- */
.link-import {
  margin-top: 14px;
  padding: 12px 14px;
  border-radius: 14px;
  background: var(--panel-2);
  border: 1px solid var(--border);
}
.li-head {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text-2);
  margin-bottom: 10px;
}
.li-row {
  display: flex;
  gap: 8px;
}
.li-input {
  flex: 1;
  min-width: 0;
  height: 34px;
  padding: 0 12px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--panel);
  color: var(--text);
  font-size: 12.5px;
  outline: none;
  transition: border-color 0.15s;
}
.li-input:focus {
  border-color: var(--accent);
}
.li-input::placeholder {
  color: var(--text-3);
}
.li-hint {
  font-size: 11px;
  color: var(--text-3);
  margin-top: 8px;
  line-height: 1.6;
}

/* ---------- 云端歌单 ---------- */
.cloud {
  margin-top: 16px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
}
.cloud-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 10px;
}
.cloud-head h3 {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-2);
  margin: 0;
}
.cloud-dim {
  font-size: 12px;
  color: var(--text-3);
  padding: 8px 0;
}
.cloud-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 8px 10px;
  border-radius: 12px;
  text-align: left;
  color: var(--text-2);
}
.cloud-item:hover {
  background: var(--hover);
  color: var(--text);
}
.cloud-item:disabled {
  opacity: 0.6;
}
.ci-main {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
  text-align: left;
  color: inherit;
}
.ci-icon {
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  border-radius: 12px;
  background: var(--panel-2);
  color: var(--text-3);
  flex: none;
}
/* 云端歌单封面：去掉 CoverArt 自带投影，38px 小图不需要 */
.ci-art {
  box-shadow: none;
}
.ci-icon.liked {
  color: #f26d8b;
  background: color-mix(in srgb, #f26d8b 14%, transparent);
}
.ci-text {
  display: flex;
  flex-direction: column;
  gap: 3px;
  flex: 1;
  min-width: 0;
}
.ci-text b {
  font-size: 13px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ci-text small {
  font-size: 11px;
  color: var(--text-3);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ci-go,
.ci-import {
  flex: none;
  font-size: 11.5px;
  padding: 5px 12px;
  border-radius: 999px;
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
}
.ci-import:disabled,
.ci-go {
  opacity: 0.85;
}
.ci-import:hover:not(:disabled) {
  background: color-mix(in srgb, var(--accent) 20%, transparent);
}
.info {
  padding: 20px;
  margin-top: 18px;
}
.info h2 {
  font-size: 15px;
  margin: 0 0 12px;
}
.info p,
.info ol {
  font-size: 12.5px;
  line-height: 1.85;
  color: var(--text-2);
  margin: 0 0 10px;
}
.info ol {
  padding-left: 20px;
}
.info code {
  font-family: var(--mono);
  font-size: 11.5px;
  padding: 1px 5px;
  border-radius: 5px;
  background: var(--panel-2);
  color: var(--text);
}
.note {
  font-size: 11.5px;
  line-height: 1.75;
  color: var(--text-3);
  padding: 10px 12px;
  border-radius: 10px;
  background: var(--panel-2);
  border: 1px solid var(--border);
  margin-top: 14px;
}

/* ---------- 登录弹层 ---------- */
.login-mask {
  position: fixed;
  inset: 0;
  z-index: 120;
  display: grid;
  place-items: center;
  padding: 20px;
  background: rgba(0, 0, 0, 0.55);
  backdrop-filter: blur(6px);
}
.login-box {
  width: min(92vw, 340px);
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 12px;
  padding: 22px 22px 18px;
  border-radius: 22px;
  background: var(--panel);
  border: 1px solid var(--border);
  box-shadow: 0 24px 60px -20px rgba(0, 0, 0, 0.55);
}
.login-title {
  font-size: 14px;
  font-weight: 600;
  text-align: center;
}
/* 二维码必须白底黑块，跟主题走会扫不出来 */
.qr-frame {
  padding: 10px;
  border-radius: 16px;
  background: #fff;
}
.qr {
  width: 220px;
  height: 220px;
}
.qr :deep(svg) {
  display: block;
  width: 100%;
  height: 100%;
}
.qr.loading {
  display: grid;
  place-items: center;
  color: #bbb;
  font-size: 28px;
  letter-spacing: 4px;
}
.login-status {
  font-size: 13px;
  color: var(--text-2);
  min-height: 20px;
  text-align: center;
}
.login-status.scanned {
  color: var(--accent);
}
.login-status.success {
  color: #32c5a0;
}
.login-status.expired,
.login-status.error,
.login-status.blocked {
  color: #ff6b6b;
  line-height: 1.5;
}
.login-blocked {
  font-size: 11.5px;
  line-height: 1.6;
  color: var(--text-3);
  padding: 10px 12px;
  border-radius: 10px;
  background: var(--panel-2);
  border: 1px solid var(--border);
  text-align: left;
}
.login-hint {
  font-size: 11.5px;
  color: var(--text-3);
}
.login-actions {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
  justify-content: center;
}
/* ---------- 验证码登录弹层 ---------- */
.captcha-box {
  width: min(92vw, 380px);
  align-items: stretch;
  gap: 10px;
}
.cap-row {
  display: flex;
  gap: 8px;
  align-items: center;
}
.cap-cc {
  flex: none;
  font-size: 12.5px;
  color: var(--text-3);
  padding: 0 10px;
  height: 38px;
  display: grid;
  place-items: center;
  border-radius: 10px;
  background: var(--panel-2);
  border: 1px solid var(--border);
}
.cap-input {
  flex: 1;
  min-width: 0;
  height: 38px;
  padding: 0 12px;
  border-radius: 10px;
  font-size: 13px;
  color: var(--text);
  background: var(--panel-2);
  border: 1px solid var(--border);
  outline: none;
  transition: border-color 0.15s;
}
.cap-input:focus {
  border-color: var(--accent);
}
.cap-send {
  flex: none;
  height: 38px;
  font-variant-numeric: tabular-nums;
}
.cap-msg {
  font-size: 12px;
  line-height: 1.5;
  padding: 8px 10px;
  border-radius: 8px;
  text-align: center;
}
.cap-msg.ok {
  color: #32c5a0;
  background: color-mix(in srgb, #32c5a0 10%, transparent);
}
.cap-msg.err {
  color: #ff6b6b;
  background: color-mix(in srgb, #ff6b6b 10%, transparent);
}
.cookie-box {
  width: min(92vw, 410px);
  align-items: stretch;
  gap: 10px;
}
.cookie-steps {
  margin: 0;
  padding-left: 20px;
  font-size: 12px;
  line-height: 1.8;
  color: var(--text-2);
}
.cookie-input {
  width: 100%;
  box-sizing: border-box;
  resize: vertical;
  padding: 10px 12px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--panel-2);
  color: var(--text);
  font-size: 12px;
  line-height: 1.5;
  font-family: ui-monospace, SFMono-Regular, Consolas, monospace;
  outline: none;
}
.cookie-input:focus {
  border-color: var(--accent);
}
.cookie-note {
  font-size: 11px;
  line-height: 1.6;
  color: var(--text-3);
}

/* 重复导入确认弹层 */
.confirm-box {
  width: min(92vw, 380px);
  align-items: stretch;
}
.confirm-text {
  font-size: 12.5px;
  line-height: 1.8;
  color: var(--text-2);
  margin: 0;
  text-align: center;
}
.confirm-text strong {
  color: var(--text);
}

@media (max-width: 860px) {
  .head {
    gap: 12px;
    padding-bottom: 12px;
  }
  .badge {
    width: 56px;
    height: 56px;
    border-radius: 18px;
  }
  .page-title {
    font-size: 21px;
  }
  .page-sub {
    margin-bottom: 0;
    font-size: 12px;
  }
  .account-card {
    padding: 14px;
  }
  .account-row {
    flex-wrap: wrap;
    gap: 10px;
  }
  .actions {
    width: 100%;
  }
  .actions .btn {
    flex: 1 1 auto;
  }
  .info {
    padding: 16px;
  }
  .body {
    padding-bottom: 20px;
  }
  .qr {
    width: 200px;
    height: 200px;
  }
}
</style>
