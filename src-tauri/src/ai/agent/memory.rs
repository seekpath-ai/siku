/// Manages conversation context to stay within token budgets
pub struct ConversationMemory {
    /// Maximum token budget for the full conversation
    pub max_tokens: usize,
    /// The system prompt (always kept)
    pub system_prompt: Option<String>,
}

impl ConversationMemory {
    pub fn new(max_tokens: usize, system_prompt: Option<String>) -> Self {
        Self {
            max_tokens,
            system_prompt,
        }
    }

    /// Estimate token count for a string (rough heuristic: ~4 chars per token)
    pub fn estimate_tokens(text: &str) -> usize {
        let chars = text.chars().count();
        // Rough heuristic: Latin ~4 chars/token, CJK ~1.5 chars/token
        let cjk_count = text.chars().filter(|c| c > &'\u{2e80}').count();
        let latin_count = chars - cjk_count;
        (latin_count / 4) + (cjk_count * 2 / 3)
    }

    /// Flat per-image estimate used for truncation budgeting. Vision APIs
    /// bill an image at a few hundred to ~2k tokens regardless of the base64
    /// payload size, so counting base64 chars (raw_bytes/3 per "token")
    /// overestimates by two orders of magnitude — and worse, a single large
    /// pasted screenshot then exceeds the whole input budget and truncate()
    /// would drop the CURRENT user message entirely, leaving the model with
    /// an empty human turn.
    const IMAGE_TOKEN_ESTIMATE: usize = 1200;

    /// Estimate tokens for a single message, counting everything the provider
    /// bills for: content, attachment images (flat per-image estimate), and
    /// tool-call names + argument JSON. +10 covers role/framing overhead.
    pub fn estimate_message_tokens(m: &crate::ai::llm::ChatMessage) -> usize {
        let mut tokens = Self::estimate_tokens(&m.content) + 10;
        if let Some(attachments) = &m.attachments {
            tokens += match serde_json::from_str::<Vec<crate::ai::llm::ImageAttachment>>(attachments) {
                Ok(list) => list.len() * Self::IMAGE_TOKEN_ESTIMATE,
                // Unparsable payloads fall back to the char heuristic, capped
                // so a corrupt blob can never nuke the whole budget either.
                Err(_) => Self::estimate_tokens(attachments).min(4 * Self::IMAGE_TOKEN_ESTIMATE),
            };
        }
        if let Some(tool_calls) = &m.tool_calls {
            for tc in tool_calls {
                tokens += Self::estimate_tokens(&tc.function.name)
                    + Self::estimate_tokens(&tc.function.arguments)
                    + 5;
            }
        }
        // Replayed thinking counts toward the input: DeepSeek concatenates it
        // into the context, so ignoring it here would let the real request
        // overflow the budget the truncator was respecting.
        if let Some(reasoning) = &m.reasoning_content {
            tokens += Self::estimate_tokens(reasoning);
        }
        tokens
    }

    /// Estimate total tokens in a list of messages
    pub fn estimate_messages_tokens(
        messages: &[crate::ai::llm::ChatMessage],
    ) -> usize {
        messages.iter().map(Self::estimate_message_tokens).sum()
    }

    /// Truncate messages to fit within the token budget.
    /// Keeps system prompt + most recent messages.
    /// Returns the truncated list.
    /// `max_tokens == 0` means no truncation.
    /// `reserved_tokens` is the headroom kept free for the model's OUTPUT
    /// (the per-round max_tokens cap), so the effective input budget is
    /// `max_tokens - reserved_tokens`.
    pub fn truncate(
        &self,
        messages: &[crate::ai::llm::ChatMessage],
        reserved_tokens: usize,
    ) -> Vec<crate::ai::llm::ChatMessage> {
        if self.max_tokens == 0 {
            return messages.to_vec();
        }

        let budget = self.max_tokens.saturating_sub(reserved_tokens);

        let mut result = Vec::new();
        let mut used = 0usize;

        // Always keep system message first
        if let Some(sys) = &self.system_prompt {
            let sys_tokens = Self::estimate_tokens(sys);
            used += sys_tokens + 10;
            result.push(crate::ai::llm::ChatMessage { reasoning_content: None,
                role: "system".to_string(),
                content: sys.clone(),
                attachments: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        // Keep the most recent messages, walking BACKWARDS in whole blocks.
        //
        // A block = an assistant message carrying tool_calls plus every tool
        // response that follows it. The API rejects a request where even one
        // response is missing, and a response is meaningless without the message
        // that asked for it, so a block is indivisible. Keeping blocks (instead
        // of pulling the assistant in as a "piggyback" on its last response and
        // then reversing the list) also preserves their order: the old code
        // emitted `tool(x), assistant(tool_calls=[x,y]), tool(y)`, which strict
        // providers answer with 400 "the following tool_call_ids did not have
        // response messages".
        let recent: Vec<&crate::ai::llm::ChatMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .collect();

        let mut kept: Vec<&[&crate::ai::llm::ChatMessage]> = Vec::new();
        let mut end = recent.len();
        while end > 0 {
            let start = Self::block_start(&recent, end);
            let block_tokens: usize = recent[start..end]
                .iter()
                .map(|m| Self::estimate_message_tokens(m))
                .sum();
            if used + block_tokens <= budget {
                used += block_tokens;
                kept.push(&recent[start..end]);
                end = start;
            } else {
                break;
            }
        }

        // Safety net: something has to survive even when the budget is already
        // blown, and it must be a message that can stand on its own. Dropping
        // everything leaves the model staring at a bare system prompt (it then
        // hallucinates from that instead of answering); keeping a lone tool
        // response is worse still, since it is unsendable by definition.
        if kept.is_empty() {
            if let Some(idx) = recent.iter().rposition(|m| m.role != "tool") {
                kept.push(std::slice::from_ref(&recent[idx]));
            }
        }

        // Add back in original order.
        for block in kept.iter().rev() {
            for msg in block.iter() {
                result.push((*msg).clone());
            }
        }

        // Final guarantee: a sendable tail. No orphan responses, no assistant
        // whose responses were left behind.
        Self::repair_tool_pairing(&mut result);

        result
    }

    /// Start index of the block that ends at `end` (exclusive).
    ///
    /// A block is a run of `tool` messages together with the assistant message
    /// that carries their `tool_calls`, when that assistant directly precedes
    /// the run.
    fn block_start(recent: &[&crate::ai::llm::ChatMessage], end: usize) -> usize {
        let mut start = end - 1;
        if recent[start].role == "tool" {
            while start > 0 && recent[start - 1].role == "tool" {
                start -= 1;
            }
            if start > 0
                && recent[start - 1].role == "assistant"
                && recent[start - 1].tool_calls.is_some()
            {
                start -= 1;
            }
        }
        start
    }

    /// Make a message list sendable by dropping tool groups that are not whole:
    /// `tool` messages nobody asked for, and an assistant whose `tool_calls` are
    /// not all answered immediately after it. Both shapes are hard 400s on
    /// strict providers (OpenAI, DeepSeek), and one split group would poison
    /// every later request of the turn, so this runs before every request as a
    /// last line of defence.
    pub fn repair_tool_pairing(messages: &mut Vec<crate::ai::llm::ChatMessage>) {
        let before = messages.len();
        let mut out: Vec<crate::ai::llm::ChatMessage> = Vec::with_capacity(before);
        let mut i = 0;
        while i < messages.len() {
            let msg = &messages[i];
            if msg.role == "assistant" && msg.tool_calls.is_some() {
                let ids: Vec<&str> = msg
                    .tool_calls
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                let mut j = i + 1;
                let mut answered: Vec<&str> = Vec::new();
                while j < messages.len() && messages[j].role == "tool" {
                    if let Some(id) = messages[j].tool_call_id.as_deref() {
                        answered.push(id);
                    }
                    j += 1;
                }
                if ids.iter().all(|id| answered.contains(id)) {
                    for m in &messages[i..j] {
                        out.push(m.clone());
                    }
                } else {
                    // We cannot invent the missing answer, so the group goes.
                    tracing::warn!(
                        missing = ids.len() - answered.len(),
                        "dropping an incomplete tool group before the request"
                    );
                }
                i = j;
                continue;
            }
            if msg.role == "tool" {
                // Orphan response: its assistant never made the request.
                i += 1;
                continue;
            }
            out.push(msg.clone());
            i += 1;
        }
        if out.len() != before {
            tracing::debug!(before, after = out.len(), "repaired tool pairing");
        }
        *messages = out;
    }
}

impl Default for ConversationMemory {
    fn default() -> Self {
        Self {
            max_tokens: 128_000,
            system_prompt: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        let english = "Hello, how are you?";
        let chinese = "你好，最近怎么样？";
        assert!(ConversationMemory::estimate_tokens(english) > 0);
        assert!(ConversationMemory::estimate_tokens(chinese) > 0);
    }

    #[test]
    fn test_truncate_keeps_recent() {
        let memory = ConversationMemory::new(500, Some("You are helpful.".into()));
        let messages: Vec<crate::ai::llm::ChatMessage> = (0..50)
            .map(|i| crate::ai::llm::ChatMessage { reasoning_content: None,
                role: "user".to_string(),
                content: format!("message {}", i),
                attachments: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
            .collect();

        let truncated = memory.truncate(&messages, 200);
        assert!(truncated.len() < messages.len());
        // System message should be first
        assert_eq!(truncated[0].role, "system");
    }

    // ── tool-group integrity ────────────────────────────────────────────────
    // Regression: a request that carries `tool_calls` without a response for
    // every id is a hard 400 on strict providers ("An assistant message with
    // 'tool_calls' must be followed by tool messages responding to each
    // 'tool_call_id'").

    fn msg(role: &str, content: &str) -> crate::ai::llm::ChatMessage {
        crate::ai::llm::ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        }
    }

    fn assistant_with_calls(ids: &[&str], filler: usize) -> crate::ai::llm::ChatMessage {
        let mut m = msg("assistant", &"t".repeat(filler));
        m.tool_calls = Some(
            ids.iter()
                .map(|id| crate::ai::llm::ToolCall {
                    id: (*id).to_string(),
                    call_type: "function".to_string(),
                    function: crate::ai::llm::FunctionCall {
                        name: "file_read".to_string(),
                        arguments: "{}".to_string(),
                    },
                })
                .collect(),
        );
        m
    }

    fn tool_result(id: &str, content: &str) -> crate::ai::llm::ChatMessage {
        let mut m = msg("tool", content);
        m.tool_call_id = Some(id.to_string());
        m
    }

    /// Every assistant tool_call must be answered right after it, in order, and
    /// every tool response must be preceded by its request.
    fn assert_sendable(messages: &[crate::ai::llm::ChatMessage]) {
        let mut i = 0;
        while i < messages.len() {
            let m = &messages[i];
            if m.role == "assistant" && m.tool_calls.is_some() {
                let ids: Vec<&str> = m
                    .tool_calls
                    .as_deref()
                    .unwrap()
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect();
                let mut j = i + 1;
                let mut answered: Vec<&str> = Vec::new();
                while j < messages.len() && messages[j].role == "tool" {
                    answered.push(messages[j].tool_call_id.as_deref().unwrap_or(""));
                    j += 1;
                }
                for id in &ids {
                    assert!(
                        answered.contains(id),
                        "tool_call {id} has no response: {:?}",
                        messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
                    );
                }
                i = j;
                continue;
            }
            assert_ne!(
                m.role, "tool",
                "orphan tool response: {:?}",
                messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
            );
            i += 1;
        }
    }

    /// Two responses for one assistant message must stay AFTER it, in their
    /// original order. The old keep-loop pulled the assistant in as a
    /// piggyback on the last response and reversed the list, producing
    /// `tool(x), assistant([x, y]), tool(y)`.
    #[test]
    fn test_truncate_keeps_tool_group_order() {
        let memory = ConversationMemory::new(100_000, Some("sys".into()));
        let messages = vec![
            msg("user", "do two reads"),
            assistant_with_calls(&["call_x", "call_y"], 20),
            tool_result("call_x", "first result"),
            tool_result("call_y", "second result"),
        ];

        let out = memory.truncate(&messages, 0);

        assert_sendable(&out);
        let roles: Vec<&str> = out.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["system", "user", "assistant", "tool", "tool"]);
        assert_eq!(out[3].tool_call_id.as_deref(), Some("call_x"));
        assert_eq!(out[4].tool_call_id.as_deref(), Some("call_y"));
    }

    /// A group that does not fit must be dropped whole: never a bare assistant
    /// with unanswered tool_calls, never a response whose request is gone.
    #[test]
    fn test_truncate_never_splits_a_tool_group() {
        // ~25 chars ≈ 10 tokens each; the budget fits the newest user message
        // plus roughly one block, never all of them.
        let memory = ConversationMemory::new(120, None);
        let messages = vec![
            msg("user", "old question"),
            assistant_with_calls(&["call_a", "call_b"], 400),
            tool_result("call_a", &"a".repeat(400)),
            tool_result("call_b", &"b".repeat(400)),
            msg("user", "new question"),
        ];

        let out = memory.truncate(&messages, 0);

        assert_sendable(&out);
        assert!(
            out.iter().any(|m| m.role == "user" && m.content == "new question"),
            "the current turn must survive: {:?}",
            out.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
        );
    }

    /// The repair pass is the last line of defence: whatever produced a broken
    /// pair, nothing unsendable may leave for the provider.
    #[test]
    fn test_repair_drops_incomplete_groups_and_orphans() {
        let mut messages = vec![
            msg("user", "hi"),
            // Assistant whose second call was never answered (a split group).
            assistant_with_calls(&["call_x", "call_y"], 0),
            tool_result("call_x", "only one answer"),
            // Unrelated orphan response.
            tool_result("call_z", "nobody asked"),
            msg("user", "next"),
        ];

        ConversationMemory::repair_tool_pairing(&mut messages);

        assert_sendable(&messages);
        let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "user"]);
    }

    /// A complete group is left exactly as it is.
    #[test]
    fn test_repair_keeps_complete_groups() {
        let mut messages = vec![
            msg("user", "hi"),
            assistant_with_calls(&["call_x"], 0),
            tool_result("call_x", "answer"),
            msg("assistant", "done"),
        ];
        let before = messages.len();

        ConversationMemory::repair_tool_pairing(&mut messages);

        assert_eq!(messages.len(), before);
        assert_sendable(&messages);
    }

    #[test]
    fn test_estimate_counts_tool_calls_and_attachments() {
        let plain = crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "assistant".to_string(),
            content: "hi".to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let with_extras = crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "assistant".to_string(),
            content: "hi".to_string(),
            attachments: Some("x".repeat(400)),
            tool_calls: Some(vec![crate::ai::llm::ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: crate::ai::llm::FunctionCall {
                    name: "file_read".to_string(),
                    arguments: format!("{{\"path\": \"{}\"}}", "y".repeat(400)),
                },
            }]),
            tool_call_id: None,
            name: None,
        };
        let plain_tokens = ConversationMemory::estimate_message_tokens(&plain);
        let extra_tokens = ConversationMemory::estimate_message_tokens(&with_extras);
        // ~800 extra chars must show up in the estimate, not just content.
        assert!(extra_tokens >= plain_tokens + 100);
        assert_eq!(
            ConversationMemory::estimate_messages_tokens(&[with_extras]),
            extra_tokens
        );
    }

    #[test]
    fn test_truncate_reserves_output_headroom() {
        // reserved_tokens is headroom for the model's OUTPUT: with
        // max_tokens=1000 and reserved=800 the input budget is only ~200
        // tokens, so old history must drop but the newest user message (the
        // current turn) must survive.
        let memory = ConversationMemory::new(1000, Some("sys".into()));
        let mut messages: Vec<crate::ai::llm::ChatMessage> = (0..100)
            .map(|i| crate::ai::llm::ChatMessage { reasoning_content: None,
                role: "user".to_string(),
                content: format!("old message {i} {}", "z".repeat(40)),
                attachments: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
            .collect();
        messages.push(crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "user".to_string(),
            content: "current question".to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        let truncated = memory.truncate(&messages, 800);
        assert!(truncated.len() < messages.len());
        assert_eq!(truncated[0].role, "system");
        assert_eq!(truncated.last().unwrap().content, "current question");

        // A bogus call that reserves the ENTIRE context budget used to leave
        // zero input budget and drop even the current message; the reserved
        // value must come from the output cap, not the context budget.
        let sane = memory.truncate(&messages, 200);
        assert!(sane.len() > truncated.len());
        assert_eq!(sane.last().unwrap().content, "current question");
    }

    #[test]
    fn test_truncate_drops_orphaned_tool_messages() {
        // A tool group split by budget: the tool responses fit individually,
        // but the assistant carrying their tool_calls does not. The truncated
        // list must not start with (or contain) a bare tool message — strict
        // providers (DeepSeek) 400 the whole request, and every later round
        // re-hits the same split.
        let memory = ConversationMemory::new(1000, Some("sys".into()));
        let big = "y".repeat(1200); // ~300 tokens per message
        let user_msg = |content: &str| crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "user".to_string(),
            content: content.to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let tool_msg = |content: String| crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "tool".to_string(),
            content,
            attachments: None,
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("file_read".to_string()),
        };
        let assistant_tc = crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "assistant".to_string(),
            content: String::new(),
            attachments: None,
            tool_calls: Some(vec![crate::ai::llm::ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: crate::ai::llm::FunctionCall {
                    name: "file_read".to_string(),
                    arguments: format!("{{\"path\": \"{big}\"}}"),
                },
            }]),
            tool_call_id: None,
            name: None,
        };
        let messages = vec![
            user_msg("old question"),
            assistant_tc,
            tool_msg(big.clone()),
            tool_msg(big),
            user_msg("latest"),
        ];

        let truncated = memory.truncate(&messages, 200);
        // Validity invariant: every tool message must follow (directly or via
        // sibling tool messages) an assistant carrying tool_calls.
        let mut group_open = false;
        for m in &truncated {
            match m.role.as_str() {
                "assistant" => group_open = m.tool_calls.is_some(),
                "tool" => assert!(group_open, "orphaned tool message survived truncation: {truncated:?}"),
                _ => group_open = false,
            }
        }
        assert_eq!(truncated.last().unwrap().content, "latest");
    }

    #[test]
    fn test_image_attachment_estimated_flat_not_by_base64_len() {
        // A 300k-char base64 payload is ~1-2k tokens to a vision API, not
        // 75k. Char-counting it once exceeded the whole input budget and got
        // the current user message dropped (model saw an empty human turn).
        let attachments = serde_json::json!([{
            "mime": "image/png",
            "base64": "A".repeat(300_000),
        }])
        .to_string();
        let msg = crate::ai::llm::ChatMessage { reasoning_content: None,
            role: "user".to_string(),
            content: "描述图片".to_string(),
            attachments: Some(attachments),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let est = ConversationMemory::estimate_message_tokens(&msg);
        assert!(
            est < 2_000,
            "image estimate should be flat (~1200), got {est}"
        );
    }

    #[test]
    fn test_truncate_never_drops_current_message() {
        // Even if the newest message alone exceeds the input budget, it must
        // survive — otherwise the model receives system prompt only and
        // hallucinates instead of answering.
        let memory = ConversationMemory::new(1000, Some("sys".into()));
        let attachments = serde_json::json!([{
            "mime": "image/png",
            "base64": "A".repeat(50_000),
        }])
        .to_string();
        let messages = vec![
            crate::ai::llm::ChatMessage { reasoning_content: None,
                role: "user".to_string(),
                content: "old question".to_string(),
                attachments: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            crate::ai::llm::ChatMessage { reasoning_content: None,
                role: "user".to_string(),
                content: "描述图片".to_string(),
                attachments: Some(attachments),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        // reserved=800 → input budget 200; the image message (~1200+) cannot
        // fit, but must still be kept.
        let truncated = memory.truncate(&messages, 800);
        let last = truncated.last().unwrap();
        assert_eq!(last.role, "user");
        assert_eq!(last.content, "描述图片");
        assert!(last.attachments.is_some());
    }
}
