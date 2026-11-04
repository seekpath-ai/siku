use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Injected snapshot script. Walks the DOM, tags every visible interactive
/// element with `data-siku-ref="<gen>-<n>"` and returns a compact JSON map.
/// The generation counter invalidates older refs automatically: after a
/// navigation or DOM rewrite the old refs simply no longer exist, and the
/// interaction tools tell the model to re-snapshot.
const SNAPSHOT_JS: &str = r#"(() => {
  const WANT_TEXT = __WANT_TEXT__;
  const gen = (window.__sikuGen || 0) + 1;
  window.__sikuGen = gen;
  for (const e of document.querySelectorAll('[data-siku-ref]')) e.removeAttribute('data-siku-ref');
  const MAX = 400;
  const SEL = 'a[href],button,input:not([type=hidden]),textarea,select,summary,'
    + '[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],'
    + '[contenteditable=true],[onclick]';
  const visible = el => {
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return false;
    const s = getComputedStyle(el);
    return s.display !== 'none' && s.visibility !== 'hidden';
  };
  const clip = (t, n) => {
    t = (t || '').replace(/\s+/g, ' ').trim();
    return t.length > n ? t.slice(0, n) + '…' : t;
  };
  const all = document.querySelectorAll(SEL);
  const out = [];
  for (const el of all) {
    if (out.length >= MAX) break;
    if (!visible(el)) continue;
    const ref = gen + '-' + (out.length + 1);
    el.setAttribute('data-siku-ref', ref);
    const tag = el.tagName.toLowerCase();
    const e = { ref, tag };
    const role = el.getAttribute('role'); if (role) e.role = role;
    const type = el.getAttribute('type'); if (type && (tag === 'input' || tag === 'button')) e.type = type;
    const text = el.innerText || el.getAttribute('aria-label') || el.getAttribute('title')
      || el.getAttribute('value') || el.getAttribute('placeholder') || '';
    if (text) e.text = clip(text, 80);
    if (tag === 'a') { const h = el.getAttribute('href'); if (h) e.href = clip(h, 120); }
    const ph = el.getAttribute('placeholder'); if (ph) e.placeholder = clip(ph, 60);
    if (tag === 'select') {
      const opt = el.options[el.selectedIndex];
      if (opt) e.value = clip(opt.text, 40);
    }
    if (el.disabled) e.disabled = true;
    out.push(e);
  }
  const res = {
    url: location.href, title: document.title, gen,
    count: out.length, total: all.length, elements: out,
  };
  if (WANT_TEXT) {
    res.text = clip(document.body ? document.body.innerText : '', 4000);
  }
  return res;
})()"#;

/// Map the active tab's interactive elements to numbered refs. This is the
/// primary way the model "sees" a page — screenshots stay exceptional.
pub struct BrowserSnapshotTool;

#[async_trait]
impl Tool for BrowserSnapshotTool {
    fn name(&self) -> &str {
        "browser_snapshot"
    }

    fn description(&self) -> &str {
        "Snapshot the active tab: returns page title/URL plus every visible interactive element \
         (links, buttons, inputs, selects…) as numbered refs like [3-12]. Pass a ref to \
         browser_click / browser_type. Refs go stale when the DOM changes (navigation, dynamic \
         updates) — re-snapshot then. Set include_text=true to also get the page's visible text. Read-only."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "include_text".into(),
            param_type: "boolean".into(),
            description: "Also return the page's visible body text (first 4000 chars), default false".into(),
            required: false,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let page = super::browser::require_page().await?;
        let want_text = args["include_text"].as_bool().unwrap_or(false);
        let js = SNAPSHOT_JS.replace("__WANT_TEXT__", if want_text { "true" } else { "false" });

        let value = page
            .evaluate(js)
            .await
            .map_err(|e| format!("snapshot failed: {e}"))?
            .into_value::<serde_json::Value>()
            .map_err(|e| format!("snapshot result unreadable: {e}"))?;

        Ok(format_snapshot(&value))
    }
}

/// Render the JSON map as compact lines — one per element, minimal tokens.
fn format_snapshot(v: &serde_json::Value) -> String {
    let title = v["title"].as_str().unwrap_or("");
    let url = v["url"].as_str().unwrap_or("");
    let gen = v["gen"].as_u64().unwrap_or(0);
    let count = v["count"].as_u64().unwrap_or(0);
    let total = v["total"].as_u64().unwrap_or(0);

    let mut out = format!("Page: {title} — {url}\n{count} interactive elements (snapshot #{gen})");
    if total > count {
        out.push_str(&format!(" — capped at {count} of {total}; scroll or narrow the page"));
    }
    out.push('\n');

    if let Some(els) = v["elements"].as_array() {
        for el in els {
            let refv = el["ref"].as_str().unwrap_or("?");
            let tag = el["tag"].as_str().unwrap_or("?");
            let mut line = format!("[{refv}] {tag}");
            if let Some(t) = el["type"].as_str() {
                line.push_str(&format!(" type={t}"));
            }
            if let Some(r) = el["role"].as_str() {
                line.push_str(&format!(" role={r}"));
            }
            if let Some(text) = el["text"].as_str().filter(|t| !t.is_empty()) {
                line.push_str(&format!(" \"{text}\""));
            }
            if let Some(ph) = el["placeholder"].as_str() {
                line.push_str(&format!(" placeholder=\"{ph}\""));
            }
            if let Some(val) = el["value"].as_str() {
                line.push_str(&format!(" value=\"{val}\""));
            }
            if let Some(href) = el["href"].as_str() {
                line.push_str(&format!(" href=\"{href}\""));
            }
            if el["disabled"].as_bool().unwrap_or(false) {
                line.push_str(" (disabled)");
            }
            out.push_str(&line);
            out.push('\n');
        }
    }

    if let Some(text) = v["text"].as_str() {
        out.push_str("\n--- visible text ---\n");
        out.push_str(text);
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_compact_lines() {
        let v = serde_json::json!({
            "title": "知网", "url": "https://cnki.net", "gen": 3, "count": 2, "total": 2,
            "elements": [
                {"ref": "3-1", "tag": "input", "type": "search", "placeholder": "搜索文献"},
                {"ref": "3-2", "tag": "a", "text": "下一页", "href": "/page/2"}
            ]
        });
        let out = format_snapshot(&v);
        assert!(out.contains("Page: 知网 — https://cnki.net"));
        assert!(out.contains("[3-1] input type=search placeholder=\"搜索文献\""));
        assert!(out.contains("[3-2] a \"下一页\" href=\"/page/2\""));
    }

    #[test]
    fn notes_capping() {
        let v = serde_json::json!({
            "title": "", "url": "u", "gen": 1, "count": 400, "total": 812, "elements": []
        });
        assert!(format_snapshot(&v).contains("capped at 400 of 812"));
    }
}
