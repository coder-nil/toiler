import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import test from "node:test";
import postcss from "postcss";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { codeLanguageIconFile } = await jiti.import("../lib/language-icons.ts");
const { resolveFileIconName } = await jiti.import("../lib/file-icon-names.ts");

const css = postcss.parse(await readFile(new URL("../app/globals.css", import.meta.url), "utf8"));
const desktop = await readFile(new URL("../components/MermaidBlock.tsx", import.meta.url), "utf8");
const mobile = await readFile(new URL("../components/MobileMarkdown.tsx", import.meta.url), "utf8");
const langLabel = await readFile(new URL("../components/MarkdownCodeLang.tsx", import.meta.url), "utf8");

function declarations(selector) {
  const values = {};
  css.walkRules((rule) => {
    if (rule.selector !== selector) return;
    rule.walkDecls((decl) => { values[decl.prop] = decl.value; });
  });
  return values;
}

/** 取出所有 `markdown-code-actions` 容器的源码：图标只该出现在动作区里。 */
function actionRegions(source) {
  const regions = [];
  const marker = '<div className="markdown-code-actions">';
  let index = source.indexOf(marker);
  while (index >= 0) {
    regions.push(source.slice(index, source.indexOf("</div>", index)));
    index = source.indexOf(marker, index + marker.length);
  }
  assert.ok(regions.length > 0, "代码块头部缺少 markdown-code-actions 容器");
  return regions;
}

test("桌面代码块的复制按钮带图标，并随复制状态切换", () => {
  // MermaidBlock 里有两个动作区：mermaid 预览按钮在前，复制按钮在后。
  const copyRegion = actionRegions(desktop).at(-1);
  assert.match(copyRegion, /<Check[\s\S]*?:\s*<Copy/, "复制按钮缺少 Copy / Check 图标分支");
  assert.match(desktop, /import \{[^}]*\bCheck\b[^}]*\bCopy\b[^}]*\} from "lucide-react"/);
});

test("mermaid 预览按钮的图标跟着动作语义走", () => {
  // 预览态按钮的动作是「看源代码」，源码态的动作是「看预览」。
  assert.match(desktop, /previewVisible\s*\?\s*<Code[\s\S]*?:\s*<Eye/);
  assert.match(desktop, /previewVisible \? t\("i18n\.source"\) : t\("i18n\.preview"\)/);
});

test("mermaid 预览按钮与复制按钮同处 actions 容器", () => {
  // 之前预览按钮直接挂在 header 上，靠 header 的 space-between 贴右；进到
  // markdown-code-actions 里才能和其它代码块共用同一套间距与图标样式。
  assert.match(
    desktop,
    /<MarkdownCodeLang language="mermaid" \/>\s*<div className="markdown-code-actions">\{previewButton\}<\/div>/,
  );
});

test("手机代码块头部沿用同一套图标", () => {
  const copyRegion = actionRegions(mobile).at(-1);
  assert.match(copyRegion, /<Check[\s\S]*?:\s*<Copy/, "手机复制按钮缺少 Copy / Check 图标分支");
  assert.match(mobile, /import \{[^}]*\bCheck\b[^}]*\bCopy\b[^}]*\} from "lucide-react"/);
});

test("图标尺寸与头部 12px 字号匹配，且不被挤压换行", () => {
  for (const [name, source] of [["MermaidBlock", desktop], ["MobileMarkdown", mobile]]) {
    for (const [, size] of source.matchAll(/<(?:Check|Copy|Code|Eye) size=\{(\d+)\}/g)) {
      assert.equal(size, "13", `${name} 的图标尺寸应统一为 13px`);
    }
  }
  const action = declarations(".markdown-code-action");
  assert.equal(action.display, "inline-flex");
  assert.equal(action["align-items"], "center");
  assert.equal(action.gap, "4px");
  assert.equal(declarations(".markdown-code-action > svg")["flex-shrink"], "0");
});

test("桌面与手机的语言标签都渲染同一条图标链路", () => {
  // 语言标签抽成 MarkdownCodeLang 后，两端必须共用它，否则「哪里带图标」会分叉。
  assert.match(desktop, /<MarkdownCodeLang language=\{lang\} \/>/);
  assert.match(desktop, /<MarkdownCodeLang language="mermaid" \/>/);
  assert.match(mobile, /<MarkdownCodeLang language=\{lang\} \/>/);
  assert.match(langLabel, /getCodeLanguageIcon\(label, 13\)/);
});

test("围栏语言到文件图标的映射都能落到真实存在的 SVG 上", async () => {
  const latte = await readdir(new URL("../public/icons/catppuccin/latte/", import.meta.url));
  const mocha = await readdir(new URL("../public/icons/catppuccin/mocha/", import.meta.url));
  for (const language of ["css", "ts", "tsx", "js", "jsx", "json", "md", "yaml", "toml", "py", "rs", "go", "sh", "sql", "dockerfile"]) {
    const file = codeLanguageIconFile(language);
    assert.ok(file, `${language} 应该有映射的文件名`);
    const icon = resolveFileIconName(file);
    assert.ok(icon, `${language} → ${file} 解析不到图标名`);
    assert.ok(latte.includes(`${icon}.svg`), `${icon}.svg 在 latte 里不存在`);
    assert.ok(mocha.includes(`${icon}.svg`), `${icon}.svg 在 mocha 里不存在`);
  }
});

test("没有对应文件图标的语言不加图标，也不落到通用文件图标", () => {
  // ```text / ```diff / ```mermaid 这类没有文件形态的语言：头部只留文字。
  for (const language of ["", "text", "diff", "mermaid", "log", "makefile"]) {
    assert.equal(codeLanguageIconFile(language), null, `${language} 不该有图标映射`);
  }
});

test("语言标签的图标与文字同行，并用 Catppuccin 原色", () => {
  const lang = declarations(".markdown-code-lang");
  assert.equal(lang.display, "inline-flex");
  assert.equal(lang["align-items"], "center");
  assert.equal(lang.gap, "5px");

  // 全局 .catppuccin-file-icon 把彩色 SVG 当遮罩涂成 --text-dim（文件树要的就是
  // 不抢读），代码块头部则要看得出语言，所以这里必须显式把 mask 拿掉并回到
  // 彩色背景，深色主题再换成 mocha 那套 SVG。两条规则都要比全局规则的特异性
  // 高，否则会被 @supports 里的那条遮罩规则盖掉。
  const colorful = declarations(".markdown-code-lang .catppuccin-file-icon");
  assert.equal(colorful.mask, "none");
  assert.equal(colorful["-webkit-mask"], "none");
  assert.match(colorful.background, /var\(--catppuccin-icon-light\)/);

  const dark = declarations("html.dark .markdown-code-lang .catppuccin-file-icon");
  assert.match(dark.background, /var\(--catppuccin-icon-dark\)/);
  assert.equal(dark.mask, "none");
});
