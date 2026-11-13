# MQTT Test Tool

一个用于**模拟服务端通过 MQTT 5.0 协议向设备端下发消息**的桌面测试工具（Windows）。

定位类似 MQTTX，直连 broker，支持向下发主题批量定时发送消息、接收设备回执，并提供「异常包标红」和「丢包统计」能力，方便测试人员快速验证设备下行链路。

## 功能特性

- **连接**：Broker 地址 + 端口 + 用户名/密码 + Client ID（自动生成可改），连接状态实时显示，配置持久化
- **多方案（Profile）**：可保存多套项目配置（如 cbb项目 / monitorsystem项目），一键切换
- **订阅**：多订阅主题增删，用于接收设备回执
- **发布**：
  - 主题支持变量配置
  - 消息内容支持 JSON / Protobuf(hex) / Plaintext 三种格式，JSON 内置语法高亮编辑器
  - 变量替换 `{{seq}}` `{{timestamp}}` `{{datetime}}` `{{clientId}}`
  - QoS 0/1/2、Retain 开关
  - 三种发送方式：手动单条 / 按数量（包数+间隔）/ 按时长（时长+单位秒/时+间隔）
- **回执配对与异常标红**：发出的包与回执按序号配对，超时未回执的包标红
- **实时统计**：已发送 / 成功 / 超时 / 等待 / 丢包率
- **消息记录**：时间 / 主题 / 方向 / QoS / 内容，筛选全部 / 已发送 / 已接收，最新消息置顶

## 下载

前往 [Releases](https://github.com/Guoouwen/MQTT-Test-Tool/releases) 页面下载最新版本：

| 形式 | 文件 | 说明 |
|---|---|---|
| 安装包 | `MQTT Test Tool_<版本>_x64-setup.exe` | 双击安装，自动创建开始菜单快捷方式和卸载入口 |
| 绿色便携版 | `MQTT-Test-Tool_<版本>_x64_portable.zip` | 解压后双击 `mqtt-downlink-tool.exe` 即用，免安装 |

> 依赖系统 WebView2（Edge 内核），Win10/11 通常已预装；个别精简版系统需手动安装一次 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)。

## 本地开发

环境要求：

- Node.js 18+
- Rust（stable，MSVC 工具链）

```bash
# 安装前端依赖
npm install

# 启动开发模式（热更新，自动弹出窗口）
npm run tauri dev

# 打包安装包（产物在 src-tauri/target/release/bundle/）
npm run tauri build
```

## 发布

本项目通过 GitHub Actions 自动发布：push 一个 `v*` 标签（如 `v1.0.0`）即可自动构建 Windows 安装包 + 绿色便携版并挂到 Release。

发版前请保持三个文件的版本号一致：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`。

```bash
npm version 1.0.1          # 同步 package.json 版本
# 手动同步 tauri.conf.json 与 Cargo.toml 的 version 后提交
git tag v1.0.1
git push origin v1.0.1
```

## 技术栈

- [Tauri 2](https://tauri.app/)（桌面壳，复用系统 WebView2，产物体积小）
- Vue 3 + TypeScript + Vite（前端）
- Rust + [rumqttc](https://github.com/bytebeamio/rumqtt)（MQTT 5.0 客户端）
- CodeMirror 6（JSON 代码编辑器）
