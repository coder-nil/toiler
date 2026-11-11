#!/usr/bin/env bash
# 把前端产物嵌进 workman 的 release 二进制。
#
# 用法：
#   tools/frontend/build-release.sh                      # 自动探测 pi-desktop 的静态导出产物
#   tools/frontend/build-release.sh /path/to/frontend    # 指定产物目录
#
# 产物要求（pi 的 `npm run build:desktop-frontend` 就是这个形状）：
#   index.html  index.txt  _next/static/...  icons/...  favicon.ico
#
# 产物目录只在这里参与编译：`WORKMAN_FRONTEND_DIR` 变化会触发 workman-server
# 重新编译（见 crates/workman-server/build.rs），所以换前端只需重跑本脚本。

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# 第一个位置参数只用来指定前端目录，不能透传给 cargo。
# 否则 `build-release.sh /path/to/frontend` 会让 cargo 拿到一个多余参数：
#   error: unexpected argument '/path/to/frontend' found
have_frontend_arg=0
if [[ $# -gt 0 && "${1:-}" != -* ]]; then
  have_frontend_arg=1
fi
src="${1:-${WORKMAN_FRONTEND_DIR:-}}"

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
找不到前端产物（需要一个含 index.html 的目录）。

两条路：
  1. 在 pi-desktop 仓库里跑 `npm run build:desktop-frontend`（产物落在
     src-tauri/frontend/），然后重跑本脚本并把目录作为参数传进来。
  2. 先不嵌真实前端：直接 `cargo build -p workman-desktop`，二进制里是
     crates/workman-server/frontend/ 的占位页（API 仍然可用）。
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
WORKMAN_FRONTEND_DIR="$src" cargo build --release -p workman-desktop "$@"
echo
echo "完成：target/release/workman-desktop"
ls -lh target/release/workman-desktop
