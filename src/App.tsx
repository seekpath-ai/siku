import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import './App.css'

function App() {
  const [greetMsg, setGreetMsg] = useState('')
  const [name, setName] = useState('')

  async function greet() {
    const msg = await invoke<string>('greet', { name })
    setGreetMsg(msg)
  }

  return (
    <main className="min-h-screen bg-[#1A1A1E] text-[#F5F5F5] flex flex-col items-center justify-center p-4">
      <h1 className="text-4xl font-bold mb-8 text-[#E67E22]">思库</h1>
      <p className="text-[#9CA3AF] mb-8">让灵感涌动</p>
      <div className="flex gap-2 mb-4">
        <input
          className="px-4 py-2 rounded bg-[#24242B] border border-[#2E2E36] focus:border-[#E67E22] outline-none"
          onChange={(e) => setName(e.currentTarget.value)}
          placeholder="输入你的名字"
        />
        <button
          className="px-4 py-2 rounded bg-[#E67E22] text-white hover:bg-[#D35400] transition-colors"
          onClick={greet}
        >
          问候
        </button>
      </div>
      {greetMsg && <p className="text-lg">{greetMsg}</p>}
    </main>
  )
}

export default App
