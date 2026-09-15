//! Runtime text is translated at display time, never frozen into worker events.
use handybox_core::{
    catalog::{ToolDescriptor, ToolId},
    tools::{
        codes::CodesIssue, crypto::CryptoIssue, diff::DiffIssue, documents::DocumentIssue,
        json::JsonIssue,
    },
};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    English,
    Chinese,
}

impl Language {
    pub fn from_chinese(chinese: bool) -> Self {
        if chinese {
            Self::Chinese
        } else {
            Self::English
        }
    }
    pub fn chinese(self) -> bool {
        self == Self::Chinese
    }
    pub fn code(self) -> &'static str {
        self.text("en", "zh-CN")
    }
    pub fn text<'a>(self, english: &'a str, chinese: &'a str) -> &'a str {
        if self.chinese() { chinese } else { english }
    }
    /// A UI family that covers the language's own script. A primary font missing
    /// even one glyph makes the renderer re-query the system fallback list for
    /// every affected text item, on every layout and every frame.
    pub fn font_family(self) -> &'static str {
        if self.chinese() {
            if cfg!(target_os = "macos") {
                "PingFang SC"
            } else if cfg!(target_os = "windows") {
                "Microsoft YaHei"
            } else {
                "Noto Sans CJK SC"
            }
        } else if cfg!(target_os = "macos") {
            "Helvetica Neue"
        } else if cfg!(target_os = "windows") {
            "Segoe UI"
        } else {
            "DejaVu Sans"
        }
    }
}

pub fn tool_text(
    tool: &ToolDescriptor,
    lang: Language,
) -> (&'static str, &'static str, &'static str) {
    if !lang.chinese() {
        return (tool.name, tool.subtitle, tool.capabilities);
    }
    match tool.id {
        ToolId::Documents => (
            "文档转换",
            "将日常文档转换为清晰的 Markdown。",
            "本地解析文档 · Markdown 源码查看 · 复制与导出",
        ),
        ToolId::Json => (
            "JSON 工作台",
            "让结构化数据更容易阅读和处理。",
            "JSON 格式化与校验\n使用 jq 语法查询、筛选和转换\n结果查看与文件导出",
        ),
        ToolId::Crypto => (
            "哈希与加密",
            "校验文件完整性，保护本地数据。",
            "SHA-256 / SHA-512 摘要与校验\nage 文件加密与解密\n密码或接收者公钥模式",
        ),
        ToolId::Diff => (
            "文本对比",
            "快速找出两个版本之间的变化。",
            "并排文本与文件对比\n逐行差异与变更统计\n统一 diff 导出",
        ),
        ToolId::Images => (
            "图像处理",
            "在本地缩放、转换和查看图片。",
            "图像格式转换与批量缩放\nPNG 无损优化\nEXIF 元数据查看",
        ),
        ToolId::Archives => (
            "压缩与解压",
            "轻松打包、查看和解压文件。",
            "ZIP / 7z 创建与解压\n归档内容预览\n路径检查与解压大小限制",
        ),
        ToolId::Codes => (
            "条码识别",
            "从图片中读取二维码和条形码。",
            "二维码与多种条形码识别\n导入图片与查看识别结果\n复制识别文本",
        ),
        ToolId::Cleanup => (
            "磁盘清理",
            "发现重复和冗余，找回磁盘空间。",
            "重复文件、空文件与大文件扫描\n相似图片查找\n先预览，再确认清理",
        ),
        ToolId::Clipboard => (
            "剪贴板",
            "为复制的内容提供一个临时工作区。",
            "文本、图片与文件内容查看\n手动收集与内容复制\n可选择启用的会话历史",
        ),
    }
}

pub fn matches_tool(tool: &ToolDescriptor, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    tool.name.to_lowercase().contains(&query)
        || tool_text(tool, Language::Chinese)
            .0
            .to_lowercase()
            .contains(&query)
        || tool.libraries.to_lowercase().contains(&query)
        || tool.key.contains(&query)
}

#[derive(Clone, Debug)]
pub enum FailureKind {
    Document(DocumentIssue),
    Json(JsonIssue),
    Crypto(CryptoIssue),
    Diff(DiffIssue),
    Codes(CodesIssue),
    Clipboard,
    Operation,
    Worker,
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: String,
}

impl Failure {
    pub fn document(error: anyhow::Error) -> Self {
        Self::new(
            error
                .downcast_ref::<DocumentIssue>()
                .copied()
                .map(FailureKind::Document),
            error,
        )
    }

    pub fn json(error: anyhow::Error) -> Self {
        Self::new(
            error
                .downcast_ref::<JsonIssue>()
                .copied()
                .map(FailureKind::Json),
            error,
        )
    }

    pub fn crypto(error: anyhow::Error) -> Self {
        Self::new(
            error
                .downcast_ref::<CryptoIssue>()
                .copied()
                .map(FailureKind::Crypto),
            error,
        )
    }

    pub fn diff(error: anyhow::Error) -> Self {
        Self::new(
            error
                .downcast_ref::<DiffIssue>()
                .copied()
                .map(FailureKind::Diff),
            error,
        )
    }

    pub fn codes(error: anyhow::Error) -> Self {
        Self::new(
            error
                .downcast_ref::<CodesIssue>()
                .copied()
                .map(FailureKind::Codes),
            error,
        )
    }

    fn new(kind: Option<FailureKind>, error: anyhow::Error) -> Self {
        // Keep native/parser diagnostics separate from translated application copy.
        let detail = error
            .chain()
            .skip(1)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        Self {
            kind: kind.unwrap_or(FailureKind::Operation),
            detail,
        }
    }
    /// Whether the failure is about the document the user is editing rather
    /// than about the operation, and therefore belongs beside that document.
    pub fn about_input(&self) -> bool {
        matches!(
            self.kind,
            FailureKind::Json(
                JsonIssue::Syntax { .. }
                    | JsonIssue::Empty
                    | JsonIssue::TooLarge
                    | JsonIssue::NotText
                    | JsonIssue::Multiple
            ) | FailureKind::Diff(DiffIssue::Empty | DiffIssue::TooLarge | DiffIssue::NotText)
        )
    }

    pub fn render(&self, lang: Language) -> String {
        let message = match self.kind {
            FailureKind::Document(issue) if !lang.chinese() => issue.to_string(),
            FailureKind::Document(issue) => match issue {
                DocumentIssue::InputMissing => "找不到输入文件。",
                DocumentIssue::Read => "无法读取文件，请检查访问权限。",
                DocumentIssue::NotFile => "请选择文件，而不是文件夹。",
                DocumentIssue::TooLarge => "文件超过 64 MiB，请选择更小的文档。",
                DocumentIssue::Empty => "文件为空，无法转换。",
                DocumentIssue::Unsupported => {
                    "不支持此文件格式，请选择 Office、PDF、EPUB、RTF 或 CSV 文档。"
                }
                DocumentIssue::NeedsOcr => {
                    "此 PDF 包含需要 OCR 的扫描或图片页面，当前版本仅支持带文本的 PDF。"
                }
                DocumentIssue::Conversion => "转换失败：文件可能已损坏、加密或包含不支持的内容。",
                DocumentIssue::InvalidExtension => "导出文件必须使用 .md 扩展名。",
                DocumentIssue::SourceOverwrite => "不能覆盖输入文档，请选择其他路径。",
                DocumentIssue::CreateOutput => "无法在目标目录创建文件。",
                DocumentIssue::WriteOutput => "写入 Markdown 失败。",
                DocumentIssue::SaveOutput => "无法保存到目标文件。",
            }
            .into(),
            FailureKind::Json(issue) if !lang.chinese() => issue.to_string(),
            FailureKind::Json(issue) => match issue {
                JsonIssue::InputMissing => "找不到输入文件。".into(),
                JsonIssue::Read => "无法读取文件，请检查访问权限。".into(),
                JsonIssue::NotFile => "请选择文件，而不是文件夹。".into(),
                JsonIssue::TooLarge => "JSON 超过 4 MiB，请选择更小的文档。".into(),
                JsonIssue::Empty => "还没有可处理的 JSON。".into(),
                JsonIssue::NotText => "该文件不是 UTF-8 文本。".into(),
                JsonIssue::Multiple => {
                    "这是 JSON 数据流，不是一份 JSON 文档：它包含多个顶层值。".into()
                }
                JsonIssue::Syntax { line, column } => {
                    format!("第 {line} 行第 {column} 列的 JSON 无效。")
                }
                JsonIssue::Filter => "无法理解这个 jq 表达式。".into(),
                JsonIssue::Query => "过滤器在处理该输入时中止。".into(),
                JsonIssue::InvalidExtension => "导出文件必须使用 .json 扩展名。".into(),
                JsonIssue::SourceOverwrite => "不能覆盖来源文档，请选择其他路径。".into(),
                JsonIssue::CreateOutput => "无法在目标目录创建文件。".into(),
                JsonIssue::WriteOutput => "写入 JSON 失败。".into(),
                JsonIssue::SaveOutput => "无法保存到目标文件。".into(),
            },
            FailureKind::Crypto(issue) if !lang.chinese() => issue.to_string(),
            FailureKind::Crypto(issue) => match issue {
                CryptoIssue::InputMissing => "找不到输入文件。",
                CryptoIssue::Read => "无法读取文件，请检查访问权限。",
                CryptoIssue::NotFile => "请选择文件，而不是文件夹。",
                CryptoIssue::TooLarge => "文件超过 2 GiB，请选择更小的文件。",
                CryptoIssue::Expected => {
                    "请填写十六进制的 SHA-256 或 SHA-512 校验和，或留空只计算摘要。"
                }
                CryptoIssue::Passphrase => "请输入密码。",
                CryptoIssue::Recipient => "接收者必须是以 age1 开头的 age 公钥。",
                CryptoIssue::Identity => "私钥以 AGE-SECRET-KEY-1 开头。",
                CryptoIssue::NotEncrypted => "这不是 age 加密文件。",
                CryptoIssue::NeedsPassphrase => "该文件由密码保护，而不是密钥，请输入它的密码。",
                CryptoIssue::NeedsKey => "该文件加密给了公钥，而不是密码，请提供对应的私钥。",
                CryptoIssue::WrongSecret => "密码或密钥与该文件加密时使用的不一致。",
                CryptoIssue::ExcessiveWork => "打开该文件所需的计算量超出本机允许的上限。",
                CryptoIssue::Encrypt => "加密失败。",
                CryptoIssue::Decrypt => "解密失败：文件可能已损坏或不完整。",
                CryptoIssue::InvalidExtension => "加密文件必须使用 .age 扩展名。",
                CryptoIssue::SourceOverwrite => "不能覆盖正在读取的文件，请选择其他路径。",
                CryptoIssue::CreateOutput => "无法在目标目录创建文件。",
                CryptoIssue::WriteOutput => "写入文件失败。",
                CryptoIssue::SaveOutput => "无法保存到目标文件。",
            }
            .into(),
            FailureKind::Diff(issue) if !lang.chinese() => issue.to_string(),
            FailureKind::Diff(issue) => match issue {
                DiffIssue::InputMissing => "找不到输入文件。",
                DiffIssue::Read => "无法读取文件，请检查访问权限。",
                DiffIssue::NotFile => "请选择文件，而不是文件夹。",
                DiffIssue::TooLarge => "每一侧最多 2 MiB，请选择更小的文本。",
                DiffIssue::Empty => "还没有可对比的内容。",
                DiffIssue::NotText => "该文件不是 UTF-8 文本。",
                DiffIssue::InvalidExtension => "导出文件必须使用 .diff 或 .patch 扩展名。",
                DiffIssue::SourceOverwrite => "不能覆盖参与对比的文件，请选择其他路径。",
                DiffIssue::CreateOutput => "无法在目标目录创建文件。",
                DiffIssue::WriteOutput => "写入差异失败。",
                DiffIssue::SaveOutput => "无法保存到目标文件。",
            }
            .into(),
            FailureKind::Codes(issue) if !lang.chinese() => issue.to_string(),
            FailureKind::Codes(issue) => match issue {
                CodesIssue::InputMissing => "找不到输入文件。",
                CodesIssue::Read => "无法读取文件，请检查访问权限。",
                CodesIssue::NotFile => "请选择文件，而不是文件夹。",
                CodesIssue::TooLarge => "图片超过 32 MiB，请选择更小的图片。",
                CodesIssue::NotImage => "该文件不是图片，或其格式当前版本无法读取。",
                CodesIssue::TooManyPixels => "图片超过 4000 万像素，请先缩小后再识别。",
            }
            .into(),
            FailureKind::Clipboard => lang
                .text("Could not access the clipboard.", "无法访问剪贴板。")
                .into(),
            FailureKind::Operation => lang
                .text(
                    "The operation stopped unexpectedly. Please try again.",
                    "操作意外停止，请重试。",
                )
                .into(),
            FailureKind::Worker => lang
                .text("Could not start the operation.", "无法启动操作。")
                .into(),
        };
        if self.detail.is_empty() {
            message
        } else {
            format!(
                "{}  ·  {}: {}",
                message,
                lang.text("Details", "技术详情"),
                self.detail
            )
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum Message {
    #[default]
    Ready,
    Picking,
    Reading,
    Converting,
    Copying,
    Saving,
    Running,
    Hashing,
    Scanning,
    Encrypting,
    Decrypting,
    KeyGenerated,
    Copied,
    Saved(PathBuf),
    Cancelled,
    Complete,
    EmptyResult,
    NoOutput,
    Error(Failure),
}

impl Message {
    /// Outcomes worth surfacing in the page. Idle and progress states are not:
    /// the file card already shows work in flight, and a banner for them would
    /// flicker on every step.
    pub fn is_notice(&self) -> bool {
        matches!(
            self,
            Self::Copied
                | Self::Saved(_)
                | Self::Cancelled
                | Self::EmptyResult
                | Self::NoOutput
                | Self::KeyGenerated
                | Self::Error(_)
        )
    }

    pub fn render(&self, lang: Language) -> String {
        match self {
            Self::Ready => lang.text(
                "Ready when you are. Choose a document to get started.",
                "准备就绪，选择一份文档开始。",
            ),
            Self::Picking => lang.text(
                "Choose a file in the file dialog…",
                "请在文件对话框中选择文件…",
            ),
            Self::Reading => lang.text("Reading your document…", "正在读取文档…"),
            Self::Converting => lang.text("Converting locally…", "正在本地转换…"),
            Self::Copying => lang.text("Copying to the clipboard…", "正在复制到剪贴板…"),
            Self::Running => lang.text("Running your filter locally…", "正在本地运行表达式…"),
            Self::Saving => lang.text("Choose where to save your file…", "请选择文件的保存位置…"),
            Self::Hashing => lang.text("Reading and hashing locally…", "正在本地读取并计算摘要…"),
            Self::Scanning => lang.text("Looking for codes locally…", "正在本地识别图片中的条码…"),
            Self::Encrypting => lang.text("Encrypting locally…", "正在本地加密…"),
            Self::Decrypting => lang.text("Decrypting locally…", "正在本地解密…"),
            Self::KeyGenerated => lang.text(
                "New key pair created. Save the secret key somewhere safe: without it, nothing encrypted to this public key can be opened again.",
                "已生成新的密钥对。请妥善保存私钥：没有它，加密给该公钥的文件将无法再打开。",
            ),
            Self::Copied => lang.text("Copied to your clipboard.", "已复制到剪贴板。"),
            Self::Cancelled => lang.text(
                "Operation cancelled. Your current result is unchanged.",
                "操作已取消，当前结果保持不变。",
            ),
            Self::Complete => lang.text(
                "Conversion complete. Your Markdown is ready.",
                "转换完成，Markdown 已就绪。",
            ),
            Self::NoOutput => lang.text(
                "The filter ran, but produced no output for this document.",
                "表达式已运行，但没有为该文档产生任何输出。",
            ),
            Self::EmptyResult => lang.text(
                "Conversion finished, but no text was found in this document.",
                "转换完成，但文档中未找到可提取的文本。",
            ),
            Self::Saved(path) => {
                return format!("{} {}", lang.text("Saved to", "已保存到"), path.display());
            }
            Self::Error(error) => return error.render(lang),
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use handybox_core::catalog::TOOLS;

    #[test]
    fn every_tool_is_localized_and_searchable_in_both_languages() {
        for tool in TOOLS {
            let (name, subtitle, capabilities) = tool_text(tool, Language::Chinese);
            assert_ne!(name, tool.name);
            assert!(!subtitle.is_empty() && !capabilities.is_empty());
            assert!(matches_tool(tool, name));
            assert!(matches_tool(tool, &tool.name.to_uppercase()));
            assert!(matches_tool(tool, tool.libraries));
        }
        assert!(!matches_tool(&TOOLS[0], "no-such-tool"));
    }

    #[test]
    fn delayed_messages_use_the_display_language_and_keep_paths_unchanged() {
        let event = Message::Saved(PathBuf::from("/tmp/中文 English.md"));
        assert!(event.render(Language::English).starts_with("Saved to"));
        let chinese = event.render(Language::Chinese);
        assert!(chinese.starts_with("已保存到"));
        assert!(chinese.ends_with("/tmp/中文 English.md"));
        let error = Failure::document(anyhow::anyhow!(DocumentIssue::NeedsOcr));
        assert!(error.render(Language::Chinese).contains("扫描"));
        assert!(error.render(Language::English).contains("OCR"));
    }

    #[test]
    fn json_failures_keep_their_position_and_their_technical_detail() {
        let issue = JsonIssue::Syntax {
            line: 3,
            column: 12,
        };
        let error = Failure::json(anyhow::anyhow!("trailing comma").context(issue));
        assert!(error.about_input());
        let english = error.render(Language::English);
        assert!(english.contains("line 3, column 12"));
        assert!(english.contains("trailing comma"), "{english}");
        let chinese = error.render(Language::Chinese);
        assert!(chinese.contains("第 3 行第 12 列"));
        assert!(chinese.contains("trailing comma"));
        // A filter is about the expression, not about the document beside it.
        let filter = Failure::json(anyhow::anyhow!("expected token").context(JsonIssue::Filter));
        assert!(!filter.about_input());
        assert!(filter.render(Language::Chinese).contains("jq"));
        // What the strict switch reports, and where it reports it.
        let stream =
            Failure::json(anyhow::anyhow!("2 top-level values").context(JsonIssue::Multiple));
        assert!(stream.about_input());
        assert!(
            stream
                .render(Language::English)
                .contains("more than one top-level value")
        );
        assert!(stream.render(Language::Chinese).contains("多个顶层值"));
    }

    #[test]
    fn crypto_failures_name_the_secret_the_file_actually_wants() {
        // The distinction age itself reports as "no matching keys": saying which
        // kind of secret is missing is the whole point of keeping it separate.
        let needs = Failure::crypto(anyhow::anyhow!(CryptoIssue::NeedsPassphrase));
        assert!(needs.render(Language::English).contains("passphrase"));
        assert!(needs.render(Language::Chinese).contains("密码"));
        let wrong = Failure::crypto(anyhow::anyhow!(CryptoIssue::WrongSecret));
        assert!(wrong.render(Language::Chinese).contains("密钥"));
        // A crypto failure belongs in the toast, never on the JSON status line.
        assert!(!wrong.about_input());
        // age's own diagnostic is kept as the cause, in either language.
        let detail =
            Failure::crypto(anyhow::anyhow!("invalid bech32").context(CryptoIssue::Identity));
        assert!(detail.render(Language::English).contains("invalid bech32"));
        assert!(detail.render(Language::Chinese).contains("invalid bech32"));
    }

    #[test]
    fn barcode_failures_are_about_the_file_rather_than_the_picture_in_it() {
        // Finding no code is an outcome the page states itself, so every failure
        // this tool can report is about opening the file and belongs in a toast.
        let not_image = Failure::codes(anyhow::anyhow!(CodesIssue::NotImage));
        assert!(!not_image.about_input());
        assert!(not_image.render(Language::English).contains("not an image"));
        assert!(not_image.render(Language::Chinese).contains("不是图片"));
        let pixels = Failure::codes(anyhow::anyhow!(CodesIssue::TooManyPixels));
        assert!(pixels.render(Language::English).contains("40 megapixels"));
        assert!(pixels.render(Language::Chinese).contains("4000 万像素"));
        // The decoder's own diagnostic is kept as the cause, in either language.
        let detail =
            Failure::codes(anyhow::anyhow!("unsupported color type").context(CodesIssue::NotImage));
        assert!(
            detail
                .render(Language::Chinese)
                .contains("unsupported color type")
        );
    }

    #[test]
    fn diff_failures_separate_the_texts_from_the_operation() {
        // Nothing to compare is a state of the two editors, so it belongs on
        // the line under them rather than in a toast that has to be dismissed.
        let empty = Failure::diff(anyhow::anyhow!(DiffIssue::Empty));
        assert!(empty.about_input());
        assert!(
            empty
                .render(Language::English)
                .contains("nothing to compare")
        );
        assert!(empty.render(Language::Chinese).contains("可对比"));
        // Saving is an operation: where it failed is not a property of a text.
        let overwrite = Failure::diff(anyhow::anyhow!(DiffIssue::SourceOverwrite));
        assert!(!overwrite.about_input());
        assert!(overwrite.render(Language::Chinese).contains("覆盖"));
        assert!(
            Failure::diff(anyhow::anyhow!(DiffIssue::TooLarge))
                .render(Language::English)
                .contains("2 MiB")
        );
    }
}
