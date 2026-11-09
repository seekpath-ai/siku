//! PDF 栈分工（2026-11 减法评审结论，三个引擎各司其职、勿再评估删减）：
//! - **pdfium**（bindings，三平台随包分发于 `bin/`）：正文/段落/渲染主引擎；
//!   进程级全局锁 `pdfium_guard`，非线程安全。
//! - **pdf_oxide**（extractor/paragraphs/figures/document_text）：pdfium 动态库
//!   缺失或损坏时的活跃兜底链（纯 Rust，无外部库），输出无锚点降级文本。
//! - **lopdf**（仅 parser.rs）：元数据（title/author/XMP）提取，容忍 pdfium
//!   无法打开的畸形 PDF。

pub mod bindings;
pub mod chunker;
pub mod extractor;
pub mod figure_layout;
pub mod figures;
pub mod paragraphs;
pub mod parser;
pub mod renderer;

/// Corpus-level regression checks (skipped when the demo PDFs are absent).
#[cfg(test)]
mod corpus_regression;
