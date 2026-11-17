import { I18nProvider } from "@/hooks/useI18n";
import { MobileRemoteView } from "@/components/MobileRemoteView";

// 查询参数由客户端读取，保留静态导出，不需要 Next 服务端。
export default function MobilePage() {
  return (
    <I18nProvider>
      <MobileRemoteView />
    </I18nProvider>
  );
}
