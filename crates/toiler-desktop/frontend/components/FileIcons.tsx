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
 * 代码块优先复用文件面板的语言图标，没有专用图标时使用通用文件图标。
 * 代码块头部保留 Catppuccin 原色，便于区分已知语言。
 */
export function getCodeLanguageIcon(language: string | null | undefined, size = 13): React.ReactNode {
  const file = codeLanguageIconFile(language);
  return file ? getFileIcon(file, size) : <GenericFileIcon size={size} />;
}
