#!/usr/bin/env bash
# 把 pi-desktop 的最新前端同步进 workman 的 release 客户端。
#
# 用法：
#   tools/sync-frontend.sh                 # 构建前端 → 同步 → 重编 → 启动
#   tools/sync-frontend.sh --no-launch     # 只重编，不启动
#   tools/sync-frontend.sh --skip-export   # 不重新构建前端，直接同步现有导出
#
# 产物落点沿用 pi-desktop 自己的约定：`<pi-desktop>/src-tauri/frontend`
# （那里被 pi-desktop 的 .gitignore 忽略）。workman 只是把它当作
# `WORKMAN_FRONTEND_DIR` 读进来编译，**不往本仓库写前端产物** ——
# `crates/workman-server/frontend/` 是提交在版本库里的占位页，不能覆盖。
#
# 两个必须记住的坑：
#
# 1. 清掉 `__NEXT_PRIVATE_STANDALONE_CONFIG`。
#    从 Pi Desktop 的 agent 会话里跑构建时这个变量会继承下来；Next 见到它就
#    **完全跳过读取 next.config.ts**，改用那份 JSON（函数字段已被序列化丢弃），
#    于是 `next build` 直接报 `TypeError: generate is not a function`，而且
#    `output` / `distDir` 也退回单独部署的默认值，静态导出根本不会发生。
#
# 2. 必须重新编译。
#    前端产物在编译期由 `include_dir!` 嵌进二进制（见
#    crates/workman-server/build.rs）。只换产物不重编，桌面版还是旧页面。

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

skip_export=0
launch=1

usage() {
  sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-export) skip_export=1; shift ;;
    --no-launch) launch=0; shift ;;
    --help|-h) usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------------------
# 1. 定位 pi-desktop
# ---------------------------------------------------------------------------
pi_root="${PI_DESKTOP_ROOT:-}"
if [[ -z "$pi_root" ]]; then
  for candidate in \
    "$root/../pi-desktop" \
    "$HOME/Development/WebStormProjects/pi-desktop" \
    "/Volumes/Chenxiaoni·SSD/Development/WebStormProjects/pi-desktop"
  do
    if [[ -f "$candidate/package.json" ]]; then
      pi_root="$candidate"
      break
    fi
  done
fi
if [[ -z "$pi_root" || ! -f "$pi_root/package.json" ]]; then
  echo "找不到 pi-desktop 仓库；用 PI_DESKTOP_ROOT=/path/to/pi-desktop 指定。" >&2
  exit 1
fi

export_dir="$pi_root/.next-desktop-frontend"
target="$pi_root/src-tauri/frontend"

fingerprint() {
  local dir="$1"
  [[ -d "$dir/_next/static/chunks/app" ]] || { echo "（无）"; return; }
  ls "$dir/_next/static/chunks/app" 2>/dev/null | grep -E '^page-.*\.js$' | head -1 || echo "（无）"
}

# ---------------------------------------------------------------------------
# 2. 构建前端静态导出
# ---------------------------------------------------------------------------
if (( skip_export )); then
  if [[ ! -f "$export_dir/index.html" ]]; then
    echo "--skip-export 但 $export_dir 没有产物，去掉该参数先完整构建一次。" >&2
    exit 1
  fi
  echo "跳过前端构建，复用 $export_dir"
else
  echo "=== 构建 pi-desktop 前端：$pi_root ==="
  (
    cd "$pi_root"
    env -u __NEXT_PRIVATE_STANDALONE_CONFIG -u __NEXT_PRIVATE_ORIGIN \
      npm run build:desktop-frontend
  )
  if [[ ! -f "$export_dir/index.html" ]]; then
    echo "构建结束但没找到 $export_dir/index.html" >&2
    exit 1
  fi
fi

echo "旧前端：$(fingerprint "$target")"
echo "新前端：$(fingerprint "$export_dir")"

# ---------------------------------------------------------------------------
# 3. 同步（整目录替换，避免旧 chunk 哈希残留）
# ---------------------------------------------------------------------------
echo "=== 同步 → $target ==="
rm -rf "$target"
mkdir -p "$target"
cp -R "$export_dir"/. "$target"/
rm -f "$target/.DS_Store"
echo "产物就位：$(du -sh "$target" | cut -f1)"

# ---------------------------------------------------------------------------
# 4. 修正前端硬编码的模型配置路径
# ---------------------------------------------------------------------------
# pi-desktop 的模型设置页把路径写死成 `~/.pi/agent/models.json`
# （`_next/static/chunks/app/page-*.js` 里一个 `<code>` 的字面量，不是
# `/api/models-config` 返回的字段），而 workman 读的是
# `<workman home>/configs/models.json`。标签与实际不符会让人以为配置没生效，
# 所以同步后改掉。
#
# 只改 `$target`（pi-desktop 仓库里被 gitignore 的产物副本），**不动
# `$export_dir`**：那才是 pi-desktop 自己的导出目录，改它会污染下一次构建。
# 重新导出会把原文写回来，所以这段补丁每次同步都要重跑。
#
# 找不到这个字面量不算错：上游改成从 API 读路径时它就会自然消失。
models_label='~/.pi/agent/models.json'
models_real='~/.agents/workman/configs/models.json'
shopt -s nullglob
page_chunks=("$target"/_next/static/chunks/app/page-*.js)
shopt -u nullglob
if (( ${#page_chunks[@]} == 0 )); then
  echo "警告：没找到 app/page-*.js，跳过模型配置路径修正。" >&2
else
  patched=0
  for chunk in "${page_chunks[@]}"; do
    if grep -qF "$models_label" "$chunk"; then
      sed -i '' "s|~/\.pi/agent/models\.json|${models_real}|g" "$chunk"
      patched=$((patched + 1))
    fi
  done
  if (( patched > 0 )); then
    echo "已把 $patched 个 chunk 里的 $models_label 改为 $models_real"
  else
    echo "模型配置路径无需修正（产物里没有该字面量）"
  fi
fi

# ---------------------------------------------------------------------------
# 5. 重新编译（+ 启动）
# ---------------------------------------------------------------------------
if (( launch )); then
  # run-desktop.sh 内部的 build-release.sh 会自动探测到同一个目录
  exec "$root/tools/run-desktop.sh"
else
  exec bash "$root/tools/frontend/build-release.sh" "$target"
fi
