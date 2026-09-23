pub mod bindings;
pub mod chunker;
pub mod extractor;
pub mod figures;
pub mod paragraphs;
pub mod parser;
pub mod renderer;

/// Corpus-level regression checks (skipped when the demo PDFs are absent).
#[cfg(test)]
mod corpus_regression;
