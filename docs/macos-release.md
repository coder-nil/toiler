# macOS 发布：签名与公证规划

当前状态：**仅有 ad-hoc 签名，未做 Developer ID 签名与公证**。
`crates/toiler-desktop/tauri.conf.json` 的 `bundle.macOS` 已配
`signingIdentity: "-"`（让 Tauri 完成整包 ad-hoc 签名，见第一章），
但 `.github/workflows/release.yml` 里没有 `notarytool` 步骤，也没有
Developer ID 证书——所以产物仍会被 Gatekeeper 拦截。

本文记录三件事：

1. 修复前产物是**坏签名**——比「没签名」更糟，这是已独立修掉的 bug。
2. 为什么 ad-hoc 签名救不了 Gatekeeper（常见的错误思路）。
3. 接入真正的 Developer ID 证书 + 公证的完整改动清单与验收标准。

先读 [windows-release.md](./windows-release.md)，那份文档的推理方式（区分
「提示来源不可信」与「检测到恶意行为」、不要求用户关防护）同样适用于 macOS。

---

## 一、先修 bug：当前的 bundle 签名是无效的

### 现象

本地 `target/release/bundle/macos/Toiler.app`（1.0.7 产物）实测：

```console
$ codesign --verify --verbose=2 Toiler.app
Toiler.app: code has no resources but signature indicates they must be present

$ codesign -dv Toiler.app
Identifier=toiler_desktop-662ae42e82b7d01a
flags=0x20002(adhoc,linker-signed)
Info.plist=not bound
Sealed Resources=none
```

`Contents/` 下没有 `_CodeSignature/` 目录，`Info.plist=not bound`，
`Identifier` 是 rustc 的 `包名-哈希` 而不是 `com.toiler.desktop`。

### 根因

Rust 链接器给二进制打的是 **linker-signed** 签名（只为满足 Apple Silicon
「所有 arm64 可执行文件必须有签名」的硬要求），它是**二进制级**的，
不覆盖 bundle 的 `Info.plist` 与 `Resources/`。

bundle 级签名本该由 `tauri-bundler` 在打包时补上，但它被跳过了。
`tauri-bundler-2.7.1/src/bundle/macos/app.rs` 的逻辑是：

```rust
} else if let Some(keychain) =
  super::sign::keychain(settings.macOS().signing_identity.as_deref())?
{
  // 签名 + 公证都在这个 if let 里
}
```

`tauri.conf.json` 没配 `signingIdentity`，`APPLE_CERTIFICATE` 之类环境变量
也不存在，于是 `keychain()` 返回 `None`，**整个签名与公证分支被跳过**。
Tauri 不会退化成「做一次 ad-hoc 签名」，而是什么都不做。

### 影响

- 坏签名下 `spctl` 报 `code has no resources but signature indicates they must be present`。
- 签名自检 `codesign --verify` 直接失败——任何安全工具、以及 macOS 自己做
  完整性检查时都会认为这个 app 被篡改过。
- 用户即使按文档去「系统设置 → 隐私与安全性 → 仍要打开」手动放行，
  放行一个**签名自检都通不过**的 app 体验很差，也更难取信于人。

### 修法（与真签名无关，现在就能做）

在 `crates/toiler-desktop/tauri.conf.json` 的 `bundle.macOS` 里加一行：

```json
"macOS": {
  "minimumSystemVersion": "11.0",
  "signingIdentity": "-"
}
```

`"-"` 是 `codesign` 的 ad-hoc 签名标识。有了它，`tauri-bundler` 的
`keychain()` 会返回 `Some`，签名分支不再被跳过。

**必须在配置里做，不能事后补签。** 原因：`tauri build` 内部先产 app、再把
app 装进 DMG，两步在同一个命令里完成。事后再对 `.app` 跑 `codesign`，
DMG 里装的仍然是签名无效的那一份。配上 `signingIdentity` 后签名发生在
打包流程内部，DMG 自然拿到签好的 app（`dmg/mod.rs:34` 会复用已生成的
app bundle）。

已实测（`tauri build --config` 临时覆盖，macOS 27.2 / tauri-bundler 2.7.1）：

```console
Signing with identity "-"
Signing .../Toiler.app/Contents/MacOS/toiler-desktop
Signing .../Toiler.app
Warn skipping app notarization, no APPLE_ID & ... environment variables found

$ codesign --verify --verbose=2 Toiler.app
Toiler.app: valid on disk
Toiler.app: satisfies its Designated Requirement

$ codesign -dv Toiler.app
Identifier=com.toiler.desktop          ← 不再是 toiler_desktop-哈希
flags=0x10002(adhoc,runtime)
Info.plist entries=14                   ← 已绑定
Sealed Resources version=2 rules=13 files=1

$ ls Toiler.app/Contents/_CodeSignature/
CodeResources                            ← 有了

$ spctl -a -vvv -t exec Toiler.app
Toiler.app: rejected                     ← 仍然过不了 Gatekeeper
```

两个要点：

1. `signingIdentity: "-"` 同时让 `hardenedRuntime`（默认 `true`）生效，
   所以会看到 `runtime` 标志。本仓库已验证不影响功能（见下文）。
2. **`"-"` 会让 DMG 本身不被签名**（`dmg/mod.rs:196` 的
   `identity != Some("-")`，对应 tauri#12288）。对 ad-hoc 场景无害——
   反正 ad-hoc 签名 DMG 也过不了 Gatekeeper。但换成真 identity 后
   DMG 就会正常签名，这是从 ad-hoc 迁移到真签名时**自动改善**的一环。

`"-"` **不能留到真签名阶段**：真发布时应由 CI 的 `APPLE_SIGNING_IDENTITY`
覆盖（Tauri 的环境变量优先级高于配置文件，已实测），或本地用临时覆盖配置：

```bash
npx --yes @tauri-apps/cli@^2 build \
  --target aarch64-apple-darwin --bundles app,dmg \
  --config '{"bundle":{"macOS":{"signingIdentity":"Developer ID Application: ..."}}}'
```

将来若引入内嵌 framework / sidecar，应改成按 Apple 建议的「由内向外」逐个
签名。Tauri 的 `sign()` 已经按这个顺序处理 `sign_paths`，无需自己写 `--deep`。

---

## 二、为什么 ad-hoc 签名救不了 Gatekeeper

常有一种误解：「自己做一次 ad-hoc 签名，就能走 app 的放行逻辑」。实测否定：

```console
# 正确完成 bundle 级 ad-hoc 签名
$ codesign --verify --verbose=2 T.app
T.app: valid on disk
T.app: satisfies its Designated Requirement     ← 签名自身没问题

# 但 Gatekeeper 的判定：
$ spctl -a -vvv -t exec T.app
T.app: rejected                                  ← 照样拒
```

`codesign` 通过只说明「签名没坏」，与「Gatekeeper 放行」是两件不同的事。
Gatekeeper 只认两种凭据：**Apple 公证** 或 **用户在系统设置里手动放行**。
ad-hoc 签名两者都不是，所以必须拿到 Developer ID 证书做真签名 + 公证。

附带结论：**别想着在 app 启动后自己清 `com.apple.quarantine` 来绕过**。
quarantine 标记是下载/解压时打上的，Gatekeeper 在 app 启动**之前**就检查，
过不了检查进程根本起不来，那段清标记的代码永远执行不到。让它执行到的唯一
前提是用户已经放行过一次，那时清除已无意义。而且这类「自我规避 Gatekeeper」
的实现，在将来做公证审核时是负资产。**不做。**

---

## 三、接入真签名与公证

### 前置条件（用户侧，无法用代码替代）

- 加入 **Apple Developer Program**（$99/年）。
- 在 Apple Developer 后台创建 **Developer ID Application** 证书
  （注意不是 "Mac App Distribution"，分发到 App Store 之外要用 Developer ID）。
- 导出为 `.p12`（含私钥）并设密码。
- 记录 **Team ID**（后台 Membership 页，10 位字符）。

### 凭据放在 GitHub Secrets

Tauri 认这套环境变量（`tauri-bundler/src/bundle/macos/sign.rs` 实测确认）：

| 变量 | 内容 | 必需 |
|---|---|---|
| `APPLE_CERTIFICATE` | `.p12` 的 base64（`base64 -i cert.p12 \| pbcopy`） | 是 |
| `APPLE_CERTIFICATE_PASSWORD` | 导出 `.p12` 时设的密码 | 是 |
| `APPLE_SIGNING_IDENTITY` | 如 `Developer ID Application: Your Name (TEAMID)` | 是 |
| `APPLE_ID` | Apple ID 邮箱 | 公证（二选一） |
| `APPLE_PASSWORD` | App 专用密码，**不是**账号密码 | 公证（二选一） |
| `APPLE_TEAM_ID` | 10 位 Team ID | 公证（二选一） |
| `APPLE_API_KEY` / `APPLE_API_ISSUER` / `APPLE_API_KEY_PATH` | App Store Connect API Key | 公证（另一选项） |

用 API Key 的那条路径还会去 `./private_keys`、`~/.private_keys`、
`~/.appstoreconnect/private_keys` 找 `AuthKey_<id>.p8`。

> `APPLE_SIGNING_IDENTITY` 与证书里的 identity 必须一致，否则 bundler 会
> 直接报错（`certificate ... does not match provided identity`）。这是好事，
> 能让配错立刻暴露。

**绝不**把 `.p12`、`.p8`、密码写进仓库、日志或聊天。CI 里用 Secrets，
本地调试用临时 keychain 并记得删除。

### 配置改动

`crates/toiler-desktop/tauri.conf.json` 的 `bundle.macOS` 已经很接近最终形态，
接入真签名时只需补 `entitlements`（`signingIdentity` 与 `hardenedRuntime`
已在修 ad-hoc 时配好）：

```json
"macOS": {
  "minimumSystemVersion": "11.0",
  "signingIdentity": "-",
  "hardenedRuntime": true,
  "entitlements": "entitlements.plist"
}
```

`signingIdentity` 留在 `"-"` 即可——CI 里的 `APPLE_SIGNING_IDENTITY`
会覆盖它（`sign.rs:35` 先读环境变量导入证书，再校验 identity 是否匹配）。
这样未配 Secrets 的 fork 仍能构建出可用的 ad-hoc 签名产物。

> 反过来，如果想让仓库默认就是真签名，也可以把真实 identity 写进配置，
> 但那会把证书名（含姓名与 Team ID）固化进仓库，不推荐。

**注意 `"-"` 与 DMG 的交互**：`dmg/mod.rs:196` 的 `identity != Some("-")`
意味着 ad-hoc 模式下 DMG 本身不签名。换成真 identity 后 DMG 会自动获得签名，
不需要额外代码。但 DMG 的**公证**仍需自己补（见下文坑 1）。

`entitlements.plist`（与 `tauri.conf.json` 同目录）最小集：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.cs.allow-jit</key><true/>
  <key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/>
</dict></plist>
```

这两项是 WebKit/JavaScriptCore 必需的，缺了 WebView 会在 hardened runtime
下崩溃。**不要**加 `allow-anything` 或 `disable-library-validation`——
前者会被公证直接拒，后者会削弱安全性。

### hardened runtime 对现有功能的影响（已实测）

这是接真签名时最需要担心的点，本地验证过：

- 带 `runtime` 签名的 app 正常启动，**API 实际返回 200**（不只看进程存活：
  `curl http://127.0.0.1:<port>/` 验过；WebView 与 axum 都正常）。
- hardened runtime **不阻止** `fork` + `exec` 子进程：用一个专门的 C 程序
  验证过（`fork` 后 `execl("/bin/zsh")` 成功），内置终端（`portable-pty`）
  与 agent 调 shell 都不受影响。
- 全部动态依赖都是 `/System` 与 `/usr/lib`，不需要额外 `disable-library-validation`。

真正需要在 Apple Silicon / Intel 真机上逐项复验的是：内置终端粘贴与 resize、
agent 起子进程、手机链路（`lan_proxy` / 隧道，涉及网络 client）、
通知（`tauri-plugin-notification`）、窗口毛玻璃（`macOSPrivateApi`）。

> `entitlements.plist` 目前还没建文件。上表两项即使不加，本地 ad-hoc 实测
> 也能正常启动；但**公证 + hardened runtime 的正式产物必须带**，否则某些
> macOS 版本上 WebKit 会因 JIT 被拦而崩溃。这是真机验收里必须盯的项。

### 公证的两个坑

**坑 1：DMG 只签名、不公证。** `dmg/mod.rs` 里只调了 `sign()`，没有
`notarize()`。而用户实际下载并打开的是 DMG。所以不能只依赖 `tauri build`，
需要显式补一步：

```bash
# 提交公证（app 与 dmg 都要，哪个被下载就公证哪个）
xcrun notarytool submit "Toiler_${VERSION}_aarch64.dmg" \
  --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" \
  --wait
# 装订票据，否则离线用户首次打开仍会联网校验失败
xcrun stapler staple "Toiler_${VERSION}_aarch64.dmg"
xcrun stapler validate "Toiler_${VERSION}_aarch64.dmg"
```

`tauri build` 自带的公证只作用于 `.app`，且需要 `APPLE_*` 变量齐全。
DMG 的公证与装订必须自己补。**`stapler staple` 不能省**：少这一步，
用户断网时打不开。

**坑 2：`tauri build` 内建的公证只处理 `.app`，且会抛掉 DMG 的票据。**
`app.rs:131` 的 `notarize()` 是对 `app_bundle_path` 调用，而 DMG 是后一步
才生成的（`dmg/mod.rs:34` 复用已签名的 app）。所以 `tauri build` 跑完时：

- `.app` 已签名、已公证、已装订（只要 `APPLE_*` 齐全）；
- `.dmg` 已签名（真 identity 时），但**未公证、未装订**。

用户下载的是 DMG，所以 DMG 的公证与装订必须自己补。**`stapler staple`
不能省**：少这一步，用户断网时打不开（`spctl` 会退化成联网校验）。

**坑 3：`--deep` 与公证的顺序。** 不要对已公证的 app 再跑 `codesign --deep`
改签名，那会作废公证票据。签名 → 公证 → 装订，顺序固定。

### CI 改动清单

`.github/workflows/release.yml`：

1. `tauri build` 步骤加 `env`，把 7 个 `APPLE_*` Secrets 传进去。
   不进 `env` 的话 Tauri 看不到证书，会静默降级成 ad-hoc 签名（日志里是
   `Warn skipping app notarization`，推送日志里很容易漏看）。
2. **现有的「macOS 验签」步骤保留**（已加）。它现在会报
   `::warning::当前为 ad-hoc 签名`；接入真证书后自动变成 `✓ 已用 Developer ID 签名`。
   建议再接真证书后把 warning 改成硬失败，防止 Secrets 被误删后静默发布。
3. 打包后加 DMG 公证与装订（仅 tag 构建）：
   ```bash
   DMG=$(ls target/${{ matrix.target }}/release/bundle/dmg/*.dmg)
   xcrun notarytool submit "$DMG" \
     --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
     --team-id "$APPLE_TEAM_ID" --wait
   xcrun stapler staple "$DMG"
   xcrun stapler validate "$DMG"
   ```
   `--wait` 必须加，否则公证仍在排队就上传了。失败要阻止上传。
4. **顺序**：打包（含 app 签名/公证/装订）→ 验签 → DMG 公证 → DMG 装订
   → 生成 SHA256 → 上传。公证会改字节，SHA-256 必须基于**最终产物**。
   现在 SHA-256 在 `release` job 里对下载的 artifact 算，只要 artifact 是
   装订后的就正确。

Secrets 没配时应**优雅降级**（跳过公证、保留 ad-hoc + warning 说明），
而不是让整个发布红掉——否则 fork 的人无法发布。这也是为什么
`signingIdentity` 留在 `"-"` 而不写真实 identity。

### 验收标准

真机（干净环境，不是开发机）逐条确认：

- [ ] `codesign --verify --deep --strict --verbose=4 Toiler.app` 通过
- [ ] `codesign -dv` 显示 `Authority=Developer ID Application: ...`、有 `Timestamp`
- [ ] `spctl -a -vvv -t exec Toiler.app` 输出 `accepted`、`source=Notarized Developer ID`
- [ ] `xcrun stapler validate Toiler_*.dmg` 通过
- [ ] 从**浏览器真实下载** DMG（浏览器的 quarantine 标记与手工 `xattr` 不同），
      双击 app 直接启动，**不出现任何 Gatekeeper 拦截**
- [ ] 断网状态下首次打开也正常（验证票据已装订）
- [ ] 内置终端、agent 子进程、手机访问、通知四项功能在签名版上复验
- [ ] Intel 与 Apple Silicon 两个产物都验

### 费用与维护

- $99/年，证书有效期 1 年，**到期不续会导致新构建无法签名**；
  已发布的旧版本不受影响。
- 证书过期后需重新签发并更新 `APPLE_CERTIFICATE`。
- 公证有速率限制，正常发布节奏不会触及。

---

## 四、与 Windows 的关系

两份文档的结论一致：签名解决的是**来源可验证性**，不保证「所有终端都不拦截」。

- macOS：真签名 + 公证后，**新下载**的产物首次打开无拦截，这个提升是确定的。
- Windows：签名改善 SmartScreen 信誉，但信誉是累积的，签名当天仍可能提示。

不要把「签名完成」写成「问题彻底解决」。

---

## 五、当前不做的事

| 项 | 原因 |
|---|---|
| app 内部自动清除 `com.apple.quarantine` | 启动前就被拦，代码执行不到；且是刻意规避 Gatekeeper，公证审核的负资产 |
| 自签名证书（Self-Signed）分发 | 用户仍需手动信任根证书，比现在更麻烦，且不解决 Gatekeeper |
| `disable-library-validation` / `allow-anything` | 削弱安全、公证会拒 |
| 改 `Info.plist` 的 `CFBundleShortVersionString` 绕过什么 | 版本由 `tauri.conf.json` 的 `version` 生成，只该由 `tools/bump-version.py` 改 |

---

## 附：本文结论的实测记录

环境：macOS 27.2 (26B123)，`tauri-bundler 2.7.1`，2026-11-19。

| 验证项 | 结果 |
|---|---|
| 修复前 1.0.7 产物 `codesign --verify` | ✗ `code has no resources but signature indicates they must be present` |
| 修复前 `Sealed Resources` / `Info.plist` | `none` / `not bound`，无 `_CodeSignature/` |
| 修复前 `Identifier` | `toiler_desktop-662ae42e82b7d01a`（rustc 哈希） |
| 修复前 CI 里的 `codesign` 步骤 | 无（全文件 grep 确认） |
| **配上 `signingIdentity: "-"` 后 `tauri build`** | ✓ 日志出现 `Signing with identity "-"`（app 与内部二进制各签一次） |
| 同上，`codesign --verify` | ✓ `valid on disk` / `satisfies its Designated Requirement` |
| 同上，`Identifier` | ✓ `com.toiler.desktop` |
| 同上，`Sealed Resources` / `Info.plist` | ✓ `version=2 rules=13 files=1` / `entries=14` |
| 同上，`_CodeSignature/CodeResources` | ✓ 存在 |
| 同上，`spctl -a -vvv -t exec` | ✗ 仍 `rejected` |
| ad-hoc 签名（不论手工还是 Tauri）后 `spctl` | ✗ `rejected`（**adhoc 不被 Gatekeeper 认可**） |
| 带 `runtime` 签名后启动 + API | ✓ 进程存活，`curl /` 返回 200（WebView + axum 正常） |
| hardened runtime 下 `fork`+`execl("/bin/zsh")` | ✓ 成功（终端与 agent 不受影响） |
| 二进制动态依赖 | 全部 `/System` / `/usr/lib`，无内嵌 dylib |
| `dmg/mod.rs` 是否公证 | ✗ 只 `sign()`，不 `notarize()`（公证只对 `.app`） |
| `dmg/mod.rs` 对 `identity == "-"` | 跳过 DMG 签名（tauri#12288）；真 identity 时会签 |
| `signingIdentity` 为 `"-"` 时 `hardenedRuntime` | ✓ 自动生效（`flags` 含 `runtime`） |
| 带 quarantine 的裸 Mach-O 直接 exec | ✓ 可执行（终端路径不经 Gatekeeper 文本门控） |
| 本机公证工具链 | ✓ `xcrun notarytool` / `xcrun stapler` 可用 |

以上均为本机可复现命令，不含任何凭据。

### 修复后的产物验证（可直接复现）

```bash
cd crates/toiler-desktop
npx --yes @tauri-apps/cli@^2 build --target aarch64-apple-darwin --bundles app
APP=../../target/aarch64-apple-darwin/release/bundle/macos/Toiler.app
codesign --verify --verbose=4 "$APP"     # 必须通过
codesign -dv "$APP" 2>&1 | grep -E "Identifier|Sealed"
spctl -a -vvv -t exec "$APP"             # 预期仍 rejected（adhoc）
```