import type { MetadataRoute } from "next";

export const dynamic = "force-static";

export default function manifest(): MetadataRoute.Manifest {
  return {
    id: "/",
    // PWA 安装名：与 app/layout.tsx 的 metadata.title 保持一致，改成 Toiler。
    name: "Toiler",
    short_name: "Toiler",
    description: "Local desktop interface for the pi coding agent",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#1a1a1a",
    theme_color: "#1a1a1a",
    categories: ["developer", "productivity"],
    lang: "en",
    icons: [
      {
        src: "/icons/icon-192.png",
        sizes: "192x192",
        type: "image/png",
        purpose: "any",
      },
      {
        src: "/icons/icon-512.png",
        sizes: "512x512",
        type: "image/png",
        purpose: "any",
      },
    ],
  };
}
