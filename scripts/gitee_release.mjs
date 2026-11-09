#!/usr/bin/env node
/**
 * 把构建产物发布到 Gitee 发行版（Release）。
 *
 * CI 与本地共用同一份逻辑：
 *   - CI：.github/workflows/release.yml 的 gitee 任务在 push v* 标签后调用；
 *   - 本地：想补传某个版本时直接跑
 *
 *       GITEE_TOKEN=xxxx GITEE_OWNER=chenl-eon GITEE_REPO=LeonMusic-releases \
 *         node scripts/gitee_release.mjs --tag v1.2.0 --dir dist-rel
 *
 * 参数：
 *   --tag <v1.2.0>       必填。发行版标签；Gitee 上不存在时按 --target 自动创建
 *   --dir <目录>         必填。要上传的目录（默认上传目录下所有普通文件）
 *   --files a,b          可选。只上传这些文件名（相对 --dir）
 *   --name <标题>        可选。发行版标题，默认与 tag 相同
 *   --body <说明>        可选。发行版描述
 *   --body-file <路径>   可选。从文件读取描述（优先于 --body）
 *   --target <分支>      可选。创建 tag 时指向的分支/提交，默认取仓库默认分支
 *   --prerelease         可选。标记为预览版（也可写 --prerelease=0 关掉）
 *   --dry-run            可选。只打印计划，不发任何请求
 *
 * 环境变量：
 *   GITEE_TOKEN   必填。私人令牌，权限需勾选 projects
 *   GITEE_OWNER   必填。Gitee 用户名 / 组织名
 *   GITEE_REPO    必填。仓库名
 *   GITEE_API     可选。默认 https://gitee.com/api/v5
 *
 * 幂等：同一个 tag 重复执行会复用已有发行版，并覆盖同名附件
 *      （先删旧附件再传，否则 Gitee 上会留下两份同名文件）。
 */
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import path from "node:path";

const API = (process.env.GITEE_API || "https://gitee.com/api/v5").replace(/\/+$/, "");
const TOKEN = (process.env.GITEE_TOKEN || "").trim();
const OWNER = (process.env.GITEE_OWNER || "").trim();
const REPO = (process.env.GITEE_REPO || "").trim();
const GH_REPO = process.env.GITHUB_REPOSITORY || "ChenL-eon/LeonMusic";

/** Gitee 免费版单附件上限 100MB（GVP 项目 200MB） */
const MAX_ASSET = 100 * 1024 * 1024;

function die(msg) {
  console.error(`✗ ${msg}`);
  process.exit(1);
}

function human(n) {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(2)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${n} B`;
}

function parseArgs(argv) {
  const out = { prerelease: undefined };
  for (let i = 0; i < argv.length; i++) {
    const raw = argv[i];
    const eq = raw.indexOf("=");
    const key = eq >= 0 ? raw.slice(0, eq) : raw;
    let inline = eq >= 0 ? raw.slice(eq + 1) : undefined;
    const next = () => {
      if (inline !== undefined) return inline;
      const v = argv[++i];
      if (v === undefined || (v.startsWith("--") && v.length > 2)) die(`参数 ${key} 缺少取值`);
      return v;
    };
    switch (key) {
      case "--tag": out.tag = next(); break;
      case "--dir": out.dir = next(); break;
      case "--files": out.files = next().split(",").map((s) => s.trim()).filter(Boolean); break;
      case "--name": out.name = next(); break;
      case "--body": out.body = next(); break;
      case "--body-file": out.bodyFile = next(); break;
      case "--target": out.target = next(); break;
      case "--prerelease":
        // 兼容三种写法：--prerelease / --prerelease=1 / --prerelease 0
        if (inline !== undefined) out.prerelease = /^(1|true|yes|on)$/i.test(inline.trim());
        else if (argv[i + 1] !== undefined && /^(0|1|true|false)$/i.test(argv[i + 1].trim())) out.prerelease = /^(1|true)$/i.test(argv[++i].trim());
        else out.prerelease = true;
        break;
      case "--dry-run": out.dryRun = true; break;
      case "-h":
      case "--help": out.help = true; break;
      default: die(`未知参数 ${raw}`);
    }
  }
  return out;
}

function printHelp() {
  console.log(
    [
      "用法：GITEE_TOKEN=xxx GITEE_OWNER=xxx GITEE_REPO=xxx node scripts/gitee_release.mjs --tag v1.2.0 --dir dist-rel",
      "",
      "  --tag <tag>          发行版标签（必填）",
      "  --dir <dir>          产物目录（必填）",
      "  --files a,b          只传指定文件（可选）",
      "  --name <title>       发行版标题（可选）",
      "  --body <text>        发行版描述（可选）",
      "  --body-file <path>   从文件读描述（可选）",
      "  --target <branch>    创建 tag 的落点（默认取仓库默认分支）",
      "  --prerelease         标记预览版",
      "  --dry-run            只打印计划",
    ].join("\n"),
  );
}

/** 收集待上传文件（跳过目录与 0 字节文件） */
function pickFiles(args) {
  const all = fs
    .readdirSync(args.dir)
    .map((n) => path.join(args.dir, n))
    .filter((p) => fs.statSync(p).isFile() && fs.statSync(p).size > 0);
  if (!args.files) return all;
  const missing = args.files.filter((n) => !all.some((p) => path.basename(p) === n));
  if (missing.length) die(`--files 里这些文件在 ${args.dir} 中不存在：${missing.join(", ")}`);
  return all.filter((p) => args.files.includes(path.basename(p)));
}

/** 统一的 API 调用：token 走 query（Gitee 所有接口都认） */
async function api(method, sub, { query, json, form } = {}) {
  const qs = new URLSearchParams({ access_token: TOKEN, ...(query || {}) }).toString();
  const url = `${API}/repos/${encodeURIComponent(OWNER)}/${encodeURIComponent(REPO)}${sub}?${qs}`;
  const init = { method };
  if (json !== undefined) {
    init.headers = { "Content-Type": "application/json;charset=UTF-8" };
    // Gitee 的 JSON 接口以 body 里的 access_token 为准，query 只是兜底，两边都带上
    init.body = JSON.stringify({ access_token: TOKEN, ...json });
  } else if (form) {
    init.body = form;
  }
  let res;
  try {
    res = await fetch(url, init);
  } catch (e) {
    // 不要把带 token 的 URL 打进日志
    throw new Error(`${method} ${sub} 请求失败：${e.cause?.code || e.message}`);
  }
  const text = await res.text();
  let data = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  if (!res.ok) {
    const detail = typeof data === "string" ? data : JSON.stringify(data);
    const err = new Error(`${method} ${sub} → HTTP ${res.status}：${(detail || "").slice(0, 400)}`);
    err.status = res.status;
    throw err;
  }
  return data;
}

/**
 * 预检仓库：
 *  - 空仓库没有提交，Gitee 无法为它建 tag，发行版必然失败 → 提前拦下并给出做法；
 *  - 顺便读默认分支，省掉手工配 GITEE_TARGET。
 */
async function preflight(args) {
  let repo;
  try {
    repo = await api("GET", "");
  } catch (e) {
    if (e.status === 401) {
      die(
        "令牌无效或已过期（HTTP 401）。\n"
          + "  Gitee 对公开接口也同样校验令牌，检查 GITEE_TOKEN；重新生成时记得勾选 projects 权限。",
      );
    }
    if (e.status === 404) {
      die(
        `找不到仓库 ${OWNER}/${REPO}。\n`
          + "  GITEE_OWNER / GITEE_REPO 要填仓库地址栏里的【路径】，不是仓库显示名：\n"
          + "    显示名 LeonMusic-releases → 路径 leon-music-releases（小写、连字符）",
      );
    }
    throw e;
  }
  if (repo.empty_repo) {
    die(
      `${OWNER}/${REPO} 还是空仓库，Gitee 无法为它创建 tag。\n`
        + "  先在仓库网页上新建一个 README.md 并提交（或本地 push 一个提交）再重跑。",
    );
  }
  const branch = args.target || repo.default_branch || "master";
  console.log(`· 仓库 ${OWNER}/${REPO} 就绪，默认分支 ${branch}`);
  return branch;
}

async function ensureRelease(args, body, target) {
  const name = args.name || args.tag;
  let existing = null;
  try {
    existing = await api("GET", `/releases/tags/${encodeURIComponent(args.tag)}`);
  } catch (e) {
    if (e.status !== 404) throw e;
  }
  // 注意：Gitee 对不存在的 tag 并不总是回 404 —— 实测会回 HTTP 200 + JSON
  // 字面量 null。只判断 404 会把 null 当成发行版对象，随后 existing.id 直接
  // 抛 "Cannot read properties of null"。这里两种形态都按"不存在"处理。
  if (existing && existing.id != null) {
    console.log(`· 发行版 ${args.tag} 已存在（id=${existing.id}），复用并同步标题/描述`);
    try {
      const patched = await api("PATCH", `/releases/${existing.id}`, {
        json: { name, body, prerelease: !!args.prerelease },
      });
      return patched && patched.id != null ? patched : existing;
    } catch (e) {
      console.warn(`  ! 更新发行版信息失败（不影响附件上传）：${e.message}`);
      return existing;
    }
  }
  const created = await api("POST", "/releases", {
    json: {
      tag_name: args.tag,
      name,
      body,
      target_commitish: target,
      prerelease: !!args.prerelease,
    },
  });
  if (created && created.id != null) {
    console.log(`· 已创建发行版 ${args.tag}（id=${created.id}）`);
    return created;
  }
  // 少数情况下创建接口会回空响应，回查一次确认到底建没建成
  const again = await api("GET", `/releases/tags/${encodeURIComponent(args.tag)}`).catch(() => null);
  if (again && again.id != null) {
    console.log(`· 已创建发行版 ${args.tag}（回查得到 id=${again.id}）`);
    return again;
  }
  die(
    `创建发行版 ${args.tag} 后既没拿到响应体，回查也找不到。\n`
      + "  请到仓库的「发行版」页面确认是否已生成，再决定重跑或手工补传。",
  );
}

async function listAssets(releaseId) {
  const out = [];
  for (let page = 1; page <= 20; page++) {
    const rows = await api("GET", `/releases/${releaseId}/attach_files`, {
      query: { page: String(page), per_page: "100" },
    });
    if (!Array.isArray(rows) || rows.length === 0) break;
    out.push(...rows);
    if (rows.length < 100) break;
  }
  return out;
}

/**
 * 上传附件。
 *
 * 不用内置 fetch：undici 只给 5 分钟等响应头（headersTimeout），而 GitHub
 * 的美国 runner 往 Gitee 传 20MB+ 的 APK 往往更久，实测直接
 * `UND_ERR_HEADERS_TIMEOUT`。这里改成 https.request 流式发送，只设 socket
 * 空闲超时——只要数据还在走就不会被打断。
 */
function uploadAsset(releaseId, filePath) {
  const name = path.basename(filePath);
  const boundary = `----leonmusic${Date.now().toString(16)}`;
  const head = Buffer.from(
    `--${boundary}\r\n`
      + 'Content-Disposition: form-data; name="access_token"\r\n\r\n'
      + `${TOKEN}\r\n`
      + `--${boundary}\r\n`
      + `Content-Disposition: form-data; name="file"; filename="${name}"\r\n`
      + "Content-Type: application/octet-stream\r\n\r\n",
    "utf8",
  );
  const tail = Buffer.from(`\r\n--${boundary}--\r\n`, "utf8");
  const size = fs.statSync(filePath).size;
  const qs = new URLSearchParams({ access_token: TOKEN }).toString();
  const url = new URL(
    `${API}/repos/${encodeURIComponent(OWNER)}/${encodeURIComponent(REPO)}`
      + `/releases/${releaseId}/attach_files?${qs}`,
  );

  return new Promise((resolve, reject) => {
    const mod = url.protocol === "https:" ? https : http;
    const req = mod.request(
      {
        method: "POST",
        hostname: url.hostname,
        port: url.port || undefined,
        path: url.pathname + url.search,
        headers: {
          "Content-Type": `multipart/form-data; boundary=${boundary}`,
          "Content-Length": head.length + size + tail.length,
        },
        timeout: 900_000, // 空闲 15 分钟；慢速但仍在传时不触发
      },
      (res) => {
        let body = "";
        res.setEncoding("utf8");
        res.on("data", (c) => (body += c));
        res.on("end", () => {
          if (res.statusCode >= 200 && res.statusCode < 300) {
            try {
              resolve(body ? JSON.parse(body) : null);
            } catch {
              resolve(body);
            }
            return;
          }
          const err = new Error(`POST attach_files → HTTP ${res.statusCode}：${body.slice(0, 400)}`);
          err.status = res.statusCode;
          reject(err);
        });
      },
    );
    req.on("timeout", () => req.destroy(new Error("上传超时：15 分钟无数据流动")));
    req.on("error", reject);
    req.write(head);
    const stream = fs.createReadStream(filePath);
    stream.on("error", reject);
    stream.on("end", () => req.end(tail));
    stream.pipe(req, { end: false });
  });
}

function defaultBody(args) {
  return [
    "> 本页安装包由 GitHub Actions 自动同步，内容与 GitHub 发行版一致。",
    "",
    `- 源码与更新日志：https://github.com/${GH_REPO}/releases/tag/${args.tag}`,
    "- Windows 便携版：解压即用，不写注册表",
    "- Windows 安装版：NSIS 安装程序，可按用户安装",
    "- Android：arm64 APK，与 GitHub 版使用同一签名",
  ].join("\n");
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) return printHelp();

  if (!args.tag) die("缺少 --tag");
  if (!args.dir) die("缺少 --dir");
  if (!fs.existsSync(args.dir)) die(`目录不存在：${args.dir}`);

  const files = pickFiles(args);
  if (files.length === 0) die(`目录里没有可上传的文件：${args.dir}`);

  const total = files.reduce((n, f) => n + fs.statSync(f).size, 0);
  for (const f of files) {
    const size = fs.statSync(f).size;
    if (size > MAX_ASSET) die(`${path.basename(f)} 有 ${human(size)}，超过 Gitee 单附件 100MB 上限`);
  }

  const body = args.bodyFile ? fs.readFileSync(args.bodyFile, "utf8") : args.body || defaultBody(args);

  console.log(`Gitee 发行版：${OWNER}/${REPO} @ ${args.tag}`);
  console.log(`待上传 ${files.length} 个文件，合计 ${human(total)}：`);
  for (const f of files) console.log(`  - ${path.basename(f)}  ${human(fs.statSync(f).size)}`);

  if (args.dryRun) {
    console.log("\n[dry-run] 未发送任何请求。");
    return;
  }

  if (!TOKEN) die("缺少环境变量 GITEE_TOKEN（私人令牌，权限需含 projects）");
  if (!OWNER || !REPO) die("缺少环境变量 GITEE_OWNER / GITEE_REPO");

  const target = await preflight(args);
  const release = await ensureRelease(args, body, target);
  const assets = await listAssets(release.id);

  for (const f of files) {
    const name = path.basename(f);
    // 先清同名旧附件，避免重复执行后出现两份
    for (const old of assets.filter((a) => a.name === name)) {
      await api("DELETE", `/releases/${release.id}/attach_files/${old.id}`);
      console.log(`  · 已删除同名旧附件 ${name}`);
    }
    const uploaded = await uploadAsset(release.id, f);
    console.log(`  ✓ ${name} → ${uploaded?.browser_download_url || "(已上传)"}`);
  }

  console.log(`\n完成：https://gitee.com/${OWNER}/${REPO}/releases/tag/${encodeURIComponent(args.tag)}`);
}

main().catch((e) => {
  console.error(`\n✗ 发布失败：${e.message}`);
  process.exit(1);
});
