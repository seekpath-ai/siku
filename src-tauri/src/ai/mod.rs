pub mod llm;
pub mod agent;
pub mod rag;
pub mod translation;
pub mod scraping;
pub mod embedder;
pub mod retriever;
pub mod region_detection;
pub mod query;

/// Retrieval evaluation harness (golden set → recall/MRR/nDCG).
#[cfg(test)]
mod retrieval_eval;
