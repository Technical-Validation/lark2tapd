# 新增编辑器与图标的第三方声明

本文件记录原生 Markdown 编辑器和飞书来源图标的归属，不替代整个依赖树的许可证清单。

## 原生 Markdown 编辑器

- `zorite-editor 0.11.1`：MIT，Copyright (c) 2026 Will Lehnertz。
- `zorite-markdown 0.10.0`：MIT，Copyright (c) 2026 Will Lehnertz。
- `gpui-bidi 0.1.1`：MIT，Copyright (c) 2026 Will Lehnertz。
- 上游：<https://github.com/packetThrower/zorite>。
- 本项目仅使用这些 MIT 库，不引入 GPL Zorite 应用。编辑器的本地补丁位于 `vendor/zorite-editor`，保留其发布来源和补丁说明。

上述三包随发布物提供相同的 MIT 版权与许可文本，完整文本保留在 `vendor/zorite-editor/LICENSE`。macOS 应用包中对应文件为 `Contents/Resources/licenses/zorite-MIT.txt`。

## 飞书来源图标

图形来自 ByteDance IconPark 的 `new-lark`，采用 Apache-2.0 许可。源码、精确来源与显示适配说明见 `crates/desktop/assets/README.md`，完整许可见同目录 `IconPark-LICENSE`。

飞书 / Lark 名称与图形用于标识任务来源，不表示本应用由商标所有者开发、认可或授权。Apache-2.0 不授予商标权。

macOS 应用包保留本文件及 `Contents/Resources/licenses/IconPark-Apache-2.0.txt`。
