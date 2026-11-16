# Windows 发布与 Defender 拦截排查

## 区分两类提示

- **SmartScreen 提示无法识别应用**：通常涉及发布者身份、下载信誉或组织策略，不等于检测到病毒。
- **Defender 隔离或删除文件**：在 Windows 安全中心 → 病毒和威胁防护 → 保护历史记录中查看具体威胁名称、受影响路径、处置时间。没有这些记录和样本，不能直接认定误报。

下载后尚未运行就被删除，首先排查安装包扫描结果，而不是猜测尚未执行的 PowerShell、终端或手机隧道触发行为检测。当前 Toiler 的界面与后端在 Rust/Tauri 同一进程中运行，不打包 Node 后端。

## 发布流程的防线

`.github/workflows/release.yml` 同时构建 NSIS `.exe` 和 WiX `.msi`。MSI 是标准安装格式，便于独立排查安装器相关误报；**换格式不保证消除拦截**，也不能修复应用本体的检测问题。

Windows 构建后执行 `tools/release/scan-windows.ps1`：

1. 确认 Defender 服务与反病毒能力可用，更新病毒库并输出版本。
2. 扫描真实 `toiler-desktop.exe` 和两种安装包，不自动修复扫描目标，不修改实时防护设置。
3. 扫描失败、检测到威胁或产物缺失时阻止上传与后续 Release。

若 GitHub runner 的 Defender 不可用或病毒库更新失败，任务也会失败，不能静默跳过。CI 扫描只代表该环境与该病毒库下的结果，不覆盖所有云端判定、终端策略或后续病毒库变化。

Release 附带 `SHA256SUMS.txt`，在 Windows 可用以下命令核对下载文件：

```powershell
Get-FileHash -LiteralPath '.\Toiler_安装包.exe' -Algorithm SHA256
```

与同一 Release 清单对应文件的值比较。校验值用于确认完整性和关联样本，**不是安全证明，也不能替代签名**。

## 接入真正的代码签名

目前发布流水线仍未签名。本次不添加自签名证书、不伪造发布者，也不要求用户关闭 Defender。

正式签名需要受信任的代码签名证书及其私钥访问能力（硬件令牌、HSM 或云签名服务）。不能仅靠修改 JSON 获得可信签名；证书、公钥发布者名称与签名服务选型确定后，再接入 CI 的安全凭据存储。

对已经安全配置好证书存储与私钥访问的 Windows 构建机，可以创建临时 Tauri 覆盖配置，例如：

```json
{
  "bundle": {
    "windows": {
      "certificateThumbprint": "替换为证书存储中真实证书的指纹",
      "digestAlgorithm": "sha256",
      "timestampUrl": "http://timestamp.digicert.com",
      "tsp": true
    }
  }
}
```

时间戳服务需按证书供应商要求选择。然后在 `crates/toiler-desktop/` 执行：

```powershell
npx --yes @tauri-apps/cli@^2 build --target x86_64-pc-windows-msvc --bundles nsis,msi --config <覆盖配置路径>
```

云签名服务可使用 Tauri 的 `bundle.windows.signCommand` 接口，具体命令依服务商 SDK 决定。GitHub 托管 runner 不会天然拥有你的证书与私钥；不要把私钥、PFX 或密码写进仓库、日志或聊天。

应让 Tauri 在打包流程中签名应用本体与安装器，而不只在最终安装包上补签。签名完成后再扫描，校验清单也必须基于最终签名产物生成。发布前分别检查应用、NSIS 和 MSI：

```powershell
Get-AuthenticodeSignature -LiteralPath '<产物路径>' | Format-List Status, StatusMessage, SignerCertificate, TimeStamperCertificate
```

确认签名有效、发布者正确且有时间戳，再发布。代码签名改善来源可验证性，但不能保证立刻获得 SmartScreen 信誉或免于 Defender 检测。

## 仍然被拦截时

1. 保留版本、官方下载链接、SHA-256、Defender 威胁名称与受影响文件路径；不要关闭防护或直接恢复运行。
2. 在微软安全智能样本提交入口以软件开发者身份提交对应样本：<https://www.microsoft.com/wdsi/filesubmission>。只提交发布产物，不上传用户配置、会话或凭据。
3. 若应用本体也被检测，排查构建来源与依赖，不能以更换安装格式、反复改字节或压缩方式代替调查。
4. 微软完成分析后，用更新的病毒库复测；在干净 Windows 环境验证下载、安装、启动、终端、手机访问及卸载。
