#!/usr/bin/env python3
"""从 pi-desktop 的 Next.js 编译产物里抽出「业务模块」源码。

产物结构：
  .next/server/app/api/**/route.js   ← 每个 route 一个 bundle，含 userland handler
  .next/server/chunks/*.js           ← 共享模块，模块 id 与 route.js 同一命名空间

用法：
  python3 tools/pi-contract/extract.py route sessions            # 抽 /api/sessions 的 handler
  python3 tools/pi-contract/extract.py module 40402              # 抽共享模块 40402
  python3 tools/pi-contract/extract.py modules 40402 56429       # 一次抽多个
  python3 tools/pi-contract/extract.py all                       # 全部 route → docs/contracts/

产物没有美化，但箭头函数体是括号配平的，所以按深度扫即可。
"""

import os
import re
import sys

ROOT = "/Applications/Pi Desktop.app/Contents/Resources/runtime/server/.next/server"
OUT_DIR = os.path.join(os.path.dirname(__file__), "raw")

DEF_RE = re.compile(r"(\d{3,6}):\((\w+),(\w+),(\w+)\)=>")


def all_js():
    for base, _dirs, files in os.walk(ROOT):
        for f in files:
            if f.endswith(".js"):
                yield os.path.join(base, f)


def slice_module(text: str, def_end: int):
    """从模块定义起点扫到配平。"""
    depth = 0
    started = False
    i = def_end
    in_str = None
    while i < len(text):
        ch = text[i]
        if in_str:
            if ch == "\\":
                i += 2
                continue
            if ch == in_str:
                in_str = None
        elif ch in "\"'`":
            in_str = ch
        elif ch == "{":
            depth += 1
            started = True
        elif ch == "}":
            depth -= 1
            if started and depth == 0:
                return text[def_end : i + 1]
        i += 1
    return text[def_end : def_end + 8000]


def route_file(route_path: str):
    return os.path.join(ROOT, "app/api", route_path, "route.js")


def extract_route(route_path: str) -> str:
    path = route_file(route_path)
    if not os.path.exists(path):
        return f"// 没有这个 route：{path}\n"
    with open(path, encoding="utf-8", errors="replace") as fh:
        text = fh.read()

    parts = [f"// route /api/{route_path}"]
    for m in DEF_RE.finditer(text):
        src = slice_module(text, m.end())
        # handler 模块一定导出 GET/POST/… 之一
        if not re.search(r"c\.d\(b,\{(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)", src):
            continue
        parts.append(f"\n// ===== handler module {m.group(1)} =====\n{src}\n")
    return "\n".join(parts)


def find_module(module_id: str):
    hits = []
    needle = f"{module_id}:("
    for path in all_js():
        with open(path, encoding="utf-8", errors="replace") as fh:
            text = fh.read()
        for m in re.finditer(re.escape(needle), text):
            src = slice_module(text, m.end())
            hits.append((path, src))
    return hits


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return
    cmd = sys.argv[1]
    if cmd == "route":
        print(extract_route(sys.argv[2]))
    elif cmd == "module":
        for mid in sys.argv[2:]:
            for path, src in find_module(mid):
                print(f"// ===== {mid} @ {os.path.basename(path)} =====\n{src}\n")
    elif cmd == "all":
        os.makedirs(OUT_DIR, exist_ok=True)
        import json

        manifest = json.load(
            open(os.path.join(ROOT, "..", "app-path-routes-manifest.json"))
        )
        n = 0
        for value in manifest.values():
            if not value.startswith("/api"):
                continue
            rel = value[len("/api/") :]
            body = extract_route(rel)
            name = rel.replace("/", "_") + ".js"
            with open(os.path.join(OUT_DIR, name), "w", encoding="utf-8") as fh:
                fh.write(body)
            n += 1
        print(f"抽出 {n} 个 route → {OUT_DIR}")
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
