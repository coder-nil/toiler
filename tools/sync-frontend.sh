#!/usr/bin/env bash
# 把 pi-desktop 的最新前端同步进 toiler 的 release 客户端。
#
# 用法：
#   tools/sync-frontend.sh                 # 构建前端 → 同步 → 重编 → 启动
#   tools/sync-frontend.sh --no-launch     # 只重编，不启动
#   tools/sync-frontend.sh --skip-export   # 不重新构建前端，直接同步现有导出
#
# 产物落点沿用 pi-desktop 自己的约定：`<pi-desktop>/src-tauri/frontend`
# （那里被 pi-desktop 的 .gitignore 忽略）。toiler 只是把它当作
# `TOILER_FRONTEND_DIR` 读进来编译，**不往本仓库写前端产物** ——
# `crates/toiler-server/frontend/` 是提交在版本库里的占位页，不能覆盖。
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
#    crates/toiler-server/build.rs）。只换产物不重编，桌面版还是旧页面。

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
# `/api/models-config` 返回的字段），而 toiler 读的是
# `<toiler home>/configs/models.json`。标签与实际不符会让人以为配置没生效，
# 所以同步后改掉。
#
# 只改 `$target`（pi-desktop 仓库里被 gitignore 的产物副本），**不动
# `$export_dir`**：那才是 pi-desktop 自己的导出目录，改它会污染下一次构建。
# 重新导出会把原文写回来，所以这段补丁每次同步都要重跑。
#
# 找不到这个字面量不算错：上游改成从 API 读路径时它就会自然消失。
models_label='~/.pi/agent/models.json'
models_real='~/.agents/toiler/configs/models.json'
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
# 5. 页面里的品牌名：Pi Desktop → Toiler
# ---------------------------------------------------------------------------
# 前端产物把产品名写死成 `Pi Desktop`：窗口标题、关于面板、PWA manifest、
# 离线页、启动错误页、service worker 日志、更新日志——用户看到的就是这几处。
#
# 只换**显示字符串**，下面两类东西不碰：
#
# - 类名 `pi-desktop`：样式表按它选中根元素，改了直接白屏；
# - `sw.js` 的缓存前缀与 `__PI_WEB_*` 全局量：前者是缓存键，后者与 Rust 侧
#   注入的同名全局量成对出现（`lan_proxy.rs` / `lib.rs`）。
#
# 图标库自带的 README 也一并改：它随产物内嵌，能被浏览器直接打开，
# 里面写的「用于浅色/深色主题」同样指本产品。
#
# 与第 4 节同理：只改 `$target`；重新导出会写回原文，所以每次同步都要重跑。
brand_old='Pi Desktop'
brand_new='Toiler'
branded=$(grep -rlIF "$brand_old" "$target" 2>/dev/null || true)
if [[ -z "$branded" ]]; then
  echo "品牌文案无需修正（产物里没有 $brand_old）"
else
  branded_count=0
  while IFS= read -r file; do
    perl -pi -e "s/\Q${brand_old}\E/${brand_new}/g" "$file"
    branded_count=$((branded_count + 1))
  done <<< "$branded"
  echo "已把 $branded_count 个文件里的 $brand_old 改为 $brand_new"
fi

# ---------------------------------------------------------------------------
# 6. 关于面板：去掉 pi 的版本号
# ---------------------------------------------------------------------------
# 上游把产品版本和 pi 版本并列显示（`v0.1.0 · pi v1.4.0`）。toiler 的后端是 rpi
# 而不是 pi，写在一起会让人以为界面版本来自 pi，所以只留产品自己的版本。
#
# 两处文案，都在 `app/page-*.js`：
# - i18n 的 `settings.aboutDescription`（中英各一份，`\xb7` 是转义后的 `·`）；
# - 「复制版本信息」拼的数组里的 `pi v${B}` 一项。
#
# 与第 4、5 节同理：只改 `$target`，重新导出后要重跑。
about_fixed=0
for chunk in "$target"/_next/static/chunks/app/page-*.js; do
  [[ -f "$chunk" ]] || continue
  if grep -qF 'pi v{piVersion}' "$chunk" || grep -qF '`pi v${B}`' "$chunk"; then
    perl -pi -e 's/v\{version\} \\xb7 pi v\{piVersion\}/v{version}/g;
                 s/`pi v\$\{B\}`,\s*//g' "$chunk"
    about_fixed=$((about_fixed + 1))
  fi
done
if (( about_fixed > 0 )); then
  echo "已从 $about_fixed 个 chunk 去掉关于面板里的 pi 版本号"
else
  echo "关于面板无需修正（产物里没有 pi 版本号文案）"
fi

# ---------------------------------------------------------------------------
# 7. 设置各分区标题：去掉旁边的路径
# ---------------------------------------------------------------------------
# 插件 / 模型 / 技能三个面板的标题行都是「标题 + <code>路径</code>」：
#
# - 插件、模型：紧跟标题的 `<code>`（项目 cwd / 模型配置文件路径）；
# - 技能：标题后还有一个 loading 图标，`<code>` 排在它后面。
#
# 左侧栏本来就常驻显示当前项目，标题旁再挂一条绝对路径只是噪声；模型页那条
# 写的是本机配置文件路径，也没必要占位。三处都删 `<code>`，保留标题与其它元素。
#
# 压缩后变量名随构建变，所以用 `children:x("common.<key>")` 作锚点；技能页那条
# 依赖它独有的样式（`maxWidth:320`）定位。详情面板里带标签的 `CWD` / 安装路径不动。
header_path_fixed=0
for chunk in "$target"/_next/static/chunks/app/page-*.js; do
  [[ -f "$chunk" ]] || continue
  if grep -qE '"common\.(plugins|models|skills)"' "$chunk"; then
    perl -0pi -e '
      my $obj  = qr/\{(?:[^{}]|\{[^{}]*\})*\}/;
      my $id   = qr/[A-Za-z_\$][\w\$]*/;
      my $tail = qr/\}\s*\)/;
      # 插件 / 模型：`<code>` 紧跟在标题 span 后面
      s/(children:$id\("common\.(?:plugins|models)"\)$tail),\(\s*0,$id\.jsx\)\("code",$obj\)/$1/g;
      # 技能：标题与 `<code>` 之间还隔着 loading 图标（条件渲染）
      s/,\s*\(\s*0,$id\.jsx\)\("code",\{style:\{fontSize:11,color:"var\(--text-muted\)",fontFamily:"var\(--font-mono\)",maxWidth:320,overflow:"hidden",textOverflow:"ellipsis",whiteSpace:"nowrap"\},children:$id\(e\)\}\)//g;
    ' "$chunk"
    header_path_fixed=$((header_path_fixed + 1))
  fi
done
if (( header_path_fixed > 0 )); then
  echo "已从 $header_path_fixed 个 chunk 去掉插件/模型/技能标题旁的路径"
else
  echo "设置标题无需修正（产物里没有标题旁路径）"
fi

# ---------------------------------------------------------------------------
# 8. MCP 页：全局配置文件的路径文案
# ---------------------------------------------------------------------------
# 前端把 global 那份写死成 pi 的 `~/.pi/agent/mcp.json`，而后端（`routes/mcp.rs`）
# 读写的是 `<configs>/mcp.json`（与 models.json 同一个目录）。不统一的话，页面上
# 写的路径与实际落的文件对不上。
mcp_patched=0
for chunk in "$target"/_next/static/chunks/app/page-*.js; do
  [[ -f "$chunk" ]] || continue
  if grep -qF '~/.pi/agent/mcp.json' "$chunk"; then
    perl -pi -e 's|~/\\.pi/agent/mcp\\.json|~/.agents/toiler/configs/mcp.json|g' "$chunk"
    mcp_patched=$((mcp_patched + 1))
  fi
done
if (( mcp_patched > 0 )); then
  echo "已把 $mcp_patched 个 chunk 里的全局 MCP 配置路径改为 toiler 的 configs/"
else
  echo "MCP 配置路径无需修正（产物里没有该字面量）"
fi

# ---------------------------------------------------------------------------
# 9. 设置侧边栏：把「快捷键」挪到「手机访问」下面
# ---------------------------------------------------------------------------
# 导航是写死的静态数组（`eP`）：general / shortcuts / plugins / models / skills /
# mcp / mobile。快捷键是低频入口，排到最后更顺手：把 shortcuts 那一项摘下来，
# 接到 mobile 后面（它原本是最后一项，所以不加尾逗号）。
setting_nav_fixed=0
for chunk in "$target"/_next/static/chunks/app/page-*.js; do
  [[ -f "$chunk" ]] || continue
  if grep -qF 'id:"shortcuts"' "$chunk"; then
    perl -0pi -e '
      my $sc;
      s/(\{id:"shortcuts",[^{}]*\}),/do { $sc = $1; "" }/e;
      s/(\{id:"mobile",[^{}]*\})(?!\s*,)/$1 . "," . $sc/e if $sc;
    ' "$chunk"
    setting_nav_fixed=$((setting_nav_fixed + 1))
  fi
done
if (( setting_nav_fixed > 0 )); then
  echo "已把 $setting_nav_fixed 个 chunk 的快捷键菜单项挪到手机访问之后"
else
  echo "设置导航无需修正（产物里没有该结构）"
fi

# ---------------------------------------------------------------------------
# 10. 重新编译（+ 启动）
# ---------------------------------------------------------------------------
if (( launch )); then
  # run-desktop.sh 内部的 build-release.sh 会自动探测到同一个目录
  exec "$root/tools/run-desktop.sh"
else
  exec bash "$root/tools/frontend/build-release.sh" "$target"
fi
