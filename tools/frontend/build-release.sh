#!/usr/bin/env bash
# 用一个**外部**的前端产物目录构建 release 二进制。
#
# 常规路径不需要这个脚本：前端源码就在 `crates/toiler-desktop/frontend/`，
# 跑 `npm run build` 产出 `dist/` 后直接 `cargo build -p toiler-desktop`，
# `build.rs` 会自动捡到。本脚本只用于两种场景：
#
#   1. 用 pi-desktop 仓库里现成的产物（不在本仓库里编译前端）；
#   2. 临时验证一份外部产物，不想动 `frontend/dist/`。
#
# 用法：
#   tools/frontend/build-release.sh                      # 自动探测 pi-desktop 的静态导出产物
#   tools/frontend/build-release.sh /path/to/frontend    # 指定产物目录
#
# 产物要求（Next 的 `output: export` 就是这个形状）：
#   index.html  index.txt  _next/static/...  icons/...  favicon.ico
#
# 产物目录通过 `TOILER_FRONTEND_DIR` 传给编译过程，优先级高于 `build.rs`
# 自动发现的 `frontend/dist/`（见 crates/toiler-server/build.rs）。

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# 第一个位置参数只用来指定前端目录，不能透传给 cargo。
# 否则 `build-release.sh /path/to/frontend` 会让 cargo 拿到一个多余参数：
#   error: unexpected argument '/path/to/frontend' found
have_frontend_arg=0
if [[ $# -gt 0 && "${1:-}" != -* ]]; then
  have_frontend_arg=1
fi
src="${1:-${TOILER_FRONTEND_DIR:-}}"

if [[ -z "$src" ]]; then
  for candidate in \
    "${PI_DESKTOP_ROOT:-}/src-tauri/frontend" \
    "$root/../pi-desktop/src-tauri/frontend" \
    "$HOME/Development/WebStormProjects/pi-desktop/src-tauri/frontend" \
    "/Volumes/Chenxiaoni·SSD/Development/WebStormProjects/pi-desktop/src-tauri/frontend"
  do
    if [[ -f "$candidate/index.html" ]]; then
      src="$candidate"
      break
    fi
  done
fi

if [[ -z "$src" || ! -f "$src/index.html" ]]; then
  cat >&2 <<'MSG'
找不到外部前端产物（需要一个含 index.html 的目录）。

常规做法不需要本脚本——前端源码就在本仓库里：
  cd crates/toiler-desktop/frontend
  npm install && npm run build
  cargo build -p toiler-desktop --release

若确实要用外部产物：
  1. 在 pi-desktop 仓库里跑 `npm run build:desktop-frontend`（产物落在
     src-tauri/frontend/），然后把目录作为参数传给本脚本。
  2. 指定产物目录：tools/frontend/build-release.sh /path/to/frontend
MSG
  exit 1
fi

echo "前端产物：$src"
if ! command -v du >/dev/null 2>&1; then
  echo "（跳过体积统计）"
else
  echo "产物体积：$(du -sh "$src" | cut -f1)"
fi

cd "$root"
# 取出前端参数后剩下的才是给 cargo 的（如 `--target aarch64-apple-darwin`）。
if (( have_frontend_arg )); then
  shift
fi
TOILER_FRONTEND_DIR="$src" cargo build --release -p toiler-desktop "$@"
echo
echo "完成：target/release/toiler-desktop"
ls -lh target/release/toiler-desktop
