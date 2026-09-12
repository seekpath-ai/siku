#!/usr/bin/env python3
"""本地 OpenAI 兼容 /v1/embeddings 服务（开发/验证用）。

Siku 的向量检索腿只在 embedding_backend = "api" 且 embedding_base_url 非空时
启用（见 src-tauri/src/ai/retriever.rs）。本脚本用 fastembed 起一个最小嵌入
服务，供应用接入或调试本地语义检索。

依赖：fastembed（自带 onnxruntime）。

运行：
    python3 -m pip install fastembed
    python3 scripts/local_embed_server.py

环境变量：
    SIKU_EMBED_MODEL  模型名（默认 BAAI/bge-small-zh-v1.5）
    SIKU_EMBED_PORT   监听端口（默认 8899）
    SIKU_EMBED_CACHE  模型缓存目录（默认系统临时目录）

在「设置 → 嵌入」里填：后端 api、Base URL http://127.0.0.1:8899/v1、
Model 与 SIKU_EMBED_MODEL 一致。
"""
import json
import os
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = os.environ.get("SIKU_EMBED_MODEL", "BAAI/bge-small-zh-v1.5")
PORT = int(os.environ.get("SIKU_EMBED_PORT", "8899"))
CACHE = os.environ.get("SIKU_EMBED_CACHE") or os.path.join(
    tempfile.gettempdir(), "siku-embed-cache"
)

print(f"loading {MODEL} ...", flush=True)
t0 = time.time()
from fastembed import TextEmbedding  # noqa: E402

_embedder = TextEmbedding(model_name=MODEL, cache_dir=CACHE)
DIM = len(list(_embedder.embed(["warmup"]))[0])
print(f"ready: {MODEL} ({DIM} dims) in {time.time()-t0:.1f}s on :{PORT}", flush=True)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep the console readable
        pass

    def _json(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            self._json(200, {"object": "list", "data": [{"id": MODEL, "object": "model"}]})
        else:
            self._json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        if not self.path.rstrip("/").endswith("/embeddings"):
            return self._json(404, {"error": {"message": "not found"}})
        n = int(self.headers.get("Content-Length", "0"))
        try:
            req = json.loads(self.rfile.read(n) or b"{}")
        except json.JSONDecodeError as e:
            return self._json(400, {"error": {"message": f"bad json: {e}"}})

        raw = req.get("input", "")
        texts = raw if isinstance(raw, list) else [raw]
        texts = [t if isinstance(t, str) else json.dumps(t) for t in texts]
        if not texts:
            return self._json(400, {"error": {"message": "input is required"}})

        # Siku 的 api_embed_texts 只读 data[].embedding 与顺序，model 回显即可。
        vectors = [v.tolist() for v in _embedder.embed(texts)]
        tokens = sum(len(t) for t in texts) // 4
        self._json(200, {
            "object": "list",
            "model": req.get("model") or MODEL,
            "data": [
                {"object": "embedding", "index": i, "embedding": v}
                for i, v in enumerate(vectors)
            ],
            "usage": {"prompt_tokens": tokens, "total_tokens": tokens},
        })


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
