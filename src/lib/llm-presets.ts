import type { LlmConfigBlock } from './types';

export interface LlmPreset {
  label: string;
  provider: string;
  models: string[];
  baseURL: string;
  apiKeyEnv?: string;
}

export const LLM_PRESETS: LlmPreset[] = [
  {
    label: 'DeepSeek',
    provider: 'deepseek',
    models: ['deepseek-v4-pro', 'deepseek-v4-flash', 'deepseek-v4-flash-vision-exp'],
    baseURL: 'https://api.deepseek.com/v1',
    apiKeyEnv: 'DEEPSEEK_API_KEY',
  },
  {
    label: 'OpenAI',
    provider: 'openai',
    models: ['gpt-4o', 'gpt-4o-mini', 'gpt-4.1', 'o4-mini'],
    baseURL: 'https://api.openai.com/v1',
    apiKeyEnv: 'OPENAI_API_KEY',
  },
  {
    label: 'Anthropic',
    provider: 'anthropic',
    models: ['claude-sonnet-4-20250514', 'claude-opus-4', 'claude-3-5-haiku'],
    baseURL: 'https://api.anthropic.com',
    apiKeyEnv: 'ANTHROPIC_API_KEY',
  },
  {
    label: 'Qwen / 通义千问',
    provider: 'qwen',
    models: ['qwen-max', 'qwen-plus', 'qwen-turbo', 'qwen3-235b-a22b'],
    baseURL: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    apiKeyEnv: 'DASHSCOPE_API_KEY',
  },
  {
    label: 'Zhipu / 智谱',
    provider: 'zhipu',
    models: ['glm-4-plus', 'glm-4-flash', 'glm-4-air', 'glm-4.5'],
    baseURL: 'https://open.bigmodel.cn/api/paas/v4',
    apiKeyEnv: 'ZHIPU_API_KEY',
  },
  {
    label: 'Kimi / 月之暗面',
    provider: 'kimi',
    models: ['moonshot-v1-auto', 'moonshot-v1-8k', 'moonshot-v1-32k', 'kimi-latest'],
    baseURL: 'https://api.moonshot.cn/v1',
    apiKeyEnv: 'MOONSHOT_API_KEY',
  },
  {
    label: 'Gemini',
    provider: 'gemini',
    models: ['gemini-2.5-pro', 'gemini-2.5-flash', 'gemini-2.0-flash'],
    baseURL: 'https://generativelanguage.googleapis.com/v1beta/openai',
    apiKeyEnv: 'GEMINI_API_KEY',
  },
  {
    label: 'Ollama (local)',
    provider: 'ollama',
    models: ['llama3', 'qwen3', 'mistral', 'codellama', 'deepseek-r1'],
    baseURL: 'http://127.0.0.1:11434/v1',
    apiKeyEnv: undefined,
  },
  {
    label: 'SiliconFlow',
    provider: 'siliconflow',
    models: ['deepseek-ai/DeepSeek-V3', 'Qwen/Qwen3-235B', 'Pro/Llama-4-Maverick'],
    baseURL: 'https://api.siliconflow.cn/v1',
    apiKeyEnv: 'SILICONFLOW_API_KEY',
  },
];

export function findPreset(provider: string): LlmPreset | undefined {
  return LLM_PRESETS.find((p) => p.provider === provider);
}

export function findPresetByURL(baseURL: string): LlmPreset | undefined {
  return LLM_PRESETS.find((p) => p.baseURL === baseURL);
}

export function defaultLlmBlock(): LlmConfigBlock {
  return {
    provider: 'deepseek',
    model: 'deepseek-v4-flash',
    api_key: '',
    base_url: 'https://api.deepseek.com/v1',
  };
}
