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

    /// Estimate total tokens in a list of messages
    pub fn estimate_messages_tokens(
        messages: &[crate::ai::llm::ChatMessage],
    ) -> usize {
        messages
            .iter()
            .map(|m| Self::estimate_tokens(&m.content) + 10) // +10 for role overhead
            .sum()
    }

    /// Truncate messages to fit within the token budget.
    /// Keeps system prompt + most recent messages.
    /// Returns the truncated list.
    /// `max_tokens == 0` means no truncation.
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

            let pair_tokens = Self::estimate_tokens(&msg.content) + 10
                + extra.map(|m| Self::estimate_tokens(&m.content) + 10).unwrap_or(0);

            if used + pair_tokens <= budget {
                if let Some(e) = extra {
                    used += Self::estimate_tokens(&e.content) + 10;
                    kept_recent.push(e);
                    i += 1;
                }
                used += Self::estimate_tokens(&msg.content) + 10;
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
}
