"use client";

import type { ComponentPropsWithoutRef } from "react";
import { isOpenableExternalUrl, takeOverExternalLink } from "@/lib/open-external-url";

type ExternalLinkProps = Omit<ComponentPropsWithoutRef<"a">, "href" | "target" | "rel" | "onClick"> & {
  href: string;
};

/** 桌面 WebView 不会为普通外链创建新窗口，浏览器端则应保留原生链接行为。 */
export function ExternalLink({ href, ...props }: ExternalLinkProps) {
  if (!isOpenableExternalUrl(href)) return <a {...props} />;

  return (
    <a
      {...props}
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      onClick={(event) => { takeOverExternalLink(event, event.currentTarget.href); }}
    />
  );
}
