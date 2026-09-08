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

    /// Estimate tokens for a single message, counting everything the provider
    /// bills for: content, attachment payloads (base64 images are huge), and
    /// tool-call names + argument JSON. +10 covers role/framing overhead.
    pub fn estimate_message_tokens(m: &crate::ai::llm::ChatMessage) -> usize {
        let mut tokens = Self::estimate_tokens(&m.content) + 10;
        if let Some(attachments) = &m.attachments {
            tokens += Self::estimate_tokens(attachments);
        }
        if let Some(tool_calls) = &m.tool_calls {
            for tc in tool_calls {
                tokens += Self::estimate_tokens(&tc.function.name)
                    + Self::estimate_tokens(&tc.function.arguments)
                    + 5;
            }
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
            result.push(crate::ai::llm::ChatMessage {
                role: "system".to_string(),
                content: sys.clone(),
                attachments: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        // Keep most recent messages first (reverse iterate)
        let recent: Vec<&crate::ai::llm::ChatMessage> = messages
            .iter()
            .filter(|m| m.role != "system")
            .collect();

        // Take from the end (most recent), keeping tool+assistant pairs together
        let mut kept_recent: Vec<&crate::ai::llm::ChatMessage> = Vec::new();
        let mut i = 0;
        while i < recent.len() {
            let msg = &recent[recent.len() - 1 - i];
            // If this is a tool message, also keep the preceding assistant message
            let extra = if msg.role == "tool" && i + 1 < recent.len() {
                let prev = &recent[recent.len() - 2 - i];
                if prev.role == "assistant" && prev.tool_calls.is_some() {
                    Some(prev)
                } else {
                    None
                }
            } else {
                None
            };

            let pair_tokens = Self::estimate_message_tokens(msg)
                + extra.map(|m| Self::estimate_message_tokens(m)).unwrap_or(0);

            if used + pair_tokens <= budget {
                if let Some(e) = extra {
                    used += Self::estimate_message_tokens(e);
                    kept_recent.push(e);
                    i += 1;
                }
                used += Self::estimate_message_tokens(msg);
                kept_recent.push(msg);
                i += 1;
            } else {
                break;
            }
        }

        // Add back in original order
        for msg in kept_recent.iter().rev() {
            result.push((*msg).clone());
        }

        result
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
            .map(|i| crate::ai::llm::ChatMessage {
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

    #[test]
    fn test_estimate_counts_tool_calls_and_attachments() {
        let plain = crate::ai::llm::ChatMessage {
            role: "assistant".to_string(),
            content: "hi".to_string(),
            attachments: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let with_extras = crate::ai::llm::ChatMessage {
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
            .map(|i| crate::ai::llm::ChatMessage {
                role: "user".to_string(),
                content: format!("old message {i} {}", "z".repeat(40)),
                attachments: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            })
            .collect();
        messages.push(crate::ai::llm::ChatMessage {
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
}
