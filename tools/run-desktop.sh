#!/usr/bin/env bash
# 一键编译并启动桌面客户端。
#
# 用法：
#   tools/run-desktop.sh                      # 增量编译 + 重启
#   tools/run-desktop.sh --no-build           # 跳过编译，只重启（仅换了前端产物时用）
#   tools/run-desktop.sh /path/to/frontend    # 指定前端产物目录
#   tools/run-desktop.sh --foreground         # 前台运行，日志直接打到终端
#
# 为什么必须先编译再启动：
# `workman-desktop` 是单二进制 —— pi 的前端产物和 axum 服务都 `include_dir!`
# 嵌在里面。改了 Rust 代码（或换了前端产物）却不重编，跑的还是旧代码，表现
# 就是「明明修了却还是老 bug」。本脚本默认每次都走一遍增量编译，就是为了
# 把这个坑堵死；编译失败时**绝不**去启动旧二进制。
#
# 端口由内核分配（`port = 0`），启动后脚本会从监听句柄反查出实际端口并打印。

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/release/workman-desktop"
log_dir="$root/.workman-local"
log="$log_dir/desktop.log"

build=1
foreground=0
frontend=""
have_frontend_arg=0

usage() {
  sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) build=0; shift ;;
    --foreground|-f) foreground=1; shift ;;
    --help|-h) usage; exit 0 ;;
    -*)
      echo "未知参数：$1" >&2
      echo >&2
      usage >&2
      exit 2
      ;;
    *)
      # 第一个位置参数是前端产物目录，不参与后面的参数解析。
      frontend="$1"
      have_frontend_arg=1
      shift
      ;;
  esac
done

# ---------------------------------------------------------------------------
# 1. 编译
# ---------------------------------------------------------------------------
if (( build )); then
  if (( have_frontend_arg )); then
    bash "$root/tools/frontend/build-release.sh" "$frontend"
  else
    bash "$root/tools/frontend/build-release.sh"
  fi
else
  if (( have_frontend_arg )); then
    echo "提示：--no-build 时前端目录参数被忽略。" >&2
  fi
  if [[ ! -x "$bin" ]]; then
    echo "还没有编译过：$bin" >&2
    echo "去掉 --no-build 再跑一次。" >&2
    exit 1
  fi
fi

# 编译失败已在 build-release.sh 里以非零码退出（set -e 会传到我们这里），
# 所以走到这一步的二进制一定是刚刚编出来的。再确认一次存在性。
if [[ ! -x "$bin" ]]; then
  echo "编译产物不存在：$bin" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# 2. 停掉已在运行的实例
# ---------------------------------------------------------------------------
# 不先停会同时开出两个窗口，而旧窗口跑的是旧代码 —— 正是「改了没生效」的
# 经典错觉。用 SIGTERM 优雅退出：desktop 侧会回收 axum 服务、手机访问代理与
# 隧道（见 `workman-desktop/src/lib.rs::shutdown`），会话文件是 append-only，
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
# 3. 启动
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
# 4. 反查实际端口（内核分配，日志里没有）
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
