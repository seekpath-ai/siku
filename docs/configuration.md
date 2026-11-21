# 配置说明

## LLM Provider

在应用「设置 → 模型」中配置，支持：

- OpenAI
- Anthropic (Claude)
- DeepSeek
- SiliconFlow
- Ollama（本地）
- Qwen（通义千问）
- Zhipu（智谱）
- Kimi（月之暗面）
- Gemini

代理为可选配置，留空直连。

## 网络搜索

Agent 的 `web_search` 按设置中的引擎链**按序回退**，首个出结果的生效。默认 Bing（国内直连）→ DuckDuckGo；Tavily / Brave Search 填 API Key、SearXNG 填实例地址后可加入回退链。

## 语义检索（可选）

关键词检索（FTS5）开箱即用，不需要配置。

语义召回是可选附加能力：在「设置 → 语义搜索」填入 OpenAI 兼容的 `/v1` 地址即启用，留空关闭。关键词与语义按 RRF 融合，可以同时启用。

| 端点 | 启动方式 | Base URL | 模型名 |
|---|---|---|---|
| Ollama | `ollama pull bge-m3` | `http://127.0.0.1:11434/v1` | `bge-m3` |
| llama.cpp | `llama-server --embeddings -m bge-m3.gguf` | `http://127.0.0.1:8080/v1` | 任意 |
| 本项目脚本 | `python3 -m pip install fastembed && python3 scripts/local_embed_server.py` | `http://127.0.0.1:8899/v1` | `BAAI/bge-small-zh-v1.5` |
| 云端 API | 无需自建 | `https://api.openai.com/v1` 等 | `text-embedding-3-small` 等 |

保存设置后，在图书馆对文献右键执行「重建索引」。端点不可用时，语义检索不参与召回，不会用占位向量凑结果。

## 日志

```bash
SIKU_LOG=debug pnpm tauri dev
```

日志目录：`~/.siku/logs/`
