//! Runtime text is translated at display time, never frozen into worker events.
use handybox_core::{
    catalog::{ToolDescriptor, ToolId},
    tools::documents::DocumentIssue,
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
        let kind = error
            .downcast_ref::<DocumentIssue>()
            .copied()
            .map(FailureKind::Document)
            .unwrap_or(FailureKind::Operation);
        // Keep native/parser diagnostics separate from translated application copy.
        let detail = error
            .chain()
            .skip(1)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ");
        Self { kind, detail }
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
    Copied,
    Saved(PathBuf),
    Cancelled,
    Complete,
    EmptyResult,
    Error(Failure),
}

impl Message {
    /// Outcomes worth surfacing in the page. Idle and progress states are not:
    /// the file card already shows work in flight, and a banner for them would
    /// flicker on every step.
    pub fn is_notice(&self) -> bool {
        matches!(
            self,
            Self::Copied | Self::Saved(_) | Self::Cancelled | Self::EmptyResult | Self::Error(_)
        )
    }

    pub fn render(&self, lang: Language) -> String {
        match self {
            Self::Ready => lang.text(
                "Ready when you are. Choose a document to get started.",
                "准备就绪，选择一份文档开始。",
            ),
            Self::Picking => lang.text(
                "Choose a document in the file dialog…",
                "请在文件对话框中选择文档…",
            ),
            Self::Reading => lang.text("Reading your document…", "正在读取文档…"),
            Self::Converting => lang.text("Converting locally…", "正在本地转换…"),
            Self::Copying => lang.text("Copying Markdown…", "正在复制 Markdown…"),
            Self::Saving => lang.text(
                "Choose where to save your Markdown…",
                "请选择 Markdown 的保存位置…",
            ),
            Self::Copied => lang.text(
                "Markdown copied to your clipboard.",
                "Markdown 已复制到剪贴板。",
            ),
            Self::Cancelled => lang.text(
                "Operation cancelled. Your current result is unchanged.",
                "操作已取消，当前结果保持不变。",
            ),
            Self::Complete => lang.text(
                "Conversion complete. Your Markdown is ready.",
                "转换完成，Markdown 已就绪。",
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
}
