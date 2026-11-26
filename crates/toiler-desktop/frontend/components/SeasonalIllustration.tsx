"use client";

import { useEffect, useState } from "react";
import { parseSeasonSnapshot, SEASONAL_IMAGES, seasonRefreshDelay, type Season } from "@/lib/seasonal-illustration";

export function SeasonalIllustration() {
  const [season, setSeason] = useState<Season | null>(null);
  const [loadedSeason, setLoadedSeason] = useState<Season | null>(null);
  const [failedSeason, setFailedSeason] = useState<Season | null>(null);

  useEffect(() => {
    let disposed = false;
    let timer: number | undefined;
    let controller: AbortController | null = null;

    const refresh = async () => {
      window.clearTimeout(timer);
      controller?.abort();
      const request = new AbortController();
      controller = request;
      const requestTimer = window.setTimeout(() => request.abort(), 8_000);
      const started = performance.now();
      let delay = 60_000;
      try {
        const response = await fetch("/api/appearance/season", { signal: request.signal, cache: "no-store" });
        if (!response.ok) throw new Error("读取季节失败");
        const snapshot = parseSeasonSnapshot(await response.json());
        if (!snapshot) throw new Error("季节数据不合法");
        if (disposed || request.signal.aborted) return;
        setSeason(snapshot.season);
        delay = seasonRefreshDelay(snapshot, performance.now() - started);
      } catch {
        // 插画不阻塞开始对话；本地服务暂不可用时保留已显示的图或品牌标记。
      } finally {
        window.clearTimeout(requestTimer);
        if (!disposed && controller === request) timer = window.setTimeout(() => void refresh(), delay);
      }
    };

    const onVisible = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    void refresh();
    window.addEventListener("focus", onVisible);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      controller?.abort();
      window.removeEventListener("focus", onVisible);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);

  const imageVisible = season !== null && loadedSeason === season && failedSeason !== season;
  return (
    <div className="chat-empty-illustration" aria-hidden="true">
      {!imageVisible && <span className="chat-empty-mark" />}
      {season !== null && failedSeason !== season && (
        // 本地透明插画直接随静态导出打包，不经过联网优化服务。
        // eslint-disable-next-line @next/next/no-img-element
        <img
          key={season}
          src={SEASONAL_IMAGES[season]}
          alt=""
          width={1080}
          height={360}
          draggable={false}
          style={{ visibility: imageVisible ? "visible" : "hidden" }}
          onLoad={() => setLoadedSeason(season)}
          onError={() => setFailedSeason(season)}
        />
      )}
    </div>
  );
}
