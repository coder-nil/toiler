import type { CSSProperties } from "react";
import { codeLanguageIconFile } from "@/lib/language-icons";
import { resolveFileIconName, type CatppuccinIconName } from "@/lib/file-icon-names";


interface IconProps {
  size?: number;
}

const CATPPUCCIN_ICONS_ROOT = "/icons/catppuccin";

function CatppuccinIcon({ name, size = 14 }: IconProps & { name: CatppuccinIconName }) {
  const style = {
    width: size,
    height: size,
    "--catppuccin-icon-light": `url(${CATPPUCCIN_ICONS_ROOT}/latte/${name}.svg)`,
    "--catppuccin-icon-dark": `url(${CATPPUCCIN_ICONS_ROOT}/mocha/${name}.svg)`,
  } as CSSProperties;

  return (
    <span
      aria-hidden="true"
      className="catppuccin-file-icon"
      style={style}
    />
  );
}

export function FolderIcon({ size = 14, open = false }: IconProps & { open?: boolean }) {
  return <CatppuccinIcon name={open ? "_folder_open" : "_folder"} size={size} />;
}

export function GenericFileIcon({ size = 14 }: IconProps) {
  return <CatppuccinIcon name="_file" size={size} />;
}

export function getFileIcon(name: string, size = 14): React.ReactNode {
  const icon = resolveFileIconName(name);
  return icon ? <CatppuccinIcon name={icon} size={size} /> : <GenericFileIcon size={size} />;
}

/**
 * 代码块头部用：围栏语言 → 文件图标，映射不到返回 `null`。
 *
 * 与 `getFileIcon` 的区别只在兜底：文件面板里未知扩展名必须画点什么（否则整行
 * 缩进会塌），而代码块头部没有图标是正常态（```text、```diff、```mermaid），
 * 塞一个通用文件图标反而让人以为那是个真文件。
 *
 * 图标本身复用 `.catppuccin-file-icon`，与文件面板、TabBar、聊天提及里用的是同一
 * 份 SVG；唯一的区别是配色：`globals.css` 里那条 `@supports` 遮罩规则把文件树
 * 的图标统一涂成 `--text-dim`（不抢读），而代码块头部保留 Catppuccin 原色，因为
 * 那里的图标是辨别语言的锥点。尺寸不走内联 px，而是按 13px 传给 `size`，与
 * header 的 12px 字号匹配。
 */
export function getCodeLanguageIcon(language: string | null | undefined, size = 13): React.ReactNode {
  const file = codeLanguageIconFile(language);
  if (!file) return null;

  const icon = resolveFileIconName(file);
  return icon ? <CatppuccinIcon name={icon} size={size} /> : null;
}
