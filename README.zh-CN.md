# HandyBox

<img src="assets/logo.png" alt="HandyBox 吉祥物" width="88" height="88">

[English](README.md) | 简体中文

一个使用 Rust 和 Slint 构建的、快速且离线优先的桌面工具箱。

HandyBox 把日常小工具集中到一个原生桌面工作区里。首个版本提供**中英文界面**、一个可用的**文档转 Markdown 工具**，以及另外八个工具的可导航、带说明的占位页面。无需账号、API Key、服务器、WebView，也不会上传任何文档。

用侧边栏底部的 **English / 中文** 可以立即切换语言。当前工具、搜索内容和已转换的文档都会保留。工具搜索同时支持两种语言和引擎名称。默认英文；选择会保存在本地，下次启动时沿用。

## 运行

先用 rustup 安装 Rust，然后在本仓库中运行：

```sh
cargo run --locked

# 启动时直接打开一个文档
cargo run --locked -- examples/sample.rtf

# 强制使用 CPU 渲染器做兼容性测试
SLINT_BACKEND=winit-software cargo run --locked

# 优化后的可执行文件：target/release/handybox
cargo build --release --locked
```

仓库锁定了 **Rust 1.88.0**、**Slint 1.13.1** 和 **anydoc 0.2.4**；更新依赖时请一并提交 `Cargo.lock`。rustup 会在需要时自动安装锁定的工具链。首次下载依赖需要联网；构建完成后的应用完全在本地处理文档。

macOS 上需安装 Xcode Command Line Tools。Windows 上请使用 MSVC Rust 工具链和 Visual Studio C++ Build Tools。Debian/Ubuntu 上请安装桌面构建依赖：

```sh
sudo apt-get install build-essential pkg-config libx11-dev libx11-xcb-dev \
  libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libgl1-mesa-dev \
  libfontconfig1-dev libwayland-dev
```

Linux 目前使用 X11（在 Wayland 会话中则走 XWayland）。原生文件选择器依赖 `xdg-desktop-portal` 以及对应桌面环境的 portal 后端。即便使用 CPU 渲染，也仍然需要图形桌面会话。

## 发布构建

常用命令都封装在 [Task](https://taskfile.dev) 里，`task --list` 可以列出全部：

```sh
task run                        # cargo run --locked
task run -- examples/sample.rtf # 启动时打开文档
task check                      # 格式检查 + clippy
task test                       # 工作区测试

task build                      # 三个平台，产物进 dist/
task build:native               # 只构建当前系统
task build:cross                # 只构建 Windows 和 Linux
task icon                       # 重新生成派生图标
```

`task build` 会在 `dist/` 下生成带版本号的优化压缩包，以及一份 `SHA256SUMS` 校验清单。三个平台统一发 zip：下载体积大约减半，也省得一个平台是压缩包、另外两个是裸文件：

```text
dist/handybox-0.1.0-macos-arm64.zip    # HandyBox.app
dist/handybox-0.1.0-windows-x64.zip    # handybox-0.1.0-windows-x64.exe
dist/handybox-0.1.0-linux-x64.zip      # handybox-0.1.0-linux-x64
```

版本号来自 `Cargo.toml` 的 `[workspace.package]`，不会在第二个地方重复书写。构建前会先清掉旧产物，`dist/` 里不会新旧版本混在一起。`dist/` 不纳入版本管理。

release 配置用构建时间换体积：`lto = "fat"`、`codegen-units = 1`、`strip = true`。fat LTO 冷构建要多花几分钟，换来约 8% 的可执行文件体积。`panic = "abort"` 还能再省一兆多的展开表，但刻意没有开启：工作线程依赖 `catch_unwind` 兜住畸形文档触发的解析器 panic，abort 会让整个应用直接退出。

macOS 发布的是 `HandyBox.app`，由 [`scripts/macos-bundle.sh`](scripts/macos-bundle.sh) 打包。裸的 Mach-O 可执行文件在 Finder 里双击会拉起 Terminal，也挂不上图标；只有 .app 才是正常的应用启动方式。脚本会写入 `Info.plist`，用 `sips`/`iconutil` 生成 `AppIcon.icns`，并用 `ditto` 压缩以保住 bundle 的可执行位和符号链接。打包在 `target/` 下进行，因此 `dist/` 里永远只有可发布的压缩包。Windows 和 Linux 的可执行文件由 [`scripts/zip-binary.sh`](scripts/zip-binary.sh) 压缩；Info-ZIP 会记录 Unix 权限位，Linux 二进制解压后依然可执行。Windows 本身就链接为 GUI 子系统程序，`.exe` 不会弹控制台窗口。

Windows 和 Linux 的二进制在 [`scripts/Dockerfile.cross`](scripts/Dockerfile.cross) 定义的纯构建容器里交叉编译，容器提供 mingw-w64 和 GNU 交叉链接器，以及 winit 后端需要链接的 Linux 桌面开发库。容器里的任何东西都不会打包进应用。宿主构建和交叉构建使用各自独立的 Cargo 缓存，不会互相覆盖产物。需要 Docker 正在运行。镜像的 tag 取自 Dockerfile 的内容哈希，改动工具链会自动触发重建，不会悄悄沿用旧镜像。

`task build` 需要在 macOS 上执行，因为 Apple 目标无法在容器里交叉编译。其他系统请分别使用 `task build:native` 和 `task build:cross`。

产物最多只有 ad-hoc 签名：没有 Developer ID 签名，也未做公证，因此首次启动时 macOS Gatekeeper 和 Windows SmartScreen 会给出提示。macOS 压缩包请用 Finder 或 `ditto -x -k` 解压；用命令行 `unzip` 会把系统给 bundle 附加的 `._` 伴生文件一并解出来，导致 `codesign` 报告封存资源缺失。

## 工具与实现状态

| 工具 | 引擎 | 当前状态 |
| --- | --- | --- |
| 文档转换 | `anydoc` | 已实现：选择/拖入文件、转换、查看 Markdown 源码、全部复制、导出 `.md` |
| JSON 工作台 | `jaq` | 占位：校验、格式化与 jq 风格转换 |
| 哈希与加密 | `sha2` + `age` | 占位：校验和、校验、加密与解密 |
| 文本对比 | `similar` | 占位：文本/文件比较与 unified diff |
| 图像处理 | `image` + `fast_image_resize` + `oxipng` + `nom-exif` | 占位：格式转换、缩放、PNG 优化与元数据查看 |
| 压缩与解压 | `zip` + `sevenz-rust2` | 占位：压缩包预览、创建与解压 |
| 条码识别 | `rxing` | 占位：从图片中解码二维码与条形码 |
| 磁盘清理 | `czkawka_core` | 占位：扫描重复与冗余文件，删除前预览 |
| 剪贴板 | `clipboard-rs` | 页面占位；文档转换已经在用这个库复制 Markdown |

计划中的引擎只记录在工具目录中，不会伪装成可用操作，也不会提前拉进生产依赖。`zip` 同时是开发依赖，用于构造真实的 DOCX 测试样本。Slint 自身带有图像相关的传递依赖，这并不代表图像处理已经实现。

## 文档转换

1. 点击 **选择文件**、把一个文件拖到窗口上，或在启动时传入文件路径。
2. 转换会在后台工作线程中自动开始。源文件不会被修改。
3. 阅读生成的 Markdown 源码。**复制全文** 复制完整结果，**导出 .md** 会打开原生保存对话框。

转换器支持 anydoc 提供的格式：Word（`doc`、`docx`、`docm`）、PowerPoint（`ppt`、`pps`、`pot`、`pptx`、`pptm`、`ppsx`、`ppsm`）、Excel（`xls`、`xlsx`、`xlsm`、`xlsb`）、OpenDocument（`odt`、`ods`、`odp`）、PDF、EPUB、RTF 和 CSV。内容检测优先于文件扩展名；没有特征签名的 CSV 则按扩展名判断。

行为与当前限制：

- 单文件处理，**输入上限 64 MiB**，即使文件在读取过程中增大也会有界读取。一次拖入多个文件时，只会处理空闲状态下接受的第一个；没有批量队列。
- 支持文本型 PDF。扫描件/纯图片 PDF 页面会明确提示需要 OCR；本项目不包含 OCR。加密或损坏的文档会报告转换错误。
- Markdown 以**源码**形式展示，不做渲染。预览最多 **200,000 个 Unicode 字符**；复制和导出始终使用完整结果。视图按行渲染并做了虚拟化，只对屏幕上的那几行排版；代价是文字不可选中。需要取出内容请用**复制全文**或**导出 .md**。
- 切换工具时结果保留在内存中。不会自动保存转换历史。选择新文件后，在处理开始时清除旧结果；取消文件选择器则保留原结果。
- 导出会先在目标目录写入临时文件，再原子地落盘。是否覆盖由原生对话框负责确认。导出会拒绝覆盖输入文件，并要求 `.md` 扩展名。确认之后不会悄悄改变目标路径。
- 仅导出 Markdown，不会打包提取出的图片资源，也不保证还原源文档的视觉排版。复杂排版的效果取决于 anydoc 的提取质量。
- 同一时间只运行一个操作。工作期间仍可导航，但解析目前无法取消。输入大小上限并不等于对解压后数据或解析器峰值内存的硬限制；anydoc 有自己的解析器限制。未来可以用独立的工作进程为大负载加上硬性的内存/时间限制和取消能力。

可以试试仓库自带的小文件：[`examples/`](examples/)。

## 架构

工作区把原生 UI 与可复用的工具操作分开：

```text
crates/
  handybox-core/                 # 不涉及 Slint 或系统对话框
    src/catalog.rs              # ToolId + 元数据：唯一事实来源
    src/tools/documents.rs      # 校验、anydoc 适配、结果、导出
    tests/documents.rs          # 转换与文件安全的集成测试
  handybox-desktop/
    build.rs                    # 编译 Slint 组件树
    src/main.rs                 # 启动、等宽字体、可选的初始文档
    src/macos.rs                # macOS Dock 图标与浅色标题栏
    src/locale.rs               # 工具翻译、类型化消息、各语言的界面字体
    src/settings.rs             # 原子写入的本地语言偏好
    src/renderer.rs             # Winit GPU 优先 / CPU 回退
    src/controller.rs           # UI 回调、路由、保留的文档状态
    src/worker.rs               # 有界的后台命令/事件桥接
    ui/app.slint                # 仅外壳：侧边栏 + 当前页 + 状态栏
    ui/theme.slint              # 共享调色板、字体、间距、圆角
    ui/icons.slint              # 内嵌 SVG 资源目录
    ui/i18n.slint               # 响应式的中英文组件文案
    ui/assets/                 # 原创 SVG 线性图标与空状态插画
    ui/types.slint              # 面向 UI 的数据模型
    ui/components/              # 可复用的视觉构件
    ui/pages/documents.slint    # 组合出转换页面
    ui/pages/placeholder.slint  # 由元数据驱动的计划中工具页面
assets/logo.png                  # 未经改动的品牌图形
assets/app-icon.png              # 派生的圆角图标：侧边栏与窗口
assets/macos-icon.png            # 按 Apple 网格派生的图标：Dock 与 .icns
Taskfile.yml                     # 运行、检查、测试与发布构建的入口
scripts/icon/                    # 生成两份图标，产物已提交
scripts/macos-bundle.sh          # 把 HandyBox.app 打包成压缩包
scripts/zip-binary.sh            # 压缩 Windows 与 Linux 可执行文件
scripts/Dockerfile.cross         # 仅用于构建的 Windows/Linux 交叉工具链
```

```text
Slint 组件 → 回调 → 控制器 → 有界命令通道
                                  ↓
                              后台工作线程
                                  ↓
                   核心工具 / 系统对话框 / 剪贴板
                                  ↓
Slint 属性 ← 控制器 ← 50 ms 事件轮询 ← 结果通道
```

**Core：** 公开操作使用 Rust 的输入/输出类型，完全不知道窗口的存在。工具目录为每个工具提供稳定的枚举 ID 和字符串路由；导航不依赖菜单下标。引擎是 `tools/` 内的适配器，因此未来的 CLI 或测试都能复用。

**桌面控制器：** 把目录条目翻译成 Slint 模型，按工具名或引擎做大小写不敏感的导航过滤，保留完整的文档结果，并把类型化事件转换成展示属性。使用弱组件句柄避免 UI 所有权循环。只有主线程会碰 Slint 属性。

**工作线程：** 一个命名线程处理有界通道，UI 侧以非阻塞方式提交。文件选择器、转换、剪贴板访问和导出都在这里执行。命令拥有自己的输入，事件拥有自己的结果。可恢复错误和解析器 panic 都会清除忙碌状态。事件定时器在整个窗口生命周期内由应用持有。没有任何服务器或网络服务。

操作结果以 `Toast` 呈现：窗口底部的浮动提示，到时自动消失。它是布局的兄弟节点而不是布局里的一行，因此出现和隐藏都不会引起页面重排，也不会占用 Markdown 面板的高度。它也从不被创建或销毁，所以淡入淡出都能播完；新的提示到来时会重置倒计时而不是沿用剩余时间。错误比成功提示停留更久，点击可以立即关闭。`StatusBar` 刻意做成静态的：它被所有工具共用，放在那里的临时消息就必须在切换导航时清理。

**UI 组合：** 页面接收属性并发出回调；它们不做文件 I/O，也不引用引擎 API。`app.slint` 只负责组合外壳和路由。`Sidebar` 组合 `NavItem`；`DocumentsPage` 组合 `PageHeader`、`FileCard`、格式 `Badge` 和 `MarkdownPanel`；`MarkdownPanel` 组合 `Action`、`CodeView` 和 `EmptyState`。`CodeView` 把行放进 `ListView`：`for` 的直接父元素是 `ListView` 时会被编译成虚拟化的 repeater，这是大文档不卡的唯一原因——用一个 `Text` 装整篇时，一份 26000 字的中文文档会让窗口冻住十几秒，因为 FemtoVG 要整形全文才能测量尺寸，而它 1000 条的整形缓存在没有词边界的文本上会持续颠簸。`StatusBar` 在各工具间共享。视觉改动应放在最小的相关组件里，共享样式令牌放在 `Theme` 中。

首个版本刻意采用小而类型化的命令/事件桥接，而不是动态插件 ABI 或通用 JSON 分发。随着真实工具实现变多，可把它们的状态和回调绑定移到 `src/controllers/<tool>.rs`，让外壳控制器只负责注册路由和全局状态。

### 品牌与图标

用户提供的 `assets/logo.png` 是品牌图形唯一且未经改动的来源，不做裁剪或改色。主题中的藏青强调色与柔和米色背景取自它的配色。

该文件是不透明的正方形，放进侧边栏会是一块硬边方砖，在 macOS Dock 里也和其他应用图标格格不入。[`scripts/icon`](scripts/icon) 从它派生出两份圆角变体，均已提交到仓库，因此普通的 `cargo build` 不会运行这个生成器；更换 logo 后用 `task icon` 重新生成即可。

| 派生文件 | 画布 | 图形占比 | 使用方 |
| --- | --- | --- | --- |
| `assets/app-icon.png` | 256px | 铺满画布 | 侧边栏 `Brand`、Slint 窗口图标 |
| `assets/macos-icon.png` | 512px | 80.5%，四周透明留白 | 运行时 Dock 图标、打包的 `AppIcon.icns` |

留白遵循 Apple 的图标网格（1024 中占 824）及其 22.5% 圆角半径。没有这圈留白，Dock 里的图标会明显比相邻应用大一号。而侧边栏和窗口图标是自己控制显示尺寸的，同样的留白只会让标识变小，这就是要分成两份的原因。

两个画布都刻意做得很小，因为它们会被编译进可执行文件：256px 足够 2 倍屏下 46pt 的侧边栏标识和窗口图标，512px 足够最大的 Dock 图标。尺寸每翻一倍，嵌入的字节数大约变成四倍——之前用 1024px 时，仅这两个图标就给每个平台的二进制增加了 1.7 MB。

`Brand` 负责侧边栏标识。`Icon` 负责展示并着色内置 SVG，`ToolIcon` 在桌面层把稳定的工具路由映射到图标。`Action` 接受可选图片，同时保留文字标签与键盘/无障碍行为。`SearchField`、状态指示器、文件操作和占位页都复用这些组件。这些 SVG 是项目原创资源，统一使用 24 × 24 视图框、1.7px 圆角描边，不依赖外部字体或网络请求。唯一的例外是 `github.svg`——它是 GitHub 自己的标识，按其品牌规范收录，仅用于标注指向本仓库的链接；它是实心剪影，不是描边图形。较大的文档空状态有自己的 SVG 插画。

`Theme` 的调色板只有浅色一套。在 macOS 的深色模式下，这会让近乎全黑的标题栏压在浅色窗口上方，因此 `macos.rs` 把应用外观固定为 Aqua，并把窗口主题设为浅色。后者无法用 Slint 自带的 `set_color_scheme` 完成：它的 winit 后端在 Apple 平台会把设置主题那段代码 cfg 掉（`use_winit_theme`），把窗口装饰留给系统跟随。设置时窗口必须已经存在，所以 `main` 先 show 组件，设置标题栏，再运行事件循环。

新增图标的步骤：把 SVG 放进 `ui/assets/icons/`，在 `ui/icons.slint` 中注册，然后使用 `Icon` 或 `Action.icon` 属性。新增菜单项时，在 `ToolIcon` 里补上工具路由映射。图标资源和视觉映射不要放进核心库。两种渲染器使用同一套内嵌资源。

### 本地化

`SettingsMenu` 是一个可复用的侧边栏组件：文字标识旁的齿轮按钮，点击弹出 `PopupWindow` 列出两种语言。整行并列的语言按钮，为一个很少改动的设置占掉了一整行侧边栏高度。两个选项保持各自的母语写法（`English`、`中文`），无论当前是哪种语言都认得出来。`ui/i18n.slint` 集中管理静态组件文案，通过响应式绑定跟随 `I18n.chinese`。Rust 侧的 `locale.rs` 负责翻译后的工具目录元数据、状态消息和应用错误描述。核心层的错误暴露为稳定的 `DocumentIssue` 分类；翻译从不依赖匹配英文错误字符串。工作线程事件携带类型化的消息/结果，在展示时才按当前语言翻译——包括在转换过程中切换语言的情况。文件路径、引擎名称和文档内容永远不翻译。

界面的比例字体跟随所选语言（`Language::font_family`）：中文用 PingFang SC、Microsoft YaHei 或 Noto Sans CJK SC，英文沿用原来的拉丁字体。这不只是排版问题，更是性能要求。当主字体缺少哪怕一个字形时，FemtoVG 渲染器就会为该文本元素重新查询系统回退字体列表——每次布局、每一帧都查一次，在 macOS 上实测约 0.6 ms/元素，另外还有一次性的 CJK 字体加载和字形光栅化开销。因此界面文案必须落在所属字体的覆盖范围内；英文徽标写作 `DOCUMENT TO MARKDOWN`，正是为了避开 Helvetica Neue 没有的 `→`。

原生文件对话框的标题和筛选器名称在打开时使用所选语言；原生按钮和系统对话框仍跟随操作系统语言。底层的系统/解析器诊断信息保留原文，放在翻译后的 **技术详情** 标签下。启动阶段的技术诊断和命令行帮助保持英文。

语言代码（`en` 或 `zh-CN`）会原子地写入平台配置目录下的 `language` 文件（`directories::ProjectDirs`）：macOS 为 `~/Library/Application Support/dev.handybox.HandyBox/`，Windows 为 `%APPDATA%\\handybox\\HandyBox\\config\\`，Linux 为 `$XDG_CONFIG_HOME/handybox/` 或 `~/.config/handybox/`。缺失或无效的设置默认回到英文。该目录不存放任何文档内容。保存失败时，所选语言在当前会话仍然生效，状态栏会提示无法记住这一偏好。

新增工具时，请在 `tool_text` 中补上两种语言的翻译；新增静态界面文案时，在 `ui/i18n.slint` 中加一个属性。新的运行时消息应作为类型化的 `Message` / `FailureKind` 变体加入，并同时提供两种语言。不要把翻译后的文本塞进工作线程事件，也不要为了切换语言重建窗口。

## 新增一个工具

1. 在 `handybox-core/src/catalog.rs` 中添加稳定的 `ToolId` 和 `ToolDescriptor`（或直接用已有的占位项）。定义名称、描述、引擎、计划中的能力与可用性。新增的「不可用」条目会自动获得导航入口和占位页面。
2. 添加 `tools/<tool>.rs`，定义类型化的输入、输出和公开操作。核心库里不要出现 Slint 和原生对话框。只有在实现真正用到时才添加引擎依赖。
3. 添加该工具的后台命令/结果变体，必要时把工具相关的任务拆成独立的 worker 模块。保持队列准入有界；昂贵的工作不能在 UI 回调里执行。为扫描和批量操作有意识地加上进度与取消能力。
4. 添加控制器模块来绑定回调并保留工具状态。始终对照工具目录校验路由。工具状态必须能在页面切换后保留。
5. 创建 `ui/pages/<tool>.slint`，复用共享组件，并在 `ui/app.slint` 中添加路由。页面特有的布局留在该页面内。只有 UI 需要展示的数据才扩展 `ui/types.slint`。
6. 测试真实的输入/输出与失败行为。对于破坏性工具，必须先给出具体的结果预览并要求显式操作，才能修改文件。对压缩包要校验解压路径和展开后的大小。剪贴板监听必须是可选开启的。
7. 把目录条目标记为可用，必要时更新工作区的工具数量，并更新本 README 的状态表。可用的条目必须有能正常工作的页面和控制器。

## 渲染

渲染器配置具备回退能力，并对照 Slint 1.13.1 的后端源码做了验证。**FemtoVG/OpenGL** 和**软件**两种渲染器都会被编译。Winit 优先选择 FemtoVG；如果窗口/绘图表面创建时无法初始化它，后端会尝试软件渲染器。把回退放在窗口创建这一步很关键：只在后端选择阶段捕获错误为时过早。

`SLINT_BACKEND=winit-software` 请求 CPU 渲染，`SLINT_BACKEND=winit-femtovg` 请求 GPU 渲染，两者都走相同的后端回退机制。设为 `winit` 或不设置则使用默认值。不支持的后端名称会在启动时报错。直接依赖的 `i-slint-backend-winit` 与 Slint 版本耦合，必须和 `slint-build` 一起升级。

界面使用基础的矩形、边框和文本，而不是依赖 GPU-only 渲染的特效。启动时的自动回退无法挽救所有会话中途的驱动故障。在目标机器上应同时验证 GPU 与强制 CPU 两种会话。

参考：[Slint 渲染器](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/)、[锁定版本的 Winit 后端](https://docs.rs/i-slint-backend-winit/1.13.1/i_slint_backend_winit/)、[anydoc API](https://docs.rs/anydoc/0.2.4/anydoc/)。

## 验证

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

集成测试覆盖了 CSV 的 Unicode/表格输出、按内容检测的 RTF、真实的 DOCX ZIP 容器、无效/空/缺失/超大输入、完整结果导出、源文件覆盖保护、Unix 上的符号链接保护、确认后的目标覆盖、Unicode 安全的预览截断以及工具目录一致性。

本地化测试覆盖了每个工具的两种语言、双语与大小写不敏感的搜索、延迟消息的翻译（路径保持原样）、本地化的错误分类，以及偏好设置的往返读写与默认值。手动验证请覆盖：转换前后切换语言、在占位页切换语言、以及选定语言后重启应用。

CI 工作流会在 macOS、Windows 和 Linux 上运行这些检查。CI 不做原生窗口交互。手动验证 UI 时，请检查全部九个导航项、搜索（包括无匹配的情况）、取消文件选择器、出错后再次转换、复制、导出/覆盖确认、超长结果、页面切换以及强制 CPU 渲染。当前实现是在 macOS 上本地构建和测试的；其他平台的结果需要在 CI 工作流运行后查看。
