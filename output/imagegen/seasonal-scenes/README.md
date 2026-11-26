# 无品牌四季小景

本目录是用户认可冬季方向后补齐的四季素材，已替换新建对话页面原有的 T 形插画。

使用 imagegen 技能的 CLI/API 模式，模型为 `gpt-image-2`。冬季通过生成模式创作，没有传入旧 T 图参考；春、夏、秋通过编辑模式以冬季图为参考，保留溪流、小屋与树木的布局。完整提示词见本目录四个 `*-prompt.txt`。

- `*-scene-source.png`：四季原始 2172 × 724 RGBA 画稿，保留服务返回的透明通道。
- `*-scene.webp`：四季 1080 × 360 透明 WebP 交付素材。
- `four-seasons-scene-preview.jpg`：四季在深浅背景上的对照预览。
- `winter-scene-background-preview.jpg`：深浅背景检查图。
- `winter-scene-page-concept.jpg`：在之前浏览器实测截图上替换插画区域的合成预览，不是已更新应用的实测截图。

四季保持同一条溪流、树木和小屋，分别用萌芽、树荫、秋叶与积雪表达季节。采用没有标志和文字的平面编辑插画，不对称构图、少量色块与留白。

交付资源复制到 `crates/toiler-desktop/frontend/public/illustrations/seasons/`，Next 构建后随静态产物嵌入 Rust 二进制。页面继续复用现有四立切换逻辑；本次仅替换美术资源，不改变日历与接口。
