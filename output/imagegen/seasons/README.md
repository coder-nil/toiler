# Toiler 四季图标

保留 T 形主体和 `>_` 终端标记，作为独立概念图片保存，尚未接入应用。

- `toiler-spring.png`：春季新绿，本地基于秋季图片进行 HSV 调色，保留叶片造型。
- `toiler-summer.png`：夏季金黄与海蓝，太阳与波纹。
- `toiler-autumn.png`：秋季铜橙与暖棕，枫叶与落叶。
- `toiler-winter.png`：冬季冰银与午夜蓝，积雪与雪花。
- `toiler-four-seasons-preview.jpg`：按春、夏、秋、冬顺序排列的对照预览。

夏、秋、冬通过 imagegen 技能的 CLI/API 编辑模式生成，模型为 `gpt-image-2`，参考图为 `crates/toiler-desktop/icons/icon.png`。实际图片尺寸以 PNG 为准。

完整提示词分别见 `spring-prompt.txt`、`summer-prompt.txt`、`autumn-prompt.txt`、`winter-prompt.txt`。春季提示词为原计划；春季 API 调用在用户明确授权后仍被自动审批拒绝，因此成品改用本地调色，未执行该提示词。
