#!/usr/bin/env python3
"""把 android-overlay 合并进 Tauri 生成的 Android 工程。

Tauri 的 `android init` 会生成 `src-tauri/gen/android/`，而这个目录是**生成物**：
删了重跑就会覆盖。所以原生代码不直接写在那里，而是放在 `android-overlay/`
（受版本控制），再用本脚本同步过去。

做的事：
1. 复制 `android-overlay/app/src/main/java/**` → `gen/android/app/src/main/java/**`
   （已存在的文件先备份到 `gen/android/.overlay-backup/`，方便对比 Tauri 模板改动）
2. 把 `manifest/permissions.xml` 插到 `<manifest>` 下
3. 把 `manifest/application-children.xml` 插到 `<application>` 内
4. 把 `manifest/media-button-receiver.xml` 插到 `<application>` 内（媒体键广播）
5. 把 `proguard/leon-jni.pro` 放到 `app/`（R8 不能混淆 JNI 按名调用的方法）
6. 给 `<application>` 补 `android:usesCleartextTraffic="true"`
   （在线音源插件常用 http 接口；如需更精细请改用 networkSecurityConfig）
7. 给 `app/build.gradle.kts` 加 `androidx.media:media` 依赖
8. 把 minSdk 兜底抬到 26（cpal 走 Oboe→AAudio，`libaaudio.so` 从 API 26 起才有）
9. 存在 `android-overlay/keystore.properties` 时注入 release 签名配置

所有步骤都是幂等的：重复执行不会产生重复节点。

用法：
    python android-overlay/apply.py            # 同步
    python android-overlay/apply.py --dry-run  # 只看会改什么
    python android-overlay/apply.py --project src-tauri/gen/android
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SRC_MAIN = ROOT / "app" / "src" / "main"
DEFAULT_PROJECT = ROOT.parent / "src-tauri" / "gen" / "android"

MEDIA_DEP = 'implementation("androidx.media:media:1.7.0")'
MARKER = "android-overlay"


def log(msg: str) -> None:
    print(f"  {msg}")


def copy_sources(project: Path, dry: bool) -> int:
    target = project / "app" / "src" / "main"
    if not target.is_dir():
        raise SystemExit(f"找不到 {target}，请先执行 `npm run android:init`")

    # 旧版本把备份写成 `<目标文件>.orig`，与目标同目录。图标落在 res/mipmap-*/
    # 时，AGP 的资源合并会直接拒绝非 .png/.xml 文件名（The file name must end
    # with .xml or .png），整包构建失败。这里先清掉历史残留，再把新备份统一
    # 放到工程根的 .overlay-backup/ 下（不在资源扫描范围内）。
    for stale in sorted(target.rglob("*.orig")):
        log(f"- 清理历史备份 {stale.relative_to(target)}")
        if not dry:
            stale.unlink()

    changed = 0
    for src in sorted(SRC_MAIN.rglob("*")):
        if not src.is_file():
            continue
        rel = src.relative_to(SRC_MAIN)
        dst = target / rel
        if dst.exists() and dst.read_bytes() == src.read_bytes():
            log(f"= {rel}")
            continue

        if dst.exists():
            backup = project / ".overlay-backup" / rel
            if not backup.exists():
                log(f"! 备份 {rel} -> .overlay-backup/{rel}")
                if not dry:
                    backup.parent.mkdir(parents=True, exist_ok=True)
                    backup.write_bytes(dst.read_bytes())

        log(f"+ {rel}")
        changed += 1
        if not dry:
            dst.parent.mkdir(parents=True, exist_ok=True)
            dst.write_bytes(src.read_bytes())
    return changed


def _read_fragment(name: str) -> str:
    return (ROOT / "manifest" / name).read_text(encoding="utf-8").strip()


PERM_RE = re.compile(r"<uses-permission\b[^>]*?/>", re.S)
SERVICE_RE = re.compile(r"<service\b[^>]*?/>", re.S)
NAME_RE = re.compile(r'android:name="([^"]+)"')


def _indent(block: str, spaces: int = 4) -> str:
    pad = " " * spaces
    out = []
    for line in block.splitlines():
        out.append(pad + line if line.strip() else line)
    return "\n".join(out)


def _existing(text: str, pattern: re.Pattern[str]) -> set[str]:
    found = set()
    for m in pattern.finditer(text):
        name = NAME_RE.search(m.group(0))
        if name:
            found.add(name.group(1))
    return found


def _missing_block(fragment: str, pattern: re.Pattern[str], existing: set[str]) -> str:
    """从片段里挑出宿主还没有的节点，统一缩进后返回。"""
    keep = []
    for m in pattern.finditer(fragment):
        elem = " ".join(m.group(0).split())  # 压平换行，保持单行元素
        name = NAME_RE.search(elem)
        if name and name.group(1) in existing:
            continue
        keep.append(elem)
    return "\n".join(_indent(e) for e in keep)


def merge_manifest(project: Path, dry: bool) -> bool:
    path = project / "app" / "src" / "main" / "AndroidManifest.xml"
    if not path.is_file():
        raise SystemExit(f"找不到 {path}")

    text = path.read_text(encoding="utf-8")
    before = text

    # 1) 权限（逐条去重：Tauri 模板里已经有 INTERNET，不能插第二遍）
    have = _existing(text, PERM_RE)
    block = _missing_block(_read_fragment("permissions.xml"), PERM_RE, have)
    if block:
        idx = text.find("<application")
        if idx < 0:
            raise SystemExit("AndroidManifest.xml 里没有 <application>，结构异常")
        note = "    <!-- 播放相关权限（由 android-overlay/apply.py 注入） -->\n"
        text = text[:idx].rstrip() + "\n" + note + block + "\n\n" + text[idx:]
        log(f"+ manifest: 权限 ×{block.count('<uses-permission')}")

    # 2) 前台服务声明
    have = _existing(text, SERVICE_RE)
    block = _missing_block(_read_fragment("application-children.xml"), SERVICE_RE, have)
    if block:
        idx = text.find("</application>")
        if idx < 0:
            raise SystemExit("AndroidManifest.xml 里没有 </application>，结构异常")
        text = text[:idx] + block + "\n\n" + text[idx:]
        log("+ manifest: PlaybackService (foregroundServiceType=mediaPlayback)")

    # 3) 允许明文 HTTP（在线音源插件的接口多为 http）
    m = re.search(r"<application\b[^>]*>", text)
    if m and "usesCleartextTraffic" not in m.group(0):
        tag = m.group(0)
        if tag.endswith("/>"):
            log("!! <application/> 自闭合，跳过 usesCleartextTraffic，请手工处理")
        else:
            patched = tag[:-1].rstrip() + '\n        android:usesCleartextTraffic="true">'
            text = text[: m.start()] + patched + text[m.end():]
            log("+ manifest: usesCleartextTraffic=true")

    if text == before:
        return False
    if not dry:
        path.write_text(text, encoding="utf-8")
    return True


def inject_media_button_receiver(project: Path, dry: bool) -> bool:
    """注入 MediaButtonReceiver（蓝牙 / 线控媒体键）。

    这一段带 `<intent-filter>` 子元素，不是自闭合标签，所以不能走
    [merge_manifest] 里按 `<service/>` 去重的那条路径，单独按类名判重。
    """
    path = project / "app" / "src" / "main" / "AndroidManifest.xml"
    if not path.is_file():
        return False
    text = path.read_text(encoding="utf-8")
    if "androidx.media.session.MediaButtonReceiver" in text:
        return False
    idx = text.find("</application>")
    if idx < 0:
        log("!! AndroidManifest.xml 里没有 </application>，跳过 MediaButtonReceiver")
        return False
    block = _indent(_read_fragment("media-button-receiver.xml"))
    text = text[:idx] + block + "\n\n" + text[idx:]
    log("+ manifest: MediaButtonReceiver（蓝牙/线控媒体键）")
    if not dry:
        path.write_text(text, encoding="utf-8")
    return True


def copy_proguard(project: Path, dry: bool) -> bool:
    """复制 JNI keep 规则到 app/ 根目录。

    `app/build.gradle.kts` 用 `fileTree(".") { include("**/*.pro") }` 收集规则，
    所以只要放进 app/ 就会被 R8 读取，不用改生成物的 gradle 文件。
    """
    src = ROOT / "proguard" / "leon-jni.pro"
    if not src.is_file():
        return False
    dst = project / "app" / "leon-jni.pro"
    if dst.exists() and dst.read_bytes() == src.read_bytes():
        return False
    log("+ app/leon-jni.pro（JNI 入口 keep 规则）")
    if not dry:
        dst.write_bytes(src.read_bytes())
    return True


def patch_gradle(project: Path, dry: bool) -> bool:
    for candidate in ("app/build.gradle.kts", "app/build.gradle"):
        path = project / candidate
        if path.is_file():
            break
    else:
        log("!! 没找到 app/build.gradle(.kts)，跳过依赖注入")
        return False

    text = path.read_text(encoding="utf-8")
    if "androidx.media:media" in text:
        return False

    m = re.search(r"dependencies\s*\{", text)
    if not m:
        log(f"!! {candidate} 里没有 dependencies 块，请手工加 {MEDIA_DEP}")
        return False

    snippet = (
        f"\n    // MediaSessionCompat + MediaStyle 通知（由 {MARKER} 注入）"
        f"\n    {MEDIA_DEP}"
    )
    text = text[: m.end()] + snippet + text[m.end():]
    log(f"+ {candidate}: {MEDIA_DEP}")
    if not dry:
        path.write_text(text, encoding="utf-8")
    return True


def patch_min_sdk(project: Path, dry: bool) -> bool:
    """把 minSdk 抬到 26。

    cpal 的 Android 后端走 Oboe → AAudio，而 NDK 里 `libaaudio.so` 从 API 26
    才开始提供。minSdk=24 时链接会直接失败：

        ld.lld: error: unable to find library -laaudio

    `tauri.conf.json` 里的 `bundle.android.minSdkVersion` 只在 `android init`
    时被读一次，生成物已存在时不会跟着变，所以这里兜底纠正。
    """
    path = project / "app" / "build.gradle.kts"
    if not path.is_file():
        return False
    text = path.read_text(encoding="utf-8")
    m = re.search(r"minSdk\s*=\s*(\d+)", text)
    if not m:
        log("!! build.gradle.kts 里没找到 minSdk，请手工确认 >= 26")
        return False
    value = int(m.group(1))
    if value >= 26:
        return False
    text = text[: m.start(1)] + "26" + text[m.end(1):]
    log(f"~ build.gradle.kts: minSdk {value} -> 26（AAudio 要求）")
    if not dry:
        path.write_text(text, encoding="utf-8")
    return True


def patch_signing(project: Path, dry: bool) -> bool:
    """把 `android-overlay/keystore.properties` 接进 release 签名。

    ## 为什么放在这里而不是写死进 build.gradle.kts

    `gen/android/` 是生成物，`android init` 一跑就重来。所以签名信息放在
    overlay 里，每次同步时重新注入——重新生成工程后只要再跑一次本脚本，
    签名配置就回来了。

    ## keystore.properties 格式

    ```properties
    storeFile=leonmusic-release.jks
    storePassword=******
    keyAlias=leonmusic
    keyPassword=******
    ```

    `storeFile` 相对 `gen/android/`，同目录下的 `.jks` 会被一起复制过去。
    `.gitignore` 已经排除 `*.jks` 与 `keystore.properties`，不要把私钥提交上去。
    """
    props_src = ROOT / "keystore.properties"
    gradle = project / "app" / "build.gradle.kts"
    if not gradle.is_file():
        return False

    text = gradle.read_text(encoding="utf-8")
    if "keystoreProperties" in text:
        return False

    if not props_src.is_file():
        log("· 没有 android-overlay/keystore.properties，release 包将保持未签名")
        return False

    props = {}
    for raw in props_src.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        props[k.strip()] = v.strip()

    store_file = props.get("storeFile")
    if not store_file:
        log("!! keystore.properties 缺少 storeFile，跳过签名")
        return False
    jks_src = ROOT / store_file
    if not jks_src.is_file():
        log(f"!! 找不到 keystore 文件 {jks_src}，跳过签名")
        return False
    for key in ("storePassword", "keyAlias", "keyPassword"):
        if not props.get(key):
            log(f"!! keystore.properties 缺少 {key}，跳过签名")
            return False

    if not dry:
        (project / "keystore.properties").write_bytes(props_src.read_bytes())
        (project / store_file).write_bytes(jks_src.read_bytes())

    m = re.search(r"^android\s*\{", text, re.M)
    if not m:
        log("!! build.gradle.kts 里没有 android {} 块，跳过签名")
        return False

    loader = (
        "\n// ---- release 签名（由 android-overlay/apply.py 注入）----\n"
        "// 凭据来自 keystore.properties；文件不存在时 release 包不签名，debug 不受影响。\n"
        # 注意：必须写 `Properties()` 而不是 `java.util.Properties()`。
        # Kotlin DSL 里 `java` 会命中 Project 的 `java {}` 扩展，
        # 于是 `java.util` 变成「在 Action<JavaPluginExtension> 上找 util」，
        # 直接 Unresolved reference 'util'。文件顶部已有 import java.util.Properties。
        "val keystoreProperties = Properties().apply {\n"
        '    val propFile = rootProject.file("keystore.properties")\n'
        "    if (propFile.exists()) propFile.inputStream().use { load(it) }\n"
        "}\n\n"
    )
    text = text[: m.start()] + loader + text[m.start():]

    m = re.search(r"^android\s*\{", text, re.M)
    signing_block = (
        "\n    signingConfigs {\n"
        '        if (keystoreProperties.getProperty("storeFile") != null) {\n'
        '            create("release") {\n'
        '                storeFile = rootProject.file(keystoreProperties.getProperty("storeFile"))\n'
        '                storePassword = keystoreProperties.getProperty("storePassword")\n'
        '                keyAlias = keystoreProperties.getProperty("keyAlias")\n'
        '                keyPassword = keystoreProperties.getProperty("keyPassword")\n'
        "            }\n"
        "        }\n"
        "    }\n"
    )
    text = text[: m.end()] + signing_block + text[m.end():]

    m = re.search(r'getByName\("release"\)\s*\{', text)
    if not m:
        log("!! buildTypes 里没有 release，签名配置未接线")
        return False
    wiring = (
        "\n            if (keystoreProperties.getProperty(\"storeFile\") != null) {\n"
        '                signingConfig = signingConfigs.getByName("release")\n'
        "            }"
    )
    text = text[: m.end()] + wiring + text[m.end():]

    log(f"+ app/build.gradle.kts: release 签名（{store_file} / alias={props['keyAlias']}）")
    if not dry:
        gradle.write_text(text, encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description="合并 android-overlay 到 Tauri Android 工程")
    parser.add_argument("--project", type=Path, default=DEFAULT_PROJECT,
                        help=f"gen/android 目录（默认 {DEFAULT_PROJECT}）")
    parser.add_argument("--dry-run", action="store_true", help="只打印，不写文件")
    args = parser.parse_args()

    project: Path = args.project.resolve()
    if not project.is_dir():
        print(f"✗ 目录不存在: {project}")
        print("  先执行:  npm run android:init")
        return 1

    print(f"→ 目标工程: {project}")
    if args.dry_run:
        print("→ DRY RUN（不会写文件）\n")

    n = copy_sources(project, args.dry_run)
    print()
    merge_manifest(project, args.dry_run)
    inject_media_button_receiver(project, args.dry_run)
    copy_proguard(project, args.dry_run)
    patch_gradle(project, args.dry_run)
    patch_min_sdk(project, args.dry_run)
    patch_signing(project, args.dry_run)

    print()
    print(f"✓ 完成，{n} 个 Kotlin 文件已同步")
    print("  下一步: npm run android:dev")
    return 0


if __name__ == "__main__":
    sys.exit(main())
