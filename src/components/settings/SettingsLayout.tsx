import { useState } from 'react';
import { HardDrive, Bot, Cpu, SlidersHorizontal, Cat, RefreshCw, Search, Layers, Palette } from 'lucide-react';

interface Section {
  key: string;
  label: string;
  icon: React.ReactNode;
  component: React.ReactNode;
}

interface Props {
  sections: Section[];
}

let lastSection: string | null = null;

export function SettingsLayout({ sections }: Props) {
  const [active, setActive] = useState(() => {
    return lastSection && sections.some((s) => s.key === lastSection) ? lastSection : sections[0]?.key;
  });
  const selectSection = (key: string) => {
    lastSection = key;
    setActive(key);
  };
  const activeSection = sections.find((s) => s.key === active) ?? sections[0];

  return (
    <div className="h-full flex flex-col bg-background text-text-primary">
      <div className="flex-1 flex overflow-hidden">
        {/* Sidebar */}
        <aside className="w-56 shrink-0 border-r border-surface-hover bg-background overflow-y-auto">
          <nav className="p-2 space-y-0.5">
            {sections.map((section) => (
              <button
                key={section.key}
                onClick={() => selectSection(section.key)}
                className={`w-full flex items-center gap-3 px-3 py-2 rounded-lg text-[13px] transition-colors text-left ${
                  active === section.key
                    ? 'bg-surface text-text-primary'
                    : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
                }`}
              >
                {section.icon}
                {section.label}
              </button>
            ))}
          </nav>
        </aside>

        {/* Content */}
        <main className="flex-1 overflow-y-auto p-8">
          <div className="max-w-2xl mx-auto">
            {activeSection?.component}
          </div>
        </main>
      </div>
    </div>
  );
}

export function GeneralIcon() { return <HardDrive size={16} />; }
export function AppearanceIcon() { return <Palette size={16} />; }
export function LlmIcon() { return <Cpu size={16} />; }
export function AgentIcon() { return <Bot size={16} />; }
export function PetIcon() { return <Cat size={16} />; }
export function DomainsIcon() { return <Layers size={16} />; }
export function SearchIcon() { return <Search size={16} />; }
export function AdvancedIcon() { return <SlidersHorizontal size={16} />; }
export function SyncIcon() { return <RefreshCw size={16} />; }
