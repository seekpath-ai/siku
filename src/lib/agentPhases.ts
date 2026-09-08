import type { AgentPhase, AgentStep, StreamingStep, ToolCallInfo } from '@/lib/types';

/** Parse the persisted tool_calls JSON of an AgentStep; a malformed or
 *  non-array payload degrades to no calls. */
export function parseToolCalls(json: string | null): ToolCallInfo[] {
  if (!json) return [];
  try {
    const parsed = JSON.parse(json);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

/** Flatten streaming steps into reasoning / tool-call phases. */
export function streamingToPhases(steps: StreamingStep[], current: StreamingStep | null): AgentPhase[] {
  const phases: AgentPhase[] = [];
  for (const step of [...steps, ...(current ? [current] : [])]) {
    if (step.reasoning_content.trim()) {
      phases.push({ kind: 'reasoning', step_index: step.step_index, content: step.reasoning_content });
    }
    for (const tc of step.tool_calls) {
      phases.push({ kind: 'tool_call', step_index: step.step_index, toolCall: tc });
    }
  }
  return phases;
}

/** Flatten PERSISTED agent steps (history) into the same phase shape, so
 *  earlier turns keep their tool-call cards after later turns start. */
export function stepsToPhases(steps: AgentStep[]): AgentPhase[] {
  const phases: AgentPhase[] = [];
  for (const step of steps) {
    if (step.reasoning_content?.trim()) {
      phases.push({ kind: 'reasoning', step_index: step.step_index, content: step.reasoning_content });
    }
    for (const tc of parseToolCalls(step.tool_calls)) {
      phases.push({ kind: 'tool_call', step_index: step.step_index, toolCall: tc });
    }
  }
  return phases;
}
