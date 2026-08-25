pub mod account;
pub mod agent;
pub mod annotation;
pub mod attachments;
pub mod bookmarks;
pub mod chat;
pub mod collections;
pub mod cron;
pub mod file_browser;
pub mod files;
pub mod graph;
pub mod image_cache;
pub mod knowledge;
pub mod library;
pub mod llm_provider;
pub mod notes;
pub mod projects;
pub mod reader;
pub mod region;
pub mod research;
pub mod search;
pub mod sync;
pub mod system;
pub mod tags;
pub mod timeline;
pub mod translation;
pub mod vault;

#[tauri::command]
pub fn greet(name: String) -> String {
    format!("你好，{}！欢迎来到 思库。", name)
}
