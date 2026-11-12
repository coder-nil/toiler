import type { Metadata, Viewport } from "next";
import { Noto_Sans_Mono } from "next/font/google";
import { PwaRegistration } from "@/components/PwaRegistration";
import { CHAT_APPEARANCE_INIT_SCRIPT } from "@/lib/chat-appearance";
import "katex/dist/katex.min.css";
import "./globals.css";

const notoSansMono = Noto_Sans_Mono({
  subsets: ["latin", "cyrillic"],
  variable: "--font-noto-mono",
  display: "swap",
});

// 品牌名不跟上游走：本应用叫 Toiler，不叫 Pi Desktop。
// 这是从 pi-desktop 搬过来的源码，上游把这些显示串写死成 "Pi Desktop"；
// 以前靠 tools/sync-frontend.sh 在构建后 sed 产物来改，现在源码就在本仓库，
// 直接在源头改，免得每次重建又退回「Pi Desktop」。
//
// 只改用户能看到的显示串。以下两类**不能动**：
// - `pi-desktop` 类名（layout.tsx 里给根元素加的那个）：样式表按它选中；
// - `__PI_WEB_*` 全局量：与 Rust 侧 `lib.rs` 注入的同名量成对出现。
export const metadata: Metadata = {
  title: "Toiler",
  description: "Toiler interface for the pi coding agent",
  applicationName: "Toiler",
  manifest: "/manifest.webmanifest",
  icons: {
    icon: [
      {
        url: "/icons/icon-192.png",
        sizes: "192x192",
        type: "image/png",
      },
    ],
    apple: [
      {
        url: "/icons/apple-touch-icon.png",
        sizes: "180x180",
        type: "image/png",
      },
    ],
  },
  appleWebApp: {
    capable: true,
    statusBarStyle: "black-translucent",
    title: "Toiler",
  },
  formatDetection: {
    telephone: false,
  },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover",
  interactiveWidget: "resizes-content",
  themeColor: [
    { media: "(prefers-color-scheme: light)", color: "#ffffff" },
    { media: "(prefers-color-scheme: dark)", color: "#1a1a1a" },
  ],
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" translate="no" className={`${notoSansMono.variable} notranslate`} suppressHydrationWarning>
      <head>
        <meta name="google" content="notranslate" />
        {process.env.NODE_ENV !== "production" && (
          <script
            dangerouslySetInnerHTML={{
              // A production PWA worker may still control this origin when switching
              // back to `next dev`, where its cached chunks are invalid for HMR.
              __html: `(function(){if(!("serviceWorker" in navigator))return;navigator.serviceWorker.getRegistrations().then(function(registrations){return Promise.all(registrations.filter(function(registration){try{return new URL(registration.active?.scriptURL||registration.waiting?.scriptURL||registration.installing?.scriptURL,location.href).pathname==="/sw.js"}catch(e){return false}}).map(function(registration){return registration.unregister()}))}).then(function(){return caches.keys()}).then(function(keys){return Promise.all(keys.filter(function(key){return key.indexOf("pi-desktop-")===0}).map(function(key){return caches.delete(key)}))}).then(function(){if(navigator.serviceWorker.controller)location.reload()}).catch(function(){})})();`,
            }}
          />
        )}
        <script
          dangerouslySetInnerHTML={{
            __html: `(function(){try{var t=localStorage.getItem("pi-theme");var dark=t==="dark"||((t==null||t===""||t==="auto")&&window.matchMedia("(prefers-color-scheme: dark)").matches);if(dark)document.documentElement.classList.add("dark")}catch(e){}if(window.__PI_WEB_DESKTOP__)document.documentElement.classList.add("pi-desktop")})();`,
          }}
        />
        <script
          dangerouslySetInnerHTML={{
            __html: CHAT_APPEARANCE_INIT_SCRIPT,
          }}
        />
        <script
          dangerouslySetInnerHTML={{
            __html: `(function(){var api=window.__PI_WEB_API_ORIGIN__;if(!api)return;api=api.replace(/\\/$/,"");var isApi=function(value){return typeof value==="string"&&value.indexOf("/api/")===0||value==="/api"};var rewrite=function(value){return isApi(value)?api+value:value};var nativeFetch=window.fetch.bind(window);window.fetch=function(input,init){if(typeof input==="string")return nativeFetch(rewrite(input),init);if(input instanceof URL)return nativeFetch(rewrite(input.toString()),init);if(input instanceof Request&&isApi(new URL(input.url).pathname))return nativeFetch(new Request(api+new URL(input.url).pathname+new URL(input.url).search,input),init);return nativeFetch(input,init)};var NativeEventSource=window.EventSource;window.EventSource=function(url,config){return new NativeEventSource(rewrite(String(url)),config)};window.EventSource.prototype=NativeEventSource.prototype;var nativeOpen=window.open;window.open=function(url,target,features){return nativeOpen.call(window,rewrite(String(url)),target,features)};var rewriteLink=function(link){var href=link.getAttribute("href");if(href&&isApi(href))link.setAttribute("href",rewrite(href))};var rewriteLinks=function(root){if(root instanceof HTMLAnchorElement)rewriteLink(root);root.querySelectorAll&&root.querySelectorAll("a[href]").forEach(rewriteLink)};document.addEventListener("DOMContentLoaded",function(){rewriteLinks(document);new MutationObserver(function(records){records.forEach(function(record){record.addedNodes.forEach(function(node){if(node instanceof Element)rewriteLinks(node)})})}).observe(document.documentElement,{childList:true,subtree:true})})})();`,
          }}
        />
      </head>
      <body translate="no" className="notranslate" suppressHydrationWarning>
        {children}
        <PwaRegistration />
      </body>
    </html>
  );
}
