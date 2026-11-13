#!/usr/bin/env bash
# 一键编译并启动桌面客户端。
#
# 用法：
#   tools/run-desktop.sh                  # 前端无改动则跳过重建 → 编译 → 重启
#   tools/run-desktop.sh --rebuild-frontend  # 强制重建前端（改了 next.config / 依赖时用）
#   tools/run-desktop.sh --skip-frontend  # 完全不碰前端，只编译 Rust（调试 Rust 时最快）
#   tools/run-desktop.sh --no-build       # 跳过编译，只重启（二进制已是最新时用）
#   tools/run-desktop.sh --foreground     # 前台运行，日志直接打到终端
#
# 为什么必须先编译再启动：
# `toiler-desktop` 是单二进制 —— 前端产物和 axum 服务都 `include_dir!` 嵌在里面。
# 改了 Rust 代码（或换了前端产物）却不重编，跑的还是旧代码，表现就是「明明修了
# 却还是老 bug」。本脚本默认每次都走一遍增量编译，就是为了把这个坑堵死；编译
# 失败时**绝不**去启动旧二进制。
#
# 前端现在在本仓库里编译（源码见 crates/toiler-desktop/frontend），产物写到该
# 目录的 dist/，由 crates/toiler-server/build.rs 自动发现。所以本脚本不再需要
# `TOILER_FRONTEND_DIR`，也不会去捡 pi-desktop 仓库里的旧产物。
#
# 端口由内核分配（`port = 0`），启动后脚本会从监听句柄反查出实际端口并打印。

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/release/toiler-desktop"
frontend_dir="$root/crates/toiler-desktop/frontend"
dist_dir="$frontend_dir/dist"
log_dir="$root/.toiler-local"
log="$log_dir/desktop.log"

build=1
foreground=0
rebuild_frontend=0
skip_frontend=0

usage() {
  sed -n '2,19p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) build=0; shift ;;
    --foreground|-f) foreground=1; shift ;;
    --rebuild-frontend) rebuild_frontend=1; shift ;;
    --skip-frontend) skip_frontend=1; shift ;;
    --help|-h) usage; exit 0 ;;
    -*)
      echo "未知参数：$1" >&2
      echo >&2
      usage >&2
      exit 2
      ;;
    *)
      echo "未知参数：$1（前端产物目录不再是参数，源码已在本仓库内）" >&2
      exit 2
      ;;
  esac
done

if (( rebuild_frontend && skip_frontend )); then
  echo "--rebuild-frontend 与 --skip-frontend 不能同时用。" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# 1. 前端：源码比产物新才重建
# ---------------------------------------------------------------------------
# `npm run build` 要 30 秒左右，每次启动都跑太慢。判据是「源码 mtime 是否比
# dist/index.html 新」：够快，也不会漏掉改动。真正需要重编的边界情况（换了
# 依赖、改了 next.config）用 --rebuild-frontend 覆盖。
frontend_changed() {
  [[ -f "$dist_dir/index.html" ]] || return 0
  # 只比较会进产物的源码目录与配置文件；node_modules 与 dist 自身排除在外。
  local newer
  newer="$(find "$frontend_dir" \
    \( -name node_modules -o -name dist -o -name .next \) -prune -o \
    -type f \( -name '*.ts' -o -name '*.tsx' -o -name '*.css' -o -name '*.mjs' \
                -o -name '*.json' -o -name '*.html' -o -name '*.png' \
                -o -name '*.ico' -o -name '*.svg' -o -name '*.webp' \) -newer "$dist_dir/index.html" -print \
    2>/dev/null | head -1)"
  [[ -n "$newer" ]]
}

if (( build && ! skip_frontend )); then
  if (( rebuild_frontend )) || frontend_changed; then
    echo "构建前端（源码有更新或强制重建）…"
    if [[ ! -d "$frontend_dir/node_modules" ]]; then
      echo "缺少 node_modules，先在 $frontend_dir 里跑 npm install" >&2
      exit 1
    fi
    ( cd "$frontend_dir" && npm run build )
  else
    echo "前端无改动，跳过重建（--rebuild-frontend 可强制）"
  fi
fi

# ---------------------------------------------------------------------------
# 2. 编译
# ---------------------------------------------------------------------------
if (( build )); then
  # 编译失败以非零码退出（set -e 会传到我们这里），
  # 所以走到后面的二进制一定是刚刚编出来的。
  cargo build --release -p toiler-desktop --manifest-path "$root/Cargo.toml"
else
  if [[ ! -x "$bin" ]]; then
    echo "还没有编译过：$bin" >&2
    echo "去掉 --no-build 再跑一次。" >&2
    exit 1
  fi
fi

if [[ ! -x "$bin" ]]; then
  echo "编译产物不存在：$bin" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 3. 停掉已在运行的实例
# ---------------------------------------------------------------------------
# 不先停会同时开出两个窗口，而旧窗口跑的是旧代码 —— 正是「改了没生效」的
# 经典错觉。用 SIGTERM 优雅退出：desktop 侧会回收 axum 服务、手机访问代理与
# 隧道（见 `toiler-desktop/src/lib.rs::shutdown`），会话文件是 append-only，
# 不会因此损坏。
existing="$(pgrep -f "$bin" 2>/dev/null || true)"
if [[ -n "$existing" ]]; then
  echo "停止已运行的实例：$(echo "$existing" | tr '\n' ' ')"
  # shellcheck disable=SC2086
  kill $existing 2>/dev/null || true
  for _ in $(seq 1 25); do
    pgrep -f "$bin" >/dev/null 2>&1 || break
    sleep 0.2
  done
  leftover="$(pgrep -f "$bin" 2>/dev/null || true)"
  if [[ -n "$leftover" ]]; then
    echo "优雅退出超时，强制结束：$(echo "$leftover" | tr '\n' ' ')" >&2
    # shellcheck disable=SC2086
    kill -9 $leftover 2>/dev/null || true
    sleep 0.3
  fi
fi

# ---------------------------------------------------------------------------
# 4. 启动
# ---------------------------------------------------------------------------
if (( foreground )); then
  echo "启动（前台）：$bin"
  exec "$bin"
fi

mkdir -p "$log_dir"
# 每轮覆盖写：日志只关心「这一次启动发生了什么」。
: >"$log"
"$bin" >"$log" 2>&1 &
pid=$!

# 2 秒内就退出说明启动失败（缺资源、端口、配置损坏等），把这轮的日志直接
# 摊出来，省得再去找文件。
sleep 2
if ! kill -0 "$pid" 2>/dev/null; then
  echo "启动失败，日志：" >&2
  cat "$log" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 5. 反查实际端口（内核分配，日志里没有）
# ---------------------------------------------------------------------------
port=""
for _ in $(seq 1 20); do
  port="$(
    lsof -nP -a -p "$pid" -iTCP -sTCP:LISTEN 2>/dev/null \
      | awk '/127\.0\.0\.1:/ {n = split($9, a, ":"); print a[n]; exit}'
  )"
  [[ -n "$port" ]] && break
  sleep 0.2
done

echo
echo "客户端已启动（pid ${pid}）"
if [[ -n "$port" ]]; then
  echo "  界面：http://127.0.0.1:${port}/"
else
  echo "  （没查到监听端口，窗口可能仍在初始化）"
fi
echo "  日志：$log"
echo "  停止：kill ${pid}"
