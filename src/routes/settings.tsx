import { createRoute } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { SettingsLayout, GeneralIcon, LlmIcon, AgentIcon, PetIcon, AdvancedIcon, SyncIcon } from '@/components/settings/SettingsLayout';
import { LlmProviderSettings } from '@/components/settings/LlmProviderSettings';
import { GeneralSettings } from '@/components/settings/GeneralSettings';
import { AgentDefaultsSettings } from '@/components/settings/AgentDefaultsSettings';
import { PetSettings } from '@/components/settings/PetSettings';
import { AdvancedSettings } from '@/components/settings/AdvancedSettings';
import { SyncSettings } from '@/components/settings/SyncSettings';

function SettingsPage() {
  return (
    <SettingsLayout
      sections={[
        { key: 'general', label: '通用', icon: <GeneralIcon />, component: <GeneralSettings /> },
        { key: 'llm', label: '模型提供商', icon: <LlmIcon />, component: <LlmProviderSettings /> },
        { key: 'agent', label: '智能体默认值', icon: <AgentIcon />, component: <AgentDefaultsSettings /> },
        { key: 'pet', label: '宠物', icon: <PetIcon />, component: <PetSettings /> },
        { key: 'advanced', label: '高级', icon: <AdvancedIcon />, component: <AdvancedSettings /> },
        { key: 'sync', label: '同步', icon: <SyncIcon />, component: <SyncSettings /> },
      ]}
    />
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/settings',
  component: SettingsPage,
});
