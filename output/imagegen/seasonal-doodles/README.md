# 新建对话页面四季插画

这是早期带 T 的品牌版本，保留作设计过程参考。应用现使用 `output/imagegen/seasonal-scenes/` 的无品牌季节小景，下述交付资源描述对应当时版本。

通过 imagegen 技能的 CLI/API 编辑模式，使用 `gpt-image-2`，以此前元旦横幅为构图和品牌参考，分别生成四张独立图片。完整提示词见本目录四个 `*-prompt.txt`。

- 春：嫩绿 T、花枝与新叶。
- 夏：青蓝 T、海浪、绿叶与阳光。
- 秋：铜橙 T、枫叶、麦穗与落叶。
- 冬：冰银蓝 T、积雪、松枝与雪花。

本次图片服务实际返回 2172 × 724 RGBA PNG，已保留原始透明通道；没有使用背景色填充或重新抠图。`*-source.png` 是原图，`four-seasons-review.png` 是深浅背景对照预览。

交付资源位于 `crates/toiler-desktop/frontend/public/illustrations/seasons/`，统一缩放为 1080 × 360，WebP 质量 92，四张合计约 256 KiB。Next 静态导出将它们复制到 `dist/`，随后 Rust 将前端一起嵌入二进制，无需联网取图。

新建对话空白页通过本地 `/api/appearance/season` 获取季节，和 Dock 图标共用 `toiler-server::seasons` 的四立天文历。图片是装饰元素，标题仍由页面渲染；加载失败时回退为原品牌标记，固定画幅避免标题跳动。

此版本只内置四季插画，不包含节日排期或 Cloudflare 服务。
